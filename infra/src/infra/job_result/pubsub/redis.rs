use std::{sync::Arc, time::Duration};

use super::{JobResultPublisher, JobResultSubscriber};
use crate::{
    error::JobWorkerError,
    infra::{job::rows::UseJobqueueAndCodec, JobQueueConfig, UseJobQueueConfig},
};
use anyhow::Result;
use async_trait::async_trait;
use command_utils::util::result::TapErr;
use debug_stub_derive::DebugStub;
use infra_utils::infra::redis::{RedisClient, UseRedisClient};
use proto::jobworkerp::data::{JobId, JobResult, JobResultData, JobResultId};
use tokio_stream::StreamExt;

#[async_trait]
impl JobResultPublisher for RedisJobResultPubSubRepositoryImpl {
    async fn publish_result(&self, id: &JobResultId, data: &JobResultData) -> Result<bool> {
        let jid = data.job_id.as_ref().unwrap();
        tracing::debug!(
            "publish_result: job_id={}, result_id={}",
            &jid.value,
            &id.value
        );
        let result_data = Self::serialize_job_result(*id, data.clone());
        self.publish(
            Self::job_result_pubsub_channel_name(jid).as_str(),
            &result_data,
        )
        .await
    }
}

#[async_trait]
impl JobResultSubscriber for RedisJobResultPubSubRepositoryImpl {
    // subscribe job result of listen after using redis and return got result immediately
    async fn subscribe_result(&self, job_id: &JobId, timeout: Option<u64>) -> Result<JobResult> {
        let cn = Self::job_result_pubsub_channel_name(job_id);
        tracing::debug!("subscribe_result: job_id={}, ch={}", &job_id.value, &cn);

        let mut sub = self
            .subscribe(cn.as_str())
            .await
            .tap_err(|e| tracing::error!("redis_err:{:?}", e))?;
        // for timeout delay
        let delay = if let Some(to) = timeout {
            tokio::time::sleep(Duration::from_millis(to))
        } else {
            // wait until far future
            tokio::time::sleep(Duration::from_secs(
                self.job_queue_config().expire_job_result_seconds as u64,
            ))
        };
        let res = {
            let mut message = sub.on_message();
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    tracing::debug!("got sigint signal....");
                    Err(JobWorkerError::RuntimeError("interrupted".to_string()).into())
                },
                val = message.next() => {
                    if let Some(msg) = val {
                        // skip if error in processing message
                        let payload: Vec<u8> = msg
                            .get_payload()
                            .tap_err(|e| tracing::error!("get_payload:{:?}", e))?;
                        let result = Self::deserialize_job_result(&payload)
                            .tap_err(|e| tracing::error!("deserialize_result:{:?}", e))?;
                        tracing::debug!("subscribe_result_received: result={:?}", &result);
                        Ok(result)
                    } else {
                        tracing::debug!("message.next() is None");
                        Err(JobWorkerError::RuntimeError("result is empty?".to_string()).into())
                    }
                }
                _ = delay => {
                        return Err(JobWorkerError::TimeoutError(format!(
                            "subscribe timeout: job_id:{}",
                            &job_id.value
                        ))
                        .into());
                }
            }
        };
        sub.unsubscribe(&cn.clone()).await?;
        tracing::info!("subscribe_result_changed end");
        res
    }
}

#[derive(Clone, DebugStub)]
pub struct RedisJobResultPubSubRepositoryImpl {
    #[debug_stub = "&'static RedisClient"]
    pub redis_client: RedisClient,
    job_queue_config: Arc<JobQueueConfig>,
}
impl RedisJobResultPubSubRepositoryImpl {
    pub fn new(redis_client: RedisClient, job_queue_config: Arc<JobQueueConfig>) -> Self {
        Self {
            redis_client,
            job_queue_config,
        }
    }
}
impl UseRedisClient for RedisJobResultPubSubRepositoryImpl {
    fn redis_client(&self) -> &RedisClient {
        &self.redis_client
    }
}

impl UseJobqueueAndCodec for RedisJobResultPubSubRepositoryImpl {}
impl UseJobQueueConfig for RedisJobResultPubSubRepositoryImpl {
    fn job_queue_config(&self) -> &JobQueueConfig {
        &self.job_queue_config
    }
}
pub trait UseRedisJobResultPubSubRepository {
    fn job_result_pubsub_repository(&self) -> &RedisJobResultPubSubRepositoryImpl;
}

#[cfg(test)]
mod test {
    use crate::infra::JobQueueConfig;

    use super::*;
    use anyhow::Result;
    use infra_utils::infra::test::setup_test_redis_client;
    use proto::jobworkerp::data::{JobResult, JobResultData, JobResultId, ResultOutput};
    use tokio::time::Duration;

    // create test of subscribe_result() and publish_result()
    #[tokio::test]
    async fn test_subscribe_result() -> Result<()> {
        let redis_client = setup_test_redis_client()?;
        let app = RedisJobResultPubSubRepositoryImpl {
            redis_client,
            job_queue_config: Arc::new(JobQueueConfig::default()),
        };
        let job_id = JobId { value: 1 };
        let job_result_id = JobResultId { value: 1212 };
        let data = JobResultData {
            job_id: Some(job_id),
            output: Some(ResultOutput {
                items: vec![b"test".to_vec()],
            }),
            ..JobResultData::default()
        };
        let job_result = JobResult {
            id: Some(job_result_id),
            data: Some(data.clone()),
        };

        // 2 subscribers
        let app2 = app.clone();
        let job_id2 = job_id;
        let jr2 = job_result.clone();
        let jh = tokio::spawn(async move {
            let res = app2.subscribe_result(&job_id2, None).await.unwrap();
            assert_eq!(res.id, jr2.id);
            assert_eq!(res.data, jr2.data);
        });
        let app2 = app.clone();
        let job_id2 = job_id;
        let jr2 = job_result.clone();
        let jh2 = tokio::spawn(async move {
            let res = app2.subscribe_result(&job_id2, Some(1000)).await.unwrap();
            assert_eq!(res.id, jr2.id);
            assert_eq!(res.data, jr2.data);
        });
        // wait until subscribe
        tokio::time::sleep(Duration::from_millis(200)).await;
        app.publish_result(&job_result_id, &data).await?;
        jh.await?;
        jh2.await?;
        Ok(())
    }
}
