use anyhow::Result;
use async_trait::async_trait;
use command_utils::text::TextUtil;
use dashmap::DashMap;
use infra::infra::module::rdb::{RdbChanRepositoryModule, UseRdbChanRepositoryModule};
use infra::infra::worker::rdb::{RdbWorkerRepository, UseRdbWorkerRepository};
use infra::infra::{IdGeneratorWrapper, UseIdGenerator};
use infra_utils::infra::rdb::UseRdbPool;
use jobworkerp_base::error::JobWorkerError;
use memory_utils::cache::moka::{MokaCacheImpl, UseMokaCache};
use proto::jobworkerp::data::{Worker, WorkerData, WorkerId};
use std::sync::Arc;

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
    memory_cache: MokaCacheImpl<Arc<String>, Worker>,
    list_memory_cache: MokaCacheImpl<Arc<String>, Vec<Worker>>,
    /// Dedicated cache for temporary workers without TTL.
    /// MokaCache has a unified TTL for all entries, so temporary workers may be evicted
    /// before long-running jobs complete. This DashMap stores temporary workers permanently
    /// until explicitly deleted via delete_temp().
    temp_worker_cache: Arc<DashMap<i64, Worker>>,
    repositories: Arc<RdbChanRepositoryModule>,
    descriptor_cache: Arc<MokaCacheImpl<Arc<String>, RunnerDataWithDescriptor>>,
    runner_app: Arc<RdbRunnerAppImpl>,
}

impl RdbWorkerAppImpl {
    pub fn new(
        storage_config: Arc<StorageConfig>,
        id_generator: Arc<IdGeneratorWrapper>,
        moka_config: &memory_utils::cache::moka::MokaCacheConfig,
        repositories: Arc<RdbChanRepositoryModule>,
        descriptor_cache: Arc<MokaCacheImpl<Arc<String>, RunnerDataWithDescriptor>>,
        runner_app: Arc<RdbRunnerAppImpl>,
    ) -> Self {
        let memory_cache = memory_utils::cache::moka::MokaCacheImpl::new(moka_config);
        let list_memory_cache = memory_utils::cache::moka::MokaCacheImpl::new(moka_config);
        Self {
            storage_config,
            id_generator,
            memory_cache,
            list_memory_cache,
            temp_worker_cache: Arc::new(DashMap::new()),
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
            wdata.name = TextUtil::generate_random_key(Some(&wdata.name));
        }
        let wid = WorkerId {
            value: self.id_generator.generate_id()?,
        };
        let worker = Worker {
            id: Some(wid),
            data: Some(wdata),
        };

        // Store in TTL-free cache to survive MokaCache expiration for long-running jobs
        self.temp_worker_cache.insert(wid.value, worker.clone());

        // Also store in MokaCache for fast lookup (fallback to temp_worker_cache if expired)
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

    // Delete a temp worker from memory cache only (not from RDB)
    async fn delete_temp(&self, id: &WorkerId) -> Result<bool> {
        // Remove from TTL-free temp_worker_cache
        let removed = self.temp_worker_cache.remove(&id.value);

        // Get worker name for cache clearing
        let worker_name = removed
            .as_ref()
            .and_then(|(_, w)| w.data.as_ref().map(|d| d.name.clone()));
        let existed = removed.is_some();

        // Clear MokaCache (id cache and name cache)
        self.clear_cache(id).await;
        if let Some(name) = worker_name {
            self.clear_cache_by_name(&name).await;
        }

        Ok(existed)
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
            .with_cache_if_some(&k, || async {
                self.rdb_worker_repository().find_by_name(name).await
            })
            .await
    }

    async fn find(&self, id: &WorkerId) -> Result<Option<Worker>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_cache_key(id));
        let cached = self
            .memory_cache
            .with_cache_if_some(&k, || async { self.rdb_worker_repository().find(id).await })
            .await?;

        if cached.is_some() {
            return Ok(cached);
        }

