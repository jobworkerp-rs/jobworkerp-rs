use super::rows::RunnerRow;
use crate::error::JobWorkerError;
use crate::infra::runner::factory::{RunnerFactory, UseRunnerFactory};
use crate::infra::{IdGeneratorWrapper, UseIdGenerator};
use anyhow::Result;
use async_trait::async_trait;
use infra_utils::infra::redis::{RedisPool, UseRedisPool};
use prost::Message;
use proto::jobworkerp::data::{Runner, RunnerData, RunnerId, RunnerType};
use redis::AsyncCommands;
use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::Arc;

// TODO use if you need (not using in default)
#[async_trait]
pub trait RedisRunnerRepository:
    UseRedisPool + UseRunnerFactory + UseIdGenerator + Sync + 'static
where
    Self: Send + 'static,
{
    const CACHE_KEY: &'static str = "RUNNER_DEF";

    async fn add_from_plugins(&self) -> Result<()> {
        let names = self.runner_factory().load_plugins().await;
        for (name, fname) in names.iter() {
            if let Some(p) = self.runner_factory().create_by_name(name, false).await {
                let runner = RunnerRow {
                    id: self.id_generator().generate_id()?,
                    name: name.clone(),
                    file_name: fname.clone(),
                    r#type: RunnerType::from_str_name(name)
                        .map(|t| t as i32)
                        .unwrap_or(0), // default: PLUGIN
                }
                .to_proto(p);
                if let Runner {
                    id: Some(id),
                    data: Some(data),
                } = runner
                {
                    match self.create(&id, &data).await {
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!("error in add_from_plugins: {:?}", e);
                        }
                    }
                } else {
                    tracing::error!("runner create error: {}, {:?}", name, runner);
                }
            } else {
                tracing::error!("loaded plugin not found: {}", name);
            }
        }
        Ok(())
    }

    async fn create(&self, id: &RunnerId, runner_data: &RunnerData) -> Result<()> {
        let res: Result<bool> = self
            .redis_pool()
            .get()
            .await?
            .hset_nx(
                Self::CACHE_KEY,
                id.value,
                Self::serialize_runner(runner_data),
            )
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into());
        match res {
            Ok(r) => {
                if r {
                    Ok(())
                } else {
                    Err(JobWorkerError::AlreadyExists(format!(
                        "runner creation error: already exists id={}",
                        id.value
                    ))
                    .into())
                }
            }
            Err(e) => Err(e),
        }
    }

    async fn upsert(&self, id: &RunnerId, runner_data: &RunnerData) -> Result<bool> {
        let m = Self::serialize_runner(runner_data);

        let res: Result<bool> = self
            .redis_pool()
            .get()
            .await?
            .hset(Self::CACHE_KEY, id.value, m)
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into());
        res
    }

    async fn delete(&self, id: &RunnerId) -> Result<bool> {
        let rem = self.find(id).await?;
        let res = self
            .redis_pool()
            .get()
            .await?
            .hdel(Self::CACHE_KEY, id.value)
            .await
            .map_err(JobWorkerError::RedisError)?;
        if let Some(Runner {
            id: _,
            data: Some(data),
        }) = rem
        {
            if let Err(e) = self.runner_factory().unload_plugins(&data.name).await {
                tracing::warn!("Failed to remove runner: {:?}", e);
            }
        }
        Ok(res)
    }

    async fn find(&self, id: &RunnerId) -> Result<Option<Runner>> {
        match self
            .redis_pool()
            .get()
            .await?
            .hget(Self::CACHE_KEY, id.value)
            .await
        {
            Ok(Some(v)) => Self::deserialize_to_runner(&v).map(|d| {
                Some(Runner {
                    id: Some(*id),
                    data: Some(d),
                })
            }),
            Ok(None) => Ok(None),
            Err(e) => Err(JobWorkerError::RedisError(e).into()),
        }
    }

    async fn find_all(&self) -> Result<Vec<Runner>> {
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
                    Self::deserialize_to_runner(v).map(|d| Runner {
                        id: Some(RunnerId { value: *id }),
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

    fn serialize_runner(w: &RunnerData) -> Vec<u8> {
        let mut buf = Vec::with_capacity(w.encoded_len());
        w.encode(&mut buf).unwrap();
        buf
    }

    fn deserialize_to_runner(buf: &Vec<u8>) -> Result<RunnerData> {
        RunnerData::decode(&mut Cursor::new(buf)).map_err(|e| JobWorkerError::CodecError(e).into())
    }
    fn deserialize_bytes_to_runner(buf: &[u8]) -> Result<RunnerData> {
        RunnerData::decode(&mut Cursor::new(buf)).map_err(|e| JobWorkerError::CodecError(e).into())
    }
}

impl<T: UseRedisPool + UseIdGenerator + UseRunnerFactory + Send + Sync + 'static>
    RedisRunnerRepository for T
{
}

#[derive(Clone, Debug)]
pub struct RedisRunnerRepositoryImpl {
    pub redis_pool: &'static RedisPool,
    pub redis_client: deadpool_redis::redis::Client,
    id_generator: Arc<IdGeneratorWrapper>,
    runner_factory: Arc<RunnerFactory>,
}
impl RedisRunnerRepositoryImpl {
    pub fn new(
        redis_pool: &'static RedisPool,
        client: deadpool_redis::redis::Client,
        id_generator: Arc<IdGeneratorWrapper>,
        runner_factory: Arc<RunnerFactory>,
    ) -> Self {
        Self {
            redis_pool,
            redis_client: client,
            id_generator,
            runner_factory,
        }
    }
}

impl UseRedisPool for RedisRunnerRepositoryImpl {
    fn redis_pool(&self) -> &'static RedisPool {
        self.redis_pool
    }
}

impl UseIdGenerator for RedisRunnerRepositoryImpl {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        &self.id_generator
    }
}
impl UseRunnerFactory for RedisRunnerRepositoryImpl {
    fn runner_factory(&self) -> &RunnerFactory {
        &self.runner_factory
    }
}

