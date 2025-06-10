use anyhow::Result;
use async_trait::async_trait;
use command_utils::text::TextUtil;
use infra::infra::job::rows::UseJobqueueAndCodec;
use infra::infra::module::rdb::{RdbChanRepositoryModule, UseRdbChanRepositoryModule};
use infra::infra::module::redis::{RedisRepositoryModule, UseRedisRepositoryModule};
use infra::infra::module::HybridRepositoryModule;
use infra::infra::worker::event::UseWorkerPublish;
use infra::infra::worker::rdb::{RdbWorkerRepository, UseRdbWorkerRepository};
use infra::infra::worker::redis::{RedisWorkerRepository, UseRedisWorkerRepository};
use infra::infra::{IdGeneratorWrapper, UseIdGenerator};
use infra_utils::infra::cache::{MokaCacheConfig, MokaCacheImpl, UseMokaCache};
use infra_utils::infra::memory::MemoryCacheImpl;
use infra_utils::infra::rdb::UseRdbPool;
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{Worker, WorkerData, WorkerId};
use std::sync::Arc;

// use crate::app::worker::builtin::{BuiltinWorker, BuiltinWorkerTrait};

use crate::app::runner::{
    RunnerApp, RunnerDataWithDescriptor, UseRunnerApp, UseRunnerAppParserWithCache,
    UseRunnerParserWithCache,
};

use super::super::{StorageConfig, UseStorageConfig};
use super::{WorkerApp, WorkerAppCacheHelper};

#[derive(Clone, Debug)]
pub struct HybridWorkerAppImpl {
    storage_config: Arc<StorageConfig>,
    id_generator: Arc<IdGeneratorWrapper>,
    moka_cache: MokaCacheImpl<Arc<String>, Worker>,
    list_memory_cache: MokaCacheImpl<Arc<String>, Vec<Worker>>,
    repositories: Arc<HybridRepositoryModule>,
    descriptor_cache: Arc<MemoryCacheImpl<Arc<String>, RunnerDataWithDescriptor>>,
    runner_app: Arc<dyn RunnerApp + 'static>,
}

impl HybridWorkerAppImpl {
    pub fn new(
        storage_config: Arc<StorageConfig>,
        id_generator: Arc<IdGeneratorWrapper>,
        moka_config: &MokaCacheConfig,
        repositories: Arc<HybridRepositoryModule>,
        descriptor_cache: Arc<MemoryCacheImpl<Arc<String>, RunnerDataWithDescriptor>>,
        runner_app: Arc<dyn RunnerApp + 'static>,
    ) -> Self {
        let list_memory_cache = infra_utils::infra::cache::MokaCacheImpl::new(moka_config);

        let memory_cache = infra_utils::infra::cache::MokaCacheImpl::new(moka_config);
        Self {
            storage_config,
            id_generator,
            moka_cache: memory_cache,
            list_memory_cache,
            repositories,
            descriptor_cache,
            runner_app,
        }
    }

    fn find_list_cache_key(
        runner_types: &[i32],
        channel: Option<&String>,
        limit: Option<i32>,
        offset: Option<i64>,
    ) -> String {
        let mut key = "worker_list:".to_string();
        if !runner_types.is_empty() {
            // sort and distinct runner types
            let runner_types = runner_types
                .iter()
                .collect::<Vec<_>>()
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>();
            key.push_str(
                &runner_types
                    .iter()
                    .map(|t| t.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
            );
            key.push(':');
        }
        if let Some(c) = channel {
            key.push_str(c);
            key.push(':');
        }
        if let Some(l) = limit {
            key.push_str(&l.to_string());
            key.push(':');
        }
        if let Some(o) = offset {
            key.push_str(&o.to_string());
        } else {
            key.push('0');
        }
        key
    }
}

// TODO now, hybrid repository (or redis?) version only
#[async_trait]
impl WorkerApp for HybridWorkerAppImpl {
    async fn create(&self, worker: &WorkerData) -> Result<WorkerId> {
        let wsid = worker
            .runner_id
            .ok_or_else(|| JobWorkerError::InvalidParameter("runner_id is required".to_string()))?;
        let _ = self
            .validate_runner_settings_data(&wsid, worker.runner_settings.as_slice())
            .await?
            .ok_or_else(|| {
                JobWorkerError::InvalidParameter("runner settings not provided".to_string())
            })?;
        let wdata = worker.clone();
        let wid = {
            let db = self.rdb_worker_repository().db_pool();
            let mut tx = db.begin().await.map_err(JobWorkerError::DBError)?;
            let id = self
                .rdb_worker_repository()
                .create(&mut *tx, &wdata)
                .await?;
            tx.commit().await.map_err(JobWorkerError::DBError)?;
            id
        };
        tracing::debug!("created worker: {:?}", wid);
        let _ = self.redis_worker_repository().delete_all().await;
        // clear caches (name and list)
        self.clear_cache_by_name(&worker.name).await;
        tracing::debug!("clear cache by name: {}", worker.name);
        let _ = self
            .redis_worker_repository()
            .publish_worker_changed(&wid, &wdata)
            .await;
        Ok(wid)
    }

