use anyhow::Result;
use async_trait::async_trait;
use deadpool_redis::redis::AsyncCommands;
use infra_utils::infra::redis::{RedisPool, UseRedisPool};
use jobworkerp_base::error::JobWorkerError;
use prost::Message;
use proto::jobworkerp::function::data::{FunctionSet, FunctionSetData, FunctionSetId};
use std::collections::BTreeMap;
use std::io::Cursor;

// TODO use if you need (not using in default)
#[async_trait]
pub trait RedisFunctionSetRepository: UseRedisPool + Sync + 'static
where
    Self: Send + 'static,
{
    const CACHE_KEY: &'static str = "FUNCTION_SET_DEF";

    async fn create(&self, id: &FunctionSetId, function_set: &FunctionSetData) -> Result<()> {
        let res: Result<bool> = self
            .redis_pool()
            .get()
            .await?
            .hset_nx(
                Self::CACHE_KEY,
                id.value,
                Self::serialize_function_set(function_set),
            )
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into());
        match res {
            Ok(r) => {
                if r {
                    Ok(())
                } else {
                    Err(JobWorkerError::AlreadyExists(format!(
                        "function_set creation error: already exists id={}",
                        id.value
                    ))
                    .into())
                }
            }
            Err(e) => Err(e),
        }
    }

    async fn upsert(&self, id: &FunctionSetId, function_set: &FunctionSetData) -> Result<bool> {
        let m = Self::serialize_function_set(function_set);

        let res: Result<bool> = self
            .redis_pool()
            .get()
            .await?
            .hset(Self::CACHE_KEY, id.value, m)
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into());
        res
    }

    async fn delete(&self, id: &FunctionSetId) -> Result<bool> {
        self.redis_pool()
            .get()
            .await?
            .hdel(Self::CACHE_KEY, id.value)
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into())
    }

    async fn find(&self, id: &FunctionSetId) -> Result<Option<FunctionSet>> {
        match self
            .redis_pool()
            .get()
            .await?
            .hget(Self::CACHE_KEY, id.value)
            .await
        {
            Ok(Some(v)) => Self::deserialize_to_function_set(&v).map(|d| {
                Some(FunctionSet {
                    id: Some(*id),
                    data: Some(d),
                })
            }),
            Ok(None) => Ok(None),
            Err(e) => Err(JobWorkerError::RedisError(e).into()),
        }
    }

    async fn find_all(&self) -> Result<Vec<FunctionSet>> {
        let res: Result<BTreeMap<i64, Vec<u8>>> = self
            .redis_pool()
            .get()
            .await?
            .hgetall(Self::CACHE_KEY)
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into());
        res.map(|tree| {
            tree.iter()
                .filter_map(|(id, v)| {
                    Self::deserialize_to_function_set(v)
                        .ok()
                        .map(|d| FunctionSet {
                            id: Some(FunctionSetId { value: *id }),
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

    fn serialize_function_set(w: &FunctionSetData) -> Vec<u8> {
        let mut buf = Vec::with_capacity(w.encoded_len());
        w.encode(&mut buf).unwrap();
        buf
    }

    fn deserialize_to_function_set(buf: &Vec<u8>) -> Result<FunctionSetData> {
        FunctionSetData::decode(&mut Cursor::new(buf))
            .map_err(|e| JobWorkerError::CodecError(e).into())
    }
    fn deserialize_bytes_to_function_set(buf: &[u8]) -> Result<FunctionSetData> {
        FunctionSetData::decode(&mut Cursor::new(buf))
            .map_err(|e| JobWorkerError::CodecError(e).into())
    }
}

impl<T: UseRedisPool + Send + Sync + 'static> RedisFunctionSetRepository for T {}

pub struct RedisFunctionSetRepositoryImpl {
    pub redis_pool: &'static RedisPool,
}

impl UseRedisPool for RedisFunctionSetRepositoryImpl {
    fn redis_pool(&self) -> &'static RedisPool {
        self.redis_pool
    }
}

pub trait UseRedisFunctionSetRepository {
    fn redis_function_set_repository(&self) -> &RedisFunctionSetRepositoryImpl;
}

#[tokio::test]
async fn redis_test() -> Result<()> {
    use proto::jobworkerp::data::{RunnerId, WorkerId};
    use proto::jobworkerp::function::data::{
        function_id, FunctionId, FunctionSetData, FunctionSetId, RunnerUsing,
    };

    let pool = infra_utils::infra::test::setup_test_redis_pool().await;

    let repo = RedisFunctionSetRepositoryImpl { redis_pool: pool };
    let id = FunctionSetId { value: 1 };
    let function_set = &FunctionSetData {
        name: "hoge1".to_string(),
        description: "hoge2".to_string(),
        category: 4,
        targets: vec![
            FunctionId {
                id: Some(function_id::Id::RunnerUsing(RunnerUsing {
                    runner_id: Some(RunnerId { value: 10 }),
                    using: None,
                })),
            },
            FunctionId {
                id: Some(function_id::Id::WorkerId(WorkerId { value: 20 })),
            },
        ],
    };
    // clear first
    repo.delete(&id).await?;

    // create and find
    repo.create(&id, function_set).await?;
    assert!(repo.create(&id, function_set).await.err().is_some()); // already exists
    let res = repo.find(&id).await?;
    assert_eq!(res.and_then(|r| r.data).as_ref(), Some(function_set));

    let mut function_set2 = function_set.clone();
    function_set2.name = "fuga1".to_string();
    function_set2.description = "fuga2".to_string();
    function_set2.category = 5;
    function_set2.targets[0] = FunctionId {
        id: Some(function_id::Id::RunnerUsing(RunnerUsing {
            runner_id: Some(RunnerId { value: 30 }),
            using: None,
        })),
    };
    function_set2.targets[1] = FunctionId {
        id: Some(function_id::Id::WorkerId(WorkerId { value: 40 })),
    };
    // update and find
    assert!(!repo.upsert(&id, &function_set2).await?);
    let res2 = repo.find(&id).await?;
    assert_eq!(res2.and_then(|r| r.data).as_ref(), Some(&function_set2));

    // delete and not found
    assert!(repo.delete(&id).await?);
    assert_eq!(repo.find(&id).await?, None);

    Ok(())
}
