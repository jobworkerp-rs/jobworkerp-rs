pub mod schedule;

use self::schedule::RedisJobScheduleRepository;
use super::queue::redis::RedisJobQueueRepository;
use super::rows::UseJobqueueAndCodec;
use super::status::redis::RedisJobProcessingStatusRepository;
use super::status::{JobProcessingStatusRepository, UseJobProcessingStatusRepository};
use crate::infra::job_result::pubsub::redis::{
    RedisJobResultPubSubRepositoryImpl, UseRedisJobResultPubSubRepository,
};
use crate::infra::{JobQueueConfig, UseJobQueueConfig};
use anyhow::Result;
use async_trait::async_trait;
use debug_stub_derive::DebugStub;
use infra_utils::infra::redis::{RedisClient, RedisPool, UseRedisLock, UseRedisPool};
use jobworkerp_base::codec::UseProstCodec;
use jobworkerp_base::error::JobWorkerError;
use prost::Message;
use proto::jobworkerp::data::{Job, JobData, JobId};
use redis::AsyncCommands;
use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

// TODO use if you need (not using in default)
#[async_trait]
pub trait RedisJobRepository: UseRedisPool + Sync + 'static
where
    Self: Send + 'static,
{
    const INDIVIDUAL_JOB_KEY_PREFIX: &'static str = "JOB_INDIVIDUAL:";

    // create with individual key TTL for running job visibility (replaces old create)
    async fn create_with_expire(&self, id: &JobId, job: &JobData, ttl: Duration) -> Result<()> {
        let job_key = format!("{}{}", Self::INDIVIDUAL_JOB_KEY_PREFIX, id.value);
        let serialized_job = Self::serialize_job(job);

        let mut conn = self.redis_pool().get().await?;

        let result: Result<String> = conn
            .set_ex(&job_key, serialized_job, ttl.as_secs())
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into());

        match result {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn upsert(&self, id: &JobId, job: &JobData, ttl: Duration) -> Result<bool> {
        let job_key = format!("{}{}", Self::INDIVIDUAL_JOB_KEY_PREFIX, id.value);
        let serialized_job = Self::serialize_job(job);

        let mut conn = self.redis_pool().get().await?;

        let result: Result<String> = conn
            .set_ex(&job_key, serialized_job, ttl.as_secs())
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into());

        match result {
            Ok(_) => Ok(true), // Always consider upsert as successful update
            Err(e) => Err(e),
        }
    }

    async fn delete(&self, id: &JobId) -> Result<bool> {
        let job_key = format!("{}{}", Self::INDIVIDUAL_JOB_KEY_PREFIX, id.value);
        let deleted: i32 = self
            .redis_pool()
            .get()
            .await?
            .del(&job_key)
            .await
            .map_err(JobWorkerError::RedisError)?;
        Ok(deleted > 0)
    }

    async fn find(&self, id: &JobId) -> Result<Option<Job>> {
        let job_key = format!("{}{}", Self::INDIVIDUAL_JOB_KEY_PREFIX, id.value);

        match self
            .redis_pool()
            .get()
            .await?
            .get::<_, Option<Vec<u8>>>(&job_key)
            .await
        {
            Ok(Some(v)) => {
                tracing::debug!("Found job {} from individual TTL key", id.value);
                Self::deserialize_to_job(&v).map(|d| {
                    Some(Job {
                        id: Some(*id),
                        data: Some(d),
                        ..Default::default()
                    })
                })
            }
            Ok(None) => {
                tracing::debug!("Job {} not found in Redis individual key", id.value);
                Ok(None)
            }
            Err(e) => Err(JobWorkerError::RedisError(e).into()),
        }
    }

    fn serialize_job(w: &JobData) -> Vec<u8> {
        let mut buf = Vec::with_capacity(w.encoded_len());
        w.encode(&mut buf).unwrap();
        buf
    }

    fn deserialize_to_job(buf: &Vec<u8>) -> Result<JobData> {
        JobData::decode(&mut Cursor::new(buf)).map_err(|e| JobWorkerError::CodecError(e).into())
    }
    fn deserialize_bytes_to_job(buf: &[u8]) -> Result<JobData> {
        JobData::decode(&mut Cursor::new(buf)).map_err(|e| JobWorkerError::CodecError(e).into())
    }
}