    async fn create_temp(&self, worker: WorkerData, with_random_name: bool) -> Result<WorkerId> {
        let wsid = worker
            .runner_id
            .ok_or_else(|| JobWorkerError::InvalidParameter("runner_id is required".to_string()))?;
        let _ = self
            .validate_runner_settings_data(&wsid, worker.runner_settings.as_slice())
            .await?
            .ok_or_else(|| {
                JobWorkerError::InvalidParameter("runner settings not provided".to_string())
            })?;
        let mut wdata = worker;
        if with_random_name {
            // generate random name
            wdata.name = TextUtil::generate_random_key(Some(&wdata.name));
        }
        let wid = WorkerId {
            value: self.id_generator().generate_id()?,
        };
        tracing::debug!("create temp worker: {:?}", wid);
        let w = Worker {
            id: Some(wid),
            data: Some(wdata.clone()),
        };
        let _ = self.redis_worker_repository().upsert(&w).await;
        // clear caches (name and list)
        self.clear_cache_by_name(&wdata.name).await;
        tracing::debug!("clear cache by name: {}", &wdata.name);
        let _ = self
            .redis_worker_repository()
            .publish_worker_changed(&wid, &wdata)
            .await;
        Ok(wid)
    }

    async fn update(&self, id: &WorkerId, worker: &Option<WorkerData>) -> Result<bool> {
        if let Some(w) = worker {
            if let Some(Worker {
                id: _,
                data: Some(owdat),
            }) = self.find(id).await?
            {
                let wsid = w.runner_id.or(owdat.runner_id).ok_or_else(|| {
                    JobWorkerError::InvalidParameter("runner_id is required".to_string())
                })?;
                let _ = self
                    .validate_runner_settings_data(&wsid, w.runner_settings.as_slice())
                    .await?
                    .ok_or_else(|| {
                        JobWorkerError::InvalidParameter("runner settings not provided".to_string())
                    })?;
                let wdata = w.clone();
                // use rdb
                let pool = self.rdb_worker_repository().db_pool();
                let mut tx = pool.begin().await.map_err(JobWorkerError::DBError)?;
                self.rdb_worker_repository()
                    .update(&mut *tx, id, &wdata)
                    .await?;
                tx.commit().await.map_err(JobWorkerError::DBError)?;

                let _ = self.redis_worker_repository().delete_all().await;
                // clear memory cache (XXX without limit offset cache)
                self.clear_cache(id).await;
                self.clear_cache_by_name(&w.name).await;

                let _ = self
                    .redis_worker_repository()
                    .publish_worker_changed(id, &wdata)
                    .await;

                Ok(true)
            } else {
                Err(JobWorkerError::NotFound(format!("worker not found: id={}", &id.value)).into())
            }
        } else {
            // empty data, only clear cache
            self.clear_cache(id).await;
            Ok(false)
        }
    }

    // TODO clear cache by name
    async fn delete(&self, id: &WorkerId) -> Result<bool> {
        let res = if let Some(Worker {
            id: _,
            data: Some(wd),
        }) = self.find(id).await?
        {
            let _ = self.rdb_worker_repository().delete(id).await?;
            let _ = self.redis_worker_repository().delete(id).await?;
            self.clear_cache(id).await;
            self.clear_cache_by_name(&wd.name).await;
            Ok(true)
        } else {
            Ok(false)
        };
        let _ = self
            .redis_worker_repository()
            .publish_worker_deleted(id)
            .await;
        res
    }

    async fn delete_all(&self) -> Result<bool> {
        let res = self.rdb_worker_repository().delete_all().await?;
        let _ = self.redis_worker_repository().delete_all().await?;
        self.clear_all_list_cache().await;
        let _ = self
            .redis_worker_repository()
            .publish_worker_all_deleted()
            .await;
        Ok(res)
    }

