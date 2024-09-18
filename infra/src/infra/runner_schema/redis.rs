use crate::error::JobWorkerError;
use anyhow::Result;
use async_trait::async_trait;
use infra_utils::infra::redis::{RedisPool, UseRedisPool};
use prost::Message;
use proto::jobworkerp::data::{RunnerSchema, RunnerSchemaData, RunnerSchemaId};
use redis::AsyncCommands;
use std::collections::BTreeMap;
use std::io::Cursor;

// TODO use if you need (not using in default)
#[async_trait]
pub trait RedisRunnerSchemaRepository: UseRedisPool + Sync + 'static
where
    Self: Send + 'static,
{
    const CACHE_KEY: &'static str = "RUNNER_SCHEMA_DEF";

    async fn create(&self, id: &RunnerSchemaId, runner_schema: &RunnerSchemaData) -> Result<()> {
        let res: Result<bool> = self
            .redis_pool()
            .get()
            .await?
            .hset_nx(
                Self::CACHE_KEY,
                id.value,
                Self::serialize_runner_schema(runner_schema),
            )
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into());
        match res {
            Ok(r) => {
                if r {
                    Ok(())
                } else {
                    Err(JobWorkerError::AlreadyExists(format!(
                        "runner_schema creation error: already exists id={}",
                        id.value
                    ))
                    .into())
                }
            }
            Err(e) => Err(e),
        }
    }

    async fn upsert(&self, id: &RunnerSchemaId, runner_schema: &RunnerSchemaData) -> Result<bool> {
        let m = Self::serialize_runner_schema(runner_schema);

        let res: Result<bool> = self
            .redis_pool()
            .get()
            .await?
            .hset(Self::CACHE_KEY, id.value, m)
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into());
        res
    }

    async fn delete(&self, id: &RunnerSchemaId) -> Result<bool> {
        self.redis_pool()
            .get()
            .await?
            .hdel(Self::CACHE_KEY, id.value)
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into())
    }

    async fn find(&self, id: &RunnerSchemaId) -> Result<Option<RunnerSchema>> {
        match self
            .redis_pool()
            .get()
            .await?
            .hget(Self::CACHE_KEY, id.value)
            .await
        {
            Ok(Some(v)) => Self::deserialize_to_runner_schema(&v).map(|d| {
                Some(RunnerSchema {
                    id: Some(*id),
                    data: Some(d),
                })
            }),
            Ok(None) => Ok(None),
            Err(e) => Err(JobWorkerError::RedisError(e).into()),
        }
    }

    async fn find_all(&self) -> Result<Vec<RunnerSchema>> {
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
                    Self::deserialize_to_runner_schema(v).map(|d| RunnerSchema {
                        id: Some(RunnerSchemaId { value: *id }),
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

    fn serialize_runner_schema(w: &RunnerSchemaData) -> Vec<u8> {
        let mut buf = Vec::with_capacity(w.encoded_len());
        w.encode(&mut buf).unwrap();
        buf
    }

    fn deserialize_to_runner_schema(buf: &Vec<u8>) -> Result<RunnerSchemaData> {
        RunnerSchemaData::decode(&mut Cursor::new(buf))
            .map_err(|e| JobWorkerError::CodecError(e).into())
    }
    fn deserialize_bytes_to_runner_schema(buf: &[u8]) -> Result<RunnerSchemaData> {
        RunnerSchemaData::decode(&mut Cursor::new(buf))
            .map_err(|e| JobWorkerError::CodecError(e).into())
    }
}

impl<T: UseRedisPool + Send + Sync + 'static> RedisRunnerSchemaRepository for T {}

#[derive(Clone, Debug)]
pub struct RedisRunnerSchemaRepositoryImpl {
    pub redis_pool: &'static RedisPool,
    pub redis_client: deadpool_redis::redis::Client,
}
impl RedisRunnerSchemaRepositoryImpl {
    pub fn new(redis_pool: &'static RedisPool, client: deadpool_redis::redis::Client) -> Self {
        Self {
            redis_pool,
            redis_client: client,
        }
    }
}

impl UseRedisPool for RedisRunnerSchemaRepositoryImpl {
    fn redis_pool(&self) -> &'static RedisPool {
        self.redis_pool
    }
}

pub trait UseRedisRunnerSchemaRepository {
    fn redis_runner_schema_repository(&self) -> &RedisRunnerSchemaRepositoryImpl;
}

#[tokio::test]
async fn redis_test() -> Result<()> {
    use command_utils::util::option::FlatMap;

    let pool = infra_utils::infra::test::setup_test_redis_pool().await;
    let cli = infra_utils::infra::test::setup_test_redis_client()?;

    let repo = RedisRunnerSchemaRepositoryImpl {
        redis_pool: pool,
        redis_client: cli,
    };
    let id = RunnerSchemaId { value: 1 };
    let runner_schema = &RunnerSchemaData {
        name: "hoge1".to_string(),
        operation_type: 3,
        operation_proto: "hoge4".to_string(),
        job_arg_proto: "hoge5".to_string(),
    };
    // clear first
    repo.delete(&id).await?;

    // create and find
    repo.create(&id, runner_schema).await?;
    assert!(repo.create(&id, runner_schema).await.err().is_some()); // already exists
    let res = repo.find(&id).await?;
    assert_eq!(res.flat_map(|r| r.data).as_ref(), Some(runner_schema));

    let mut runner_schema2 = runner_schema.clone();
    runner_schema2.name = "fuga1".to_string();
    runner_schema2.operation_type = 4;
    runner_schema2.operation_proto = "fuga4".to_string();
    runner_schema2.job_arg_proto = "fuga5".to_string();
    // update and find
    assert!(!repo.upsert(&id, &runner_schema2).await?);
    let res2 = repo.find(&id).await?;
    assert_eq!(res2.flat_map(|r| r.data).as_ref(), Some(&runner_schema2));

    // delete and not found
    assert!(repo.delete(&id).await?);
    assert_eq!(repo.find(&id).await?, None);

    Ok(())
}