#[derive(Clone, DebugStub)]
pub struct RedisJobRepositoryImpl {
    job_queue_config: Arc<JobQueueConfig>,
    #[debug_stub = "RedisPool"]
    pub redis_pool: &'static RedisPool,
    pub redis_job_processing_status_repository: Arc<RedisJobProcessingStatusRepository>,
    pub job_result_pubsub_repository: RedisJobResultPubSubRepositoryImpl,
}

impl RedisJobRepositoryImpl {
    pub fn new(
        job_queue_config: Arc<JobQueueConfig>,
        redis_pool: &'static RedisPool,
        redis_client: RedisClient,
    ) -> Self {
        Self {
            job_queue_config: job_queue_config.clone(),
            redis_pool,
            redis_job_processing_status_repository: Arc::new(
                RedisJobProcessingStatusRepository::new(redis_pool),
            ),
            job_result_pubsub_repository: RedisJobResultPubSubRepositoryImpl::new(
                redis_client,
                job_queue_config,
            ),
        }
    }
}

impl UseRedisPool for RedisJobRepositoryImpl {
    fn redis_pool(&self) -> &'static RedisPool {
        self.redis_pool
    }
}

// impl<T: UseRedisPool + Send + Sync + 'static> RedisJobRepository for T {}
impl RedisJobRepository for RedisJobRepositoryImpl {}

impl UseJobQueueConfig for RedisJobRepositoryImpl {
    fn job_queue_config(&self) -> &JobQueueConfig {
        &self.job_queue_config
    }
}
// for use jobqueue by redis
impl UseProstCodec for RedisJobRepositoryImpl {}
impl UseJobqueueAndCodec for RedisJobRepositoryImpl {}
impl RedisJobQueueRepository for RedisJobRepositoryImpl {}
impl UseRedisJobResultPubSubRepository for RedisJobRepositoryImpl {
    fn job_result_pubsub_repository(
        &self,
    ) -> &crate::infra::job_result::pubsub::redis::RedisJobResultPubSubRepositoryImpl {
        &self.job_result_pubsub_repository
    }
}
impl UseJobProcessingStatusRepository for RedisJobRepositoryImpl {
    fn job_processing_status_repository(&self) -> Arc<dyn JobProcessingStatusRepository> {
        self.redis_job_processing_status_repository.clone()
    }
}
impl UseRedisLock for RedisJobRepositoryImpl {}
impl RedisJobScheduleRepository for RedisJobRepositoryImpl {}

pub trait UseRedisJobRepository {
    fn redis_job_repository(&self) -> &RedisJobRepositoryImpl;
}

