use crate::error::JobWorkerError;
use crate::infra::job::rows::UseJobqueueAndCodec;
use anyhow::Result;
use async_trait::async_trait;
use command_utils::util::option::FlatMap;
use command_utils::util::result::Exists;
use debug_stub_derive::DebugStub;
use infra_utils::infra::redis::{RedisPool, UseRedisClient, UseRedisPool};
use prost::Message;
use proto::jobworkerp::data::{Worker, WorkerData, WorkerId};
use redis::AsyncCommands;
use std::collections::BTreeMap;
use std::io::Cursor;

use super::event::UseWorkerPublish;

// TODO use if you need (not using in default)
#[async_trait]
pub trait RedisWorkerRepository: UseRedisPool + UseWorkerPublish + Sync + 'static
where
    Self: Send + 'static,
{
    const CACHE_KEY: &'static str = "WORKER_DEF";
    const NAME_CACHE_KEY: &'static str = "WORKER_NAME_CACHE";

    fn expire_sec(&self) -> Option<usize>;

    /// update if exists, create if not exists
    /// if worker.data.name is None, do nothing
    ///
    /// XXX different key for id
    ///
    /// # Returns
    /// - Ok(true) if created
    /// - Ok(false) if updated or not exists
    async fn upsert(&self, worker: &Worker) -> Result<bool> {
        self._upsert_by_name(worker).await?;
        if let (Some(i), Some(d)) = (worker.id.as_ref(), worker.data.as_ref()) {
            self._upsert_by_id(i, d).await
        } else if let Some(i) = worker.id.as_ref() {
            self.delete(i).await?;
            Ok(false)
        } else {
            Ok(false)
        }
    }

    /// update if exists, create if not exists specified with worker name
    /// if worker.data.name is None, do nothing
    ///
    /// XXX different key for id
    ///
    /// # Returns
    /// - Ok(true) if created
    /// - Ok(false) if updated
    async fn _upsert_by_name(&self, worker: &Worker) -> Result<()> {
        if let Some(n) = worker.data.as_ref().map(|d| &d.name) {
            let mut p = self.redis_pool().get().await?;
            let res = p
                .hset(Self::NAME_CACHE_KEY, n, Self::serialize_worker(worker))
                .await
                .map_err(JobWorkerError::RedisError)?;
            if res {
                // on created
                if let Some(ex) = self.expire_sec() {
                    p.expire::<&str, ()>(Self::NAME_CACHE_KEY, ex as i64)
                        .await?
                };
            }
            Ok(())
        } else {
            // do nothing
            Ok(())
        }
    }

    /// update if exists, create if not exists
    ///
    /// # Returns
    /// - Ok(true) if created
    /// - Ok(false) if updated
    async fn _upsert_by_id(&self, id: &WorkerId, worker: &WorkerData) -> Result<bool> {
        let w = Worker {
            id: Some(*id),
            data: Some(worker.clone()),
        };
        let m = Self::serialize_worker(&w);
        let mut p = self.redis_pool().get().await?;
        let res: Result<bool> = p
            .hset(Self::CACHE_KEY, id.value, m)
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into());
        if res.as_ref().exists(|r| *r) {
            if let Some(ex) = self.expire_sec() {
                p.expire::<&str, ()>(Self::CACHE_KEY, ex as i64).await?;
            };
        }
        res
    }

    async fn delete(&self, id: &WorkerId) -> Result<bool> {
        let g = self.find(id).await?;
        if let Some(wn) = g.as_ref().flat_map(|w| w.data.as_ref().map(|d| &d.name)) {
            self.delete_by_name(wn).await?;
            self.delete_by_id(id).await
        } else {
            Ok(false)
        }
    }
    async fn delete_by_id(&self, id: &WorkerId) -> Result<bool> {
        self.redis_pool()
            .get()
            .await?
            .hdel(Self::CACHE_KEY, id.value)
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into())
    }

    async fn delete_by_name(&self, name: &String) -> Result<bool> {
        self.redis_pool()
            .get()
            .await?
            .hdel(Self::NAME_CACHE_KEY, name)
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into())
    }

    async fn delete_all(&self) -> Result<bool> {
        self.redis_pool()
            .get()
            .await?
            .del::<&str, ()>(Self::CACHE_KEY)
            .await
            .map_err(JobWorkerError::RedisError)?;
        self.redis_pool()
            .get()
            .await?
            .del::<&str, bool>(Self::NAME_CACHE_KEY)
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into())
    }

    async fn find(&self, id: &WorkerId) -> Result<Option<Worker>> {
        match self
            .redis_pool()
            .get()
            .await?
            .hget(Self::CACHE_KEY, id.value)
            .await
        {
            Ok(Some(v)) => Self::deserialize_worker(&v).map(Some),
            Ok(None) => Ok(None),
            Err(e) => Err(JobWorkerError::RedisError(e).into()),
        }
    }

    //XXX different key for id
    async fn find_by_name(&self, name: &str) -> Result<Option<Worker>> {
        match self
            .redis_pool()
            .get()
            .await?
            .hget(Self::NAME_CACHE_KEY, name)
            .await
        {
            Ok(Some(v)) => Self::deserialize_worker(&v).map(Some),
            Ok(None) => Ok(None),
            Err(e) => Err(JobWorkerError::RedisError(e).into()),
        }
    }

    async fn find_all(&self) -> Result<Vec<Worker>> {
        let res: Result<BTreeMap<i64, Vec<u8>>> = self
            .redis_pool()
            .get()
            .await?
            .hgetall(Self::CACHE_KEY)
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into());
        res.map(|tree| {
            tree.iter()
                .flat_map(|(_id, v)| Self::deserialize_worker(v))
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

    fn deserialize_bytes_to_worker(buf: &[u8]) -> Result<Worker> {
        Worker::decode(&mut Cursor::new(buf)).map_err(|e| JobWorkerError::CodecError(e).into())
    }
}

#[derive(Clone, DebugStub)]
pub struct RedisWorkerRepositoryImpl {
    #[debug_stub = "&`static RedisPool"]
    pub redis_pool: &'static RedisPool,
    pub redis_client: deadpool_redis::redis::Client, // for pubsub
    pub timeout_sec: Option<usize>,
}

impl RedisWorkerRepositoryImpl {
    pub fn new(
        redis_pool: &'static RedisPool,
        redis_client: deadpool_redis::redis::Client,
        timeout_sec: Option<usize>,
    ) -> Self {
        Self {
            redis_pool,
            redis_client,
            timeout_sec,
        }
    }
}

impl UseRedisPool for RedisWorkerRepositoryImpl {
    fn redis_pool(&self) -> &'static RedisPool {
        self.redis_pool
    }
}
impl UseRedisClient for RedisWorkerRepositoryImpl {
    fn redis_client(&self) -> &deadpool_redis::redis::Client {
        &self.redis_client
    }
}

