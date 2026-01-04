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
            if let Err(e) = self.runner_spec_factory().unload_plugins(&data.name).await {
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
                    JobWorkerError::InvalidParameter(format!("cannot deserialize runner: {e:?}"))
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
    pub redis_client: redis::Client,
    id_generator: Arc<IdGeneratorWrapper>,
    runner_spec_factory: Arc<RunnerSpecFactory>,
}
impl RedisRunnerRepositoryImpl {
    pub fn new(
        redis_pool: &'static RedisPool,
        client: redis::Client,
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
    fn runner_spec_factory(&self) -> &RunnerSpecFactory {
        &self.runner_spec_factory
    }
}

pub trait UseRedisRunnerRepository {
    fn redis_runner_repository(&self) -> &RedisRunnerRepositoryImpl;
}

#[tokio::test]
async fn redis_test() -> Result<()> {
    use crate::infra::module::test::TEST_PLUGIN_DIR;
    use jobworkerp_runner::runner::mcp::proxy::McpServerFactory;
    use jobworkerp_runner::runner::plugins::Plugins;
    use proto::jobworkerp::data::{RunnerData, RunnerId, StreamingOutputType};

    let pool = infra_utils::infra::test::setup_test_redis_pool().await;
    let cli = infra_utils::infra::test::setup_test_redis_client()?;
    let runner_spec_factory = Arc::new(RunnerSpecFactory::new(
        Arc::new(Plugins::new()),
        Arc::new(McpServerFactory::default()),
    ));
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
        definition: "test".to_string(),
        method_proto_map: Some(proto::jobworkerp::data::MethodProtoMap {
            schemas: {
                let mut map = std::collections::HashMap::new();
                map.insert(
                    "run".to_string(),
                    proto::jobworkerp::data::MethodSchema {
                        args_proto: "hoge5".to_string(),
                        result_proto: "hoge7".to_string(),
                        description: Some("test method".to_string()),
                        output_type: StreamingOutputType::NonStreaming as i32,
                    },
                );
                map
            },
        }),
    };
    // clear first
    repo.delete(&id).await?;
    let runner_with_schema = RunnerWithSchema {
        id: Some(id),
        data: Some(runner_data.clone()),
        settings_schema: "hoge14".to_string(),
        method_json_schema_map: Some(proto::jobworkerp::data::MethodJsonSchemaMap {
            schemas: std::collections::HashMap::new(),
        }),
    };

    // create and find
    repo.create(&id, &runner_with_schema).await?;
    assert!(repo.create(&id, &runner_with_schema).await.err().is_some()); // already exists
    let res = repo.find(&id).await?;
    assert_eq!(res.and_then(|r| r.data).as_ref(), Some(runner_data));

    let mut runner_data2 = runner_data.clone();
    runner_data2.name = "fuga1".to_string();
    runner_data2.method_proto_map = Some(proto::jobworkerp::data::MethodProtoMap {
        schemas: {
            let mut map = std::collections::HashMap::new();
            map.insert(
                "run".to_string(),
                proto::jobworkerp::data::MethodSchema {
                    args_proto: "fuga5".to_string(),
                    result_proto: "hoge7".to_string(),
                    description: Some("test method".to_string()),
                    output_type: StreamingOutputType::NonStreaming as i32,
                },
            );
            map
        },
    });
    let runner_with_schema2 = RunnerWithSchema {
        id: Some(id),
        data: Some(runner_data2.clone()),
        settings_schema: "fuga14".to_string(),
        method_json_schema_map: Some(proto::jobworkerp::data::MethodJsonSchemaMap {
            schemas: std::collections::HashMap::new(),
        }),
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
