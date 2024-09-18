use anyhow::Result;
use async_trait::async_trait;
use command_utils::util::result::TapErr;
use debug_stub_derive::DebugStub;
use infra::infra::module::redis::{RedisRepositoryModule, UseRedisRepositoryModule};
use infra::infra::runner_schema::redis::{
    RedisRunnerSchemaRepository, UseRedisRunnerSchemaRepository,
};
use infra::infra::{IdGeneratorWrapper, UseIdGenerator};
use infra_utils::infra::lock::RwLockWithKey;
use infra_utils::infra::memory::{self, MemoryCacheConfig, MemoryCacheImpl, UseMemoryCache};
use proto::jobworkerp::data::{RunnerSchema, RunnerSchemaData, RunnerSchemaId};
use std::sync::Arc;
use std::time::Duration;
use stretto::AsyncCache;

use super::super::{StorageConfig, UseStorageConfig};
use super::{
    RunnerSchemaApp, RunnerSchemaCacheHelper, RunnerSchemaWithDescriptor,
    UseRunnerSchemaParserWithCache,
};

// TODO cache control
#[derive(DebugStub)]
pub struct RedisRunnerSchemaAppImpl {
    storage_config: Arc<StorageConfig>,
    id_generator: Arc<IdGeneratorWrapper>,
    #[debug_stub = "AsyncCache<Arc<String>, Vec<RunnerSchema>>"]
    async_cache: AsyncCache<Arc<String>, Vec<RunnerSchema>>,
    memory_cache: MemoryCacheImpl<Arc<String>, RunnerSchemaWithDescriptor>,
    repositories: Arc<RedisRepositoryModule>,
    key_lock: RwLockWithKey<Arc<String>>,
}

impl RedisRunnerSchemaAppImpl {
    pub fn new(
        storage_config: Arc<StorageConfig>,
        id_generator: Arc<IdGeneratorWrapper>,
        memory_cache_config: &MemoryCacheConfig,
        repositories: Arc<RedisRepositoryModule>,
    ) -> Self {
        Self {
            storage_config,
            id_generator,
            async_cache: memory::new_memory_cache(memory_cache_config),
            memory_cache: MemoryCacheImpl::new(memory_cache_config, None),
            repositories,
            key_lock: RwLockWithKey::default(),
        }
    }
}
// TODO now, hybrid repository (or redis?) version only
#[async_trait]
impl RunnerSchemaApp for RedisRunnerSchemaAppImpl {
    async fn create_runner_schema(
        &self,
        runner_schema: RunnerSchemaData,
    ) -> Result<RunnerSchemaId> {
        let schema = self.validate_and_get_runner_schema(runner_schema)?;
        let id = self.id_generator().generate_id()?;
        let rid = RunnerSchemaId { value: id };
        self.redis_runner_schema_repository()
            .upsert(&rid, &schema.schema)
            .await?;
        // clear list cache
        let _ = self
            .memory_cache
            .delete_cache_locked(&Self::find_all_list_cache_key())
            .await; // ignore error

        // let _ = self
        //     .redis_runner_schema_repository()
        //     .publish_runner_schema_changed(&rid, runner_schema)
        //     .await;

        Ok(rid)
    }

    async fn update_runner_schema(
        &self,
        id: &RunnerSchemaId,
        runner_schema: &Option<RunnerSchemaData>,
    ) -> Result<bool> {
        if let Some(rs) = runner_schema {
            self.redis_runner_schema_repository().upsert(id, rs).await?;
            // clear memory cache (XXX without limit offset cache)
            // XXX ignore error
            let _ = self
                .delete_cache_locked(&Self::find_cache_key(&id.value))
                .await;
            // TODO
            // let _ = self
            //     .redis_runner_schema_repository()
            //     .publish_runner_schema_changed(id, rs)
            //     .await;
            Ok(true)
        } else {
            // empty data, delete
            let _ = self
                .delete_cache_locked(&Self::find_cache_key(&id.value))
                .await?;
            Ok(true)
        }
    }

