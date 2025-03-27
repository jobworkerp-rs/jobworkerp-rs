use anyhow::Result;
use async_trait::async_trait;
use command_utils::util::option::FlatMap;
use infra::infra::job::rows::UseJobqueueAndCodec;
use infra::infra::module::rdb::{RdbChanRepositoryModule, UseRdbChanRepositoryModule};
use infra::infra::module::redis::{RedisRepositoryModule, UseRedisRepositoryModule};
use infra::infra::module::HybridRepositoryModule;
use infra::infra::worker::event::UseWorkerPublish;
use infra::infra::worker::rdb::{RdbWorkerRepository, UseRdbWorkerRepository};
use infra::infra::worker::redis::{RedisWorkerRepository, UseRedisWorkerRepository};
use infra::infra::{IdGeneratorWrapper, UseIdGenerator};
use infra_utils::infra::memory::{MemoryCacheConfig, MemoryCacheImpl, UseMemoryCache};
use infra_utils::infra::rdb::UseRdbPool;
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{Worker, WorkerData, WorkerId};
use std::sync::Arc;
use std::time::Duration;

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
    memory_cache: MemoryCacheImpl<Arc<String>, Worker>,
    list_memory_cache: MemoryCacheImpl<Arc<String>, Vec<Worker>>,
    repositories: Arc<HybridRepositoryModule>,
    descriptor_cache: Arc<MemoryCacheImpl<Arc<String>, RunnerDataWithDescriptor>>,
    runner_app: Arc<dyn RunnerApp + 'static>,
}

impl HybridWorkerAppImpl {
    pub fn new(
        storage_config: Arc<StorageConfig>,
        id_generator: Arc<IdGeneratorWrapper>,
        mc_config: &MemoryCacheConfig,
        repositories: Arc<HybridRepositoryModule>,
        descriptor_cache: Arc<MemoryCacheImpl<Arc<String>, RunnerDataWithDescriptor>>,
        runner_app: Arc<dyn RunnerApp + 'static>,
    ) -> Self {
        let list_memory_cache = infra_utils::infra::memory::MemoryCacheImpl::new(
            mc_config,
            Some(Duration::from_secs(5 * 60)),
        );

        let memory_cache = infra_utils::infra::memory::MemoryCacheImpl::new(
            mc_config,
            Some(Duration::from_secs(5 * 60)),
        );
        Self {
            storage_config,
            id_generator,
            memory_cache,
            list_memory_cache,
            repositories,
            descriptor_cache,
            runner_app,
        }
    }
}

// TODO now, hybrid repository (or redis?) version only
#[async_trait]
impl WorkerApp for HybridWorkerAppImpl {
    async fn create(&self, worker: &WorkerData) -> Result<WorkerId> {
        let wsid = worker
            .runner_id
            .ok_or_else(|| JobWorkerError::InvalidParameter("runner_id is required".to_string()))?;
        let RunnerDataWithDescriptor {
            runner_data,
            runner_settings_descriptor: _,
            args_descriptor: _,
            result_descriptor: _,
        } = self
            .validate_runner_settings_data(&wsid, worker.runner_settings.as_slice())
            .await?
            .ok_or_else(|| {
                JobWorkerError::InvalidParameter("runner settings not provided".to_string())
            })?;
        let mut wdata = worker.clone();
        // overwrite output_as_stream only if data is provided from runner
        if let Some(output_as_stream) = runner_data.output_as_stream {
            wdata.output_as_stream = output_as_stream;
        }
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
                let RunnerDataWithDescriptor {
                    runner_data,
                    runner_settings_descriptor: _,
                    args_descriptor: _,
                    result_descriptor: _,
                } = self
                    .validate_runner_settings_data(&wsid, w.runner_settings.as_slice())
                    .await?
                    .ok_or_else(|| {
                        JobWorkerError::InvalidParameter("runner settings not provided".to_string())
                    })?;
                let mut wdata = w.clone();
                // overwrite output_as_stream only if data is provided from runner
                if let Some(output_as_stream) = runner_data.output_as_stream {
                    wdata.output_as_stream = output_as_stream;
                }

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
        self.memory_cache
            .with_cache_if_some(&k, None, || async {
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
            .map(|w| w.flat_map(|wd| wd.data))
    }

    async fn find_by_name(&self, name: &str) -> Result<Option<Worker>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_name_cache_key(name));
        tracing::debug!("find worker: {}, cache key: {}", name, k);
        self.memory_cache
            .with_cache_if_some(&k, None, || async {
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

    async fn find_list(&self, limit: Option<i32>, offset: Option<i64>) -> Result<Vec<Worker>>
    where
        Self: Send + 'static,
    {
        // let k = Arc::new(Self::find_list_cache_key(limit, offset));
        // self.memory_cache
        //     .with_cache(&k, None, || async {
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
        // })
        // .await
    }

    async fn find_all_worker_list(&self) -> Result<Vec<Worker>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_all_list_cache_key());
        self.list_memory_cache
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
    fn memory_cache(&self) -> &MemoryCacheImpl<Arc<String>, Worker> {
        &self.memory_cache
    }
    fn list_memory_cache(&self) -> &MemoryCacheImpl<Arc<String>, Vec<Worker>> {
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
    use anyhow::Result;
    use command_utils::util::option::FlatMap;
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
        let descriptor_cache = Arc::new(MemoryCacheImpl::new(&mc_config, None));
        let storage_config = Arc::new(StorageConfig {
            r#type: StorageType::Scalable,
            restore_at_startup: Some(false),
        });
        let runner_app = HybridRunnerAppImpl::new(
            storage_config.clone(),
            &mc_config,
            repositories.clone(),
            descriptor_cache.clone(),
        );
        runner_app.load_runner().await?;
        let worker_app = HybridWorkerAppImpl::new(
            storage_config.clone(),
            id_generator.clone(),
            &mc_config,
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
            assert_eq!(id1.value, 1);
            assert_eq!(id2.value, 2);
            assert_eq!(id3.value, 3);
            // find list
            let list = app.find_list(None, None).await?;
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
            let fd = f.flat_map(|w| w.data);
            assert!(fd.is_some());
            assert_eq!(fd.unwrap().name, w4.name);
            // find list
            let list = app.find_list(None, None).await?;
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
