use crate::infra::job::rows::UseJobqueueAndCodec;
use crate::infra::JobQueueConfig;
use crate::{error::JobWorkerError, infra::UseJobQueueConfig};
use anyhow::Result;
use async_trait::async_trait;
use debug_stub_derive::DebugStub;
use infra_utils::infra::redis::{RedisPool, UseRedisPool};
use proto::jobworkerp::data::{JobId, JobResult, JobResultData, JobResultId, ResponseType};
use redis::AsyncCommands;
use std::collections::BTreeMap;
use std::sync::Arc;

// TODO use if you need (not using in default)
#[async_trait]
pub trait RedisJobResultRepository:
    UseRedisPool + UseJobqueueAndCodec + UseJobQueueConfig + Sync + 'static
// + UseRedisClient
where
    Self: Send + 'static,
{
    const CACHE_KEY: &'static str = "JOB_RESULT_DEF";

    #[inline]
    fn job_id_cache_key(job_id: &JobId) -> String {
        ["jr:jid:", &job_id.value.to_string()].join("")
    }

    async fn create(&self, id: &JobResultId, job_result: &JobResultData) -> Result<()> {
        let job_id = job_result.job_id.as_ref();
        let mut con = self.redis_pool().get().await?;
        let v = Self::serialize_job_result(id.clone(), job_result.clone());
        let res: Result<bool> = con
            .hset_nx(Self::CACHE_KEY, id.value, &v)
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into());

        if let Some(jid) = job_id {
            // set cache for job_id
            if job_result.response_type == ResponseType::ListenAfter as i32 {
                let _jr: Result<bool> = con
                    .set_ex(
                        Self::job_id_cache_key(jid),
                        &v,
                        self.job_queue_config().expire_job_result_seconds as u64,
                    )
                    .await
                    .map_err(|e| JobWorkerError::RedisError(e).into());
            }
        }

        match res {
            Ok(r) => {
                if r {
                    Ok(())
                } else {
                    Err(JobWorkerError::AlreadyExists(format!(
                        "job_result creation error: already exists id={}",
                        id.value
                    ))
                    .into())
                }
            }
            Err(e) => Err(e),
        }
    }

    async fn upsert(&self, id: &JobResultId, job_result: &JobResultData) -> Result<bool> {
        let job_id = job_result.job_id.as_ref();
        let mut con = self.redis_pool().get().await?;
        let v = Self::serialize_job_result(id.clone(), job_result.clone());
        let res: Result<bool> = con
            .hset(Self::CACHE_KEY, id.value, &v)
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into());

        if let Some(jid) = job_id {
            // set cache for job_id
            if job_result.response_type == ResponseType::ListenAfter as i32 {
                let _jr: Result<bool> = con
                    .set_ex(
                        Self::job_id_cache_key(jid),
                        &v,
                        self.job_queue_config().expire_job_result_seconds as u64,
                    )
                    .await
                    .map_err(|e| JobWorkerError::RedisError(e).into());
            }
        }

        res
    }

    // only use for cache by job_id (response_type=ListenAfter)
    async fn upsert_only_by_job_id(
        &self,
        id: &JobResultId,
        job_result: &JobResultData,
    ) -> Result<bool> {
        let job_id = job_result.job_id.as_ref();
        let v = Self::serialize_job_result(id.clone(), job_result.clone());
        if let Some(jid) = job_id {
            // set cache for job_id
            self.redis_pool()
                .get()
                .await?
                .set_ex(
                    Self::job_id_cache_key(jid),
                    &v,
                    self.job_queue_config().expire_job_result_seconds as u64,
                )
                .await
                .map_err(|e| JobWorkerError::RedisError(e).into())
        } else {
            Ok(false)
        }
    }

    // XXX not delete job_id cache (keep until expire)
    async fn delete(&self, id: &JobResultId) -> Result<bool> {
        self.redis_pool()
            .get()
            .await?
            .hdel(Self::CACHE_KEY, id.value)
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into())
    }

    async fn find(&self, id: &JobResultId) -> Result<Option<JobResult>> {
        match self
            .redis_pool()
            .get()
            .await?
            .hget(Self::CACHE_KEY, id.value)
            .await
        {
            Ok(Some(v)) => Self::deserialize_job_result(&v).map(Some),
            Ok(None) => Ok(None),
            Err(e) => Err(JobWorkerError::RedisError(e).into()),
        }
    }

    async fn find_by_job_id(&self, job_id: &JobId) -> Result<Option<JobResult>> {
        match self
            .redis_pool()
            .get()
            .await?
            .get(Self::job_id_cache_key(job_id))
            .await
        {
            Ok(Some(v)) => Self::deserialize_job_result(&v).map(Some),
            Ok(None) => Ok(None),
            Err(e) => Err(JobWorkerError::RedisError(e).into()),
        }
    }

    async fn find_all(&self) -> Result<Vec<JobResult>> {
        let res: Result<BTreeMap<i64, Vec<u8>>> = self
            .redis_pool()
            .get()
            .await?
            .hgetall(Self::CACHE_KEY)
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into());
        res.map(|tree| {
            tree.iter()
                .flat_map(|(_id, v)| Self::deserialize_job_result(v))
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
}

#[derive(Clone, DebugStub)]
pub struct RedisJobResultRepositoryImpl {
    job_queue_config: Arc<JobQueueConfig>,
    #[debug_stub = "&'static RedisPool"]
    pub redis_pool: &'static RedisPool,
}

impl RedisJobResultRepositoryImpl {
    pub fn new(job_queue_config: Arc<JobQueueConfig>, redis_pool: &'static RedisPool) -> Self {
        Self {
            job_queue_config,
            // redis_client,
            redis_pool,
        }
    }
}

impl UseRedisPool for RedisJobResultRepositoryImpl {
    fn redis_pool(&self) -> &'static RedisPool {
        self.redis_pool
    }
}
// // for subscribe result channel
// impl UseRedisClient for RedisJobResultRepositoryImpl {
//     fn redis_client(&self) -> &redis::Client {
//         &self.redis_client
//     }
// }

