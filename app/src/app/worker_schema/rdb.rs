use super::{
    UseWorkerSchemaParserWithCache, WorkerSchemaApp, WorkerSchemaCacheHelper,
    WorkerSchemaWithDescriptor,
};
use crate::app::StorageConfig;
use anyhow::Result;
use async_trait::async_trait;
use debug_stub_derive::DebugStub;
use infra::infra::module::rdb::RdbChanRepositoryModule;
use infra::infra::worker_schema::rdb::WorkerSchemaRepository;
use infra::infra::worker_schema::rdb::{
    RdbWorkerSchemaRepositoryImpl, UseRdbWorkerSchemaRepository,
};
use infra_utils::infra::lock::RwLockWithKey;
use infra_utils::infra::memory::{self, MemoryCacheConfig, MemoryCacheImpl, UseMemoryCache};
use infra_utils::infra::rdb::UseRdbPool;
use proto::jobworkerp::data::{WorkerSchema, WorkerSchemaId};
use std::{sync::Arc, time::Duration};
use stretto::AsyncCache;

#[derive(Clone, DebugStub)]
pub struct RdbWorkerSchemaAppImpl {
    storage_config: Arc<StorageConfig>,
    #[debug_stub = "AsyncCache<Arc<String>, Vec<WorkerSchema>>"]
    async_cache: AsyncCache<Arc<String>, Vec<WorkerSchema>>,
    descriptor_cache: Arc<MemoryCacheImpl<Arc<String>, WorkerSchemaWithDescriptor>>,
    repositories: Arc<RdbChanRepositoryModule>,
    cache_ttl: Option<Duration>,
    #[debug_stub = "Arc<RwLockWithKey<Arc<String>>>"]
    key_lock: Arc<RwLockWithKey<Arc<String>>>,
}

impl RdbWorkerSchemaAppImpl {
    pub fn new(
        storage_config: Arc<StorageConfig>,
        memory_cache_config: &MemoryCacheConfig,
        repositories: Arc<RdbChanRepositoryModule>,
        descriptor_cache: Arc<MemoryCacheImpl<Arc<String>, WorkerSchemaWithDescriptor>>,
    ) -> Self {
        Self {
            storage_config,
            async_cache: memory::new_memory_cache(memory_cache_config),
            descriptor_cache,
            repositories,
            cache_ttl: Some(Duration::from_secs(60)), // TODO from setting
            key_lock: Arc::new(RwLockWithKey::new(memory_cache_config.num_counters)),
        }
    }
    async fn add_worker_schema(&self) -> Result<()> {
        self.worker_schema_repository().add_from_plugins().await?;
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key())
            .await;
        Ok(())
    }
    // TODO update by reloading plugin files
    // async fn update_worker_schema(
    //     &self,
    //     id: &WorkerSchemaId,
    //     worker_schema: &Option<WorkerSchemaData>,
    // ) -> Result<bool> {
    //     if let Some(w) = worker_schema {
    //         let pool = self.worker_schema_repository().db_pool();
    //         let mut tx = pool.begin().await.map_err(JobWorkerError::DBError)?;
    //         self.worker_schema_repository()
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

impl UseWorkerSchemaParserWithCache for RdbWorkerSchemaAppImpl {
    fn descriptor_cache(&self) -> &MemoryCacheImpl<Arc<String>, WorkerSchemaWithDescriptor> {
        &self.descriptor_cache
    }
}

#[async_trait]
impl WorkerSchemaApp for RdbWorkerSchemaAppImpl {
    async fn load_worker_schema(&self) -> Result<bool> {
        self.add_worker_schema().await?;
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

impl UseRdbWorkerSchemaRepository for RdbWorkerSchemaAppImpl {
    fn worker_schema_repository(&self) -> &RdbWorkerSchemaRepositoryImpl {
        &self.repositories.worker_schema_repository
    }
}

impl UseMemoryCache<Arc<String>, Vec<WorkerSchema>> for RdbWorkerSchemaAppImpl {
    fn cache(&self) -> &AsyncCache<Arc<String>, Vec<WorkerSchema>> {
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
impl WorkerSchemaCacheHelper for RdbWorkerSchemaAppImpl {}
