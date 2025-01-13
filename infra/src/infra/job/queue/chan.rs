use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use crate::infra::job::rows::UseJobqueueAndCodec;
use crate::infra::JobQueueConfig;
use crate::{error::JobWorkerError, infra::UseJobQueueConfig};
use anyhow::Result;
use async_trait::async_trait;
use command_utils::util::option::FlatMap as _;
use command_utils::util::result::FlatMap as _;
use futures::FutureExt;
use infra_utils::infra::chan::mpmc::{Chan, UseChanBuffer};
use infra_utils::infra::chan::{ChanBuffer, ChanBufferItem};
use proto::jobworkerp::data::{Job, JobId, JobResult, JobResultData, JobResultId, Priority};
use signal_hook::consts::SIGINT;
use signal_hook_tokio::Signals;

#[async_trait]
pub trait ChanJobQueueRepository:
    UseChanBuffer<Item = Vec<u8>> + UseJobqueueAndCodec + UseJobQueueConfig + Sync + 'static
{
    // for front (send job to worker)
    // return: jobqueue size
    #[inline]
    async fn enqueue_job(&self, channel_name: Option<&String>, job: &Job) -> Result<i64>
    where
        Self: Send + Sync,
    {
        let cn = channel_name
            .unwrap_or(&Self::DEFAULT_CHANNEL_NAME.to_string())
            .to_owned();
        let qn = Self::queue_channel_name(cn, job.data.as_ref().map(|d| &d.priority));
        match self
            .chan_buf()
            .send_to_chan(
                &qn,
                Self::serialize_job(job),
                job.data.as_ref().flat_map(|d| d.uniq_key.clone()),
                None,
                false,
            ) // expect for multiple value
            .await
        {
            Ok(_) => Ok(self.chan_buf().count_chan_opt(qn).await.unwrap_or(0) as i64),
            Err(e) => Err(JobWorkerError::ChanError(e).into()),
        }
    }

    // channel names are ordered by priority (first is highest)
    #[inline]
    async fn receive_job_from_channels(&self, channel_names: Vec<String>) -> Result<Job>
    where
        Self: Send + Sync,
    {
        // receive from multiple channels immediately (if queue is empty, select each channel for waiting)
        for cn in &channel_names {
            tracing::debug!("receive_job_from_channels: channel: {:?}", cn);
            match self.chan_buf().try_receive_from_chan(cn, None).await {
                Ok(v) => {
                    return Self::deserialize_job(&v);
                }
                Err(e) => {
                    // channel is empty (or other error)
                    tracing::trace!("try_receive_job_from_channels: {:?}", e);
                }
            }
        }
        let (res, _idx, _l) = futures::future::select_all(
            channel_names
                .iter()
                .map(|cn| self.chan_buf().receive_from_chan(cn, None, None).boxed()),
        )
        .await;
        res.map_err(|e| JobWorkerError::ChanError(e).into())
            .and_then(|v| Self::deserialize_job(&v))
    }

    // send job result from worker to front directly
    #[inline]
    async fn enqueue_result_direct(&self, id: &JobResultId, res: &JobResultData) -> Result<bool> {
        let v = Self::serialize_job_result(*id, res.clone());
        if let Some(jid) = res.job_id.as_ref() {
            let cn = Self::result_queue_name(jid);
            tracing::debug!("send_result_direct: job_id: {:?}, queue: {}", jid, &cn);
            // job id based queue (onetime, use ttl)
            let _ = self
                .chan_buf()
                .send_to_chan(
                    &cn,
                    v,
                    None,
                    // set expire for not calling listen_after api
                    Some(&Duration::from_secs(
                        self.job_queue_config().expire_job_result_seconds as u64,
                    )),
                    true, // only if exists
                )
                .await
                .map_err(JobWorkerError::ChanError)?;
            Ok(true)
        } else {
            tracing::warn!("job_id is not set in job_result: {:?}", res);
            Ok(false)
        }
    }

    // wait response from worker for direct response job
    // TODO shutdown lock until receive result ? (but not recorded...)
    #[inline]
    async fn wait_for_result_queue_for_response(
        &self,
        job_id: &JobId,
        timeout: Option<&u64>,
    ) -> Result<JobResult> {
        // TODO retry control
        let nm = Self::result_queue_name(job_id);
        tracing::debug!(
            "wait_for_result_data_for_response: job_id: {:?}, queue:{}",
            job_id,
            &nm
        );
        let signal: Signals = Signals::new([SIGINT]).expect("cannot get signals");
        let handle = signal.handle();
        let res = tokio::select! {
            _ = tokio::spawn(async {
                let mut stream = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).expect("signal error");
                stream.recv().await
            }) => {
                handle.close();
                Err(JobWorkerError::OtherError("interrupt direct waiting process".to_string()).into())
            },
            val = self.chan_buf().receive_from_chan(nm, timeout.flat_map(|t| if *t == 0 {None} else {Some(Duration::from_millis(*t))}), None) => {
                let r: Result<JobResult> = val.map_err(|e|JobWorkerError::ChanError(e).into())
                    .flat_map(|v| Self::deserialize_job_result(&v));
                r
            },
        };
        tracing::debug!("wait_for_result_queue_for_response: got res: {:?}", res);
        res
    }
    // TODO?
    // cannot iterate channel buffer
    async fn find_from_queue(
        &self,
        _channel: Option<&String>,
        _priority: Priority,
        _id: &JobId,
    ) -> Result<Option<Job>> {
        Ok(None)
    }
    // cannot iterate channel buffer
    async fn find_multi_from_queue(
        &self,
        _channel: Option<&String>,
        _priority: Priority,
        _ids: &HashSet<i64>,
    ) -> Result<Vec<Job>> {
        Ok(vec![])
    }

    async fn delete_from_queue(
        &self,
        _channel: Option<&String>,
        _priority: Priority,
        _job: &Job,
    ) -> Result<i32> {
        Ok(0)
    }
    async fn count_queue(&self, channel: Option<&String>, priority: Priority) -> Result<i64> {
        let c = Self::queue_channel_name(
            channel.unwrap_or(&Self::DEFAULT_CHANNEL_NAME.to_string()),
            Some(priority as i32).as_ref(),
        );
        Ok(self.chan_buf().count_chan_opt(c).await.unwrap_or(0) as i64)
    }
}

