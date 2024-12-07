use std::{pin::Pin, sync::Arc, time::Duration};

use anyhow::Result;
use async_trait::async_trait;
use command_utils::util::result::TapErr;
use futures::{Stream, StreamExt};
use infra_utils::infra::chan::{
    broadcast::{BroadcastChan, UseBroadcastChanBuffer},
    ChanBuffer, ChanBufferItem,
};
use proto::jobworkerp::data::{JobId, JobResult, JobResultData, JobResultId, WorkerId};

use crate::{
    error::JobWorkerError,
    infra::{job::rows::UseJobqueueAndCodec, JobQueueConfig, UseJobQueueConfig},
};

use super::{JobResultPublisher, JobResultSubscriber};

/// publish job result to channel
/// (note: not broadcast to all receiver: assume single instance use)
#[async_trait]
impl JobResultPublisher for ChanJobResultPubSubRepositoryImpl {
    async fn publish_result(
        &self,
        id: &JobResultId,
        data: &JobResultData,
        to_listen: bool,
    ) -> Result<bool> {
        // TODO send to worker id channel if listening clients exist
        // chan.receiver_count()
        let jid = data
            .job_id
            .as_ref()
            .ok_or(JobWorkerError::InvalidParameter(format!(
                "job_id not found: result_id={}",
                &id.value
            )))?;
        tracing::debug!(
            "publish_result: job_id={}, result_id={}",
            &jid.value,
            &id.value
        );
        let wid = data
            .worker_id
            .as_ref()
            .ok_or(JobWorkerError::InvalidParameter(format!(
                "worker_id not found: job_id={}",
                &jid.value
            )))?;
        let result_data = Self::serialize_job_result(*id, data.clone());
        let res = if to_listen {
            self.broadcast_chan_buf()
                .send_to_chan(
                    Self::job_result_pubsub_channel_name(jid).as_str(),
                    result_data.clone(),
                    None,
                    Some(&Duration::from_secs(
                        self.job_queue_config().expire_job_result_seconds as u64,
                    )),
                    true,
                )
                .await
                .inspect(|&r| {
                    if !r {
                        tracing::warn!("publish_result: not sent: job_id={}", &jid.value);
                    }
                })
        } else {
            Ok(false)
        };
        let res2 = self
            .broadcast_chan_buf()
            .send_to_chan(
                Self::job_result_by_worker_pubsub_channel_name(wid).as_str(),
                result_data,
                None,
                None,
                true,
            )
            .await
            .inspect(|&r| {
                if !r {
                    tracing::warn!("publish_result: not sent for stream: job_id={}", &jid.value);
                }
            })
            .inspect_err(|e| tracing::warn!("send_to_chan_err:{:?}", e))?;
        res.map(|r| r || res2)
    }
}

#[async_trait]
impl JobResultSubscriber for ChanJobResultPubSubRepositoryImpl {
    // subscribe job result of listen after by all listening client using broadcast_chan and return got result immediately
    async fn subscribe_result(&self, job_id: &JobId, timeout: Option<u64>) -> Result<JobResult> {
        let cn = Self::job_result_pubsub_channel_name(job_id);
        tracing::debug!("subscribe_result: job_id={}, ch={}", &job_id.value, &cn);

        let message = self
            .broadcast_chan_buf()
            .receive_from_chan(
                cn.as_str(),
                timeout.map(Duration::from_millis),
                Some(&Duration::from_secs(
                    self.job_queue_config().expire_job_result_seconds as u64,
                )),
            )
            .await
            .tap_err(|e| tracing::error!("receive_from_chan_err:{:?}", e))?;
        let res = Self::deserialize_job_result(&message)
            .tap_err(|e| tracing::error!("deserialize_result:{:?}", e))?;
        tracing::debug!("subscribe_result_received: result={:?}", &res);
        Ok(res)
    }
    async fn subscribe_result_stream_by_worker(
        &self,
        worker_id: WorkerId,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<JobResult>> + Send>>> {
        let cn = Self::job_result_by_worker_pubsub_channel_name(&worker_id);
        tracing::debug!(
            "subscribe_result_by_worker: worker_id={}, ch={}",
            &worker_id.value,
            &cn
        );
        let res: Pin<Box<dyn Stream<Item = Result<JobResult>> + Send>> = Box::pin(
            self.broadcast_chan_buf()
                .receive_stream_from_chan(
                    cn,
                    Some(&Duration::from_secs(
                        self.job_queue_config().expire_job_result_seconds as u64,
                    )),
                )
                .await
                .tap_err(|e| tracing::error!("receive_from_chan_err:{:?}", e))?
                .then(|r| async move {
                    Self::deserialize_job_result(&r)
                        .tap_err(|e| tracing::error!("deserialize_result:{:?}", e))
                }),
        );
        Ok(res)
    }
}

#[derive(Clone, Debug)]
pub struct ChanJobResultPubSubRepositoryImpl {
    broadcast_chan_buf: ChanBuffer<Vec<u8>, BroadcastChan<ChanBufferItem<Vec<u8>>>>,
    job_queue_config: Arc<JobQueueConfig>,
}
impl ChanJobResultPubSubRepositoryImpl {
    pub fn new(
        broadcast_chan_buf: ChanBuffer<Vec<u8>, BroadcastChan<ChanBufferItem<Vec<u8>>>>,
        job_queue_config: Arc<JobQueueConfig>,
    ) -> Self {
        ChanJobResultPubSubRepositoryImpl {
            broadcast_chan_buf,
            job_queue_config,
        }
    }
}
impl UseBroadcastChanBuffer for ChanJobResultPubSubRepositoryImpl {
    type Item = Vec<u8>;
    fn broadcast_chan_buf(
        &self,
    ) -> &ChanBuffer<Self::Item, BroadcastChan<ChanBufferItem<Self::Item>>> {
        &self.broadcast_chan_buf
    }
}

impl UseJobqueueAndCodec for ChanJobResultPubSubRepositoryImpl {}
impl UseJobQueueConfig for ChanJobResultPubSubRepositoryImpl {
    fn job_queue_config(&self) -> &JobQueueConfig {
        &self.job_queue_config
    }
}
pub trait UseChanJobResultPubSubRepository {
    fn job_result_pubsub_repository(&self) -> &ChanJobResultPubSubRepositoryImpl;
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::infra::JobQueueConfig;
    use anyhow::Result;
    use infra_utils::infra::chan::ChanBuffer;
    use proto::jobworkerp::data::{JobResult, JobResultData, JobResultId, ResultOutput};
    use tokio::time::Duration;

