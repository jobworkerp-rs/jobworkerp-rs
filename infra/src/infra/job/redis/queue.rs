use std::collections::HashSet;

use crate::infra::job::rows::UseJobqueueAndCodec;
use crate::{error::JobWorkerError, infra::UseJobQueueConfig};
use anyhow::Result;
use async_trait::async_trait;
use command_utils::util::result::FlatMap;
use infra_utils::infra::redis::UseRedisPool;
use proto::jobworkerp::data::{Job, JobId, JobResult, JobResultData, JobResultId, Priority};
use redis::AsyncCommands;
use signal_hook::consts::SIGINT;
use signal_hook_tokio::Signals;

#[async_trait]
pub trait RedisJobQueueRepository:
    UseRedisPool + UseJobqueueAndCodec + UseJobQueueConfig + Sync + 'static
where
    Self: Send + 'static,
{
    // for front (send job to worker)
    // return: jobqueue size
    #[inline]
    async fn enqueue_job(&self, channel_name: Option<&String>, job: &Job) -> Result<i64> {
        let cn = channel_name
            .unwrap_or(&Self::DEFAULT_CHANNEL_NAME.to_string())
            .to_owned();
        self.redis_pool()
            .get()
            .await?
            .rpush(
                Self::queue_channel_name(cn, job.data.as_ref().map(|d| &d.priority)),
                Self::serialize_job(job),
            ) // expect for multiple value
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into())
    }

    // send job result from worker to front directly
    #[inline]
    async fn enqueue_result_direct(&self, id: &JobResultId, res: &JobResultData) -> Result<bool> {
        let mut con = self.redis_pool().clone().get().await?;
        let v = Self::serialize_job_result(*id, res.clone());
        if let Some(jid) = res.job_id.as_ref() {
            tracing::debug!("send_result_direct: job_id: {:?}", jid);
            let cn = Self::result_queue_name(jid);
            let _: i64 = con
                .rpush(&cn, &v)
                .await
                .map_err(JobWorkerError::RedisError)?;
            // set expire for not calling listen_after api
            let _r: bool = con
                .expire(
                    &cn,
                    self.job_queue_config().expire_job_result_seconds as i64,
                )
                .await?;
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
        tracing::debug!("wait_for_result_data_for_response: job_id: {:?}", job_id);
        let signal: Signals = Signals::new([SIGINT]).expect("cannot get signals");
        let handle = signal.handle();
        let c = Self::result_queue_name(job_id); //XXX unwrap
        let mut th_p = self.redis_pool().get().await?;
        let res = tokio::select! {
            _ = tokio::spawn(async {
                let mut stream = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).expect("signal error");
                stream.recv().await
            }) => {
                handle.close();
                Err(JobWorkerError::OtherError("interrupt direct waiting process".to_string()).into())
            },
            val = th_p.blpop::<'_, String, Vec<Vec<u8>>>(c, (*timeout.unwrap_or(&0)/1000) as f64) => {
                let r: Result<JobResult> = val.map_err(|e|JobWorkerError::RedisError(e).into())
                    .flat_map(|v| Self::deserialize_job_result(&v[1]));
                r
            },
        };
        tracing::debug!("wait_for_result_queue_for_response: got res: {:?}", res);
        res
    }
    // iterate queue and find job with id (heavy operation when queue is long)
    async fn find_from_queue(
        &self,
        channel: Option<&String>,
        priority: Priority,
        id: &JobId,
    ) -> Result<Option<Job>> {
        let limit = 1000;
        let c = Self::queue_channel_name(
            channel.unwrap_or(&Self::DEFAULT_CHANNEL_NAME.to_string()),
            Some(priority as i32).as_ref(),
        );
        let mut redis = self.redis_pool().get().await?;
        // measure length by LLEN, iterate list of redis by LRANGE by limit for find job with id
        let length = redis.llen(c.clone()).await?;
        let mut job = None;
        let mut i = 0;
        while i < length {
            let mut r = redis
                .lrange::<'_, String, Vec<Vec<u8>>>(c.clone(), i, i + limit)
                .await
                .map_err(JobWorkerError::RedisError)?;
            i += limit;
            while let Some(j) = r.pop() {
                let j = Self::deserialize_job(&j)?;
                if j.id.as_ref().unwrap().value == id.value {
                    job = Some(j);
                    break;
                }
            }
            if job.is_some() {
                break;
            }
        }
        Ok(job)
    }
    // iterate queue and find job with id (heavy operation when queue is long)
    async fn find_multi_from_queue(
        &self,
        channel: Option<&String>,
        priority: Priority,
        ids: &HashSet<i64>,
    ) -> Result<Vec<Job>> {
        let limit = 1000;
        let c = Self::queue_channel_name(
            channel.unwrap_or(&Self::DEFAULT_CHANNEL_NAME.to_string()),
            Some(priority as i32).as_ref(),
        );
        let mut redis = self.redis_pool().get().await?;
        // measure length by LLEN, iterate list of redis by LRANGE by limit for find job with id
        let length = redis.llen(c.clone()).await?;
        let mut jobs = Vec::new();
        let mut i = 0;
        while i < length {
            let mut r = redis
                .lrange::<'_, String, Vec<Vec<u8>>>(c.clone(), i, i + limit)
                .await
                .map_err(JobWorkerError::RedisError)?;
            i += limit;
            while let Some(j) = r.pop() {
                let j = Self::deserialize_job(&j)?;
                if ids.contains(&j.id.as_ref().unwrap().value) {
                    jobs.push(j);
                }
            }
        }
        Ok(jobs)
    }

    async fn delete_from_queue(
        &self,
        channel: Option<&String>,
        priority: Priority,
        job: &Job,
    ) -> Result<i32> {
        let c = Self::queue_channel_name(
            channel.unwrap_or(&Self::DEFAULT_CHANNEL_NAME.to_string()),
            Some(priority as i32).as_ref(),
        );
        let mut redis = self.redis_pool().get().await.unwrap();
        redis
            .lrem::<'_, String, Vec<u8>, i32>(c, 0, Self::serialize_job(job))
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into())
    }
    async fn count_queue(&self, channel: Option<&String>, priority: Priority) -> Result<i64> {
        let c = Self::queue_channel_name(
            channel.unwrap_or(&Self::DEFAULT_CHANNEL_NAME.to_string()),
            Some(priority as i32).as_ref(),
        );
        let mut redis = self.redis_pool().get().await.unwrap();
        redis
            .llen(c)
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into())
    }
}

