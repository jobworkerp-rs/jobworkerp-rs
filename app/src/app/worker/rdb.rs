use anyhow::Result;
use async_trait::async_trait;
use command_utils::text::TextUtil;
use infra::infra::module::rdb::{RdbChanRepositoryModule, UseRdbChanRepositoryModule};
use infra::infra::worker::rdb::{RdbWorkerRepository, UseRdbWorkerRepository};
use infra::infra::{IdGeneratorWrapper, UseIdGenerator};
use infra_utils::infra::memory::{MemoryCacheConfig, MemoryCacheImpl, UseMemoryCache};
use infra_utils::infra::rdb::UseRdbPool;
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{Worker, WorkerData, WorkerId};
use std::sync::Arc;
use std::time::Duration;

use crate::app::runner::rdb::RdbRunnerAppImpl;
use crate::app::runner::{
    RunnerApp, RunnerDataWithDescriptor, UseRunnerApp, UseRunnerAppParserWithCache,
    UseRunnerParserWithCache,
};

use super::super::{StorageConfig, UseStorageConfig};
use super::{WorkerApp, WorkerAppCacheHelper};

#[derive(Clone, Debug)]
pub struct RdbWorkerAppImpl {
    storage_config: Arc<StorageConfig>,
    id_generator: Arc<IdGeneratorWrapper>,
    memory_cache: MemoryCacheImpl<Arc<String>, Worker>,
    list_memory_cache: MemoryCacheImpl<Arc<String>, Vec<Worker>>,
    repositories: Arc<RdbChanRepositoryModule>,
    descriptor_cache: Arc<MemoryCacheImpl<Arc<String>, RunnerDataWithDescriptor>>,
    runner_app: Arc<RdbRunnerAppImpl>,
}

