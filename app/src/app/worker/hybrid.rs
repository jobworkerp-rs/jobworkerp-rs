use anyhow::Result;
use async_trait::async_trait;
use common::infra::memory::UseMemoryCache;
use common::infra::rdb::UseRdbPool;
use common::util::option::FlatMap;
use infra::error::JobWorkerError;
use infra::infra::job::rows::UseJobqueueAndCodec;
use infra::infra::module::rdb::{RdbRepositoryModule, UseRdbRepositoryModule};
use infra::infra::module::redis::{RedisRepositoryModule, UseRedisRepositoryModule};
use infra::infra::module::HybridRepositoryModule;
use infra::infra::worker::rdb::{RdbWorkerRepository, UseRdbWorkerRepository};
use infra::infra::worker::redis::{RedisWorkerRepository, UseRedisWorkerRepository};
use infra::infra::{IdGeneratorWrapper, UseIdGenerator};
use proto::jobworkerp::data::{Worker, WorkerData, WorkerId};
use std::{sync::Arc, time::Duration};
use stretto::AsyncCache;

use crate::app::worker::builtin::{BuiltinWorker, BuiltinWorkerTrait};

use super::super::{StorageConfig, UseStorageConfig};
use super::{WorkerApp, WorkerAppCacheHelper};
pub struct HybridWorkerAppImpl {
    storage_config: Arc<StorageConfig>,
    id_generator: Arc<IdGeneratorWrapper>,
    memory_cache: AsyncCache<Arc<String>, Vec<Worker>>,
    repositories: Arc<HybridRepositoryModule>,
}

impl HybridWorkerAppImpl {
    pub fn new(
        storage_config: Arc<StorageConfig>,
        id_generator: Arc<IdGeneratorWrapper>,
        memory_cache: AsyncCache<Arc<String>, Vec<Worker>>,
        repositories: Arc<HybridRepositoryModule>,
    ) -> Self {
        Self {
            storage_config,
            id_generator,
            memory_cache,
            repositories,
        }
    }
}

// TODO now, hybrid repository (or redis?) version only
#[async_trait]
impl WorkerApp for HybridWorkerAppImpl {
    async fn create(&self, worker: &WorkerData) -> Result<WorkerId> {
        let wid = {
            let db = self.rdb_worker_repository().db_pool();
            let mut tx = db.begin().await.map_err(JobWorkerError::DBError)?;
            let id = self.rdb_worker_repository().create(&mut tx, worker).await?;
            tx.commit().await.map_err(JobWorkerError::DBError)?;
            id
        };
        let w = Worker {
            id: Some(wid.clone()),
            data: Some(worker.clone()),
        };
        self.redis_worker_repository().upsert(&w, false).await?;
        // clear list cache
        let kl = Arc::new(Self::find_all_list_cache_key());
        self.delete_cache(&kl).await;
        Ok(wid)
    }

    async fn update(&self, id: &WorkerId, worker: &Option<WorkerData>) -> Result<bool> {
        if let Some(w) = worker {
            // use rdb
            let pool = self.rdb_worker_repository().db_pool();
            let mut tx = pool.begin().await.map_err(JobWorkerError::DBError)?;
            self.rdb_worker_repository().update(&mut tx, id, w).await?;
            tx.commit().await.map_err(JobWorkerError::DBError)?;

            let wk = Worker {
                id: Some(id.clone()),
                data: worker.clone(),
            };

            self.redis_worker_repository().upsert(&wk, false).await?;
            // clear memory cache (XXX without limit offset cache)
            self.clear_cache(id).await;
            self.clear_cache_by_name(&w.name).await;
            Ok(true)
        } else {
            // empty data, only clear cache
            self.clear_cache(id).await;
            Ok(false)
        }
    }

    // TODO clear cache by name
    async fn delete(&self, id: &WorkerId) -> Result<bool> {
        let _ = self.redis_worker_repository().delete(id, false).await?;
        let res = self.rdb_worker_repository().delete(id).await?;
        self.clear_cache(id).await;
        Ok(res)
    }

