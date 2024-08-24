use anyhow::Result;
use async_trait::async_trait;
use command_utils::util::option::FlatMap;
use infra::error::JobWorkerError;
use infra::infra::job::rows::UseJobqueueAndCodec;
use infra::infra::module::rdb::{RdbChanRepositoryModule, UseRdbChanRepositoryModule};
use infra::infra::module::redis::{RedisRepositoryModule, UseRedisRepositoryModule};
use infra::infra::module::HybridRepositoryModule;
use infra::infra::worker::event::UseWorkerPublish;
use infra::infra::worker::rdb::{RdbWorkerRepository, UseRdbWorkerRepository};
use infra::infra::worker::redis::{RedisWorkerRepository, UseRedisWorkerRepository};
use infra::infra::{IdGeneratorWrapper, UseIdGenerator};
use infra_utils::infra::memory::{MemoryCacheImpl, UseMemoryCache};
use infra_utils::infra::rdb::UseRdbPool;
use proto::jobworkerp::data::{Worker, WorkerData, WorkerId};
use std::sync::Arc;

use crate::app::worker::builtin::{BuiltinWorker, BuiltinWorkerTrait};

use super::super::{StorageConfig, UseStorageConfig};
use super::{WorkerApp, WorkerAppCacheHelper};
pub struct HybridWorkerAppImpl {
    storage_config: Arc<StorageConfig>,
    id_generator: Arc<IdGeneratorWrapper>,
    memory_cache: MemoryCacheImpl<Arc<String>, Vec<Worker>>,
    repositories: Arc<HybridRepositoryModule>,
}

