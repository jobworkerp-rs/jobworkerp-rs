use super::{RunnerApp, RunnerCacheHelper, RunnerDataWithDescriptor, UseRunnerParserWithCache};
use crate::app::StorageConfig;
use anyhow::Result;
use async_trait::async_trait;
use debug_stub_derive::DebugStub;
use infra::infra::module::rdb::RdbChanRepositoryModule;
use infra::infra::runner::rdb::RunnerRepository;
use infra::infra::runner::rdb::{RdbRunnerRepositoryImpl, UseRdbRunnerRepository};
use infra::infra::runner::rows::RunnerWithSchema;
use infra::infra::IdGeneratorWrapper;
use infra_utils::infra::lock::RwLockWithKey;
use infra_utils::infra::memory::{self, MemoryCacheConfig, MemoryCacheImpl, UseMemoryCache};
use infra_utils::infra::rdb::UseRdbPool;
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{RunnerId, RunnerType};
use std::{sync::Arc, time::Duration};
use stretto::AsyncCache;

// TODO merge with hybrid implementation
#[derive(Clone, DebugStub)]
pub struct RdbRunnerAppImpl {
    plugin_dir: String,
    storage_config: Arc<StorageConfig>,
    #[debug_stub = "AsyncCache<Arc<String>, Vec<RunnerWithSchema>>"]
    async_cache: AsyncCache<Arc<String>, Vec<RunnerWithSchema>>,
    descriptor_cache: Arc<MemoryCacheImpl<Arc<String>, RunnerDataWithDescriptor>>,
    repositories: Arc<RdbChanRepositoryModule>,
    cache_ttl: Option<Duration>,
    #[debug_stub = "Arc<RwLockWithKey<Arc<String>>>"]
    key_lock: Arc<RwLockWithKey<Arc<String>>>,
    id_generator: Arc<IdGeneratorWrapper>,
}

impl RdbRunnerAppImpl {
    pub fn new(
        plugin_dir: String,
        storage_config: Arc<StorageConfig>,
        memory_cache_config: &MemoryCacheConfig,
        repositories: Arc<RdbChanRepositoryModule>,
        descriptor_cache: Arc<MemoryCacheImpl<Arc<String>, RunnerDataWithDescriptor>>,
        id_generator: Arc<IdGeneratorWrapper>,
    ) -> Self {
        Self {
            plugin_dir,
            storage_config,
            async_cache: memory::new_memory_cache(memory_cache_config),
            descriptor_cache,
            repositories,
            cache_ttl: Some(Duration::from_secs(60)), // TODO from setting
            key_lock: Arc::new(RwLockWithKey::new(memory_cache_config.num_counters)),
            id_generator,
        }
    }
    // load runners from db in initialization
    async fn load_runners_from_db(&self) -> Result<()> {
        let runners = self.runner_repository().find_list(None, None).await?;
        for data in runners.into_iter().flat_map(|r| r.data) {
            if let Err(e) = match data.runner_type {
                val if val == RunnerType::McpServer as i32 => self
                    .runner_repository()
                    .load_mcp_server(
                        data.name.as_str(),
                        data.description.as_str(),
                        data.definition.as_str(),
                    )
                    .await
                    .map(|_| ()),
                val if val == RunnerType::Plugin as i32 => self
                    .runner_repository()
                    .load_plugin(Some(data.name.as_str()), &data.definition)
                    .await
                    .map(|_| ()),
                _ => Ok(()), // skip other types
            } {
                tracing::error!("load runner error: {:?}", e);
                return Err(e);
            }
        }
        Ok(())
    }

    async fn add_runner(&self) -> Result<()> {
        self.load_runners_from_db().await?;
        self.runner_repository()
            .add_from_plugins_from(self.plugin_dir.as_str())
            .await?;
        self.runner_repository().add_from_mcp_config_file().await?;
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key())
            .await;
        Ok(())
    }
    // TODO update by reloading plugin files
    // async fn update_runner(
    //     &self,
    //     id: &RunnerId,
    //     runner: &Option<RunnerData>,
    // ) -> Result<bool> {
    //     if let Some(w) = runner {
    //         let pool = self.runner_repository().db_pool();
    //         let mut tx = pool.begin().await.map_err(JobWorkerError::DBError)?;
    //         self.runner_repository()
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

impl UseRunnerParserWithCache for RdbRunnerAppImpl {
    fn descriptor_cache(&self) -> &MemoryCacheImpl<Arc<String>, RunnerDataWithDescriptor> {
        &self.descriptor_cache
    }
}

#[async_trait]
impl RunnerApp for RdbRunnerAppImpl {
    async fn load_runner(&self) -> Result<bool> {
        self.add_runner().await?;
        Ok(true)
    }