#[cfg(test)]
// create test (functional test without mock)
mod test {
    use std::sync::Arc;

    use crate::infra::JobQueueConfig;

    // create test of 'send_job()': store job with send_job() to redis and get job value from redis (by command)
    use super::*;
    use command_utils::util::datetime;
    use infra_utils::infra::redis::RedisPool;
    use infra_utils::infra::redis::UseRedisPool;
    use infra_utils::infra::test::setup_test_redis_pool;
    use proto::jobworkerp::data::JobResultData;
    use proto::jobworkerp::data::ResultOutput;
    use proto::jobworkerp::data::RunnerArg;
    use proto::jobworkerp::data::{Job, JobData, JobId, ResultStatus, WorkerId};
    use redis::AsyncCommands;

    struct RedisJobQueueRepositoryImpl {
        job_queue_config: Arc<JobQueueConfig>,
        pub redis_pool: &'static RedisPool,
    }
    impl UseJobQueueConfig for RedisJobQueueRepositoryImpl {
        fn job_queue_config(&self) -> &JobQueueConfig {
            &self.job_queue_config
        }
    }
    impl UseRedisPool for RedisJobQueueRepositoryImpl {
        fn redis_pool(&self) -> &'static RedisPool {
            self.redis_pool
        }
    }
    impl UseJobqueueAndCodec for RedisJobQueueRepositoryImpl {}
    impl RedisJobQueueRepository for RedisJobQueueRepositoryImpl {}

    #[tokio::test]
    async fn send_job_test() -> Result<()> {
        let redis_pool = setup_test_redis_pool().await;
        let job_queue_config = Arc::new(JobQueueConfig {
            expire_job_result_seconds: 10,
            fetch_interval: 1000,
        });
        // clear queue before test
        redis_pool
            .get()
            .await?
            .del::<'_, String, i64>(RedisJobQueueRepositoryImpl::queue_channel_name(
                RedisJobQueueRepositoryImpl::DEFAULT_CHANNEL_NAME,
                Some(&1),
            ))
            .await?;
        let repo = RedisJobQueueRepositoryImpl {
            job_queue_config,
            redis_pool,
        };
        let arg = RunnerArg {
            data: Some(proto::jobworkerp::data::runner_arg::Data::Command(
                proto::jobworkerp::data::CommandArg {
                    args: vec!["test".to_string()],
                },
            )),
        };
        let job = Job {
            id: None,
            data: Some(JobData {
                worker_id: Some(WorkerId { value: 1 }),
                arg: Some(arg),
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
        let mut conn = redis_pool.get().await?;
        let res: Vec<Vec<u8>> = conn
            .lrange(
                RedisJobQueueRepositoryImpl::queue_channel_name(
                    RedisJobQueueRepositoryImpl::DEFAULT_CHANNEL_NAME,
                    Some(&1),
                ),
                0,
                -1,
            )
            .await
            .map_err(JobWorkerError::RedisError)?;
        assert_eq!(res.len(), 1);
        Ok(())
    }
    // create test of 'send_result()': store job result with send_result() to redis and get job result value from wait_for_result_data_directly()
    #[tokio::test]
    async fn send_result_test() -> Result<()> {
        let redis_pool = setup_test_redis_pool().await;
        let job_queue_config = Arc::new(JobQueueConfig {
            expire_job_result_seconds: 10,
            fetch_interval: 1000,
        });
        let repo = RedisJobQueueRepositoryImpl {
            job_queue_config,
            redis_pool,
        };
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
        let r = repo
            .enqueue_result_direct(&job_result_id, &job_result_data)
            .await?;
        assert!(r);
        let res = repo
            .wait_for_result_queue_for_response(&job_id, None)
            .await?;
        assert_eq!(res.data.unwrap(), job_result_data);
        Ok(())
    }
}
