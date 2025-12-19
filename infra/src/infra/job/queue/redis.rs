use crate::infra::job::queue::JobQueueCancellationRepository;
use crate::infra::job::rows::UseJobqueueAndCodec;
use crate::infra::job_result::pubsub::redis::{
    RedisJobResultPubSubRepositoryImpl, UseRedisJobResultPubSubRepository,
};
use crate::infra::job_result::pubsub::JobResultSubscriber;
use crate::infra::{JobQueueConfig, UseJobQueueConfig};
use anyhow::Result;
use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use infra_utils::infra::redis::UseRedisClient;
use infra_utils::infra::redis::{RedisPool, UseRedisPool};
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{
    Job, JobId, JobResult, JobResultData, JobResultId, Priority, ResultOutputItem,
};
use redis::AsyncCommands;
use signal_hook::consts::SIGINT;
use signal_hook_tokio::Signals;
use std::collections::HashSet;
use std::sync::Arc;

#[async_trait]
pub trait RedisJobQueueRepository:
    UseRedisPool
    + UseRedisJobResultPubSubRepository
    + UseJobqueueAndCodec
    + UseJobQueueConfig
    + Sync
    + 'static
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
        timeout: Option<u64>,
        request_streaming: bool,
    ) -> Result<(JobResult, Option<BoxStream<'static, ResultOutputItem>>)> {
        tracing::debug!(
            "wait_for_result_data_for_response: job_id: {:?} timeout:{}, mode: {}",
            job_id,
            timeout.unwrap_or(0),
            if request_streaming {
                "streaming"
            } else {
                "direct"
            }
        );
        let signal: Signals = Signals::new([SIGINT]).expect("cannot get signals");
        let handle = signal.handle();
        let c = Self::result_queue_name(job_id);
        let mut th_p = self.redis_pool().get().await?;
        let pop_future = async {
            tokio::select! {
                _ = tokio::spawn(async {
                    let mut sig_stream = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                        .expect("signal error");
                    sig_stream.recv().await
                }) => {
                    handle.close();
                    Err(JobWorkerError::RuntimeError("interrupt direct waiting process".to_string()).into())
                },
                val = th_p.blpop::<String, Vec<Vec<u8>>>(c, (timeout.unwrap_or(0)/1000) as f64) => {
                    let r: Result<JobResult> = val
                        .map_err(|e| JobWorkerError::RedisError(e).into())
                        .and_then(|v| {
                            if v.is_empty() {
                                Err(JobWorkerError::RuntimeError("timeout".to_string()).into())
                            } else {
                                Self::deserialize_job_result(&v[1])
                            }
                        });
                    r
                },
            }
        };

        let subscribe_future = async {
            if request_streaming {
                self.job_result_pubsub_repository()
                    .subscribe_result_stream(job_id, timeout)
                    .await
                    .inspect_err(|e| {
                        tracing::error!(
                            "subscribe_result_stream error: {:?}, job_id: {:?}",
                            e,
                            job_id
                        )
                    })
                    .ok()
            } else {
                None
            }
        };

        let (pop_result, subscribe_result) = tokio::join!(pop_future, subscribe_future);

        // Handle streaming request inconsistency: if streaming was requested but stream creation failed
        if request_streaming && subscribe_result.is_none() {
            tracing::warn!(
                "wait_for_result_queue_for_response: streaming requested but no stream available for job_id: {:?}",
                job_id
            );
        }

        match pop_result {
            Ok(r) => {
                // even if streaming was requested, to prevent HTTP/2 protocol errors
                let should_disable_stream = if let Some(ref data) = r.data {
                    use proto::jobworkerp::data::ResultStatus;
                    data.status != ResultStatus::Success as i32
                } else {
                    false
                };

                let final_stream = if should_disable_stream {
                    tracing::debug!(
                        "wait_for_result_queue_for_response: disabling stream for error result, job_id: {:?}, status: {:?}",
                        job_id,
                        r.data.as_ref().map(|d| d.status)
                    );
                    None
                } else {
                    subscribe_result
                };

                tracing::debug!(
                    "wait_for_result_queue_for_response: got res: {:?} {}",
                    r.id,
                    if final_stream.is_some() {
                        "with stream"
                    } else {
                        "without stream"
                    }
                );

                Ok((r, final_stream))
            }
            Err(e) => Err(e),
        }
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
        channel: Option<&str>,
        priority: Priority,
        ids: Option<&HashSet<i64>>,
    ) -> Result<Vec<Job>> {
        let limit = 1000;
        let c = Self::queue_channel_name(
            channel.unwrap_or(Self::DEFAULT_CHANNEL_NAME),
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
                if ids.is_none_or(|ids| ids.contains(&j.id.as_ref().unwrap().value)) {
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

/// Redis implementation of JobQueueRepository with cancellation support
#[derive(Clone, Debug)]
pub struct RedisJobQueueRepositoryImpl {
    pub job_queue_config: Arc<JobQueueConfig>,
    pub redis_pool: &'static RedisPool,
    pub job_result_pubsub_repository: RedisJobResultPubSubRepositoryImpl,
}

impl RedisJobQueueRepositoryImpl {
    pub fn new(
        job_queue_config: Arc<JobQueueConfig>,
        redis_pool: &'static RedisPool,
        redis_client: redis::Client,
    ) -> Self {
        let job_result_pubsub_repository =
            RedisJobResultPubSubRepositoryImpl::new(redis_client, job_queue_config.clone());

        Self {
            job_queue_config,
            redis_pool,
            job_result_pubsub_repository,
        }
    }
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

impl UseRedisJobResultPubSubRepository for RedisJobQueueRepositoryImpl {
    fn job_result_pubsub_repository(&self) -> &RedisJobResultPubSubRepositoryImpl {
        &self.job_result_pubsub_repository
    }
}

impl RedisJobQueueRepository for RedisJobQueueRepositoryImpl {}

#[async_trait]
impl JobQueueCancellationRepository for RedisJobQueueRepositoryImpl {
    /// Broadcast job cancellation notification to all workers using Redis Pub/Sub
    async fn broadcast_job_cancellation(&self, job_id: &JobId) -> Result<()> {
        const JOB_CANCELLATION_CHANNEL: &str = "job_cancellation_channel";

        let message = <Self as UseJobqueueAndCodec>::serialize_message(job_id);
        let _: i32 = self
            .redis_pool()
            .get()
            .await?
            .publish(JOB_CANCELLATION_CHANNEL, message)
            .await
            .map_err(JobWorkerError::RedisError)?;

        tracing::info!(
            "Broadcasted cancellation for job {} to all workers via Redis Pub/Sub",
            job_id.value
        );
        Ok(())
    }

    /// Subscribe to job cancellation notifications with timeout and cleanup support
    ///
    /// **Leak prevention**: Job timeout + margin for automatic disconnection (when timeout > 0)
    /// **timeout=0 means unlimited**: No automatic timeout, waits until cleanup signal
    /// **Simple design**: No complex control needed, leverages redis-rs standard functionality
    async fn subscribe_job_cancellation_with_timeout(
        &self,
        callback: Box<dyn Fn(JobId) -> BoxFuture<'static, Result<()>> + Send + Sync + 'static>,
        job_timeout_ms: u64, // Job timeout time (milliseconds), 0 means unlimited
        mut cleanup_receiver: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<()> {
        const JOB_CANCELLATION_CHANNEL: &str = "job_cancellation_channel";

        // timeout=0 means unlimited (no timeout), otherwise job timeout + 30 seconds margin
        let pubsub_timeout = if job_timeout_ms == 0 {
            None
        } else {
            Some(std::time::Duration::from_millis(job_timeout_ms + 30_000))
        };

        let mut pubsub = self
            .job_result_pubsub_repository
            .redis_client()
            .get_async_pubsub()
            .await?;

        pubsub.subscribe(JOB_CANCELLATION_CHANNEL).await?;

        tracing::debug!(
            "Started job cancellation subscription with {:?} timeout on channel: {}",
            pubsub_timeout
                .map(|d| format!("{} ms", d.as_millis()))
                .unwrap_or_else(|| "unlimited".to_string()),
            JOB_CANCELLATION_CHANNEL
        );

        use futures::StreamExt;
        let mut message_stream = pubsub.on_message();

        loop {
            tokio::select! {
                // Receive pubsub messages (with optional application-level timeout)
                msg_result = async {
                    match pubsub_timeout {
                        Some(timeout) => {
                            tokio::time::timeout(timeout, message_stream.next())
                                .await
                                .map_err(|_| ()) // Convert timeout error to Err(())
                        }
                        None => {
                            // No timeout - wait indefinitely
                            Ok(message_stream.next().await)
                        }
                    }
                } => {
                    match msg_result {
                        Ok(Some(message)) => {
                            match message.get_payload::<Vec<u8>>() {
                                Ok(payload_bytes) => {
                                    match <Self as UseJobqueueAndCodec>::deserialize_message::<JobId>(&payload_bytes) {
                                        Ok(job_id) => {
                                            tracing::debug!("Received cancellation message for job {}", job_id.value);
                                            if let Err(e) = callback(job_id).await {
                                                tracing::error!("Cancellation callback error: {:?}", e);
                                            }
                                        }
                                        Err(e) => {
                                            tracing::warn!("Failed to deserialize job ID from cancellation message: {:?}", e);
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("Failed to get message payload from cancellation message: {:?}", e);
                                }
                            }
                        }
                        Ok(None) => {
                            // Pubsub stream ended
                            tracing::debug!("Pubsub connection ended");
                            break;
                        }
                        Err(()) => {
                            // Application-level timeout occurred
                            tracing::debug!("Pubsub connection timed out after {:?}", pubsub_timeout);
                            break;
                        }
                    }
                }
                // Manual cleanup signal
                _ = &mut cleanup_receiver => {
                    tracing::debug!("Received cleanup signal, terminating pubsub");
                    break;
                }
            }
        }

        // Pubsub is automatically dropped and connection released
        tracing::debug!("Job cancellation subscription terminated");
        Ok(())
    }
}

pub trait UseRedisJobQueueRepository {
    fn redis_job_queue_repository(&self) -> &RedisJobQueueRepositoryImpl;
}

#[cfg(test)]
// create test (functional test without mock)
mod test {
    use std::collections::HashMap;
    use std::sync::Arc;

    use crate::infra::job::rows::JobqueueAndCodec;
    use crate::infra::job_result::pubsub::redis::RedisJobResultPubSubRepositoryImpl;
    use crate::infra::JobQueueConfig;

    // create test of 'send_job()': store job with send_job() to redis and get job value from redis (by command)
    use super::*;
    use command_utils::util::datetime;
    use infra_utils::infra::redis::RedisPool;
    use infra_utils::infra::redis::UseRedisPool;
    use infra_utils::infra::test::setup_test_redis_client;
    use infra_utils::infra::test::setup_test_redis_pool;
    use proto::jobworkerp::data::JobResultData;
    use proto::jobworkerp::data::ResultOutput;
    use proto::jobworkerp::data::{Job, JobData, JobId, ResultStatus, WorkerId};
    use redis::AsyncCommands;

    struct RedisJobQueueRepositoryImpl {
        job_queue_config: Arc<JobQueueConfig>,
        pub redis_pool: &'static RedisPool,
        job_result_pubsub_repository: RedisJobResultPubSubRepositoryImpl,
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

    impl UseRedisJobResultPubSubRepository for RedisJobQueueRepositoryImpl {
        fn job_result_pubsub_repository(&self) -> &RedisJobResultPubSubRepositoryImpl {
            &self.job_result_pubsub_repository
        }
    }
    impl RedisJobQueueRepository for RedisJobQueueRepositoryImpl {}

    #[tokio::test]
    async fn send_job_test() -> Result<()> {
        let redis_pool = setup_test_redis_pool().await;
        let redis_client = setup_test_redis_client()?;
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
        let job_result_pubsub_repository =
            RedisJobResultPubSubRepositoryImpl::new(redis_client, job_queue_config.clone());
        let repo = RedisJobQueueRepositoryImpl {
            job_queue_config,
            redis_pool,
            job_result_pubsub_repository,
        };
        let args = JobqueueAndCodec::serialize_message(&proto::TestArgs {
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
                streaming_type: 0,
                using: None,
            }),
            metadata: HashMap::new(),
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
        let redis_client = setup_test_redis_client()?;
        let job_queue_config = Arc::new(JobQueueConfig {
            expire_job_result_seconds: 10,
            fetch_interval: 1000,
        });
        let job_result_pubsub_repository =
            RedisJobResultPubSubRepositoryImpl::new(redis_client, job_queue_config.clone());
        let repo = RedisJobQueueRepositoryImpl {
            job_queue_config,
            redis_pool,
            job_result_pubsub_repository,
        };
        let job_result_id = JobResultId { value: 111 };
        let job_id = JobId { value: 1 };
        let job_result_data = JobResultData {
            job_id: Some(job_id),
            status: ResultStatus::Success as i32,
            output: Some(ResultOutput {
                items: "test".as_bytes().to_owned(),
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
            .wait_for_result_queue_for_response(&job_id, None, false)
            .await?;
        assert_eq!(res.0.data.unwrap(), job_result_data);
        assert!(res.1.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn test_job_cancellation_protobuf_pubsub() -> Result<()> {
        use super::super::JobQueueCancellationRepository;
        use futures::future::BoxFuture;
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let redis_pool = setup_test_redis_pool().await;
        let redis_client = setup_test_redis_client()?;
        let job_queue_config = Arc::new(JobQueueConfig {
            expire_job_result_seconds: 10,
            fetch_interval: 1000,
        });
        let job_result_pubsub_repository =
            RedisJobResultPubSubRepositoryImpl::new(redis_client, job_queue_config.clone());
        let repo = Arc::new(super::RedisJobQueueRepositoryImpl {
            job_queue_config,
            redis_pool,
            job_result_pubsub_repository,
        });

        // Test job ID
        let test_job_id = JobId { value: 67891 };

        // Set up receiver to capture the cancellation message
        let received_job_ids = Arc::new(Mutex::new(Vec::new()));
        let received_job_ids_clone = received_job_ids.clone();

        let callback: Box<dyn Fn(JobId) -> BoxFuture<'static, Result<()>> + Send + Sync + 'static> =
            Box::new(move |job_id: JobId| {
                let received_job_ids = received_job_ids_clone.clone();
                Box::pin(async move {
                    received_job_ids.lock().await.push(job_id);
                    Ok(())
                }) as BoxFuture<'static, Result<()>>
            });

        let (tx, rx) = tokio::sync::oneshot::channel();
        // Start subscription (spawn to avoid blocking)
        let repo_clone = repo.clone();
        let subscription_handle = tokio::spawn(async move {
            repo_clone
                .subscribe_job_cancellation_with_timeout(callback, 1000, rx)
                .await
        });

        // Small delay to ensure subscription is active
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Broadcast cancellation
        repo.broadcast_job_cancellation(&test_job_id).await?;

        // Wait for message to be processed
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

        let received = received_job_ids.lock().await;
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].value, test_job_id.value);

        // Cleanup: stop subscription
        let _ = tx.send(());
        let _ = subscription_handle.await;

        tracing::info!(
            "Successfully sent and received JobId via protobuf in Redis Pub/Sub cancellation"
        );
        Ok(())
    }
}
