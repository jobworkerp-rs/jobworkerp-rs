use super::{
    UseWorkerSchemaParserWithCache, WorkerSchemaApp, WorkerSchemaCacheHelper,
    WorkerSchemaWithDescriptor,
};
use crate::app::StorageConfig;
use anyhow::Result;
use async_trait::async_trait;
use debug_stub_derive::DebugStub;
use infra::infra::module::HybridRepositoryModule;
use infra::infra::worker_schema::rdb::WorkerSchemaRepository;
use infra::infra::worker_schema::rdb::{RdbWorkerSchemaRepositoryImpl, UseWorkerSchemaRepository};
use infra_utils::infra::lock::RwLockWithKey;
use infra_utils::infra::memory::{self, MemoryCacheConfig, MemoryCacheImpl, UseMemoryCache};
use infra_utils::infra::rdb::UseRdbPool;
use proto::jobworkerp::data::{WorkerSchema, WorkerSchemaId};
use std::{sync::Arc, time::Duration};
use stretto::AsyncCache;

// TODO use redis as cache ? (same implementation as rdb now)
#[derive(Clone, DebugStub)]
pub struct HybridWorkerSchemaAppImpl {
    storage_config: Arc<StorageConfig>,
    #[debug_stub = "AsyncCache<Arc<String>, Vec<WorkerSchema>>"]
    async_cache: AsyncCache<Arc<String>, Vec<WorkerSchema>>,
    descriptor_cache: Arc<MemoryCacheImpl<Arc<String>, WorkerSchemaWithDescriptor>>,
    repositories: Arc<HybridRepositoryModule>,
    key_lock: Arc<RwLockWithKey<Arc<String>>>,
}

impl HybridWorkerSchemaAppImpl {
    pub fn new(
        storage_config: Arc<StorageConfig>,
        memory_cache_config: &MemoryCacheConfig,
        repositories: Arc<HybridRepositoryModule>,
        descriptor_cache: Arc<MemoryCacheImpl<Arc<String>, WorkerSchemaWithDescriptor>>,
    ) -> Self {
        Self {
            storage_config,
            async_cache: memory::new_memory_cache(memory_cache_config),
            descriptor_cache,
            repositories,
            key_lock: Arc::new(RwLockWithKey::new(memory_cache_config.max_cost as usize)),
        }
    }
}

impl UseWorkerSchemaParserWithCache for HybridWorkerSchemaAppImpl {
    fn descriptor_cache(&self) -> &MemoryCacheImpl<Arc<String>, WorkerSchemaWithDescriptor> {
        &self.descriptor_cache
    }
}

#[async_trait]
impl WorkerSchemaApp for HybridWorkerSchemaAppImpl {
    async fn load_worker_schema(&self) -> Result<bool> {
        self.worker_schema_repository().add_from_plugins().await?;
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key())
            .await;
        Ok(true)
    }
    async fn delete_worker_schema(&self, id: &WorkerSchemaId) -> Result<bool> {
        let res = self.worker_schema_repository().delete(id).await?;
        // clear memory cache
        let _ = self
            .delete_cache_locked(&Self::find_cache_key(&id.value))
            .await;
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key())
            .await;
        Ok(res)
    }

    async fn find_worker_schema(
        &self,
        id: &WorkerSchemaId,
        ttl: Option<&Duration>,
    ) -> Result<Option<WorkerSchema>>
    where
        Self: Send + 'static,
    {
        let k = Self::find_cache_key(&id.value);
        self.with_cache_locked(&k, ttl, || async {
            let v = self.worker_schema_repository().find(id).await;
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
    async fn find_worker_schema_list(
        &self,
        limit: Option<&i32>,
        offset: Option<&i64>,
        _ttl: Option<&Duration>,
    ) -> Result<Vec<WorkerSchema>>
    where
        Self: Send + 'static,
    {
        // TODO cache
        self.worker_schema_repository()
            .find_list(limit, offset)
            .await
    }

    async fn find_worker_schema_all_list(&self, ttl: Option<&Duration>) -> Result<Vec<WorkerSchema>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_all_list_cache_key());
        self.with_cache_locked(&k, ttl, || async {
            self.worker_schema_repository().find_list(None, None).await
        })
        .await
    }

    async fn count(&self) -> Result<i64>
    where
        Self: Send + 'static,
    {
        // TODO cache
        self.worker_schema_repository()
            .count_list_tx(self.worker_schema_repository().db_pool())
            .await
    }

    // for test
    #[cfg(test)]
    async fn create_test_schema(
        &self,
        schema_id: &WorkerSchemaId,
        name: &str,
    ) -> Result<WorkerSchemaWithDescriptor> {
        use super::test::test_worker_schema_with_descriptor;
        use infra::infra::worker_schema::rows::WorkerSchemaRow;
        use proto::jobworkerp::data::RunnerType;

        let schema = test_worker_schema_with_descriptor(name);
        let _res = self
            .worker_schema_repository()
            .create(&WorkerSchemaRow {
                id: schema_id.value,
                name: name.to_string(),
                file_name: format!("lib{}.so", name),
                r#type: RunnerType::Plugin as i32,
            })
            .await?;
        self.store_proto_cache(schema_id, &schema).await;
        // clear memory cache
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key())
            .await;
        Ok(schema)
    }
}

