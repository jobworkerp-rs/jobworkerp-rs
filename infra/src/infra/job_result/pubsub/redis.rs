use super::{JobResultPublisher, JobResultSubscriber};
use crate::infra::{JobQueueConfig, UseJobQueueConfig, job::rows::UseJobqueueAndCodec};
use anyhow::Result;
use async_trait::async_trait;
use debug_stub_derive::DebugStub;
use futures::{Stream, StreamExt, stream::BoxStream};
use infra_utils::infra::redis::{RedisClient, UseRedisClient};
use jobworkerp_base::{
    codec::{ProstMessageCodec, UseProstCodec},
    error::JobWorkerError,
};
use proto::jobworkerp::data::{
    JobId, JobResult, JobResultData, JobResultId, ResultOutputItem, WorkerId, result_output_item,
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
                id.value
            )))?;
        let wid = data
            .worker_id
            .as_ref()
            .ok_or(JobWorkerError::InvalidParameter(format!(
                "worker_id not found: job_id={}",
                jid.value
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
        let job_result = JobResult {
            id: Some(*id),
            data: Some(data.clone()),
            ..Default::default()
        };
        let result_data = Self::serialize_message(&job_result)?;
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
            .map(|item| {
                let result = ProstMessageCodec::serialize_message(&item);
                if let Err(e) = &result {
                    tracing::error!(
                        "Failed to serialize ResultOutputItem: {:?}, item: {:?}",
                        e,
                        item
                    );
                }
                result
            })
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
        self.subscribe_result_with_ready(job_id, timeout, None)
            .await
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
                    job_id.value
                ))
                .into()),
            }
        } else {
            self.subscribe(cn.as_str())
                .await
                .inspect_err(|e| tracing::error!("redis_err:{:?}", e))
        }?;

        #[cfg(feature = "stream-subscription-test-hook")]
        super::test_hooks::hold_stream_subscription(job_id).await;

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
                                    Some(result)
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
            // For End marker: emit it, then emit None to trigger take_while termination
            .flat_map(|item| {
                if matches!(item.item, Some(result_output_item::Item::End(_))) {
                    // End marker: return End followed by None (termination signal)
                    futures::stream::iter(vec![Some(item), None])
                } else {
                    // Data item: wrap in Some
                    futures::stream::iter(vec![Some(item)])
                }
            })
            // Terminate when None is received (after End marker)
            .take_while(|opt| futures::future::ready(opt.is_some()))
            // Unwrap Option
            .filter_map(futures::future::ready);

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
                Self::deserialize_message::<JobResult>(&payload)
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
    /// Subscribe to a job's result and send on `ready` once the subscription is
    /// active. Callers that enqueue right after `ready` cannot lose a fast result.
    pub async fn subscribe_result_with_ready(
        &self,
        job_id: &JobId,
        timeout: Option<u64>,
        ready: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<JobResult> {
        self.subscribe_result_with_fallback(job_id, timeout, || async move {
            if let Some(ready) = ready {
                let _ = ready.send(());
            }
            None
        })
        .await
        .map(|(result, _)| result)
    }

    /// Subscribe to a job's result, then run `check` for a result that already
    /// exists (stored or cached before the subscription). Running the check only
    /// after subscribing closes the gap where a result published between a
    /// lookup and a later subscription would never be seen. Returns the result
    /// and whether it came from `check`.
    ///
    /// `check` is called exactly once, after the subscription is active;
    /// callers rely on that to signal readiness from inside it.
    pub async fn subscribe_result_with_fallback<F, Fut>(
        &self,
        job_id: &JobId,
        timeout: Option<u64>,
        check: F,
    ) -> Result<(JobResult, bool)>
    where
        F: FnOnce() -> Fut + Send,
        Fut: std::future::Future<Output = Option<JobResult>> + Send,
    {
        let cn = Self::job_result_pubsub_channel_name(job_id);
        tracing::debug!("subscribe_result: job_id={}, ch={}", &job_id.value, &cn);

        let mut sub = self
            .subscribe(cn.as_str())
            .await
            .inspect_err(|e| tracing::error!("redis_err:{:?}", e))?;
        if let Some(found) = check().await {
            sub.unsubscribe(&cn).await?;
            return Ok((found, true));
        }
        let res = {
            let mut message = sub.on_message();
            let receive_result = async {
                match tokio_stream::StreamExt::next(&mut message).await {
                    Some(msg) => {
                        let payload: Vec<u8> = msg
                            .get_payload()
                            .inspect_err(|e| tracing::error!("get_payload:{e:?}"))?;
                        let result = Self::deserialize_message::<JobResult>(&payload)
                            .inspect_err(|e| tracing::error!("deserialize_result:{e:?}"))?;
                        tracing::debug!("subscribe_result_received: result={:?}", &result.id);
                        Ok(result)
                    }
                    None => {
                        tracing::debug!("message.next() is None");
                        Err(JobWorkerError::RuntimeError("result is empty?".to_string()).into())
                    }
                }
            };
            if let Some(timeout) = timeout {
                tokio::select! {
                    _ = command_utils::util::shutdown::shutdown_signal() => {
                        tracing::debug!("got shutdown signal....");
                        Err(JobWorkerError::RuntimeError("interrupted".to_string()).into())
                    },
                    result = receive_result => result,
                    _ = tokio::time::sleep(Duration::from_millis(timeout)) => {
                        Err(JobWorkerError::TimeoutError(format!(
                            "subscribe timeout: job_id:{}",
                            job_id.value
                        )).into())
                    }
                }
            } else {
                tokio::select! {
                    _ = command_utils::util::shutdown::shutdown_signal() => {
                        tracing::debug!("got shutdown signal....");
                        Err(JobWorkerError::RuntimeError("interrupted".to_string()).into())
                    },
                    result = receive_result => result,
                }
            }
        };
        sub.unsubscribe(&cn).await?;
        tracing::info!("subscribe_result_changed end");
        res.map(|result| (result, false))
    }

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
    async fn subscribe_result_without_timeout_ignores_result_retention() -> Result<()> {
        let redis_client = setup_test_redis_client()?;
        let app = RedisJobResultPubSubRepositoryImpl {
            redis_client,
            job_queue_config: Arc::new(JobQueueConfig {
                expire_job_result_seconds: 0,
                ..JobQueueConfig::default()
            }),
        };
        let job_id = JobId { value: 9_001 };
        let result_id = JobResultId { value: 9_002 };
        let data = JobResultData {
            job_id: Some(job_id),
            worker_id: Some(WorkerId { value: 1 }),
            ..JobResultData::default()
        };

        let subscriber = app.clone();
        let receiver =
            tokio::spawn(async move { subscriber.subscribe_result(&job_id, None).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        app.publish_result(&result_id, &data, true).await?;

        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), receiver)
                .await
                .expect("unbounded subscription should receive a later result")??
                .id,
            Some(result_id)
        );
        Ok(())
    }

    fn fallback_test_result(job_id: JobId, result_id: i64) -> (JobResultId, JobResultData) {
        // A worker id no other test listens on: results are also published
        // to the per-worker channel.
        let data = JobResultData {
            job_id: Some(job_id),
            worker_id: Some(WorkerId { value: 9_999_001 }),
            ..JobResultData::default()
        };
        (JobResultId { value: result_id }, data)
    }

    #[tokio::test]
    async fn subscribe_result_with_fallback_returns_result_found_by_check() -> Result<()> {
        let app = RedisJobResultPubSubRepositoryImpl {
            redis_client: setup_test_redis_client()?,
            job_queue_config: Arc::new(JobQueueConfig::default()),
        };
        let job_id = JobId { value: 9_101 };
        let (result_id, data) = fallback_test_result(job_id, 9_102);
        let stored = JobResult {
            id: Some(result_id),
            data: Some(data),
            ..Default::default()
        };
        let (res, from_fallback) = tokio::time::timeout(
            Duration::from_secs(1),
            app.subscribe_result_with_fallback(&job_id, Some(5_000), || async { Some(stored) }),
        )
        .await??;
        assert!(from_fallback);
        assert_eq!(res.id, Some(result_id));
        Ok(())
    }

    // The check runs only once the subscription is active, so a result
    // published at that point (a job finishing right then) is not missed.
    #[tokio::test]
    async fn subscribe_result_with_fallback_checks_after_subscribing() -> Result<()> {
        let app = RedisJobResultPubSubRepositoryImpl {
            redis_client: setup_test_redis_client()?,
            job_queue_config: Arc::new(JobQueueConfig::default()),
        };
        let job_id = JobId { value: 9_201 };
        let (result_id, data) = fallback_test_result(job_id, 9_202);
        let publisher = app.clone();
        let (res, from_fallback) = tokio::time::timeout(
            Duration::from_secs(2),
            app.subscribe_result_with_fallback(&job_id, Some(5_000), || async move {
                publisher
                    .publish_result(&result_id, &data, true)
                    .await
                    .unwrap();
                None
            }),
        )
        .await??;
        assert!(!from_fallback);
        assert_eq!(res.id, Some(result_id));
        Ok(())
    }

    async fn wait_for_job_subscription_counts(
        client: &redis::Client,
        job_id: &JobId,
        expected: usize,
    ) -> Result<()> {
        let result_channel =
            RedisJobResultPubSubRepositoryImpl::job_result_pubsub_channel_name(job_id);
        let stream_channel =
            RedisJobResultPubSubRepositoryImpl::job_result_stream_pubsub_channel_name(job_id);
        let mut connection = client.get_multiplexed_async_connection().await?;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let counts: Vec<(String, usize)> = redis::cmd("PUBSUB")
                    .arg("NUMSUB")
                    .arg(&result_channel)
                    .arg(&stream_channel)
                    .query_async(&mut connection)
                    .await?;
                if counts.len() == 2 && counts.iter().all(|(_, count)| *count == expected) {
                    return Ok::<(), anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("timed out waiting for {expected} Redis subscribers"))??;
        Ok(())
    }

    #[tokio::test]
    async fn dropping_result_and_stream_listeners_releases_redis_subscriptions() -> Result<()> {
        let redis_client = setup_test_redis_client()?;
        let app = Arc::new(RedisJobResultPubSubRepositoryImpl {
            redis_client: redis_client.clone(),
            job_queue_config: Arc::new(JobQueueConfig::default()),
        });
        let job_id = JobId { value: 9_301 };
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let result_app = app.clone();
        // Aborts the subscription even if an assertion below returns early.
        let result_task = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            result_app
                .subscribe_result_with_ready(&job_id, None, Some(ready_tx))
                .await
        }));

        tokio::time::timeout(Duration::from_secs(2), ready_rx)
            .await
            .map_err(|_| anyhow::anyhow!("result subscription did not become ready"))??;
        let stream = app.subscribe_result_stream(&job_id, None).await?;
        wait_for_job_subscription_counts(&redis_client, &job_id, 1).await?;

        result_task.abort();
        let _ = result_task.await;
        drop(stream);
        wait_for_job_subscription_counts(&redis_client, &job_id, 0).await?;
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