    async fn find(&self, id: &WorkerId) -> Result<Option<Worker>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_cache_key(id));
        tracing::debug!("find worker: {}, cache key: {}", id.value, k);
        self.moka_cache
            .with_cache_if_some(&k, || async {
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
            })
            .await
    }

    async fn find_data_by_name(&self, name: &str) -> Result<Option<WorkerData>>
    where
        Self: Send + 'static,
    {
        self.find_by_name(name)
            .await
            .map(|w| w.and_then(|wd| wd.data))
    }

    async fn find_by_name(&self, name: &str) -> Result<Option<Worker>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_name_cache_key(name));
        tracing::debug!("find worker: {}, cache key: {}", name, k);
        self.moka_cache
            .with_cache_if_some(&k, || async {
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
            })
            .await
    }

    async fn find_list(
        &self,
        runner_types: Vec<i32>,
        channel: Option<String>,
        limit: Option<i32>,
        offset: Option<i64>,
    ) -> Result<Vec<Worker>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_list_cache_key(
            runner_types.as_slice(),
            channel.as_ref(),
            limit,
            offset,
        ));
        self.list_memory_cache()
            .with_cache(&k, || async move {
                // not use rdb in normal case
                // match self.redis_worker_repository().find_all().await {
                //     Ok(v) if !v.is_empty() => {
                //         tracing::debug!("worker list from redis: ({:?}, {:?})", limit, offset);
                //         // soft paging
                //         let start = offset.unwrap_or(0);
                //         if let Some(l) = limit {
                //             Ok(v.into_iter()
                //                 .skip(start as usize)
                //                 .take(l as usize)
                //                 .collect())
                //         } else {
                //             Ok(v.into_iter().skip(start as usize).collect())
                //         }
                //     }
                //     // empty
                //     Ok(_v) => {
                tracing::debug!("worker list from rdb: ({:?}, {:?})", limit, offset);
                // fallback to rdb if rdb is enabled
                let list = self
                    .rdb_worker_repository()
                    .find_list(runner_types, channel, limit, offset)
                    .await?;
                if !list.is_empty() {
                    for w in list.iter() {
                        let _ = self.redis_worker_repository().upsert(w).await;
                    }
                }
                Ok(list)
                // }
                // Err(err) => {
                //     tracing::debug!(
                //         "worker list from rdb (redis error): ({:?}, {:?})",
                //         limit,
                //         offset
                //     );
                //     tracing::warn!("workers find error from redis: {:?}", err);
                //     // fallback to rdb if rdb is enabled
                //     let list = self
                //         .rdb_worker_repository()
                //         .find_list(limit, offset)
                //         .await?;
                //     if !list.is_empty() {
                //         for w in list.iter() {
                //             let _ = self.redis_worker_repository().upsert(w).await;
                //         }
                //     }
                //     Ok(list)
                // }
            })
            .await
        // })
        // .await
    }

    async fn find_all_worker_list(&self) -> Result<Vec<Worker>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_all_list_cache_key());
        self.list_memory_cache
            .with_cache(&k, || async {
                match self.redis_worker_repository().find_all().await {
                    Ok(v) if !v.is_empty() => Ok(v),
                    // empty
                    Ok(_v) => {
                        tracing::debug!("workers not find (empty) from redis: get from db");
                        // fallback to rdb if rdb is enabled
                        let list = self
                            .rdb_worker_repository()
                            .find_list(vec![], None, None, None)
                            .await;
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
                        self.rdb_worker_repository()
                            .find_list(vec![], None, None, None)
                            .await
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

    // for pubsub (XXX common logic...)
    async fn clear_cache_by(&self, id: Option<&WorkerId>, name: Option<&String>) -> Result<()> {
        if let Some(i) = id {
            self.clear_cache(i).await;
        }
        if let Some(n) = name {
            self.clear_cache_by_name(n).await;
        }
        if id.is_none() && name.is_none() {
            self.clear_cache_all().await;
        }
        Ok(())
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

impl UseRunnerApp for HybridWorkerAppImpl {
    fn runner_app(&self) -> Arc<dyn RunnerApp> {
        self.runner_app.clone()
    }
}
impl UseRunnerParserWithCache for HybridWorkerAppImpl {
    fn descriptor_cache(&self) -> &MemoryCacheImpl<Arc<String>, RunnerDataWithDescriptor> {
        &self.descriptor_cache
    }
}

impl UseRunnerAppParserWithCache for HybridWorkerAppImpl {}

impl WorkerAppCacheHelper for HybridWorkerAppImpl {
    fn memory_cache(&self) -> &MokaCacheImpl<Arc<String>, Worker> {
        &self.moka_cache
    }
    fn list_memory_cache(&self) -> &MokaCacheImpl<Arc<String>, Vec<Worker>> {
        &self.list_memory_cache
    }
}
// create test
#[cfg(test)]
mod tests {
    use crate::app::runner::hybrid::HybridRunnerAppImpl;
    use crate::app::runner::RunnerApp;
    use crate::app::worker::hybrid::HybridWorkerAppImpl;
    use crate::app::worker::WorkerApp;
    use crate::app::StorageConfig;
    use crate::module::test::TEST_PLUGIN_DIR;
    use anyhow::Result;
    use infra::infra::job::rows::{JobqueueAndCodec, UseJobqueueAndCodec};
    use infra::infra::module::rdb::test::setup_test_rdb_module;
    use infra::infra::module::redis::test::setup_test_redis_module;
    use infra::infra::module::HybridRepositoryModule;
    use infra::infra::IdGeneratorWrapper;
    use infra_utils::infra::memory::MemoryCacheImpl;
    use infra_utils::infra::test::TEST_RUNTIME;
    use proto::jobworkerp::data::{RunnerId, StorageType, WorkerData};
    use proto::TestRunnerSettings;
    use std::sync::Arc;
    use std::time::Duration;

    async fn create_test_app(use_mock_id: bool) -> Result<HybridWorkerAppImpl> {
        let rdb_module = setup_test_rdb_module().await;
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
        let moka_config = infra_utils::infra::cache::MokaCacheConfig {
            num_counters: 10000,
            ttl: Some(Duration::from_secs(60 * 60)), // 1 hour
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
            id_generator.clone(),
        );
        runner_app.load_runner().await?;
        let worker_app = HybridWorkerAppImpl::new(
            storage_config.clone(),
            id_generator.clone(),
            &moka_config,
            repositories.clone(),
            descriptor_cache,
            Arc::new(runner_app),
        );
        Ok(worker_app)
    }

    #[test]
    fn test_integrated() -> Result<()> {
        // test create 3 workers and find list and update 1 worker and find list and delete 1 worker and find list
        TEST_RUNTIME.block_on(async {
            let app = create_test_app(false).await?;
            let runner_settings = JobqueueAndCodec::serialize_message(&TestRunnerSettings {
                name: "testRunner1".to_string(),
            });
            let w1 = WorkerData {
                name: "test1".to_string(),
                runner_settings: runner_settings.clone(),
                runner_id: Some(RunnerId { value: 1 }),
                ..Default::default()
            };
            let w2 = WorkerData {
                name: "test2".to_string(),
                runner_settings: runner_settings.clone(),
                runner_id: Some(RunnerId { value: 1 }),
                ..Default::default()
            };
            let w3 = WorkerData {
                name: "test3".to_string(),
                runner_settings: runner_settings.clone(),
                runner_id: Some(RunnerId { value: 1 }),
                ..Default::default()
            };
            let id1 = app.create(&w1).await?;
            let id2 = app.create(&w2).await?;
            let id3 = app.create(&w3).await?;
            assert!(id1.value > 0);
            assert_eq!(id2.value, id1.value + 1);
            assert_eq!(id3.value, id2.value + 1);
            // find list
            let list = app.find_list(vec![], None, None, None).await?;
            assert_eq!(list.len(), 3);
            assert_eq!(app.count().await?, 3);
            // update
            let w4 = WorkerData {
                name: "test4".to_string(),
                runner_settings: runner_settings.clone(),
                // runner_id: Some(RunnerId { value: 10000 }),
                ..Default::default()
            };
            let res = app.update(&id1, &Some(w4.clone())).await?;
            assert!(res);

            let f = app.find(&id1).await?;
            assert!(f.is_some());
            let fd = f.and_then(|w| w.data);
            assert!(fd.is_some());
            assert_eq!(fd.unwrap().name, w4.name);
            // find list
            let list = app.find_list(vec![], None, None, None).await?;
            assert_eq!(list.len(), 3);
            assert_eq!(app.count().await?, 3);
            // delete
            let res = app.delete(&id1).await?;
            assert!(res);
            // find list
            let list = app.find_list(vec![], None, None, None).await?;
            assert_eq!(app.count().await?, 2);
            assert_eq!(list.len(), 2);
            Ok(())
        })
    }

    #[test]
    fn test_create_temp() -> Result<()> {
        // Test creating a temporary worker, finding it, and deleting it
        TEST_RUNTIME.block_on(async {
            let app = create_test_app(false).await?;
            let runner_settings = JobqueueAndCodec::serialize_message(&TestRunnerSettings {
                name: "testTempRunner".to_string(),
            });
            let temp_worker = WorkerData {
                name: "temp_worker".to_string(),
                runner_settings: runner_settings.clone(),
                runner_id: Some(RunnerId { value: 1 }),
                ..Default::default()
            };

            // Create temporary worker
            let id = app.create_temp(temp_worker, true).await?;
            assert!(id.value > 0);

            // Find the worker
            let found = app.find(&id).await?;
            assert!(found.is_some());
            let worker_data = found.and_then(|w| w.data);
            assert!(worker_data.is_some());

            // Verify the name has the original name as a prefix
            assert!(worker_data
                .as_ref()
                .unwrap()
                .name
                .starts_with("temp_worker"));

            // Delete the worker
            let deleted = app.delete(&id).await?;
            assert!(deleted);

            // Verify it's gone
            let not_found = app.find(&id).await?;
            assert!(not_found.is_none());

            Ok(())
        })
    }
}