#[derive(Clone, Debug)]
pub struct ChanJobQueueRepositoryImpl {
    pub chan_pool: ChanBuffer<Vec<u8>, Chan<ChanBufferItem<Vec<u8>>>>,
    pub job_queue_config: Arc<JobQueueConfig>,
}
impl UseChanBuffer for ChanJobQueueRepositoryImpl {
    type Item = Vec<u8>;
    fn chan_buf(&self) -> &ChanBuffer<Vec<u8>, Chan<ChanBufferItem<Vec<u8>>>> {
        &self.chan_pool
    }
}
impl UseJobQueueConfig for ChanJobQueueRepositoryImpl {
    fn job_queue_config(&self) -> &JobQueueConfig {
        &self.job_queue_config
    }
}

impl UseJobqueueAndCodec for ChanJobQueueRepositoryImpl {}
impl ChanJobQueueRepository for ChanJobQueueRepositoryImpl {}
impl ChanJobQueueRepositoryImpl {
    pub fn new(
        job_queue_config: Arc<JobQueueConfig>,
        chan_pool: ChanBuffer<Vec<u8>, Chan<ChanBufferItem<Vec<u8>>>>,
    ) -> Self {
        ChanJobQueueRepositoryImpl {
            chan_pool,
            job_queue_config,
        }
    }
}
pub trait UseChanJobQueueRepository {
    fn chan_job_queue_repository(&self) -> &ChanJobQueueRepositoryImpl;
}

#[cfg(test)]
// create test (functional test without mock)
mod test {
    use super::*;
    use crate::infra::job::rows::JobqueueAndCodec;
    use crate::infra::JobQueueConfig;
    use command_utils::util::datetime;
    use infra_utils::infra::chan::mpmc::Chan;
    use infra_utils::infra::chan::mpmc::UseChanBuffer;
    use infra_utils::infra::chan::ChanBuffer;
    use infra_utils::infra::chan::ChanBufferItem;
    use proto::jobworkerp::data::JobResultData;
    use proto::jobworkerp::data::ResultOutput;
    use proto::jobworkerp::data::{Job, JobData, JobId, ResultStatus, WorkerId};
    use std::sync::Arc;

    #[derive(Clone)]
    struct ChanJobQueueRepositoryImpl {
        job_queue_config: Arc<JobQueueConfig>,
        chan_buf: ChanBuffer<Vec<u8>, Chan<ChanBufferItem<Vec<u8>>>>,
    }
    impl UseJobQueueConfig for ChanJobQueueRepositoryImpl {
        fn job_queue_config(&self) -> &JobQueueConfig {
            &self.job_queue_config
        }
    }
    impl UseChanBuffer for ChanJobQueueRepositoryImpl {
        type Item = Vec<u8>;
        fn chan_buf(&self) -> &ChanBuffer<Vec<u8>, Chan<ChanBufferItem<Vec<u8>>>> {
            &self.chan_buf
        }
    }
    impl UseJobqueueAndCodec for ChanJobQueueRepositoryImpl {}
    impl ChanJobQueueRepository for ChanJobQueueRepositoryImpl {}

