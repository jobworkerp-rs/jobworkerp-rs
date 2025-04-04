use super::{RunnerApp, RunnerCacheHelper, RunnerDataWithDescriptor, UseRunnerParserWithCache};
use crate::app::StorageConfig;
use anyhow::Result;
use async_trait::async_trait;
use debug_stub_derive::DebugStub;
use infra::infra::module::rdb::RdbChanRepositoryModule;
use infra::infra::runner::rdb::RunnerRepository;
use infra::infra::runner::rdb::{RdbRunnerRepositoryImpl, UseRdbRunnerRepository};
use infra::infra::runner::rows::RunnerWithSchema;
use infra_utils::infra::lock::RwLockWithKey;
use infra_utils::infra::memory::{self, MemoryCacheConfig, MemoryCacheImpl, UseMemoryCache};
use infra_utils::infra::rdb::UseRdbPool;
use proto::jobworkerp::data::RunnerId;
use std::{sync::Arc, time::Duration};
use stretto::AsyncCache;

#[derive(Clone, DebugStub)]
pub struct RdbRunnerAppImpl {
    plugin_dir: String,
    storage_config: Arc<StorageConfig>,
    #[debug_stub = "AsyncCache<Arc<String>, Vec<RunnerWithSchema>>"]
    async_cache: AsyncCache<Arc<String>, Vec<RunnerWithSchema>>,
    descriptor_cache: Arc<MemoryCacheImpl<Arc<String>, RunnerDataWithDescriptor>>,
    repositories: Arc<RdbChanRepositoryModule>,
    cache_ttl: Option<Duration>,
    #[debug_stub = "Arc<RwLockWithKey<Arc<String>>>"]
    key_lock: Arc<RwLockWithKey<Arc<String>>>,
}

impl RdbRunnerAppImpl {
    pub fn new(
        plugin_dir: String,
        storage_config: Arc<StorageConfig>,
        memory_cache_config: &MemoryCacheConfig,
        repositories: Arc<RdbChanRepositoryModule>,
        descriptor_cache: Arc<MemoryCacheImpl<Arc<String>, RunnerDataWithDescriptor>>,
    ) -> Self {
        Self {
            plugin_dir,
            storage_config,
            async_cache: memory::new_memory_cache(memory_cache_config),
            descriptor_cache,
            repositories,
            cache_ttl: Some(Duration::from_secs(60)), // TODO from setting
            key_lock: Arc::new(RwLockWithKey::new(memory_cache_config.num_counters)),
        }
    }
    async fn add_runner(&self) -> Result<()> {
        self.runner_repository()
            .add_from_plugins_from(self.plugin_dir.as_str())
            .await?;
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key())
            .await;
        Ok(())
    }
    // TODO update by reloading plugin files
    // async fn update_runner(
    //     &self,
    //     id: &RunnerId,
    //     runner: &Option<RunnerData>,
    // ) -> Result<bool> {
    //     if let Some(w) = runner {
    //         let pool = self.runner_repository().db_pool();
    //         let mut tx = pool.begin().await.map_err(JobWorkerError::DBError)?;
    //         self.runner_repository()
    //             .update(&mut *tx, id, w)
    //             .await?;
    //         tx.commit().await.map_err(JobWorkerError::DBError)?;
    //         // clear memory cache
    //         let _ = self
    //             .delete_cache_locked(&Self::find_cache_key(&id.value))
    //             .await;
    //         let _ = self
    //             .delete_cache_locked(&Self::find_all_list_cache_key())
    //             .await;
    //         Ok(true)
    //     } else {
    //         // all empty, no update
    //         Ok(false)
    //     }
    // }
}

impl UseRunnerParserWithCache for RdbRunnerAppImpl {
    fn descriptor_cache(&self) -> &MemoryCacheImpl<Arc<String>, RunnerDataWithDescriptor> {
        &self.descriptor_cache
    }
}

#[async_trait]
impl RunnerApp for RdbRunnerAppImpl {
    async fn load_runner(&self) -> Result<bool> {
        self.add_runner().await?;
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
        let _res = self
            .runner_repository()
            .create(&RunnerRow {
                id: runner_id.value,
                name: name.to_string(),
                description: runner_data.runner_data.description.clone(),
                file_name: format!("lib{}.so", name),
                r#type: RunnerType::Plugin as i32,
            })
            .await?;
        self.store_proto_cache(runner_id, &runner_data).await;
        // clear memory cache
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key())
            .await;
        Ok(runner_data)
    }
}

impl UseRdbRunnerRepository for RdbRunnerAppImpl {
    fn runner_repository(&self) -> &RdbRunnerRepositoryImpl {
        &self.repositories.runner_repository
    }
}

impl UseMemoryCache<Arc<String>, Vec<RunnerWithSchema>> for RdbRunnerAppImpl {
    fn cache(&self) -> &AsyncCache<Arc<String>, Vec<RunnerWithSchema>> {
        &self.async_cache
    }

    #[doc = " default cache ttl"]
    fn default_ttl(&self) -> Option<&Duration> {
        // TODO from setting
        self.cache_ttl.as_ref()
    }

    fn key_lock(&self) -> &RwLockWithKey<Arc<String>> {
        &self.key_lock
    }
}
impl RunnerCacheHelper for RdbRunnerAppImpl {}