        // Fallback to temp_worker_cache if MokaCache expired
        Ok(self
            .temp_worker_cache
            .get(&id.value)
            .map(|entry| entry.value().clone()))
    }

    #[allow(clippy::too_many_arguments)]
    async fn find_list(
        &self,
        runner_types: Vec<i32>,
        channel: Option<String>,
        limit: Option<i32>,
        offset: Option<i64>,
        name_filter: Option<String>,
        is_periodic: Option<bool>,
        runner_ids: Vec<i64>,
        sort_by: Option<proto::jobworkerp::data::WorkerSortField>,
        ascending: Option<bool>,
    ) -> Result<Vec<Worker>>
    where
        Self: Send + 'static,
    {
        // not cache with offset limit
        // let k = Arc::new(Self::find_list_cache_key(limit, offset));
        // self.memory_cache
        //     .with_cache(&k, None, || async {
        // not use rdb in normal case
        self.rdb_worker_repository()
            .find_list(
                runner_types,
                channel,
                limit,
                offset,
                name_filter,
                is_periodic,
                runner_ids,
                sort_by,
                ascending,
            )
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
                self.rdb_worker_repository()
                    .find_list(vec![], None, None, None, None, None, vec![], None, None)
                    .await
            })
            .await
    }
    async fn count(&self) -> Result<i64>
    where
        Self: Send + 'static,
    {
        self.rdb_worker_repository().count().await
    }

    async fn count_by(
        &self,
        runner_types: Vec<i32>,
        channel: Option<String>,
        name_filter: Option<String>,
        is_periodic: Option<bool>,
        runner_ids: Vec<i64>,
    ) -> Result<i64>
    where
        Self: Send + 'static,
    {
        self.rdb_worker_repository()
            .count_by(runner_types, channel, name_filter, is_periodic, runner_ids)
            .await
    }

    async fn count_by_channel(&self) -> Result<Vec<(String, i64)>>
    where
        Self: Send + 'static,
    {
        self.rdb_worker_repository().count_by_channel().await
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
    fn memory_cache(&self) -> &MokaCacheImpl<Arc<String>, Worker> {
        &self.memory_cache
    }
    fn list_memory_cache(&self) -> &MokaCacheImpl<Arc<String>, Vec<Worker>> {
        &self.list_memory_cache
    }
}
impl UseRunnerApp for RdbWorkerAppImpl {
    fn runner_app(&self) -> Arc<dyn RunnerApp> {
        self.runner_app.clone()
    }
}
impl UseRunnerParserWithCache for RdbWorkerAppImpl {
    fn descriptor_cache(&self) -> &MokaCacheImpl<Arc<String>, RunnerDataWithDescriptor> {
        &self.descriptor_cache
    }
}

impl UseRunnerAppParserWithCache for RdbWorkerAppImpl {}

#[cfg(test)]
mod tests {
    use crate::app::runner::rdb::RdbRunnerAppImpl;
    use crate::app::runner::RunnerApp;
    use crate::app::worker::rdb::RdbWorkerAppImpl;
    use crate::app::worker::WorkerApp;
    use crate::app::StorageConfig;
    use crate::module::test::TEST_PLUGIN_DIR;
    use anyhow::Result;
    use infra::infra::job::rows::JobqueueAndCodec;
    use infra::infra::module::rdb::test::setup_test_rdb_module;
    use infra::infra::IdGeneratorWrapper;
    use infra_utils::infra::test::TEST_RUNTIME;
    use jobworkerp_base::codec::UseProstCodec;
    use memory_utils::cache::moka::MokaCacheImpl;
    use proto::jobworkerp::data::{RunnerId, StorageType, WorkerData};
    use proto::TestRunnerSettings;
    use std::sync::Arc;
    use std::time::Duration;

    async fn create_test_app(use_mock_id: bool) -> Result<RdbWorkerAppImpl> {
        let rdb_module = Arc::new(setup_test_rdb_module(false).await);

        // Mock id generator or use real one
        let id_generator = if use_mock_id {
            Arc::new(IdGeneratorWrapper::new_mock())
        } else {
            Arc::new(IdGeneratorWrapper::new())
        };

        // Memory cache configuration
        let moka_config = memory_utils::cache::moka::MokaCacheConfig {
            num_counters: 10000,
            ttl: Some(Duration::from_secs(60)),
        };

        let descriptor_cache = Arc::new(MokaCacheImpl::new(&moka_config));
        let storage_config = Arc::new(StorageConfig {
            r#type: StorageType::Standalone,
            restore_at_startup: Some(false),
        });

        let runner_app = RdbRunnerAppImpl::new(
            TEST_PLUGIN_DIR.to_string(),
            storage_config.clone(),
            &moka_config,
            rdb_module.clone(),
            descriptor_cache.clone(),
        );
        runner_app.load_runner().await?;

        let worker_app = RdbWorkerAppImpl::new(
            storage_config.clone(),
            id_generator.clone(),
            &moka_config,
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
            })?;

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

