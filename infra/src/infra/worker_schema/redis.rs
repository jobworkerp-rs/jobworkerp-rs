use crate::error::JobWorkerError;
use anyhow::Result;
use async_trait::async_trait;
use infra_utils::infra::redis::{RedisPool, UseRedisPool};
use prost::Message;
use proto::jobworkerp::data::{WorkerSchema, WorkerSchemaData, WorkerSchemaId};
use redis::AsyncCommands;
use std::collections::BTreeMap;
use std::io::Cursor;

// TODO use if you need (not using in default)
#[async_trait]
pub trait RedisWorkerSchemaRepository: UseRedisPool + Sync + 'static
where
    Self: Send + 'static,
{
    const CACHE_KEY: &'static str = "RUNNER_SCHEMA_DEF";

    async fn create(&self, id: &WorkerSchemaId, worker_schema: &WorkerSchemaData) -> Result<()> {
        let res: Result<bool> = self
            .redis_pool()
            .get()
            .await?
            .hset_nx(
                Self::CACHE_KEY,
                id.value,
                Self::serialize_worker_schema(worker_schema),
            )
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into());
        match res {
            Ok(r) => {
                if r {
                    Ok(())
                } else {
                    Err(JobWorkerError::AlreadyExists(format!(
                        "worker_schema creation error: already exists id={}",
                        id.value
                    ))
                    .into())
                }
            }
            Err(e) => Err(e),
        }
    }

    async fn upsert(&self, id: &WorkerSchemaId, worker_schema: &WorkerSchemaData) -> Result<bool> {
        let m = Self::serialize_worker_schema(worker_schema);

        let res: Result<bool> = self
            .redis_pool()
            .get()
            .await?
            .hset(Self::CACHE_KEY, id.value, m)
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into());
        res
    }

    async fn delete(&self, id: &WorkerSchemaId) -> Result<bool> {
        self.redis_pool()
            .get()
            .await?
            .hdel(Self::CACHE_KEY, id.value)
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into())
    }

    async fn find(&self, id: &WorkerSchemaId) -> Result<Option<WorkerSchema>> {
        match self
            .redis_pool()
            .get()
            .await?
            .hget(Self::CACHE_KEY, id.value)
            .await
        {
            Ok(Some(v)) => Self::deserialize_to_worker_schema(&v).map(|d| {
                Some(WorkerSchema {
                    id: Some(*id),
                    data: Some(d),
                })
            }),
            Ok(None) => Ok(None),
            Err(e) => Err(JobWorkerError::RedisError(e).into()),
        }
    }

    async fn find_all(&self) -> Result<Vec<WorkerSchema>> {
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
                    Self::deserialize_to_worker_schema(v).map(|d| WorkerSchema {
                        id: Some(WorkerSchemaId { value: *id }),
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

    fn serialize_worker_schema(w: &WorkerSchemaData) -> Vec<u8> {
        let mut buf = Vec::with_capacity(w.encoded_len());
        w.encode(&mut buf).unwrap();
        buf
    }

    fn deserialize_to_worker_schema(buf: &Vec<u8>) -> Result<WorkerSchemaData> {
        WorkerSchemaData::decode(&mut Cursor::new(buf))
            .map_err(|e| JobWorkerError::CodecError(e).into())
    }
    fn deserialize_bytes_to_worker_schema(buf: &[u8]) -> Result<WorkerSchemaData> {
        WorkerSchemaData::decode(&mut Cursor::new(buf))
            .map_err(|e| JobWorkerError::CodecError(e).into())
    }
}

impl<T: UseRedisPool + Send + Sync + 'static> RedisWorkerSchemaRepository for T {}

#[derive(Clone, Debug)]
pub struct RedisWorkerSchemaRepositoryImpl {
    pub redis_pool: &'static RedisPool,
    pub redis_client: deadpool_redis::redis::Client,
}
impl RedisWorkerSchemaRepositoryImpl {
    pub fn new(redis_pool: &'static RedisPool, client: deadpool_redis::redis::Client) -> Self {
        Self {
            redis_pool,
            redis_client: client,
        }
    }
}

impl UseRedisPool for RedisWorkerSchemaRepositoryImpl {
    fn redis_pool(&self) -> &'static RedisPool {
        self.redis_pool
    }
}

pub trait UseRedisWorkerSchemaRepository {
    fn redis_worker_schema_repository(&self) -> &RedisWorkerSchemaRepositoryImpl;
}

#[tokio::test]
async fn redis_test() -> Result<()> {
    use command_utils::util::option::FlatMap;

    let pool = infra_utils::infra::test::setup_test_redis_pool().await;
    let cli = infra_utils::infra::test::setup_test_redis_client()?;

    let repo = RedisWorkerSchemaRepositoryImpl {
        redis_pool: pool,
        redis_client: cli,
    };
    let id = WorkerSchemaId { value: 1 };
    let worker_schema = &WorkerSchemaData {
        name: "hoge1".to_string(),
        operation_type: 3,
        job_arg_proto: "hoge5".to_string(),
    };
    // clear first
    repo.delete(&id).await?;

    // create and find
    repo.create(&id, worker_schema).await?;
    assert!(repo.create(&id, worker_schema).await.err().is_some()); // already exists
    let res = repo.find(&id).await?;
    assert_eq!(res.flat_map(|r| r.data).as_ref(), Some(worker_schema));

    let mut worker_schema2 = worker_schema.clone();
    worker_schema2.name = "fuga1".to_string();
    worker_schema2.operation_type = 4;
    worker_schema2.job_arg_proto = "fuga5".to_string();
    // update and find
    assert!(!repo.upsert(&id, &worker_schema2).await?);
    let res2 = repo.find(&id).await?;
    assert_eq!(res2.flat_map(|r| r.data).as_ref(), Some(&worker_schema2));

    // delete and not found
    assert!(repo.delete(&id).await?);
    assert_eq!(repo.find(&id).await?, None);

    Ok(())
}
