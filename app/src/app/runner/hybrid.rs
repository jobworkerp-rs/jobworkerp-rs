use super::{RunnerApp, RunnerCacheHelper, RunnerDataWithDescriptor, UseRunnerParserWithCache};
use crate::app::StorageConfig;
use anyhow::Result;
use async_trait::async_trait;
use debug_stub_derive::DebugStub;
use infra::infra::module::HybridRepositoryModule;
use infra::infra::runner::rdb::RunnerRepository;
use infra::infra::runner::rdb::{RdbRunnerRepositoryImpl, UseRdbRunnerRepository};
use infra::infra::runner::rows::RunnerWithSchema;
use infra_utils::infra::lock::RwLockWithKey;
use infra_utils::infra::memory::{self, MemoryCacheConfig, MemoryCacheImpl, UseMemoryCache};
use infra_utils::infra::rdb::UseRdbPool;
use proto::jobworkerp::data::RunnerId;
use std::{sync::Arc, time::Duration};
use stretto::AsyncCache;

// TODO use redis as cache ? (same implementation as rdb now)
#[derive(Clone, DebugStub)]
pub struct HybridRunnerAppImpl {
    plugin_dir: String,
    storage_config: Arc<StorageConfig>,
    #[debug_stub = "AsyncCache<Arc<String>, Vec<Runner>>"]
    async_cache: AsyncCache<Arc<String>, Vec<RunnerWithSchema>>,
    descriptor_cache: Arc<MemoryCacheImpl<Arc<String>, RunnerDataWithDescriptor>>,
    repositories: Arc<HybridRepositoryModule>,
    key_lock: Arc<RwLockWithKey<Arc<String>>>,
}

impl HybridRunnerAppImpl {
    pub fn new(
        plugin_dir: String,
        storage_config: Arc<StorageConfig>,
        memory_cache_config: &MemoryCacheConfig,
        repositories: Arc<HybridRepositoryModule>,
        descriptor_cache: Arc<MemoryCacheImpl<Arc<String>, RunnerDataWithDescriptor>>,
    ) -> Self {
        Self {
            plugin_dir,
            storage_config,
            async_cache: memory::new_memory_cache(memory_cache_config),
            descriptor_cache,
            repositories,
            key_lock: Arc::new(RwLockWithKey::new(memory_cache_config.max_cost as usize)),
        }
    }
}

impl UseRunnerParserWithCache for HybridRunnerAppImpl {
    fn descriptor_cache(&self) -> &MemoryCacheImpl<Arc<String>, RunnerDataWithDescriptor> {
        &self.descriptor_cache
    }
}

#[async_trait]
impl RunnerApp for HybridRunnerAppImpl {
    async fn load_runner(&self) -> Result<bool> {
        self.runner_repository()
            .add_from_plugins_from(self.plugin_dir.as_str())
            .await?;
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key())
            .await;
        Ok(true)
    }
    async fn delete_runner(&self, id: &RunnerId) -> Result<bool> {
        let res = self.runner_repository().delete(id).await?;
        // clear memory cache
        let _ = self
            .delete_cache_locked(&Self::find_cache_key(&id.value))
            .await;
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key())
            .await;
        Ok(res)
    }

    async fn find_runner(
        &self,
        id: &RunnerId,
        ttl: Option<&Duration>,
    ) -> Result<Option<RunnerWithSchema>>
    where
        Self: Send + 'static,
    {
        let k = Self::find_cache_key(&id.value);
        self.with_cache_locked(&k, ttl, || async {
            let v = self.runner_repository().find(id).await;
            match v {
                Ok(opt) => Ok(match opt {
                    Some(v) => vec![v],
                    None => Vec::new(),
                }),
                Err(e) => Err(e),
            }
        })
        .await
        .map(|r| r.first().map(|o| (*o).clone()))
    }

    // XXX no cache
    async fn find_runner_list(
        &self,
        limit: Option<&i32>,
        offset: Option<&i64>,
        _ttl: Option<&Duration>,
    ) -> Result<Vec<RunnerWithSchema>>
    where
        Self: Send + 'static,
    {
        // TODO cache
        self.runner_repository().find_list(limit, offset).await
    }

    async fn find_runner_all_list(&self, ttl: Option<&Duration>) -> Result<Vec<RunnerWithSchema>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_all_list_cache_key());
        self.with_cache_locked(&k, ttl, || async {
            self.runner_repository().find_list(None, None).await
        })
        .await
    }

    async fn count(&self) -> Result<i64>
    where
        Self: Send + 'static,
    {
        // TODO cache
        self.runner_repository()
            .count_list_tx(self.runner_repository().db_pool())
            .await
    }

    // for test
    #[cfg(any(test, feature = "test-utils"))]
    async fn create_test_runner(
        &self,
        runner_id: &RunnerId,
        name: &str,
    ) -> Result<RunnerDataWithDescriptor> {
        use crate::app::runner::test::test_runner_with_descriptor;
        use infra::infra::runner::rows::RunnerRow;
        use proto::jobworkerp::data::RunnerType;

        let runner_data = test_runner_with_descriptor(name);
        let res = self
            .runner_repository()
            .create(&RunnerRow {
                id: runner_id.value,
                name: name.to_string(),
                description: runner_data.runner_data.description.clone(),
                file_name: format!("lib{}.so", name),
                r#type: RunnerType::Plugin as i32,
            })
            .await?;
        assert!(res);
        self.store_proto_cache(runner_id, &runner_data).await;
        // clear memory cache
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key())
            .await;
        Ok(runner_data)
    }
}