    async fn delete_runner_schema(&self, id: &RunnerSchemaId) -> Result<bool> {
        let res = self.redis_runner_schema_repository().delete(id).await?;
        let _ = self
            .delete_cache_locked(&Self::find_cache_key(&id.value))
            .await;
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key())
            .await;
        // TODO
        // let _ = self
        //     .redis_runner_schema_repository()
        //     .publish_runner_schema_deleted(id)
        //     .await;
        Ok(res)
    }

    async fn find_runner_schema(
        &self,
        id: &RunnerSchemaId,
        ttl: Option<&Duration>,
    ) -> Result<Option<RunnerSchema>>
    where
        Self: Send + 'static,
    {
        let k = Self::find_cache_key(&id.value);
        self.with_cache_locked(&k, ttl, || async {
            self.redis_runner_schema_repository()
                .find(id)
                .await
                .tap_err(|err| {
                    tracing::warn!(
                        "cannot access redis and rdb in finding runner_schema: {}",
                        err
                    )
                })
                .map(|r| r.map(|o| vec![o]).unwrap_or_default()) // XXX cache type: vector
        })
        .await
        .map(|r| r.first().map(|o| (*o).clone()))
    }

    async fn find_runner_schema_list(
        &self,
        limit: Option<&i32>,
        offset: Option<&i64>,
        _ttl: Option<&Duration>,
    ) -> Result<Vec<RunnerSchema>>
    where
        Self: Send + 'static,
    {
        // let k = Arc::new(Self::find_list_cache_key(limit, offset));
        // self.memory_cache
        //     .with_cache(&k, None, || async {
        self.redis_runner_schema_repository()
            .find_all()
            .await
            .map(|v| {
                // soft paging
                let start = offset.unwrap_or(&0);
                if let Some(l) = limit {
                    v.into_iter()
                        .skip(*start as usize)
                        .take(*l as usize)
                        .collect()
                } else {
                    v.into_iter().skip(*start as usize).collect()
                }
            })
        // })
        // .await
    }

    async fn find_runner_schema_all_list(&self, ttl: Option<&Duration>) -> Result<Vec<RunnerSchema>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_all_list_cache_key());
        self.with_cache(&k, ttl, || async {
            // not use rdb in normal case
            self.redis_runner_schema_repository().find_all().await
        })
        .await
    }
    async fn count(&self) -> Result<i64>
    where
        Self: Send + 'static,
    {
        // TODO cache
        // find from redis first
        let cnt = self.redis_runner_schema_repository().count().await?;
        Ok(cnt)
    }
}

impl UseStorageConfig for RedisRunnerSchemaAppImpl {
    fn storage_config(&self) -> &StorageConfig {
        &self.storage_config
    }
}
impl UseIdGenerator for RedisRunnerSchemaAppImpl {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        &self.id_generator
    }
}

impl UseRedisRepositoryModule for RedisRunnerSchemaAppImpl {
    fn redis_repository_module(&self) -> &RedisRepositoryModule {
        &self.repositories
    }
}
impl UseMemoryCache<Arc<String>, Vec<RunnerSchema>> for RedisRunnerSchemaAppImpl {
    fn cache(&self) -> &AsyncCache<Arc<String>, Vec<RunnerSchema>> {
        &self.async_cache
    }

    #[doc = " default cache ttl"]
    fn default_ttl(&self) -> Option<&Duration> {
        None
    }

    fn key_lock(&self) -> &RwLockWithKey<Arc<String>> {
        &self.key_lock
    }
}
impl RunnerSchemaCacheHelper for RedisRunnerSchemaAppImpl {}

impl UseRunnerSchemaParserWithCache for RedisRunnerSchemaAppImpl {
    fn cache(&self) -> &MemoryCacheImpl<Arc<String>, RunnerSchemaWithDescriptor> {
        &self.memory_cache
    }
}
