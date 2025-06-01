use super::{JobResultPublisher, JobResultSubscriber};
use crate::infra::{job::rows::UseJobqueueAndCodec, JobQueueConfig, UseJobQueueConfig};
use anyhow::Result;
use async_trait::async_trait;
use debug_stub_derive::DebugStub;
use futures::{stream::BoxStream, Stream, StreamExt};
use infra_utils::infra::redis::{RedisClient, UseRedisClient};
use jobworkerp_base::{
    codec::{ProstMessageCodec, UseProstCodec},
    error::JobWorkerError,
};
use proto::jobworkerp::data::{
    result_output_item, JobId, JobResult, JobResultData, JobResultId, ResultOutputItem, WorkerId,
};
use std::{pin::Pin, sync::Arc, time::Duration};

#[async_trait]
impl JobResultPublisher for RedisJobResultPubSubRepositoryImpl {
    async fn publish_result(
        &self,
        id: &JobResultId,
        data: &JobResultData,
        to_listen: bool,
    ) -> Result<bool> {
        // TODO send to worker id channel if listening clients exist
        // https://redis.io/docs/latest/commands/pubsub-numsub/
        let jid = data
            .job_id
            .as_ref()
            .ok_or(JobWorkerError::InvalidParameter(format!(
                "job_id not found: result_id={}",
                &id.value
            )))?;
        let wid = data
            .worker_id
            .as_ref()
            .ok_or(JobWorkerError::InvalidParameter(format!(
                "worker_id not found: job_id={}",
                &jid.value
            )))?;
        tracing::debug!(
            "publish_result: job_id={}, result_id={}",
            &jid.value,
            &id.value
        );
        let chs = if to_listen {
            vec![
                Self::job_result_pubsub_channel_name(jid),
                Self::job_result_by_worker_pubsub_channel_name(wid),
            ]
        } else {
            vec![Self::job_result_by_worker_pubsub_channel_name(wid)]
        };
        let result_data = Self::serialize_job_result(*id, data.clone());
        self.publish_multi_if_listen(chs.as_slice(), &result_data)
            .await
    }

