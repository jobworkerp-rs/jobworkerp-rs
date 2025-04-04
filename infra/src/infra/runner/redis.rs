use super::rows::RunnerWithSchema;
use crate::infra::{IdGeneratorWrapper, UseIdGenerator};
use anyhow::Result;
use async_trait::async_trait;
use infra_utils::infra::redis::{RedisPool, UseRedisPool};
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::runner::factory::{RunnerSpecFactory, UseRunnerSpecFactory};
use proto::jobworkerp::data::RunnerId;
use redis::AsyncCommands;
use std::collections::BTreeMap;
use std::sync::Arc;

// TODO use if you need (not using in default)
#[async_trait]
pub trait RedisRunnerRepository:
    UseRedisPool + UseRunnerSpecFactory + UseIdGenerator + Sync + 'static
where
    Self: Send + 'static,
{
    const CACHE_KEY: &'static str = "RUNNER_DEF";

    // async fn add_from_plugins(&self) -> Result<()> {
    //     let metas = self.plugin_runner_factory().load_plugins_from(TEST_PLUGIN_DIR).await;
    //     for meta in metas.iter() {
    //         if let Some(p) = self
    //             .plugin_runner_factory()
    //             .create_plugin_by_name(&meta.name, false)
    //             .await
    //         {
    //             let runner = RunnerRow {
    //                 id: self.id_generator().generate_id()?,
    //                 name: meta.name.clone(),
    //                 description: meta.description.clone(),
    //                 file_name: meta.filename.clone(),
    //                 r#type: RunnerType::from_str_name(&meta.name)
    //                     .map(|t| t as i32)
    //                     .unwrap_or(0), // default: PLUGIN
    //             }
    //             .to_runner_with_schema(p);
    //             if let Some(id) = runner.id {
    //                 match self.create(&id, &runner).await {
    //                     Ok(_) => {}
    //                     Err(e) => {
    //                         tracing::warn!("error in add_from_plugins: {:?}", e);
    //                     }
    //                 }
    //             } else {
    //                 tracing::error!("runner id not found: {}", &meta.name);
    //             }
    //         } else {
    //             tracing::error!("loaded plugin not found: {}", &meta.name);
    //         }
    //     }
    //     Ok(())
    // }

    async fn create(&self, id: &RunnerId, runner: &RunnerWithSchema) -> Result<()> {
        let res: Result<bool> = self
            .redis_pool()
            .get()
            .await?
            .hset_nx(
                Self::CACHE_KEY,
                id.value,
                ProstMessageCodec::serialize_message(runner)?,
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

    async fn upsert(&self, id: &RunnerId, runner_data: &RunnerWithSchema) -> Result<bool> {
        let m = ProstMessageCodec::serialize_message(runner_data)?;

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
        if let Some(RunnerWithSchema {
            id: _,
            data: Some(data),
            ..
        }) = rem
        {
            if let Err(e) = self
                .plugin_runner_factory()
                .unload_plugins(&data.name)
                .await
            {
                tracing::warn!("Failed to remove runner: {:?}", e);
            }
        }
        Ok(res)
    }

    async fn find(&self, id: &RunnerId) -> Result<Option<RunnerWithSchema>> {
        match self
            .redis_pool()
            .get()
            .await?
            .hget::<_, _, Option<Vec<u8>>>(Self::CACHE_KEY, id.value)
            .await
        {
            Ok(Some(v)) => ProstMessageCodec::deserialize_message::<RunnerWithSchema>(v.as_slice())
                .map(Some)
                .map_err(|e| {
                    JobWorkerError::InvalidParameter(format!("cannot deserialize runner: {:?}", e))
                        .into()
                }),
            Ok(None) => Ok(None),
            Err(e) => Err(JobWorkerError::RedisError(e).into()),
        }
    }

    async fn find_all(&self) -> Result<Vec<RunnerWithSchema>> {
        let res: Result<BTreeMap<i64, Vec<u8>>> = self
            .redis_pool()
            .get()
            .await?
            .hgetall(Self::CACHE_KEY)
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into());
        match res {
            Ok(tree) => Ok(tree
                .iter()
                .flat_map(|(_id, v)| {
                    ProstMessageCodec::deserialize_message::<RunnerWithSchema>(v)
                        .inspect_err(|e| tracing::warn!("cannot deserialize runner: {:?}", e))
                })
                .collect()),
            Err(e) => {
                tracing::warn!("error in find_all: {:?}", e);
                Err(e)
            }
        }
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

impl<T: UseRedisPool + UseIdGenerator + UseRunnerSpecFactory + Send + Sync + 'static>
    RedisRunnerRepository for T
{
}

#[derive(Clone, Debug)]
pub struct RedisRunnerRepositoryImpl {
    pub redis_pool: &'static RedisPool,
    pub redis_client: deadpool_redis::redis::Client,
    id_generator: Arc<IdGeneratorWrapper>,
    runner_spec_factory: Arc<RunnerSpecFactory>,
}
impl RedisRunnerRepositoryImpl {
    pub fn new(
        redis_pool: &'static RedisPool,
        client: deadpool_redis::redis::Client,
        id_generator: Arc<IdGeneratorWrapper>,
        runner_factory: Arc<RunnerSpecFactory>,
    ) -> Self {
        Self {
            redis_pool,
            redis_client: client,
            id_generator,
            runner_spec_factory: runner_factory,
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
impl UseRunnerSpecFactory for RedisRunnerRepositoryImpl {
    fn plugin_runner_factory(&self) -> &RunnerSpecFactory {
        &self.runner_spec_factory
    }
}

pub trait UseRedisRunnerRepository {
    fn redis_runner_repository(&self) -> &RedisRunnerRepositoryImpl;
}

#[tokio::test]
async fn redis_test() -> Result<()> {
    use crate::infra::module::test::TEST_PLUGIN_DIR;
    use jobworkerp_runner::runner::plugins::Plugins;
    use proto::jobworkerp::data::{RunnerData, RunnerId, StreamingOutputType};

    let pool = infra_utils::infra::test::setup_test_redis_pool().await;
    let cli = infra_utils::infra::test::setup_test_redis_client()?;
    let runner_spec_factory = Arc::new(RunnerSpecFactory::new(Arc::new(Plugins::new())));
    runner_spec_factory.load_plugins_from(TEST_PLUGIN_DIR).await;

    let repo = RedisRunnerRepositoryImpl {
        redis_pool: pool,
        redis_client: cli,
        id_generator: Arc::new(IdGeneratorWrapper::new()),
        runner_spec_factory,
    };
    let id = RunnerId { value: 1 };
    let runner_data = &RunnerData {
        name: "hoge1".to_string(),
        description: "hoge2".to_string(),
        runner_type: 1,
        runner_settings_proto: "hoge3".to_string(),
        job_args_proto: "hoge5".to_string(),
        result_output_proto: Some("hoge7".to_string()),
        output_type: StreamingOutputType::NonStreaming as i32,
    };
    // clear first
    repo.delete(&id).await?;
    let runner_with_schema = RunnerWithSchema {
        id: Some(id),
        data: Some(runner_data.clone()),
        settings_schema: "hoge4".to_string(),
        arguments_schema: "hoge6".to_string(),
        output_schema: Some("hoge8".to_string()),
    };

    // create and find
    repo.create(&id, &runner_with_schema).await?;
    assert!(repo.create(&id, &runner_with_schema).await.err().is_some()); // already exists
    let res = repo.find(&id).await?;
    assert_eq!(res.and_then(|r| r.data).as_ref(), Some(runner_data));

    let mut runner_data2 = runner_data.clone();
    runner_data2.name = "fuga1".to_string();
    runner_data2.job_args_proto = "fuga5".to_string();
    let runner_with_schema2 = RunnerWithSchema {
        id: Some(id),
        data: Some(runner_data2.clone()),
        settings_schema: "fuga4".to_string(),
        arguments_schema: "fuga6".to_string(),
        output_schema: Some("fuga8".to_string()),
    };
    // update and find
    assert!(!repo.upsert(&id, &runner_with_schema2).await?);
    let res2 = repo.find(&id).await?;
    assert_eq!(res2.and_then(|r| r.data).as_ref(), Some(&runner_data2));

    // delete and not found
    assert!(repo.delete(&id).await?);
    assert_eq!(repo.find(&id).await?, None);

    Ok(())
}