    // create test of subscribe_result() and publish_result()
    #[tokio::test]
    async fn test_subscribe_result() -> Result<()> {
        let app = ChanJobResultPubSubRepositoryImpl {
            broadcast_chan_buf: ChanBuffer::new(None, 10000),
            job_queue_config: Arc::new(JobQueueConfig {
                expire_job_result_seconds: 60,
                fetch_interval: 1000,
            }),
        };
        let job_id = JobId { value: 11 };
        let job_result_id = JobResultId { value: 1212 };
        let worker_id = WorkerId { value: 1 };
        let data = JobResultData {
            job_id: Some(job_id),
            worker_id: Some(worker_id),
            output: Some(ResultOutput {
                items: vec![b"test".to_vec()],
            }),
            ..JobResultData::default()
        };
        let job_result = JobResult {
            id: Some(job_result_id),
            data: Some(data.clone()),
        };

        let mut jhv = Vec::with_capacity(10);
        // 10 subscribers
        for _i in 0..10 {
            let app1 = app.clone();
            let job_id1 = job_id;
            let jr1 = job_result.clone();
            let jh = tokio::spawn(async move {
                let res = app1.subscribe_result(&job_id1, Some(3000)).await.unwrap();
                assert_eq!(res.id, jr1.id);
                assert_eq!(res.data, jr1.data);
            });
            jhv.push(jh);
        }
        tokio::time::sleep(Duration::from_millis(800)).await;
        app.publish_result(&job_result_id, &data, true).await?;
        let res = futures::future::join_all(jhv.into_iter()).await;
        assert_eq!(res.len(), 10);
        assert!(res.iter().all(|r| r.is_ok()));

        Ok(())
    }
    #[tokio::test]
    async fn test_subscribe_result_stream_by_worker() -> Result<()> {
        let app = ChanJobResultPubSubRepositoryImpl {
            broadcast_chan_buf: ChanBuffer::new(None, 10000),
            job_queue_config: Arc::new(JobQueueConfig {
                expire_job_result_seconds: 60,
                fetch_interval: 1000,
            }),
        };
        let worker_id = WorkerId { value: 1 };
        let job_result_id = JobResultId { value: 1212 };
        let data = JobResultData {
            job_id: Some(JobId { value: 11 }),
            worker_id: Some(worker_id),
            output: Some(ResultOutput {
                items: vec![b"test".to_vec()],
            }),
            ..JobResultData::default()
        };
        let job_result = JobResult {
            id: Some(job_result_id),
            data: Some(data.clone()),
        };

        let mut jhv = Vec::with_capacity(10);
        // 10 subscribers
        for _i in 0..10 {
            let app1 = app.clone();
            let worker_id1 = worker_id;
            let jr1 = job_result.clone();
            let jh = tokio::spawn(async move {
                let mut stream = app1
                    .subscribe_result_stream_by_worker(worker_id1)
                    .await
                    .unwrap();
                // listen stream 10 times
                let res = stream.next().await.unwrap().unwrap();
                assert_eq!(res.id, jr1.id);
                assert_eq!(res.data, jr1.data);
            });
            jhv.push(jh);
        }
        tokio::time::sleep(Duration::from_millis(800)).await;
        // publish 10 times
        for _i in 0..10 {
            app.publish_result(&job_result_id, &data, true).await?;
        }
        let res = futures::future::join_all(jhv.into_iter()).await;
        assert_eq!(res.len(), 10);
        assert!(res.iter().all(|r| r.is_ok()));

        Ok(())
    }
}