    async fn publish_result_stream_data(
        &self,
        job_id: JobId,
        stream: BoxStream<'static, ResultOutputItem>,
    ) -> Result<bool> {
        let ch = Self::job_result_stream_pubsub_channel_name(&job_id);
        let res_stream = stream
            .map(|item| ProstMessageCodec::serialize_message(&item))
            .filter_map(|r| async move { r.ok() })
            .boxed();

        self.publish_stream(ch.as_str(), res_stream)
            .await
            .map(|_| true)
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
            .inspect_err(|e| tracing::error!("redis_err:{:?}", e))?;
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
                val = tokio_stream::StreamExt::next(&mut message) => {
                    if let Some(msg) = val {
                        // skip if error in processing message
                        let payload: Vec<u8> = msg
                            .get_payload()
                            .inspect_err(|e| tracing::error!("get_payload:{:?}", e))?;
                        let result = Self::deserialize_job_result(&payload)
                            .inspect_err(|e| tracing::error!("deserialize_result:{:?}", e))?;
                        tracing::debug!("subscribe_result_received: result={:?}", &result.id);
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
    // subscribe job result of streaming using redis
    async fn subscribe_result_stream(
        &self,
        job_id: &JobId,
        timeout: Option<u64>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        use futures::stream::StreamExt;
        let cn = Self::job_result_stream_pubsub_channel_name(job_id);
        tracing::debug!(
            "subscribe_result stream: job_id={}, ch={}",
            &job_id.value,
            &cn
        );

        // pubsub timeout
        let sub = if let Some(timeout) = timeout {
            let res =
                tokio::time::timeout(Duration::from_millis(timeout), self.subscribe(cn.as_str()))
                    .await;
            match res {
                Ok(Ok(s)) => Ok(s),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(JobWorkerError::RuntimeError(format!(
                    "subscribe timeout: job_id:{}",
                    &job_id.value
                ))
                .into()),
            }
        } else {
            self.subscribe(cn.as_str())
                .await
                .inspect_err(|e| tracing::error!("redis_err:{:?}", e))
        }?;

        let repo = Arc::new(self.clone());
        let cn2 = Arc::new(cn);

        let msg_stream = sub
            .into_on_message()
            .filter_map(move |msg| {
                let repo = repo.clone();
                let cn2 = cn2.clone();
                async move {
                    match msg.get_payload::<Vec<u8>>() {
                        Ok(payload) => {
                            tracing::debug!(
                                "subscribe_result_stream_received: payload len={:?}",
                                &payload.len()
                            );
                            let result =
                                ProstMessageCodec::deserialize_message::<ResultOutputItem>(
                                    &payload,
                                )
                                .inspect_err(|e| tracing::error!("deserialize_result:{:?}", e))
                                .ok()?;
                            match result.item {
                                Some(result_output_item::Item::End(_)) => {
                                    tracing::debug!(
                                        "subscribe_result_stream_received: end of stream"
                                    );
                                    let _ = repo.unsubscribe(cn2.as_str()).await.inspect_err(|e| {
                                        tracing::error!("unsubscribe:{:?}", e);
                                    });
                                    tracing::debug!(
                                        "subscribe_result_stream_received: unsubscribed: {}",
                                        cn2.as_str()
                                    );
                                    Some(ResultOutputItem { item: None }) // maek item none
                                }
                                Some(res) => {
                                    tracing::debug!(
                                        "subscribe_result_stream_received: result len={:?}",
                                        &res.encoded_len()
                                    );
                                    Some(ResultOutputItem { item: Some(res) })
                                }
                                // skip if error in processing message
                                None => {
                                    tracing::info!(
                                        "subscribe_result_stream_received: item is None"
                                    );
                                    None
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!("get_payload:{:?}", e);
                            None
                        }
                    }
                }
            })
            .take_while(|item| futures::future::ready(item.item.is_some()));
        Ok(msg_stream.boxed())
        // }
    }
    // subscribe job result of listen after using redis and return got result immediately
    async fn subscribe_result_stream_by_worker(
        &self,
        worker_id: WorkerId,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<JobResult>> + Send>>> {
        use futures::stream::StreamExt;
        let cn = Self::job_result_by_worker_pubsub_channel_name(&worker_id);
        tracing::debug!(
            "subscribe_result: worker_id={}, ch={}",
            &worker_id.value,
            &cn
        );

        let sub = self
            .subscribe(cn.as_str())
            .await
            .inspect_err(|e| tracing::error!("redis_err:{:?}", e))?;
        // for timeout delay
        let res = Box::pin({
            sub.into_on_message().map(|msg| {
                let payload: Vec<u8> = msg
                    .get_payload()
                    .inspect_err(|e| tracing::error!("get_payload:{:?}", e))?;
                Self::deserialize_job_result(&payload)
                    .inspect_err(|e| tracing::error!("deserialize_result:{:?}", e))
            })
        });
        Ok(res)
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

impl UseProstCodec for RedisJobResultPubSubRepositoryImpl {}
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
    use std::collections::HashMap;

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
        let worker_id = WorkerId { value: 11 };
        let job_result_id = JobResultId { value: 1212 };
        let data = JobResultData {
            job_id: Some(job_id),
            worker_id: Some(worker_id),
            output: Some(ResultOutput {
                items: b"test".to_vec(),
            }),
            ..JobResultData::default()
        };
        let job_result = JobResult {
            id: Some(job_result_id),
            data: Some(data.clone()),
            metadata: HashMap::new(),
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
        let app3 = app.clone();
        let jr3 = job_result.clone();
        let jh3 = tokio::spawn(async move {
            let res3 = app3
                .subscribe_result_stream_by_worker(worker_id)
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap();
            assert_eq!(res3.id, jr3.id);
            assert_eq!(res3.data, jr3.data);
        });
        // wait until subscribe
        tokio::time::sleep(Duration::from_millis(200)).await;
        app.publish_result(&job_result_id, &data, true).await?;
        jh.await?;
        jh2.await?;
        jh3.await?;
        Ok(())
    }
    #[tokio::test]
    async fn test_subscribe_result_stream_by_worker() -> Result<()> {
        let redis_client = setup_test_redis_client()?;
        let app = RedisJobResultPubSubRepositoryImpl {
            redis_client,
            job_queue_config: Arc::new(JobQueueConfig::default()),
        };
        let worker_id = WorkerId { value: 1 };
        let job_result_id = JobResultId { value: 1212 };
        let data = JobResultData {
            job_id: Some(JobId { value: 11 }),
            worker_id: Some(worker_id),
            output: Some(ResultOutput {
                items: b"test".to_vec(),
            }),
            ..JobResultData::default()
        };
        let job_result = JobResult {
            id: Some(job_result_id),
            data: Some(data.clone()),
            metadata: HashMap::new(),
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
                for _j in 0..10 {
                    let res = stream.next().await.unwrap().unwrap();
                    // println!(
                    //     "test_subscribe_result_stream_by_worker:{}, res: {:?}",
                    //     _j, &res.id
                    // );
                    assert_eq!(res.id, jr1.id);
                    assert_eq!(res.data, jr1.data);
                }
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
