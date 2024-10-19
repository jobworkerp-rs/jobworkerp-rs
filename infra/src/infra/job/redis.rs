pub mod schedule;

use self::schedule::RedisJobScheduleRepository;
use super::queue::redis::RedisJobQueueRepository;
use super::rows::UseJobqueueAndCodec;
use super::status::redis::RedisJobStatusRepository;
use super::status::{JobStatusRepository, UseJobStatusRepository};
use crate::error::JobWorkerError;
use crate::infra::{JobQueueConfig, UseJobQueueConfig};
use anyhow::Result;
use async_trait::async_trait;
use debug_stub_derive::DebugStub;
use infra_utils::infra::redis::{RedisPool, UseRedisLock, UseRedisPool};
use prost::Message;
use proto::jobworkerp::data::{Job, JobData, JobId};
use redis::AsyncCommands;
use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::Arc;

// TODO use if you need (not using in default)
#[async_trait]
pub trait RedisJobRepository: UseRedisPool + Sync + 'static
where
    Self: Send + 'static,
{
    const CACHE_KEY: &'static str = "JOB_DEF";

    // use for cache
    async fn create(&self, id: &JobId, job: &JobData) -> Result<()> {
        let res: Result<bool> = self
            .redis_pool()
            .get()
            .await?
            .hset_nx(Self::CACHE_KEY, id.value, Self::serialize_job(job))
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into());
        match res {
            Ok(r) => {
                if r {
                    Ok(())
                } else {
                    Err(JobWorkerError::AlreadyExists(format!(
                        "job creation error: already exists id={}",
                        id.value
                    ))
                    .into())
                }
            }
            Err(e) => Err(e),
        }
    }

    async fn upsert(&self, id: &JobId, job: &JobData) -> Result<bool> {
        let m = Self::serialize_job(job);

        let res: Result<bool> = self
            .redis_pool()
            .get()
            .await?
            .hset(Self::CACHE_KEY, id.value, m)
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into());
        res
    }

    async fn delete(&self, id: &JobId) -> Result<bool> {
        self.redis_pool()
            .get()
            .await?
            .hdel(Self::CACHE_KEY, id.value)
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into())
    }

    async fn find(&self, id: &JobId) -> Result<Option<Job>> {
        match self
            .redis_pool()
            .get()
            .await?
            .hget(Self::CACHE_KEY, id.value)
            .await
        {
            Ok(Some(v)) => Self::deserialize_to_job(&v).map(|d| {
                Some(Job {
                    id: Some(*id),
                    data: Some(d),
                })
            }),
            Ok(None) => Ok(None),
            Err(e) => Err(JobWorkerError::RedisError(e).into()),
        }
    }

    async fn find_all(&self) -> Result<Vec<Job>> {
        let res: Result<BTreeMap<i64, Vec<u8>>> = self
            .redis_pool()
            .get()
            .await?
            .hgetall(Self::CACHE_KEY)
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into());
        res.map(|tree| {
            tree.iter()
                .flat_map(|(id, v)| {
                    Self::deserialize_to_job(v).map(|d| Job {
                        id: Some(JobId { value: *id }),
                        data: Some(d),
                    })
                })
                .collect()
        })
    }

    async fn count(&self) -> Result<i64> {
        self.redis_pool()
            .get()
            .await?
            .hlen(Self::CACHE_KEY)
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into())
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
    pub redis_job_status_repository: Arc<RedisJobStatusRepository>,
}

impl RedisJobRepositoryImpl {
    pub fn new(job_queue_config: Arc<JobQueueConfig>, redis_pool: &'static RedisPool) -> Self {
        Self {
            job_queue_config,
            redis_pool,
            redis_job_status_repository: Arc::new(RedisJobStatusRepository::new(redis_pool)),
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
impl UseJobqueueAndCodec for RedisJobRepositoryImpl {}
impl RedisJobQueueRepository for RedisJobRepositoryImpl {}
impl UseJobStatusRepository for RedisJobRepositoryImpl {
    fn job_status_repository(&self) -> Arc<dyn JobStatusRepository> {
        self.redis_job_status_repository.clone()
    }
}
impl UseRedisLock for RedisJobRepositoryImpl {}
impl RedisJobScheduleRepository for RedisJobRepositoryImpl {}

pub trait UseRedisJobRepository {
    fn redis_job_repository(&self) -> &RedisJobRepositoryImpl;
}

#[tokio::test]
async fn redis_test() -> Result<()> {
    use command_utils::util::option::FlatMap;
    use proto::jobworkerp::data::WorkerId;

    let pool = infra_utils::infra::test::setup_test_redis_pool().await;
    let job_queue_config = Arc::new(JobQueueConfig {
        expire_job_result_seconds: 60,
        fetch_interval: 1000,
    });

    let repo = RedisJobRepositoryImpl {
        job_queue_config,
        redis_pool: pool,
        redis_job_status_repository: Arc::new(RedisJobStatusRepository::new(pool)),
    };
    let id = JobId { value: 1 };
    let jarg = RedisJobRepositoryImpl::serialize_message(&proto::jobworkerp::data::TestArg {
        args: vec!["GET".to_string(), "/".to_string()],
    });
    let job = &JobData {
        worker_id: Some(WorkerId { value: 2 }),
        arg: jarg,
        uniq_key: Some("hoge3".to_string()),
        enqueue_time: 5,
        grabbed_until_time: Some(6),
        run_after_time: 7,
        retried: 8,
        priority: 9,
        timeout: 1000,
    };
    // clear first
    repo.delete(&id).await?;

    // create and find
    repo.create(&id, job).await?;
    assert!(repo.create(&id, job).await.err().is_some()); // already exists
    let res = repo.find(&id).await?;
    assert_eq!(res.flat_map(|r| r.data).as_ref(), Some(job));

    let mut job2 = job.clone();
    job2.worker_id = Some(WorkerId { value: 3 });
    job2.arg = RedisJobRepositoryImpl::serialize_message(&proto::jobworkerp::data::TestArg {
        args: vec!["POST".to_string(), "/form".to_string()],
    });
    job2.uniq_key = Some("fuga3".to_string());
    job2.enqueue_time = 6;
    job2.grabbed_until_time = Some(7);
    job2.run_after_time = 8;
    job2.retried = 9;
    job2.priority = 10;
    job2.timeout = 2000;
    // update and find
    assert!(!repo.upsert(&id, &job2).await?);
    let res2 = repo.find(&id).await?;
    assert_eq!(res2.flat_map(|r| r.data).as_ref(), Some(&job2));

    // delete and not found
    assert!(repo.delete(&id).await?);
    assert_eq!(repo.find(&id).await?, None);

    Ok(())
}
