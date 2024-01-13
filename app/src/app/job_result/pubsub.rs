use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use command_utils::util::result::TapErr;
use infra::{error::JobWorkerError, infra::job::rows::UseJobqueueAndCodec};
use infra_utils::infra::redis::UseRedisClient;
use proto::jobworkerp::data::{JobId, JobResult, JobResultData, JobResultId};
use tokio_stream::StreamExt;

#[async_trait]
pub trait JobResultPublishApp: UseRedisClient + UseJobqueueAndCodec {
    async fn publish_result(&self, id: &JobResultId, data: &JobResultData) -> Result<()> {
        let jid = data.job_id.as_ref().unwrap();
        tracing::debug!(
            "publish_result: job_id={}, result_id={}",
            &jid.value,
            &id.value
        );
        let result_data = Self::serialize_job_result(id.clone(), data.clone());
        self.publish(
            Self::job_result_pubsub_channel_name(jid).as_str(),
            &result_data,
        )
        .await
    }
}

#[async_trait]
pub trait JobResultSubscribeApp: UseRedisClient + UseJobqueueAndCodec {
    // subscribe job result of listen after using redis and return got result immediately
    async fn subscribe_result(&self, job_id: &JobId, timeout: Option<&u64>) -> Result<JobResult> {
        let cn = Self::job_result_pubsub_channel_name(job_id);
        tracing::debug!("subscribe_result: job_id={}, ch={}", &job_id.value, &cn);

        let mut sub = self
            .subscribe(cn.as_str())
            .await
            .tap_err(|e| tracing::error!("redis_err:{:?}", e))?;
        // for timeout delay
        let delay = if let Some(to) = timeout {
            tokio::time::sleep(Duration::from_millis(*to))
        } else {
            // wait until far future
            tokio::time::sleep(Duration::from_secs(u64::MAX))
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
                        tracing::debug!("subscribe_result_changed: result changed: {:?}", &result);
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

// pub trait JobResultPubsubApp: JobResultPublishApp + JobResultSubscribeApp {}

#[cfg(test)]
mod test {
    use super::*;
    use anyhow::Result;
    use infra_utils::infra::{redis::RedisClient, test::setup_test_redis_client};
    use proto::jobworkerp::data::{JobResult, JobResultData, JobResultId, ResultOutput};
    use tokio::time::Duration;

    // create test of subscribe_result() and publish_result()
    #[tokio::test]
    async fn test_subscribe_result() -> Result<()> {
        let redis_client = setup_test_redis_client()?;
        let app = TestApp { redis_client };
        let job_id = JobId { value: 1 };
        let job_result_id = JobResultId { value: 1212 };
        let data = JobResultData {
            job_id: Some(job_id.clone()),
            output: Some(ResultOutput {
                items: vec![b"test".to_vec()],
            }),
            ..JobResultData::default()
        };
        let job_result = JobResult {
            id: Some(job_result_id.clone()),
            data: Some(data.clone()),
        };

        // 2 subscribers
        let app2 = app.clone();
        let job_id2 = job_id.clone();
        let jr2 = job_result.clone();
        let jh = tokio::spawn(async move {
            let res = app2.subscribe_result(&job_id2, None).await.unwrap();
            assert_eq!(res.id, jr2.id);
            assert_eq!(res.data, jr2.data);
        });
        let app2 = app.clone();
        let job_id2 = job_id.clone();
        let jr2 = job_result.clone();
        let jh2 = tokio::spawn(async move {
            let res = app2.subscribe_result(&job_id2, Some(&1000)).await.unwrap();
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
    #[derive(Clone)]
    struct TestApp {
        redis_client: RedisClient,
    }
    #[async_trait]
    impl UseRedisClient for TestApp {
        fn redis_client(&self) -> &RedisClient {
            &self.redis_client
        }
    }
    #[async_trait]
    impl UseJobqueueAndCodec for TestApp {}
    impl JobResultPublishApp for TestApp {}
    impl JobResultSubscribeApp for TestApp {}
}