pub trait UseRedisRunnerRepository {
    fn redis_runner_repository(&self) -> &RedisRunnerRepositoryImpl;
}

#[tokio::test]
async fn redis_test() -> Result<()> {
    use command_utils::util::option::FlatMap;

    let pool = infra_utils::infra::test::setup_test_redis_pool().await;
    let cli = infra_utils::infra::test::setup_test_redis_client()?;
    let runner_factory = Arc::new(RunnerFactory::new());
    runner_factory.load_plugins().await;

    let repo = RedisRunnerRepositoryImpl {
        redis_pool: pool,
        redis_client: cli,
        id_generator: Arc::new(IdGeneratorWrapper::new()),
        runner_factory,
    };
    let id = RunnerId { value: 1 };
    let runner_data = &RunnerData {
        name: "hoge1".to_string(),
        runner_type: 1,
        runner_settings_proto: "hoge3".to_string(),
        job_args_proto: "hoge5".to_string(),
        result_output_proto: Some("hoge7".to_string()),
        output_as_stream: Some(false),
    };
    // clear first
    repo.delete(&id).await?;

    // create and find
    repo.create(&id, runner_data).await?;
    assert!(repo.create(&id, runner_data).await.err().is_some()); // already exists
    let res = repo.find(&id).await?;
    assert_eq!(res.flat_map(|r| r.data).as_ref(), Some(runner_data));

    let mut runner_data2 = runner_data.clone();
    runner_data2.name = "fuga1".to_string();
    runner_data2.job_args_proto = "fuga5".to_string();
    // update and find
    assert!(!repo.upsert(&id, &runner_data2).await?);
    let res2 = repo.find(&id).await?;
    assert_eq!(res2.flat_map(|r| r.data).as_ref(), Some(&runner_data2));

    // delete and not found
    assert!(repo.delete(&id).await?);
    assert_eq!(repo.find(&id).await?, None);

    Ok(())
}