    async fn create_runner(
        &self,
        name: &str,
        description: &str,
        runner_type: i32,
        definition: &str,
    ) -> Result<RunnerId> {
        let runner_id = self.id_generator.generate_id()?;
        match runner_type {
            val if val == RunnerType::McpServer as i32 => {
                let _ = self
                    .runner_repository()
                    .create_mcp_server(name, description, definition)
                    .await?;
            }
            val if val == RunnerType::Plugin as i32 => {
                let _ = self
                    .runner_repository()
                    .create_plugin(name, description, definition)
                    .await?;
            }
            _ => {}
        }
        let res = match runner_type {
            val if val == RunnerType::McpServer as i32 => self
                .runner_repository()
                .load_mcp_server(name, description, definition)
                .await
                .map(|_| true),
            val if val == RunnerType::Plugin as i32 => self
                .runner_repository()
                .load_plugin(Some(name), definition)
                .await
                .map(|_| true),
            _ => Err(JobWorkerError::InvalidParameter(format!(
                "Invalid runner type: {}",
                runner_type
            ))
            .into()),
        }?;
        // let res = self
        //     .runner_repository()
        //     .create(&RunnerRow {
        //         id: runner_id,
        //         name: name.to_string(),
        //         description: description.to_string(),
        //         definition: definition.to_string(),
        //         r#type: runner_type,
        //     })
        //     .await?;
        if res {
            // clear memory cache
            let _ = self
                .delete_cache_locked(&Self::find_all_list_cache_key())
                .await;
            Ok(RunnerId { value: runner_id })
        } else {
            Err(JobWorkerError::AlreadyExists(format!("Runner already exists: {}", name)).into())
        }
    }

    async fn delete_runner(&self, id: &RunnerId) -> Result<bool> {
        let res = self.runner_repository().remove(id).await?;
        // clear memory cache
        let _ = self
            .delete_cache_locked(&Self::find_cache_key(&id.value))
            .await;
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key())
            .await;
        Ok(res)
    }

    async fn find_runner(
        &self,
        id: &RunnerId,
        ttl: Option<&Duration>,
    ) -> Result<Option<RunnerWithSchema>>
    where
        Self: Send + 'static,
    {
        let k = Self::find_cache_key(&id.value);
        self.with_cache_locked(&k, ttl, || async {
            let v = self.runner_repository().find(id).await;
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

    async fn find_runner_by_name(
        &self,
        name: &str,
        ttl: Option<&Duration>,
    ) -> Result<Option<RunnerWithSchema>>
    where
        Self: Send + 'static,
    {
        let k = Self::find_name_cache_key(name);
        self.with_cache_locked(&k, ttl, || async {
            let v = self.runner_repository().find_by_name(name).await;
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
    async fn find_runner_list(
        &self,
        limit: Option<&i32>,
        offset: Option<&i64>,
        _ttl: Option<&Duration>,
    ) -> Result<Vec<RunnerWithSchema>>
    where
        Self: Send + 'static,
    {
        // TODO cache
        self.runner_repository().find_list(limit, offset).await
    }

    async fn find_runner_all_list(&self, ttl: Option<&Duration>) -> Result<Vec<RunnerWithSchema>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_all_list_cache_key());
        self.with_cache_locked(&k, ttl, || async {
            self.runner_repository().find_list(None, None).await
        })
        .await
    }

    async fn count(&self) -> Result<i64>
    where
        Self: Send + 'static,
    {
        // TODO cache
        self.runner_repository()
            .count_list_tx(self.runner_repository().db_pool())
            .await
    }

    // for test
    #[cfg(any(test, feature = "test-utils"))]
    async fn create_test_runner(
        &self,
        runner_id: &RunnerId,
        name: &str,
    ) -> Result<RunnerDataWithDescriptor> {
        use crate::app::runner::test::test_runner_with_descriptor;
        use infra::infra::runner::rows::RunnerRow;
        use proto::jobworkerp::data::RunnerType;

        let runner_data = test_runner_with_descriptor(name);
        let _res = self
            .runner_repository()
            ._create(&RunnerRow {
                id: runner_id.value,
                name: name.to_string(),
                description: runner_data.runner_data.description.clone(),
                definition: format!("./target/debug/lib{}.so", name),
                r#type: RunnerType::Plugin as i32,
            })
            .await?;
        self.store_proto_cache(runner_id, &runner_data).await;
        // clear memory cache
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key())
            .await;
        Ok(runner_data)
    }
}

impl UseRdbRunnerRepository for RdbRunnerAppImpl {
    fn runner_repository(&self) -> &RdbRunnerRepositoryImpl {
        &self.repositories.runner_repository
    }
}

impl UseMemoryCache<Arc<String>, Vec<RunnerWithSchema>> for RdbRunnerAppImpl {
    fn cache(&self) -> &AsyncCache<Arc<String>, Vec<RunnerWithSchema>> {
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
impl RunnerCacheHelper for RdbRunnerAppImpl {}