#[tokio::test]
async fn redis_test() -> Result<()> {
    use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
    use proto::jobworkerp::data::WorkerId;

    let pool = infra_utils::infra::test::setup_test_redis_pool().await;
    let redis_client = infra_utils::infra::test::setup_test_redis_client()?;
    let job_queue_config = Arc::new(JobQueueConfig {
        expire_job_result_seconds: 60,
        fetch_interval: 1000,
    });

    let repo = RedisJobRepositoryImpl {
        job_queue_config: job_queue_config.clone(),
        redis_pool: pool,
        redis_job_processing_status_repository: Arc::new(RedisJobProcessingStatusRepository::new(
            pool,
        )),
        job_result_pubsub_repository:
            crate::infra::job_result::pubsub::redis::RedisJobResultPubSubRepositoryImpl::new(
                redis_client,
                job_queue_config.clone(),
            ),
    };
    let id = JobId { value: 1 };
    let jargs = ProstMessageCodec::serialize_message(&proto::TestArgs {
        args: vec!["GET".to_string(), "/".to_string()],
    })?;
    let job = &JobData {
        worker_id: Some(WorkerId { value: 2 }),
        args: jargs,
        uniq_key: Some("hoge3".to_string()),
        enqueue_time: 5,
        grabbed_until_time: Some(6),
        run_after_time: 7,
        retried: 8,
        priority: 9,
        timeout: 1000,
        streaming_type: 1,
        using: None,
    };
    // clear first
    repo.delete(&id).await?;

    // create and find
    let ttl = Duration::from_secs(3600); // 1 hour TTL
    repo.create_with_expire(&id, job, ttl).await?;
    let res = repo.find(&id).await?;
    assert_eq!(res.and_then(|r| r.data).as_ref(), Some(job));

    let mut job2 = job.clone();
    job2.worker_id = Some(WorkerId { value: 3 });
    job2.args = ProstMessageCodec::serialize_message(&proto::TestArgs {
        args: vec!["POST".to_string(), "/form".to_string()],
    })?;
    job2.uniq_key = Some("fuga3".to_string());
    job2.enqueue_time = 6;
    job2.grabbed_until_time = Some(7);
    job2.run_after_time = 8;
    job2.retried = 9;
    job2.priority = 10;
    job2.timeout = 2000;
    job2.streaming_type = 0;
    // update and find
    assert!(repo.upsert(&id, &job2, ttl).await?);
    let res2 = repo.find(&id).await?;
    assert_eq!(res2.and_then(|r| r.data).as_ref(), Some(&job2));

    // delete and not found
    assert!(repo.delete(&id).await?);
    assert_eq!(repo.find(&id).await?, None);

    Ok(())
}

#[tokio::test]
async fn redis_individual_ttl_test() -> Result<()> {
    use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
    use proto::jobworkerp::data::WorkerId;
    use std::time::Duration;

    let pool = infra_utils::infra::test::setup_test_redis_pool().await;
    let redis_client = infra_utils::infra::test::setup_test_redis_client()?;
    let job_queue_config = Arc::new(JobQueueConfig {
        expire_job_result_seconds: 60,
        fetch_interval: 1000,
    });

    let repo = RedisJobRepositoryImpl {
        job_queue_config: job_queue_config.clone(),
        redis_pool: pool,
        redis_job_processing_status_repository: Arc::new(RedisJobProcessingStatusRepository::new(
            pool,
        )),
        job_result_pubsub_repository:
            crate::infra::job_result::pubsub::redis::RedisJobResultPubSubRepositoryImpl::new(
                redis_client,
                job_queue_config.clone(),
            ),
    };

    let id = JobId { value: 12345 };
    let jargs = ProstMessageCodec::serialize_message(&proto::TestArgs {
        args: vec!["sleep".to_string(), "1".to_string()],
    })?;
    let job = &JobData {
        worker_id: Some(WorkerId { value: 2 }),
        args: jargs,
        uniq_key: Some("ttl_test".to_string()),
        enqueue_time: 5,
        grabbed_until_time: Some(6),
        run_after_time: 7,
        retried: 8,
        priority: 9,
        timeout: 5000, // 5 seconds
        streaming_type: 0,
        using: None,
    };

    // Test create_with_expire and find
    let ttl = Duration::from_secs(10); // 10 seconds TTL
    repo.create_with_expire(&id, job, ttl).await?;

    // Should be able to find from individual key
    let found_job = repo.find(&id).await?;
    assert!(found_job.is_some());
    assert_eq!(found_job.unwrap().data.as_ref(), Some(job));

    // Wait for TTL to expire and test again
    tokio::time::sleep(Duration::from_secs(11)).await;
    let expired_result = repo.find(&id).await?;
    assert!(expired_result.is_none());

    tracing::info!("Individual TTL test completed successfully");
    Ok(())
}
