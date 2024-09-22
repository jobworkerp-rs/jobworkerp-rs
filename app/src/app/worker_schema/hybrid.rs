use anyhow::Result;
use async_trait::async_trait;
use debug_stub_derive::DebugStub;
use infra::infra::module::HybridRepositoryModule;
use infra::infra::worker_schema::rdb::{RdbWorkerSchemaRepositoryImpl, UseWorkerSchemaRepository};
use infra::infra::{IdGeneratorWrapper, UseIdGenerator};
use infra::{error::JobWorkerError, infra::worker_schema::rdb::WorkerSchemaRepository};
use infra_utils::infra::lock::RwLockWithKey;
use infra_utils::infra::memory::{self, MemoryCacheConfig, MemoryCacheImpl, UseMemoryCache};
use infra_utils::infra::rdb::UseRdbPool;
use proto::jobworkerp::data::{WorkerSchema, WorkerSchemaData, WorkerSchemaId};
use std::{sync::Arc, time::Duration};
use stretto::AsyncCache;

use crate::app::StorageConfig;

use super::{
    UseWorkerSchemaParserWithCache, WorkerSchemaApp, WorkerSchemaCacheHelper,
    WorkerSchemaWithDescriptor,
};

// TODO use redis as cache ? (same implementation as rdb now)
#[derive(Clone, DebugStub)]
pub struct HybridWorkerSchemaAppImpl {
    storage_config: Arc<StorageConfig>,
    id_generator: Arc<IdGeneratorWrapper>,
    #[debug_stub = "AsyncCache<Arc<String>, Vec<WorkerSchema>>"]
    async_cache: AsyncCache<Arc<String>, Vec<WorkerSchema>>,
    memory_cache: MemoryCacheImpl<Arc<String>, WorkerSchemaWithDescriptor>,
    repositories: Arc<HybridRepositoryModule>,
}

impl HybridWorkerSchemaAppImpl {
    pub fn new(
        storage_config: Arc<StorageConfig>,
        id_generator: Arc<IdGeneratorWrapper>,
        memory_cache_config: &MemoryCacheConfig,
        repositories: Arc<HybridRepositoryModule>,
    ) -> Self {
        Self {
            storage_config,
            id_generator,
            async_cache: memory::new_memory_cache(memory_cache_config),
            memory_cache: MemoryCacheImpl::new(memory_cache_config, None),
            repositories,
        }
    }
}

impl UseIdGenerator for HybridWorkerSchemaAppImpl {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        &self.id_generator
    }
}
impl UseWorkerSchemaParserWithCache for HybridWorkerSchemaAppImpl {
    fn cache(&self) -> &MemoryCacheImpl<Arc<String>, WorkerSchemaWithDescriptor> {
        &self.memory_cache
    }
}

#[async_trait]
impl WorkerSchemaApp for HybridWorkerSchemaAppImpl {
    async fn create_worker_schema(
        &self,
        worker_schema: WorkerSchemaData,
    ) -> Result<WorkerSchemaId> {
        let id = WorkerSchemaId {
            value: self.id_generator().generate_id()?,
        };
        let db = self.worker_schema_repository().db_pool();
        let mut tx = db.begin().await.map_err(JobWorkerError::DBError)?;
        let id = self
            .worker_schema_repository()
            .create(&mut *tx, id, &worker_schema)
            .await?;
        tx.commit().await.map_err(JobWorkerError::DBError)?;
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key())
            .await;

        Ok(id)
    }

    // offset, limit 付きのリストキャッシュは更新されないので反映に時間かかる (ttl短かめにする)
    async fn update_worker_schema(
        &self,
        id: &WorkerSchemaId,
        worker_schema: &Option<WorkerSchemaData>,
    ) -> Result<bool> {
        if let Some(w) = worker_schema {
            let pool = self.worker_schema_repository().db_pool();
            let mut tx = pool.begin().await.map_err(JobWorkerError::DBError)?;
            self.worker_schema_repository()
                .update(&mut *tx, id, w)
                .await?;
            tx.commit().await.map_err(JobWorkerError::DBError)?;
            // clear memory cache
            let _ = self
                .delete_cache_locked(&Self::find_cache_key(&id.value))
                .await;
            let _ = self
                .delete_cache_locked(&Self::find_all_list_cache_key())
                .await;
            Ok(true)
        } else {
            // all empty, no update
            Ok(false)
        }
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
        todo!()
    }

    fn key_lock(&self) -> &RwLockWithKey<Arc<String>> {
        todo!()
    }
}
impl WorkerSchemaCacheHelper for HybridWorkerSchemaAppImpl {}