impl RdbWorkerAppImpl {
    pub fn new(
        storage_config: Arc<StorageConfig>,
        id_generator: Arc<IdGeneratorWrapper>,
        mc_config: &MemoryCacheConfig,
        repositories: Arc<RdbChanRepositoryModule>,
        descriptor_cache: Arc<MemoryCacheImpl<Arc<String>, RunnerDataWithDescriptor>>,
        runner_app: Arc<RdbRunnerAppImpl>,
    ) -> Self {
        let memory_cache = infra_utils::infra::memory::MemoryCacheImpl::new(
            mc_config,
            Some(Duration::from_secs(5 * 60)),
        );
        let list_memory_cache = infra_utils::infra::memory::MemoryCacheImpl::new(
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

#[async_trait]
impl WorkerApp for RdbWorkerAppImpl {
    async fn create(&self, worker: &WorkerData) -> Result<WorkerId> {
        let wsid = worker
            .runner_id
            .ok_or_else(|| JobWorkerError::InvalidParameter("runner_id is required".to_string()))?;
        self.validate_runner_settings_data(&wsid, worker.runner_settings.as_slice())
            .await?;

        let db = self.rdb_worker_repository().db_pool();
        let mut tx = db.begin().await.map_err(JobWorkerError::DBError)?;
        let wid = self
            .rdb_worker_repository()
            .create(&mut *tx, worker)
            .await?;
        tx.commit().await.map_err(JobWorkerError::DBError)?;
        // clear name cache (and list cache)
        self.clear_cache_by_name(&worker.name).await;
        // not broadcast to runner (single instance for rdb only)
        Ok(wid)
    }

    async fn create_temp(&self, worker: WorkerData, with_random_name: bool) -> Result<WorkerId> {
        let wsid = worker
            .runner_id
            .ok_or_else(|| JobWorkerError::InvalidParameter("runner_id is required".to_string()))?;
        self.validate_runner_settings_data(&wsid, worker.runner_settings.as_slice())
            .await?;

        let mut wdata = worker;
        if with_random_name {
            // generate random name
            wdata.name = TextUtil::generate_random_key(Some(&wdata.name));
        }
        let wid = WorkerId {
            value: self.id_generator.generate_id()?,
        };
        let worker = Worker {
            id: Some(wid),
            data: Some(wdata),
        };
        // clear name cache (and list cache)
        self.create_cache(&wid, &worker).await?;
        // not broadcast to runner (single instance for rdb only)
        Ok(wid)
    }

    async fn update(&self, id: &WorkerId, worker: &Option<WorkerData>) -> Result<bool> {
        if let Some(w) = worker {
            let wsid = w.runner_id.ok_or_else(|| {
                JobWorkerError::InvalidParameter("runner_id is required".to_string())
            })?;
            self.validate_runner_settings_data(&wsid, w.runner_settings.as_slice())
                .await?;

            let pool = self.rdb_worker_repository().db_pool();
            let mut tx = pool.begin().await.map_err(JobWorkerError::DBError)?;
            self.rdb_worker_repository().update(&mut *tx, id, w).await?;
            tx.commit().await.map_err(JobWorkerError::DBError)?;

            // clear memory cache (XXX without limit offset cache)
            self.clear_cache(id).await;
            self.clear_cache_by_name(&w.name).await;
            Ok(true)
        } else {
            // empty data, only clear id cache
            self.clear_cache(id).await;
            Ok(false)
        }
    }

    async fn delete(&self, id: &WorkerId) -> Result<bool> {
        if let Some(Worker {
            id: _,
            data: Some(wd),
        }) = self.find(id).await?
        {
            self.rdb_worker_repository().delete(id).await?;
            self.clear_cache(id).await;
            self.clear_cache(id).await;
            self.clear_cache_by_name(&wd.name).await;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn delete_all(&self) -> Result<bool> {
        let res = self.rdb_worker_repository().delete_all().await?;
        self.clear_cache_all().await;
        Ok(res)
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
        self.memory_cache
            .with_cache_if_some(&k, None, || async {
                self.rdb_worker_repository().find_by_name(name).await
            })
            .await
    }

    async fn find(&self, id: &WorkerId) -> Result<Option<Worker>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_cache_key(id));
        self.memory_cache
            .with_cache_if_some(&k, None, || async {
                self.rdb_worker_repository().find(id).await
            })
            .await
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
        self.list_memory_cache
            .with_cache(&k, None, || async {
                self.rdb_worker_repository().find_list(None, None).await
            })
            .await
    }
    async fn count(&self) -> Result<i64>
    where
        Self: Send + 'static,
    {
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
    fn memory_cache(&self) -> &MemoryCacheImpl<Arc<String>, Worker> {
        &self.memory_cache
    }
    fn list_memory_cache(&self) -> &MemoryCacheImpl<Arc<String>, Vec<Worker>> {
        &self.list_memory_cache
    }
}
impl UseRunnerApp for RdbWorkerAppImpl {
    fn runner_app(&self) -> Arc<dyn RunnerApp> {
        self.runner_app.clone()
    }
}
impl UseRunnerParserWithCache for RdbWorkerAppImpl {
    fn descriptor_cache(&self) -> &MemoryCacheImpl<Arc<String>, RunnerDataWithDescriptor> {
        &self.descriptor_cache
    }
}

impl UseRunnerAppParserWithCache for RdbWorkerAppImpl {}

// Add tests for RdbWorkerAppImpl
#[cfg(test)]
mod tests {
    use crate::app::runner::rdb::RdbRunnerAppImpl;
    use crate::app::runner::RunnerApp;
    use crate::app::worker::rdb::RdbWorkerAppImpl;
    use crate::app::worker::WorkerApp;
    use crate::app::StorageConfig;
    use crate::module::test::TEST_PLUGIN_DIR;
    use anyhow::Result;
    use infra::infra::job::rows::{JobqueueAndCodec, UseJobqueueAndCodec};
    use infra::infra::module::rdb::test::setup_test_rdb_module;
    use infra::infra::IdGeneratorWrapper;
    use infra_utils::infra::memory::MemoryCacheImpl;
    use infra_utils::infra::test::TEST_RUNTIME;
    use proto::jobworkerp::data::{RunnerId, StorageType, WorkerData};
    use proto::TestRunnerSettings;
    use std::sync::Arc;

    async fn create_test_app(use_mock_id: bool) -> Result<RdbWorkerAppImpl> {
        let rdb_module = Arc::new(setup_test_rdb_module().await);

        // Mock id generator or use real one
        let id_generator = if use_mock_id {
            Arc::new(IdGeneratorWrapper::new_mock())
        } else {
            Arc::new(IdGeneratorWrapper::new())
        };

        // Memory cache configuration
        let mc_config = infra_utils::infra::memory::MemoryCacheConfig {
            num_counters: 10000,
            max_cost: 10000,
            use_metrics: false,
        };

        let descriptor_cache = Arc::new(MemoryCacheImpl::new(&mc_config, None));
        let storage_config = Arc::new(StorageConfig {
            r#type: StorageType::Standalone,
            restore_at_startup: Some(false),
        });

        // Create and initialize runner app
        let runner_app = RdbRunnerAppImpl::new(
            TEST_PLUGIN_DIR.to_string(),
            storage_config.clone(),
            &mc_config,
            rdb_module.clone(),
            descriptor_cache.clone(),
            id_generator.clone(),
        );
        runner_app.load_runner().await?;

        // Create worker app with runner app
        let worker_app = RdbWorkerAppImpl::new(
            storage_config.clone(),
            id_generator.clone(),
            &mc_config,
            rdb_module,
            descriptor_cache,
            Arc::new(runner_app),
        );

        Ok(worker_app)
    }

    #[test]
    fn test_integrated() -> Result<()> {
        // Test creating multiple workers, finding them, updating one, and deleting one
        TEST_RUNTIME.block_on(async {
            let app = create_test_app(false).await?;
            let runner_settings = JobqueueAndCodec::serialize_message(&TestRunnerSettings {
                name: "testRunner1".to_string(),
            });

            // Create three workers
            let w1 = WorkerData {
                name: "test_rdb_1".to_string(),
                runner_settings: runner_settings.clone(),
                runner_id: Some(RunnerId { value: 1 }),
                ..Default::default()
            };
            let w2 = WorkerData {
                name: "test_rdb_2".to_string(),
                runner_settings: runner_settings.clone(),
                runner_id: Some(RunnerId { value: 1 }),
                ..Default::default()
            };
            let w3 = WorkerData {
                name: "test_rdb_3".to_string(),
                runner_settings: runner_settings.clone(),
                runner_id: Some(RunnerId { value: 1 }),
                ..Default::default()
            };

            // Create the workers and verify IDs
            let id1 = app.create(&w1).await?;
            let id2 = app.create(&w2).await?;
            let id3 = app.create(&w3).await?;

            assert!(id1.value > 0);
            assert_eq!(id2.value, id1.value + 1);
            assert_eq!(id3.value, id2.value + 1);

            // Find worker list and verify count
            let list = app.find_list(None, None).await?;
            assert_eq!(list.len(), 3);
            assert_eq!(app.count().await?, 3);

            // Update a worker
            let w4 = WorkerData {
                name: "test_rdb_updated".to_string(),
                runner_settings: runner_settings.clone(),
                runner_id: Some(RunnerId { value: 1 }),
                ..Default::default()
            };
            let res = app.update(&id1, &Some(w4.clone())).await?;
            assert!(res);

            // Verify the update worked
            let found = app.find(&id1).await?;
            assert!(found.is_some());
            let worker_data = found.and_then(|w| w.data);
            assert!(worker_data.is_some());
            assert_eq!(worker_data.unwrap().name, w4.name);

            // Verify we can retrieve by name
            let found_by_name = app.find_by_name("test_rdb_updated").await?;
            assert!(found_by_name.is_some());

            // Delete a worker
            let deleted = app.delete(&id1).await?;
            assert!(deleted);

            // Verify it's gone
            let list = app.find_list(None, None).await?;
            assert_eq!(list.len(), 2);
            assert_eq!(app.count().await?, 2);

            // Cleanup
            let _ = app.delete_all().await?;

            Ok(())
        })
    }

    #[test]
    fn test_create_temp() -> Result<()> {
        // Test creating a temporary worker, finding it, and deleting it
        TEST_RUNTIME.block_on(async {
            let app = create_test_app(false).await?;
            let runner_settings = JobqueueAndCodec::serialize_message(&TestRunnerSettings {
                name: "testRdbTempRunner".to_string(),
            });

            let temp_worker = WorkerData {
                name: "temp_rdb_worker".to_string(),
                runner_settings: runner_settings.clone(),
                runner_id: Some(RunnerId { value: 1 }),
                ..Default::default()
            };

            // Create temporary worker
            let id = app.create_temp(temp_worker.clone(), true).await?;
            assert!(id.value > 0);

            // Find the worker
            let found = app.find(&id).await?;
            assert!(found.is_some());
            let worker_data = found.and_then(|w| w.data);
            assert!(worker_data.is_some());
            assert_eq!(worker_data.as_ref().unwrap().name, temp_worker.name);

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
