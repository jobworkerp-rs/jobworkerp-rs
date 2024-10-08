use anyhow::Result;
use async_trait::async_trait;
use command_utils::util::option::FlatMap;
use infra::error::JobWorkerError;
use infra::infra::module::rdb::{RdbChanRepositoryModule, UseRdbChanRepositoryModule};
use infra::infra::worker::rdb::{RdbWorkerRepository, UseRdbWorkerRepository};
use infra::infra::{IdGeneratorWrapper, UseIdGenerator};
use infra_utils::infra::memory::{MemoryCacheImpl, UseMemoryCache};
use infra_utils::infra::rdb::UseRdbPool;
use proto::jobworkerp::data::{Worker, WorkerData, WorkerId};
use std::sync::Arc;

use super::super::{StorageConfig, UseStorageConfig};
use super::builtin::{BuiltinWorker, BuiltinWorkerTrait};
use super::{WorkerApp, WorkerAppCacheHelper};

#[derive(Clone, Debug)]
pub struct RdbWorkerAppImpl {
    storage_config: Arc<StorageConfig>,
    id_generator: Arc<IdGeneratorWrapper>,
    memory_cache: MemoryCacheImpl<Arc<String>, Vec<Worker>>,
    repositories: Arc<RdbChanRepositoryModule>,
}

impl RdbWorkerAppImpl {
    pub fn new(
        storage_config: Arc<StorageConfig>,
        id_generator: Arc<IdGeneratorWrapper>,
        memory_cache: MemoryCacheImpl<Arc<String>, Vec<Worker>>,
        repositories: Arc<RdbChanRepositoryModule>,
    ) -> Self {
        Self {
            storage_config,
            id_generator,
            memory_cache,
            repositories,
        }
    }
}

#[async_trait]
impl WorkerApp for RdbWorkerAppImpl {
    async fn create(&self, worker: &WorkerData) -> Result<WorkerId> {
        let db = self.rdb_worker_repository().db_pool();
        let mut tx = db.begin().await.map_err(JobWorkerError::DBError)?;
        let wid = self
            .rdb_worker_repository()
            .create(&mut *tx, worker)
            .await?;
        tx.commit().await.map_err(JobWorkerError::DBError)?;
        // clear list cache
        let kl = Arc::new(Self::find_all_list_cache_key());
        let _ = self.memory_cache.delete_cache(&kl).await; // ignore error
        Ok(wid)
    }

    async fn update(&self, id: &WorkerId, worker: &Option<WorkerData>) -> Result<bool> {
        if let Some(w) = worker {
            let pool = self.rdb_worker_repository().db_pool();
            let mut tx = pool.begin().await.map_err(JobWorkerError::DBError)?;
            self.rdb_worker_repository().update(&mut *tx, id, w).await?;
            tx.commit().await.map_err(JobWorkerError::DBError)?;

            // clear memory cache (XXX without limit offset cache)
            self.clear_cache(id).await;
            Ok(true)
        } else {
            // empty data, only clear cache
            self.clear_cache(id).await;
            Ok(false)
        }
    }

    async fn delete(&self, id: &WorkerId) -> Result<bool> {
        let res = self.rdb_worker_repository().delete(id).await?;
        self.clear_cache(id).await;
        Ok(res)
    }

    async fn delete_all(&self) -> Result<bool> {
        let res = self.rdb_worker_repository().delete_all().await?;
        self.clear_all_cache().await;
        Ok(res)
    }
    async fn clear_cache_by(&self, id: Option<&WorkerId>, name: Option<&String>) -> Result<()> {
        if let Some(i) = id {
            self.clear_cache(i).await;
        }
        if let Some(n) = name {
            self.clear_cache_by_name(n).await;
        }
        Ok(())
    }

    async fn find_data_by_name(&self, name: &str) -> Result<Option<WorkerData>>
    where
        Self: Send + 'static,
    {
        self.find_by_name(name)
            .await
            .map(|w| w.flat_map(|wd| wd.data))
    }

    async fn find_by_name(&self, name: &str) -> Result<Option<Worker>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_name_cache_key(name));
        self.memory_cache
            .with_cache(&k, None, || async {
                self.rdb_worker_repository()
                    .find_by_name(name)
                    .await
                    .map(|r| r.map(|o| vec![o]).unwrap_or_default()) // XXX cache type: vector
            })
            .await
            .map(|r| r.first().map(|o| (*o).clone()))
    }

    async fn find(&self, id: &WorkerId) -> Result<Option<Worker>>
    where
        Self: Send + 'static,
    {
        // find from builtin workers first
        if let Some(w) = BuiltinWorker::find_worker_by_id(id) {
            return Ok(Some(w));
        }
        let k = Arc::new(Self::find_cache_key(id));
        self.memory_cache
            .with_cache(&k, None, || async {
                self.rdb_worker_repository()
                    .find(id)
                    .await
                    .map(|r| r.map(|o| vec![o]).unwrap_or_default()) // XXX cache type: vector
            })
            .await
            .map(|r| r.first().map(|o| (*o).clone()))
    }

    async fn find_list(&self, limit: Option<i32>, offset: Option<i64>) -> Result<Vec<Worker>>
    where
        Self: Send + 'static,
    {
        // not cache with offset limit
        // let k = Arc::new(Self::find_list_cache_key(limit, offset));
        // self.memory_cache
        //     .with_cache(&k, None, || async {
        // not use rdb in normal case
        self.rdb_worker_repository().find_list(limit, offset).await
        // })
        // .await
    }

    async fn find_all_worker_list(&self) -> Result<Vec<Worker>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_all_list_cache_key());
        self.memory_cache
            .with_cache(&k, None, || async {
                self.rdb_worker_repository().find_list(None, None).await
            })
            .await
            .map(|v| {
                let mut v = v;
                v.extend(BuiltinWorker::workers_list());
                v
            })
    }
    async fn count(&self) -> Result<i64>
    where
        Self: Send + 'static,
    {
        self.rdb_worker_repository().count().await
    }
}
impl UseRdbChanRepositoryModule for RdbWorkerAppImpl {
    fn rdb_repository_module(&self) -> &RdbChanRepositoryModule {
        &self.repositories
    }
}

impl UseStorageConfig for RdbWorkerAppImpl {
    fn storage_config(&self) -> &StorageConfig {
        &self.storage_config
    }
}
impl UseIdGenerator for RdbWorkerAppImpl {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        &self.id_generator
    }
}

impl WorkerAppCacheHelper for RdbWorkerAppImpl {
    fn memory_cache(&self) -> &MemoryCacheImpl<Arc<String>, Vec<Worker>> {
        &self.memory_cache
    }
}
