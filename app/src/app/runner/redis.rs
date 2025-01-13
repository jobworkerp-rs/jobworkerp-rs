use anyhow::Result;
use async_trait::async_trait;
use command_utils::util::result::TapErr;
use debug_stub_derive::DebugStub;
use infra::infra::module::redis::{RedisRepositoryModule, UseRedisRepositoryModule};
use infra::infra::runner::redis::{RedisRunnerRepository, UseRedisRunnerRepository};
use infra::infra::{IdGeneratorWrapper, UseIdGenerator};
use infra_utils::infra::lock::RwLockWithKey;
use infra_utils::infra::memory::{self, MemoryCacheConfig, MemoryCacheImpl, UseMemoryCache};
use proto::jobworkerp::data::{Runner, RunnerId};
use std::sync::Arc;
use std::time::Duration;
use stretto::AsyncCache;

use super::super::{StorageConfig, UseStorageConfig};
use super::{RunnerApp, RunnerCacheHelper, RunnerDataWithDescriptor, UseRunnerParserWithCache};

// TODO cache control
#[derive(DebugStub)]
pub struct RedisRunnerAppImpl {
    storage_config: Arc<StorageConfig>,
    id_generator: Arc<IdGeneratorWrapper>,
    #[debug_stub = "AsyncCache<Arc<String>, Vec<Runner>>"]
    async_cache: AsyncCache<Arc<String>, Vec<Runner>>,
    descriptor_cache: Arc<MemoryCacheImpl<Arc<String>, RunnerDataWithDescriptor>>,
    repositories: Arc<RedisRepositoryModule>,
    key_lock: RwLockWithKey<Arc<String>>,
}

impl RedisRunnerAppImpl {
    pub fn new(
        storage_config: Arc<StorageConfig>,
        id_generator: Arc<IdGeneratorWrapper>,
        memory_cache_config: &MemoryCacheConfig,
        repositories: Arc<RedisRepositoryModule>,
        descriptor_cache: Arc<MemoryCacheImpl<Arc<String>, RunnerDataWithDescriptor>>,
    ) -> Self {
        Self {
            storage_config,
            id_generator,
            async_cache: memory::new_memory_cache(memory_cache_config),
            descriptor_cache,
            repositories,
            key_lock: RwLockWithKey::default(),
        }
    }
}
// TODO now, hybrid repository (or redis?) version only
#[async_trait]
impl RunnerApp for RedisRunnerAppImpl {
    async fn load_runner(&self) -> Result<bool> {
        self.redis_runner_repository().add_from_plugins().await?;
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key())
            .await;
        Ok(true)
    }

    async fn delete_runner(&self, id: &RunnerId) -> Result<bool> {
        let res = self.redis_runner_repository().delete(id).await?;
        let _ = self
            .delete_cache_locked(&Self::find_cache_key(&id.value))
            .await;
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key())
            .await;
        // TODO
        // let _ = self
        //     .redis_runner_repository()
        //     .publish_runner_deleted(id)
        //     .await;
        Ok(res)
    }

    async fn find_runner(&self, id: &RunnerId, ttl: Option<&Duration>) -> Result<Option<Runner>>
    where
        Self: Send + 'static,
    {
        let k = Self::find_cache_key(&id.value);
        self.with_cache_locked(&k, ttl, || async {
            self.redis_runner_repository()
                .find(id)
                .await
                .tap_err(|err| {
                    tracing::warn!("cannot access redis and rdb in finding runner: {}", err)
                })
                .map(|r| r.map(|o| vec![o]).unwrap_or_default()) // XXX cache type: vector
        })
        .await
        .map(|r| r.first().map(|o| (*o).clone()))
    }

    async fn find_runner_list(
        &self,
        limit: Option<&i32>,
        offset: Option<&i64>,
        _ttl: Option<&Duration>,
    ) -> Result<Vec<Runner>>
    where
        Self: Send + 'static,
    {
        // let k = Arc::new(Self::find_list_cache_key(limit, offset));
        // self.memory_cache
        //     .with_cache(&k, None, || async {
        self.redis_runner_repository().find_all().await.map(|v| {
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

    async fn find_runner_all_list(&self, ttl: Option<&Duration>) -> Result<Vec<Runner>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_all_list_cache_key());
        self.with_cache(&k, ttl, || async {
            // not use rdb in normal case
            self.redis_runner_repository().find_all().await
        })
        .await
    }
    async fn count(&self) -> Result<i64>
    where
        Self: Send + 'static,
    {
        // TODO cache
        // find from redis first
        let cnt = self.redis_runner_repository().count().await?;
        Ok(cnt)
    }
    // for test
    #[cfg(test)]
    async fn create_test_runner(
        &self,
        runner_id: &RunnerId,
        name: &str,
    ) -> Result<RunnerDataWithDescriptor> {
        use super::test::test_runner_with_descriptor;

        let runner = test_runner_with_descriptor(name);
        let _res = self
            .redis_runner_repository()
            .upsert(runner_id, &runner.runner_data)
            .await?;
        self.store_proto_cache(runner_id, &runner).await;
        // clear memory cache
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key())
            .await;
        Ok(runner)
    }
}

impl UseStorageConfig for RedisRunnerAppImpl {
    fn storage_config(&self) -> &StorageConfig {
        &self.storage_config
    }
}
impl UseIdGenerator for RedisRunnerAppImpl {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        &self.id_generator
    }
}

impl UseRedisRepositoryModule for RedisRunnerAppImpl {
    fn redis_repository_module(&self) -> &RedisRepositoryModule {
        &self.repositories
    }
}
impl UseMemoryCache<Arc<String>, Vec<Runner>> for RedisRunnerAppImpl {
    fn cache(&self) -> &AsyncCache<Arc<String>, Vec<Runner>> {
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
impl RunnerCacheHelper for RedisRunnerAppImpl {}

impl UseRunnerParserWithCache for RedisRunnerAppImpl {
    fn descriptor_cache(&self) -> &MemoryCacheImpl<Arc<String>, RunnerDataWithDescriptor> {
        &self.descriptor_cache
    }
}