impl UseRdbRunnerRepository for HybridRunnerAppImpl {
    fn runner_repository(&self) -> &RdbRunnerRepositoryImpl {
        &self.repositories.rdb_chan_module.runner_repository
    }
}

impl UseMemoryCache<Arc<String>, Vec<RunnerWithSchema>> for HybridRunnerAppImpl {
    fn cache(&self) -> &AsyncCache<Arc<String>, Vec<RunnerWithSchema>> {
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
impl RunnerCacheHelper for HybridRunnerAppImpl {}

#[cfg(test)]
mod test {
    use crate::app::runner::hybrid::HybridRunnerAppImpl;
    use crate::app::runner::RunnerApp;
    use crate::app::{StorageConfig, StorageType};
    use crate::module::test::TEST_PLUGIN_DIR;
    use anyhow::Result;
    use infra::infra::module::rdb::test::setup_test_rdb_module;
    use infra::infra::module::redis::test::setup_test_redis_module;
    use infra::infra::module::HybridRepositoryModule;
    use infra_utils::infra::memory::MemoryCacheImpl;
    use infra_utils::infra::test::TEST_RUNTIME;
    use proto::jobworkerp::data::RunnerId;
    use std::sync::Arc;

    async fn create_test_app() -> Result<HybridRunnerAppImpl> {
        let rdb_module = setup_test_rdb_module().await;
        let redis_module = setup_test_redis_module().await;
        let repositories = Arc::new(HybridRepositoryModule {
            redis_module,
            rdb_chan_module: rdb_module,
        });
        let mc_config = infra_utils::infra::memory::MemoryCacheConfig {
            num_counters: 1000000,
            max_cost: 1000000,
            use_metrics: false,
        };
        let descriptor_cache = Arc::new(MemoryCacheImpl::new(&mc_config, None));
        let storage_config = Arc::new(StorageConfig {
            r#type: StorageType::Scalable,
            restore_at_startup: Some(false),
        });
        let runner_app = HybridRunnerAppImpl::new(
            TEST_PLUGIN_DIR.to_string(),
            storage_config.clone(),
            &mc_config,
            repositories.clone(),
            descriptor_cache.clone(),
        );
        runner_app.load_runner().await?;
        // let _ = runner_app
        //     .create_test_runner(&RunnerId { value: 1 }, "Test")
        //     .await?;
        Ok(runner_app)
    }

    // create test (create and find)
    #[test]
    fn test_hybrid_runner() {
        TEST_RUNTIME.block_on(async {
            let app = create_test_app().await.unwrap();
            let res = app.find_runner(&RunnerId { value: 1 }, None).await.unwrap();
            assert!(res.is_some());
            let res = app.find_runner_list(None, None, None).await.unwrap();
            assert!(!res.is_empty());
        });
    }

    // create test (create and find)
    #[test]
    fn test_hybrid_runner_count() {
        TEST_RUNTIME.block_on(async {
            let app = create_test_app().await.unwrap();
            let res = app.count().await;
            assert!(res.is_ok());
        });
    }
}