impl UseJobqueueAndCodec for RedisJobResultRepositoryImpl {}
impl UseJobQueueConfig for RedisJobResultRepositoryImpl {
    fn job_queue_config(&self) -> &JobQueueConfig {
        &self.job_queue_config
    }
}

impl RedisJobResultRepository for RedisJobResultRepositoryImpl {}

pub trait UseRedisJobResultRepository {
    fn redis_job_result_repository(&self) -> &RedisJobResultRepositoryImpl;
}

#[tokio::test]
async fn redis_test() -> Result<()> {
    use command_utils::util::option::FlatMap;
    use proto::jobworkerp::data::{JobId, ResponseType, ResultOutput, RunnerArg, WorkerId};

    let pool = infra_utils::infra::test::setup_test_redis_pool().await;
    let job_queue_config = Arc::new(JobQueueConfig {
        expire_job_result_seconds: 60,
        fetch_interval: 1000,
    });

    let repo = RedisJobResultRepositoryImpl {
        job_queue_config,
        redis_pool: pool,
    };
    let id = JobResultId { value: 1 };
    let jarg = RunnerArg {
        data: Some(proto::jobworkerp::data::runner_arg::Data::GrpcUnary(
            proto::jobworkerp::data::GrpcUnaryArg {
                path: "test".to_string(),
                request: b"test".to_vec(),
            },
        )),
    };
    let job_result = &JobResultData {
        job_id: Some(JobId { value: 1 }),
        worker_id: Some(WorkerId { value: 2 }),
        worker_name: "hoge2".to_string(),
        arg: Some(jarg),
        uniq_key: Some("hoge4".to_string()),
        status: 6,
        output: Some(ResultOutput {
            items: vec!["hoge6".as_bytes().to_vec()],
        }),
        retried: 8,
        max_retry: 9,
        priority: 1,
        timeout: 1000,
        enqueue_time: 9,
        run_after_time: 10,
        start_time: 11,
        end_time: 12,
        response_type: ResponseType::NoResult as i32,
        store_success: true,
        store_failure: true,
    };
    // clear first
    repo.delete(&id).await?;

    // create and find
    repo.create(&id, job_result).await?;
    assert!(repo.create(&id, job_result).await.err().is_some()); // already exists
    let res = repo.find(&id).await?;
    assert_eq!(res.flat_map(|r| r.data).as_ref(), Some(job_result));

    let mut job_result2 = job_result.clone();
    job_result2.worker_id = Some(WorkerId { value: 3 });
    job_result2.worker_name = "fuga2".to_string();
    job_result2.arg = Some(RunnerArg {
        data: Some(proto::jobworkerp::data::runner_arg::Data::GrpcUnary(
            proto::jobworkerp::data::GrpcUnaryArg {
                path: "test2".to_string(),
                request: b"test2".to_vec(),
            },
        )),
    });
    job_result2.uniq_key = Some("fuga4".to_string());
    job_result2.status = 7;
    job_result2.output = Some(ResultOutput {
        items: vec!["fuga6".as_bytes().to_vec()],
    });
    job_result2.max_retry = 9;
    job_result2.retried = 9;
    job_result2.priority = -1;
    job_result2.timeout = 2000;
    job_result2.enqueue_time = 10;
    job_result2.run_after_time = 11;
    job_result2.start_time = 12;
    job_result2.end_time = 13;
    job_result2.response_type = ResponseType::Direct as i32;
    job_result2.store_success = false;
    job_result2.store_failure = false;
    // update and find
    assert!(!repo.upsert(&id, &job_result2).await?);
    let res2 = repo.find(&id).await?;
    assert_eq!(res2.flat_map(|r| r.data).as_ref(), Some(&job_result2));

    // delete and not found
    assert!(repo.delete(&id).await?);
    assert_eq!(repo.find(&id).await?, None);

    Ok(())
}