impl UseWorkerSchemaRepository for HybridWorkerSchemaAppImpl {
    fn worker_schema_repository(&self) -> &RdbWorkerSchemaRepositoryImpl {
        &self.repositories.rdb_chan_module.worker_schema_repository
    }
}

impl UseMemoryCache<Arc<String>, Vec<WorkerSchema>> for HybridWorkerSchemaAppImpl {
    fn cache(&self) -> &AsyncCache<Arc<String>, Vec<WorkerSchema>> {
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
impl WorkerSchemaCacheHelper for HybridWorkerSchemaAppImpl {}

#[cfg(test)]
mod test {
    use crate::app::worker_schema::hybrid::HybridWorkerSchemaAppImpl;
    use crate::app::worker_schema::WorkerSchemaApp;
    use crate::app::{StorageConfig, StorageType};
    use anyhow::Result;
    use infra::infra::module::rdb::test::setup_test_rdb_module;
    use infra::infra::module::redis::test::setup_test_redis_module;
    use infra::infra::module::HybridRepositoryModule;
    use infra_utils::infra::memory::MemoryCacheImpl;
    use infra_utils::infra::test::TEST_RUNTIME;
    use proto::jobworkerp::data::WorkerSchemaId;
    use std::sync::Arc;

    fn create_test_app() -> Result<HybridWorkerSchemaAppImpl> {
        let rdb_module = setup_test_rdb_module();
        TEST_RUNTIME.block_on(async {
            let redis_module = setup_test_redis_module().await;
            let repositories = Arc::new(HybridRepositoryModule {
                redis_module,
                rdb_chan_module: rdb_module,
            });
            let mc_config = infra_utils::infra::memory::MemoryCacheConfig {
                num_counters: 10000,
                max_cost: 10000,
                use_metrics: false,
            };
            let descriptor_cache = Arc::new(MemoryCacheImpl::new(&mc_config, None));
            let storage_config = Arc::new(StorageConfig {
                r#type: StorageType::Hybrid,
                restore_at_startup: Some(false),
            });
            let worker_schema_app = HybridWorkerSchemaAppImpl::new(
                storage_config.clone(),
                &mc_config,
                repositories.clone(),
                descriptor_cache.clone(),
            );
            worker_schema_app.load_worker_schema().await?;
            // let _ = worker_schema_app
            //     .create_test_schema(&WorkerSchemaId { value: 1 }, "Test")
            //     .await?;
            Ok(worker_schema_app)
        })
    }

    // create test (create and find)
    #[test]
    fn test_hybrid_worker_schema() {
        let app = create_test_app().unwrap();
        TEST_RUNTIME.block_on(async {
            let res = app
                .find_worker_schema(&WorkerSchemaId { value: 1 }, None)
                .await
                .unwrap();
            assert!(res.is_some());
            let res = app.find_worker_schema_list(None, None, None).await.unwrap();
            assert!(!res.is_empty());
        });
    }

    // create test (create and find)
    #[test]
    fn test_hybrid_worker_schema_count() {
        let app = create_test_app().unwrap();
        TEST_RUNTIME.block_on(async {
            let res = app.count().await;
            assert!(res.is_ok());
        });
    }
}