    async fn delete_all(&self) -> Result<bool> {
        let res = self.rdb_worker_repository().delete_all().await?;
        let _ = self.redis_worker_repository().delete_all().await?;
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
        if id.is_none() && name.is_none() {
            self.clear_all_cache().await;
        }
        Ok(())
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
        tracing::debug!("find worker: {}, cache key: {}", id.value, k);
        self.with_cache(&k, Self::CACHE_TTL, || async {
            tracing::trace!("not find from memory");
            match self.redis_worker_repository().find(id).await {
                Ok(Some(v)) => Ok(Some(v)),
                Ok(None) => {
                    // fallback to rdb if rdb is enabled
                    let dbr = self.rdb_worker_repository().find(id).await;
                    if let Ok(Some(w)) = &dbr {
                        if w.data.is_some() {
                            self.redis_worker_repository().upsert(w, true).await?;
                        } else {
                            tracing::debug!("not find from rdb");
                        }
                    }
                    dbr
                }
                Err(err) => {
                    tracing::warn!("not find from redis by error: {}", err);
                    // fallback to rdb if rdb is enabled
                    let dbr = self.rdb_worker_repository().find(id).await;
                    if let Ok(Some(w)) = &dbr {
                        if w.data.is_some() {
                            self.redis_worker_repository().upsert(w, true).await?;
                        } else {
                            tracing::debug!("not find from rdb");
                        }
                    }
                    dbr
                }
            }
            .map(|r| r.map(|o| vec![o]).unwrap_or_default()) // XXX cache type: vector
        })
        .await
        .map(|r| r.first().map(|o| (*o).clone()))
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
        tracing::debug!("find worker: {}, cache key: {}", name, k);
        self.with_cache(&k, Self::CACHE_TTL, || async {
            tracing::debug!("not find from memory");
            match self.redis_worker_repository().find_by_name(name).await {
                Ok(Some(v)) => Ok(Some(v)),
                Ok(None) => {
                    // fallback to rdb if rdb is enabled
                    let dbr = self.rdb_worker_repository().find_by_name(name).await;
                    if let Ok(Some(w)) = &dbr {
                        self.redis_worker_repository()
                            .upsert(w, true) // store as cache
                            .await?;
                    }
                    dbr
                }
                Err(err) => {
                    tracing::warn!("not find from redis by error: {}", err);
                    // fallback to rdb if rdb is enabled
                    let dbr = self.rdb_worker_repository().find_by_name(name).await;
                    if let Ok(Some(w)) = &dbr {
                        self.redis_worker_repository()
                            .upsert(w, true) // store as cache
                            .await?;
                    }
                    dbr
                }
            }
            .map(|r| r.map(|o| vec![o]).unwrap_or_default()) // XXX cache type: vector
        })
        .await
        .map(|r| r.first().map(|o| (*o).clone()))
    }

    async fn find_list(&self, limit: Option<i32>, offset: Option<i64>) -> Result<Vec<Worker>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_list_cache_key(limit, offset));
        self.with_cache(&k, Self::CACHE_TTL, || async {
            // not use rdb in normal case
            match self.redis_worker_repository().find_all().await {
                Ok(v) if !v.is_empty() => {
                    // soft paging
                    let start = offset.unwrap_or(0);
                    if let Some(l) = limit {
                        Ok(v.into_iter()
                            .skip(start as usize)
                            .take(l as usize)
                            .collect())
                    } else {
                        Ok(v.into_iter().skip(start as usize).collect())
                    }
                }
                // empty
                Ok(_v) => {
                    // fallback to rdb if rdb is enabled
                    self.rdb_worker_repository().find_list(limit, offset).await
                }
                Err(err) => {
                    tracing::warn!("workers find error from redis: {:?}", err);
                    // fallback to rdb if rdb is enabled
                    self.rdb_worker_repository().find_list(limit, offset).await
                }
            }
        })
        .await
    }

    async fn find_all_worker_list(&self) -> Result<Vec<Worker>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_all_list_cache_key());
        self.with_cache(&k, Self::CACHE_TTL, || async {
            match self.redis_worker_repository().find_all().await {
                Ok(v) if !v.is_empty() => Ok(v),
                // empty
                Ok(_v) => {
                    tracing::debug!("workers not find (empty) from redis: get from db");
                    // fallback to rdb if rdb is enabled
                    let list = self.rdb_worker_repository().find_list(None, None).await;
                    if let Ok(l) = list.as_ref() {
                        for w in l {
                            let _ = self.redis_worker_repository().upsert(w, true).await;
                        }
                    }
                    list
                }
                Err(err) => {
                    // fallback to rdb if rdb is enabled
                    tracing::warn!("workers find error from redis: {:?}", err);
                    self.rdb_worker_repository().find_list(None, None).await
                }
            }
        })
        .await
    }
    async fn count(&self) -> Result<i64>
    where
        Self: Send + 'static,
    {
        // TODO cache
        // find from redis first
        let cnt = self.redis_worker_repository().count().await?;
        if cnt > 0 {
            return Ok(cnt + BuiltinWorker::workers_list().len() as i64);
        }
        // fallback to rdb if rdb is enabled
        self.rdb_worker_repository().count().await
    }
}
impl UseRdbRepositoryModule for HybridWorkerAppImpl {
    fn rdb_repository_module(&self) -> &RdbRepositoryModule {
        &self.repositories.rdb_module
    }
}

impl UseStorageConfig for HybridWorkerAppImpl {
    fn storage_config(&self) -> &StorageConfig {
        &self.storage_config
    }
}
impl UseIdGenerator for HybridWorkerAppImpl {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        &self.id_generator
    }
}

impl UseRedisRepositoryModule for HybridWorkerAppImpl {
    fn redis_repository_module(&self) -> &RedisRepositoryModule {
        &self.repositories.redis_module
    }
}

impl UseMemoryCache<Arc<String>, Vec<Worker>> for HybridWorkerAppImpl {
    const CACHE_TTL: Option<Duration> = Some(Duration::from_secs(5 * 60)); // 5 min
    fn cache(&self) -> &AsyncCache<Arc<String>, Vec<Worker>> {
        &self.memory_cache
    }
}
impl UseJobqueueAndCodec for HybridWorkerAppImpl {}
impl WorkerAppCacheHelper for HybridWorkerAppImpl {}