impl HybridWorkerAppImpl {
    pub fn new(
        storage_config: Arc<StorageConfig>,
        id_generator: Arc<IdGeneratorWrapper>,
        memory_cache: MemoryCacheImpl<Arc<String>, Vec<Worker>>,
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
            let id = self
                .rdb_worker_repository()
                .create(&mut *tx, worker)
                .await?;
            tx.commit().await.map_err(JobWorkerError::DBError)?;
            id
        };
        let _ = self.redis_worker_repository().delete_all().await;
        // clear list cache
        let kl = Arc::new(Self::find_all_list_cache_key());
        let _ = self.memory_cache.delete_cache(&kl).await; // ignore error
        let _ = self
            .redis_worker_repository()
            .publish_worker_changed(&wid, worker)
            .await;
        Ok(wid)
    }

    async fn update(&self, id: &WorkerId, worker: &Option<WorkerData>) -> Result<bool> {
        if let Some(w) = worker {
            // use rdb
            let pool = self.rdb_worker_repository().db_pool();
            let mut tx = pool.begin().await.map_err(JobWorkerError::DBError)?;
            self.rdb_worker_repository().update(&mut *tx, id, w).await?;
            tx.commit().await.map_err(JobWorkerError::DBError)?;

            let _ = self.redis_worker_repository().delete_all().await;
            // clear memory cache (XXX without limit offset cache)
            self.clear_cache(id).await;
            self.clear_cache_by_name(&w.name).await;

            let _ = self
                .redis_worker_repository()
                .publish_worker_changed(id, w)
                .await;

            Ok(true)
        } else {
            // empty data, only clear cache
            self.clear_cache(id).await;
            Ok(false)
        }
    }

    // TODO clear cache by name
    async fn delete(&self, id: &WorkerId) -> Result<bool> {
        let _ = self.redis_worker_repository().delete(id).await?;
        let res = self.rdb_worker_repository().delete(id).await?;
        self.clear_cache(id).await;
        let _ = self
            .redis_worker_repository()
            .publish_worker_deleted(id)
            .await;
        Ok(res)
    }

    async fn delete_all(&self) -> Result<bool> {
        let res = self.rdb_worker_repository().delete_all().await?;
        let _ = self.redis_worker_repository().delete_all().await?;
        self.clear_all_cache().await;
        let _ = self
            .redis_worker_repository()
            .publish_worker_all_deleted()
            .await;
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
        self.memory_cache
            .with_cache(&k, None, || async {
                tracing::trace!("not find from memory");
                match self.redis_worker_repository().find(id).await {
                    Ok(Some(v)) => Ok(Some(v)),
                    Ok(None) => {
                        // fallback to rdb if rdb is enabled
                        self.rdb_worker_repository().find(id).await
                    }
                    Err(err) => {
                        tracing::warn!("not find from redis by error: {}", err);
                        // fallback to rdb if rdb is enabled
                        self.rdb_worker_repository().find(id).await
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
        self.memory_cache
            .with_cache(&k, None, || async {
                tracing::debug!("not find from memory");
                match self.redis_worker_repository().find_by_name(name).await {
                    Ok(Some(v)) => Ok(Some(v)),
                    Ok(None) => {
                        // fallback to rdb if rdb is enabled
                        self.rdb_worker_repository().find_by_name(name).await
                    }
                    Err(err) => {
                        tracing::warn!("not find from redis by error: {}", err);
                        // fallback to rdb if rdb is enabled
                        self.rdb_worker_repository().find_by_name(name).await
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
        self.memory_cache
            .with_cache(&k, None, || async {
                // not use rdb in normal case
                match self.redis_worker_repository().find_all().await {
                    Ok(v) if !v.is_empty() => {
                        tracing::debug!("worker list from redis: ({:?}, {:?})", limit, offset);
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
                        tracing::debug!("worker list from rdb: ({:?}, {:?})", limit, offset);
                        // fallback to rdb if rdb is enabled
                        let list = self
                            .rdb_worker_repository()
                            .find_list(limit, offset)
                            .await?;
                        if !list.is_empty() {
                            for w in list.iter() {
                                let _ = self.redis_worker_repository().upsert(w).await;
                            }
                        }
                        Ok(list)
                    }
                    Err(err) => {
                        tracing::debug!(
                            "worker list from rdb (redis error): ({:?}, {:?})",
                            limit,
                            offset
                        );
                        tracing::warn!("workers find error from redis: {:?}", err);
                        // fallback to rdb if rdb is enabled
                        let list = self
                            .rdb_worker_repository()
                            .find_list(limit, offset)
                            .await?;
                        if !list.is_empty() {
                            for w in list.iter() {
                                let _ = self.redis_worker_repository().upsert(w).await;
                            }
                        }
                        Ok(list)
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
        self.memory_cache
            .with_cache(&k, None, || async {
                match self.redis_worker_repository().find_all().await {
                    Ok(v) if !v.is_empty() => Ok(v),
                    // empty
                    Ok(_v) => {
                        tracing::debug!("workers not find (empty) from redis: get from db");
                        // fallback to rdb if rdb is enabled
                        let list = self.rdb_worker_repository().find_list(None, None).await;
                        if let Ok(l) = list.as_ref() {
                            for w in l {
                                let _ = self.redis_worker_repository().upsert(w).await;
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
            return Ok(cnt);
        }
        // fallback to rdb if rdb is enabled
        self.rdb_worker_repository().count().await
    }
}
impl UseRdbChanRepositoryModule for HybridWorkerAppImpl {
    fn rdb_repository_module(&self) -> &RdbChanRepositoryModule {
        &self.repositories.rdb_chan_module
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
impl UseJobqueueAndCodec for HybridWorkerAppImpl {}
impl WorkerAppCacheHelper for HybridWorkerAppImpl {
    fn memory_cache(&self) -> &MemoryCacheImpl<Arc<String>, Vec<Worker>> {
        &self.memory_cache
    }
}

// create test
#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::app::worker::hybrid::HybridWorkerAppImpl;
    use crate::app::worker::WorkerApp;
    use crate::app::{StorageConfig, StorageType};
    use anyhow::Result;
    use command_utils::util::option::FlatMap;
    use infra::infra::module::rdb::test::setup_test_rdb_module;
    use infra::infra::module::redis::test::setup_test_redis_module;
    use infra::infra::module::HybridRepositoryModule;
    use infra::infra::IdGeneratorWrapper;
    use infra_utils::infra::test::TEST_RUNTIME;
    use proto::jobworkerp::data::worker_operation::Operation;
    use proto::jobworkerp::data::{WorkerData, WorkerOperation};

    fn create_test_app(use_mock_id: bool) -> Result<HybridWorkerAppImpl> {
        let rdb_module = setup_test_rdb_module();
        TEST_RUNTIME.block_on(async {
            let redis_module = setup_test_redis_module().await;
            let repositories = Arc::new(HybridRepositoryModule {
                redis_module,
                rdb_chan_module: rdb_module,
            });
            // mock id generator (generate 1 until called set method)
            let id_generator = if use_mock_id {
                Arc::new(IdGeneratorWrapper::new_mock())
            } else {
                Arc::new(IdGeneratorWrapper::new())
            };
            let mc_config = infra_utils::infra::memory::MemoryCacheConfig {
                num_counters: 10000,
                max_cost: 10000,
                use_metrics: false,
            };
            let worker_memory_cache = infra_utils::infra::memory::MemoryCacheImpl::new(
                &mc_config,
                Some(Duration::from_secs(5 * 60)),
            );
            let storage_config = Arc::new(StorageConfig {
                r#type: StorageType::Hybrid,
                restore_at_startup: Some(false),
            });
            let worker_app = HybridWorkerAppImpl::new(
                storage_config.clone(),
                id_generator.clone(),
                worker_memory_cache,
                repositories.clone(),
            );
            Ok(worker_app)
        })
    }

    #[test]
    fn test_integrated() -> Result<()> {
        // test create 3 workers and find list and update 1 worker and find list and delete 1 worker and find list
        let app = create_test_app(false)?;
        TEST_RUNTIME.block_on(async {
            let operation = WorkerOperation {
                operation: Some(Operation::Command(
                    proto::jobworkerp::data::CommandOperation {
                        name: "ls".to_string(),
                    },
                )),
            };
            let w1 = WorkerData {
                name: "test1".to_string(),
                operation: Some(operation.clone()),
                ..Default::default()
            };
            let w2 = WorkerData {
                name: "test2".to_string(),
                operation: Some(operation.clone()),
                ..Default::default()
            };
            let w3 = WorkerData {
                name: "test3".to_string(),
                operation: Some(operation.clone()),
                ..Default::default()
            };
            let id1 = app.create(&w1).await?;
            let id2 = app.create(&w2).await?;
            let id3 = app.create(&w3).await?;
            assert_eq!(id1.value, 1);
            assert_eq!(id2.value, 2);
            assert_eq!(id3.value, 3);
            // find list
            let list = app.find_list(None, None).await?;
            // println!("==== created: {:#?}", list);
            assert_eq!(list.len(), 3);
            assert_eq!(app.count().await?, 3);
            // update
            let w4 = WorkerData {
                name: "test4".to_string(),
                operation: Some(operation.clone()),
                ..Default::default()
            };
            let res = app.update(&id1, &Some(w4.clone())).await?;
            assert!(res);

            let f = app.find(&id1).await?;
            assert!(f.is_some());
            let fd = f.flat_map(|w| w.data);
            assert!(fd.is_some());
            assert_eq!(fd.unwrap().name, w4.name);
            // find list
            let list = app.find_list(None, None).await?;
            // println!("==== updated: {:#?}", list);
            assert_eq!(list.len(), 3);
            assert_eq!(app.count().await?, 3);
            // delete
            let res = app.delete(&id1).await?;
            assert!(res);
            // find list
            let list = app.find_list(None, None).await?;
            assert_eq!(app.count().await?, 2);
            assert_eq!(list.len(), 2);
            Ok(())
        })
    }
}