    #[tokio::test]
    async fn send_job_test() -> Result<()> {
        let chan_buf = ChanBuffer::new(None, 10000);
        let job_queue_config = Arc::new(JobQueueConfig {
            expire_job_result_seconds: 10,
            fetch_interval: 1000,
        });
        let repo = ChanJobQueueRepositoryImpl {
            job_queue_config,
            chan_buf: chan_buf.clone(),
        };
        let args = ChanJobQueueRepositoryImpl::serialize_message(&proto::TestArgs {
            args: vec!["test".to_string()],
        });
        let job = Job {
            id: None,
            data: Some(JobData {
                worker_id: Some(WorkerId { value: 1 }),
                args,
                uniq_key: Some("test".to_string()),
                enqueue_time: datetime::now_millis(),
                grabbed_until_time: None,
                run_after_time: 0i64,
                retried: 0,
                priority: 1,
                timeout: 1000,
            }),
        };
        let r = repo.enqueue_job(None, &job).await?;
        assert_eq!(r, 1);

        assert_eq!(
            chan_buf
                .get_chan_if_exists(ChanJobQueueRepositoryImpl::queue_channel_name(
                    ChanJobQueueRepositoryImpl::DEFAULT_CHANNEL_NAME,
                    Some(&1),
                ))
                .await
                .unwrap()
                .count(),
            1
        );
        Ok(())
    }
    // test of 'send_result()': store job result with send_result() to chan and get job result value from wait_for_result_data_directly()
    #[tokio::test]
    async fn send_result_test() -> Result<()> {
        let chan_buf = ChanBuffer::new(None, 10000);
        let job_queue_config = Arc::new(JobQueueConfig {
            expire_job_result_seconds: 10,
            fetch_interval: 1000,
        });
        let repo = Arc::new(ChanJobQueueRepositoryImpl {
            job_queue_config,
            chan_buf,
        });
        let job_result_id = JobResultId { value: 111 };
        let job_id = JobId { value: 1 };
        let job_result_data = JobResultData {
            job_id: Some(job_id),
            status: ResultStatus::Success as i32,
            output: Some(ResultOutput {
                items: vec!["test".as_bytes().to_owned()],
            }),
            timeout: 2000,
            enqueue_time: datetime::now_millis() - 10000,
            run_after_time: datetime::now_millis() - 10000,
            start_time: datetime::now_millis() - 1000,
            end_time: datetime::now_millis(),
            ..Default::default()
        };
        // let r = repo.send_result_direct(job_result_data.clone()).await?;
        // assert!(r);
        // let res = repo.wait_for_result_data_for_response(&job_id).await?;
        let repo2 = repo.clone();
        let jr2 = job_result_data.clone();
        let jh = tokio::task::spawn(async move {
            let res = repo2
                .wait_for_result_queue_for_response(&job_id, None)
                .await
                .unwrap();
            assert_eq!(res.data.unwrap(), jr2);
        });
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        let r = repo
            .enqueue_result_direct(&job_result_id, &job_result_data)
            .await?;
        assert!(r);
        jh.await?;

        Ok(())
    }

    // test of 'receive_job_from_channels()' : get job from chan and check the value (priority)
    #[tokio::test]
    async fn send_jobs_and_receive_job_test() -> Result<()> {
        let chan_buf = ChanBuffer::new(None, 10000);
        let job_queue_config = Arc::new(JobQueueConfig {
            expire_job_result_seconds: 10,
            fetch_interval: 1000,
        });
        let repo = ChanJobQueueRepositoryImpl {
            job_queue_config,
            chan_buf,
        };
        let args = JobqueueAndCodec::serialize_message(&proto::TestArgs {
            args: vec!["test".to_string()],
        });
        let job1 = Job {
            id: Some(JobId { value: 1 }),
            data: Some(JobData {
                worker_id: Some(WorkerId { value: 1 }),
                args: args.clone(),
                uniq_key: None,
                enqueue_time: datetime::now_millis(),
                grabbed_until_time: None,
                run_after_time: 0i64,
                retried: 0,
                priority: 1,
                timeout: 1000,
            }),
        };
        let job2 = Job {
            id: Some(JobId { value: 2 }),
            data: Some(JobData {
                worker_id: Some(WorkerId { value: 1 }),
                args: args.clone(),
                uniq_key: None,
                enqueue_time: datetime::now_millis(),
                grabbed_until_time: None,
                run_after_time: 0i64,
                retried: 0,
                priority: 2,
                timeout: 1000,
            }),
        };
        let r = repo.enqueue_job(None, &job1).await?;
        assert_eq!(r, 1);
        let r = repo.enqueue_job(None, &job2).await?;
        assert_eq!(r, 1);
        let cn1 = ChanJobQueueRepositoryImpl::queue_channel_name(
            ChanJobQueueRepositoryImpl::DEFAULT_CHANNEL_NAME,
            Some(&1),
        );
        let cn2 = ChanJobQueueRepositoryImpl::queue_channel_name(
            ChanJobQueueRepositoryImpl::DEFAULT_CHANNEL_NAME,
            Some(&2),
        );
        let c = vec![cn2, cn1];
        // receive job from multiple channels
        let res = repo.receive_job_from_channels(c.clone()).await?;
        assert_eq!(res.id.unwrap(), job2.id.unwrap());
        let res = repo.receive_job_from_channels(c).await?;
        assert_eq!(res.id.unwrap(), job1.id.unwrap());
        Ok(())
    }
}