impl UseJobqueueAndCodec for RedisWorkerRepositoryImpl {}
impl UseWorkerPublish for RedisWorkerRepositoryImpl {}

impl RedisWorkerRepository for RedisWorkerRepositoryImpl {
    fn expire_sec(&self) -> Option<usize> {
        self.timeout_sec
    }
}

pub trait UseRedisWorkerRepository {
    fn redis_worker_repository(&self) -> &RedisWorkerRepositoryImpl;
}

#[tokio::test]
async fn redis_test() -> Result<()> {
    use command_utils::util::option::FlatMap;
    use proto::jobworkerp::data::RetryPolicy;
    use proto::jobworkerp::data::{
        CommandOperation, QueueType, ResponseType, RunnerSchemaId, WorkerData, WorkerId,
    };

    let pool = infra_utils::infra::test::setup_test_redis_pool().await;
    let cli = infra_utils::infra::test::setup_test_redis_client()?;

    let repo = RedisWorkerRepositoryImpl {
        redis_pool: pool,
        redis_client: cli,
        timeout_sec: Some(10),
    };
    let id = WorkerId { value: 1 };
    let worker = &WorkerData {
        name: "hoge1".to_string(),
        schema_id: Some(RunnerSchemaId { value: 2 }),
        operation: RedisWorkerRepositoryImpl::serialize_message(&CommandOperation {
            name: "hoge1".to_string(),
        }),
        retry_policy: Some(RetryPolicy {
            r#type: 5,
            interval: 6,
            max_interval: 7,
            max_retry: 8,
            basis: 9.0,
        }),
        periodic_interval: 11,
        channel: Some("hoge9".to_string()),
        queue_type: QueueType::Rdb as i32,
        response_type: ResponseType::ListenAfter as i32,
        store_success: true,
        store_failure: true,
        next_workers: vec![],
        use_static: false,
    };
    // clear first
    repo.delete(&id).await?;
    let wk = Worker {
        id: Some(id),
        data: Some(worker.clone()),
    };
    // create and find
    assert!(repo.upsert(&wk).await?); // newly created
    assert!(!(repo.upsert(&wk).await?)); // already exists (update)
    let res = repo.find(&id).await?;
    assert_eq!(res.flat_map(|r| r.data).as_ref(), Some(worker));

    let mut worker2 = worker.clone();
    worker2.name = "fuga1".to_string();
    worker2.schema_id = Some(RunnerSchemaId { value: 5 });
    worker2.operation = RedisWorkerRepositoryImpl::serialize_message(&CommandOperation {
        name: "fuga2".to_string(),
    });
    worker2.retry_policy = Some(RetryPolicy {
        r#type: 6,
        interval: 7,
        max_interval: 8,
        max_retry: 9,
        basis: 10.0,
    });
    worker2.periodic_interval = 12;
    worker2.channel = Some("fuga8".to_string());
    worker2.queue_type = QueueType::Redis as i32;
    worker2.response_type = ResponseType::Direct as i32;
    worker2.store_success = false;
    worker2.store_failure = false;

    let wk2 = Worker {
        id: Some(id),
        data: Some(worker2.clone()),
    };
    // update and find
    assert!(!repo.upsert(&wk2).await?);
    let res2 = repo.find(&id).await?;
    assert_eq!(res2.flat_map(|r| r.data).as_ref(), Some(&worker2));

    // delete and not found
    assert!(repo.delete(&id).await?);
    assert_eq!(repo.find(&id).await?, None);

    Ok(())
}