            let id1 = app.create(&w1).await?;
            let id2 = app.create(&w2).await?;
            let id3 = app.create(&w3).await?;

            assert!(id1.value > 0);
            assert_eq!(id2.value, id1.value + 1);
            assert_eq!(id3.value, id2.value + 1);

            // Find worker list and verify count
            let list = app
                .find_list(vec![], None, None, None, None, None, vec![], None, None)
                .await?;
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

            let found = app.find(&id1).await?;
            assert!(found.is_some());
            let worker_data = found.and_then(|w| w.data);
            assert!(worker_data.is_some());
            assert_eq!(worker_data.unwrap().name, w4.name);

            let found_by_name = app.find_by_name("test_rdb_updated").await?;
            assert!(found_by_name.is_some());

            let deleted = app.delete(&id1).await?;
            assert!(deleted);

            let list = app
                .find_list(vec![], None, None, None, None, None, vec![], None, None)
                .await?;
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
            })?;

            let temp_worker = WorkerData {
                name: "temp_rdb_worker".to_string(),
                runner_settings: runner_settings.clone(),
                runner_id: Some(RunnerId { value: 1 }),
                ..Default::default()
            };

            let id = app.create_temp(temp_worker.clone(), true).await?;
            assert!(id.value > 0);

            // Find the worker
            let found = app.find(&id).await?;
            assert!(found.is_some());
            let worker_data = found.and_then(|w| w.data);
            assert!(worker_data.is_some());
            assert!(worker_data
                .as_ref()
                .unwrap()
                .name
                .starts_with(&temp_worker.name));

            let deleted = app.delete_temp(&id).await?;
            assert!(deleted);

            let not_found = app.find(&id).await?;
            assert!(not_found.is_none());

            Ok(())
        })
    }

    async fn create_test_app_with_short_ttl() -> Result<RdbWorkerAppImpl> {
        let rdb_module = Arc::new(setup_test_rdb_module(false).await);
        let id_generator = Arc::new(IdGeneratorWrapper::new());

        // Very short TTL for testing cache expiration behavior
        let moka_config = memory_utils::cache::moka::MokaCacheConfig {
            num_counters: 10000,
            ttl: Some(Duration::from_millis(100)),
        };

        let descriptor_cache = Arc::new(MokaCacheImpl::new(&moka_config));
        let storage_config = Arc::new(StorageConfig {
            r#type: StorageType::Standalone,
            restore_at_startup: Some(false),
        });

        let runner_app = RdbRunnerAppImpl::new(
            TEST_PLUGIN_DIR.to_string(),
            storage_config.clone(),
            &moka_config,
            rdb_module.clone(),
            descriptor_cache.clone(),
        );
        runner_app.load_runner().await?;

        let worker_app = RdbWorkerAppImpl::new(
            storage_config.clone(),
            id_generator.clone(),
            &moka_config,
            rdb_module,
            descriptor_cache,
            Arc::new(runner_app),
        );

        Ok(worker_app)
    }

    #[test]
    fn test_create_temp_survives_cache_ttl() -> Result<()> {
        // Test that temporary workers survive MokaCache TTL expiration
        TEST_RUNTIME.block_on(async {
            let app = create_test_app_with_short_ttl().await?;
            let runner_settings = JobqueueAndCodec::serialize_message(&TestRunnerSettings {
                name: "testTempTtlRunner".to_string(),
            })?;

            let temp_worker = WorkerData {
                name: "temp_ttl_worker".to_string(),
                runner_settings: runner_settings.clone(),
                runner_id: Some(RunnerId { value: 1 }),
                ..Default::default()
            };

            let id = app.create_temp(temp_worker.clone(), true).await?;
            assert!(id.value > 0);

            // Wait for MokaCache TTL to expire (100ms + buffer)
            tokio::time::sleep(Duration::from_millis(200)).await;

            // Worker should still be found via temp_worker_cache fallback
            let found = app.find(&id).await?;
            assert!(
                found.is_some(),
                "temporary worker should survive MokaCache TTL expiration"
            );

            let worker_data = found.and_then(|w| w.data);
            assert!(worker_data.is_some());
            assert!(worker_data
                .as_ref()
                .unwrap()
                .name
                .starts_with(&temp_worker.name));

            // delete_temp should still work after TTL expiration
            let deleted = app.delete_temp(&id).await?;
            assert!(deleted);

            // After deletion, worker should not be found
            let not_found = app.find(&id).await?;
            assert!(not_found.is_none());

            Ok(())
        })
    }
}
