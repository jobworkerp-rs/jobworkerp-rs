use super::{RunnerApp, RunnerCacheHelper, RunnerDataWithDescriptor, UseRunnerParserWithCache};
use crate::app::StorageConfig;
use anyhow::Result;
use async_trait::async_trait;
use debug_stub_derive::DebugStub;
use infra::infra::module::rdb::RdbChanRepositoryModule;
use infra::infra::runner::rdb::RunnerRepository;
use infra::infra::runner::rdb::{RdbRunnerRepositoryImpl, UseRdbRunnerRepository};
use infra::infra::runner::rows::RunnerWithSchema;
use infra_utils::infra::rdb::UseRdbPool;
use jobworkerp_base::error::JobWorkerError;
use memory_utils::cache::moka::{MokaCacheConfig, MokaCacheImpl, UseMokaCache};
use moka::future::Cache;
use proto::jobworkerp::data::{RunnerId, RunnerType};
use std::{sync::Arc, time::Duration};

// TODO merge with hybrid implementation
#[derive(Clone, DebugStub)]
pub struct RdbRunnerAppImpl {
    plugin_dir: String,
    storage_config: Arc<StorageConfig>,
    #[debug_stub = "MokaCache<Arc<String>, Vec<RunnerWithSchema>>"]
    async_cache: Arc<MokaCacheImpl<Arc<String>, Vec<RunnerWithSchema>>>,
    #[debug_stub = "MokaCache<Arc<String>, RunnerWithSchema>"]
    descriptor_cache: Arc<MokaCacheImpl<Arc<String>, RunnerDataWithDescriptor>>,
    repositories: Arc<RdbChanRepositoryModule>,
    cache_ttl: Option<Duration>,
}

impl RdbRunnerAppImpl {
    pub fn new(
        plugin_dir: String,
        storage_config: Arc<StorageConfig>,
        memory_cache_config: &MokaCacheConfig,
        repositories: Arc<RdbChanRepositoryModule>,
        descriptor_cache: Arc<MokaCacheImpl<Arc<String>, RunnerDataWithDescriptor>>,
    ) -> Self {
        Self {
            plugin_dir,
            storage_config,
            async_cache: Arc::new(MokaCacheImpl::new(memory_cache_config)),
            descriptor_cache,
            repositories,
            cache_ttl: Some(Duration::from_secs(60)), // TODO from setting
        }
    }
    // load runners from db in initialization
    async fn load_runners_from_db(&self) {
        if let Ok(runners) = self.runner_repository().find_list(true, None, None).await {
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
                        .load_plugin(Some(data.name.as_str()), &data.definition, false)
                        .await
                        .map(|_| ()),
                    _ => Ok(()), // skip other types
                } {
                    if let Some(JobWorkerError::AlreadyExists(_)) =
                        e.downcast_ref::<JobWorkerError>()
                    {
                        tracing::debug!("load runner is already exists: {:?}", e);
                    } else {
                        tracing::warn!("load runner error: {:?}", e);
                    }
                }
            }
        } else {
            tracing::warn!("load runners from db error");
        }
    }

    async fn add_runner(&self) -> Result<()> {
        self.runner_repository()
            .add_from_plugins_from(self.plugin_dir.as_str())
            .await?;
        self.runner_repository().add_from_mcp_config_file().await?;
        self.load_runners_from_db().await;
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key(true))
            .await;
        Ok(())
    }
}

impl UseRunnerParserWithCache for RdbRunnerAppImpl {
    fn descriptor_cache(&self) -> &MokaCacheImpl<Arc<String>, RunnerDataWithDescriptor> {
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
        let id = match runner_type {
            val if val == RunnerType::McpServer as i32 => {
                self.runner_repository()
                    .create_mcp_server(name, description, definition)
                    .await
            }
            val if val == RunnerType::Plugin as i32 => {
                self.runner_repository()
                    .create_plugin(name, description, definition)
                    .await
            }
            _ => Err(JobWorkerError::InvalidParameter(format!(
                "invalid runner type: {runner_type}"
            ))
            .into()),
        }?;
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key(true))
            .await;
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key(false))
            .await;
        Ok(id)
    }

    async fn delete_runner(&self, id: &RunnerId) -> Result<bool> {
        let res = self.runner_repository().remove(id).await?;
        // clear memory cache
        let _ = self
            .delete_cache_locked(&Self::find_cache_key(&id.value))
            .await;
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key(true))
            .await;
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key(false))
            .await;
        Ok(res)
    }

    async fn find_runner(&self, id: &RunnerId) -> Result<Option<RunnerWithSchema>>
    where
        Self: Send + 'static,
    {
        let k = Self::find_cache_key(&id.value);
        self.with_cache(&k, || async {
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

    async fn find_runner_by_name(&self, name: &str) -> Result<Option<RunnerWithSchema>>
    where
        Self: Send + 'static,
    {
        let k = Self::find_name_cache_key(name);
        self.with_cache(&k, || async {
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
        include_full: bool,
        limit: Option<&i32>,
        offset: Option<&i64>,
    ) -> Result<Vec<RunnerWithSchema>>
    where
        Self: Send + 'static,
    {
        if let Some(all) = self
            .find_cache(&Self::find_list_cache_key(include_full, limit, offset))
            .await
        {
            if let Some(lim) = limit {
                let offset = offset.map(|o| *o as usize).unwrap_or(0);
                Ok(all.into_iter().skip(offset).take(*lim as usize).collect())
            } else {
                Ok(all)
            }
        } else {
            self.runner_repository()
                .find_list(include_full, limit, offset)
                .await
        }
    }

    async fn find_runner_all_list(&self, include_full: bool) -> Result<Vec<RunnerWithSchema>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_all_list_cache_key(include_full));
        self.with_cache(&k, || async {
            self.runner_repository()
                .find_list(include_full, None, None)
                .await
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

    /// Find runners with filtering and sorting (Admin UI)
    #[allow(clippy::too_many_arguments)]
    async fn find_runner_list_by(
        &self,
        runner_types: Vec<i32>,
        name_filter: Option<String>,
        limit: Option<i32>,
        offset: Option<i64>,
        sort_by: Option<proto::jobworkerp::data::RunnerSortField>,
        ascending: Option<bool>,
    ) -> Result<Vec<RunnerWithSchema>>
    where
        Self: Send + 'static,
    {
        // No cache for filtered results (too many combinations)
        self.runner_repository()
            .find_list_by(runner_types, name_filter, limit, offset, sort_by, ascending)
            .await
    }

    /// Count runners with filtering (Admin UI)
    async fn count_by(&self, runner_types: Vec<i32>, name_filter: Option<String>) -> Result<i64>
    where
        Self: Send + 'static,
    {
        // No cache for filtered results
        self.runner_repository()
            .count_by(runner_types, name_filter)
            .await
    }

    /// Refresh MCP runner's sub_method_protos by re-fetching tools from MCP server
    async fn refresh_mcp_runner(
        &self,
        runner_id: Option<&RunnerId>,
    ) -> Result<(Vec<String>, Vec<(String, String)>)>
    where
        Self: Send + 'static,
    {
        let mut updated = Vec::new();
        let mut failures = Vec::new();

        // Get the runners to refresh
        let runners = if let Some(id) = runner_id {
            // Single runner
            match self.runner_repository().find(id).await? {
                Some(runner) => vec![runner],
                None => {
                    return Err(JobWorkerError::NotFound(format!(
                        "Runner with id {} not found",
                        id.value
                    ))
                    .into());
                }
            }
        } else {
            // All MCP runners
            self.runner_repository()
                .find_list_by(
                    vec![RunnerType::McpServer as i32],
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .await?
        };

        for runner in runners {
            let Some(data) = &runner.data else {
                continue;
            };

            // Only refresh MCP runners
            if data.runner_type != RunnerType::McpServer as i32 {
                failures.push((data.name.clone(), "Not an MCP runner".to_string()));
                continue;
            }

            let runner_name = data.name.clone();

            // Re-load the MCP server to refresh tools
            match self
                .runner_repository()
                .load_mcp_server(&data.name, &data.description, &data.definition)
                .await
            {
                Ok(_) => {
                    updated.push(runner_name.clone());
                    // Clear cache for this runner
                    if let Some(id) = &runner.id {
                        let _ = self
                            .delete_cache_locked(&Self::find_cache_key(&id.value))
                            .await;
                    }
                    tracing::info!("Refreshed MCP runner: {}", runner_name);
                }
                Err(e) => {
                    let error_msg = format!("{}", e);
                    tracing::warn!(
                        "Failed to refresh MCP runner {}: {}",
                        runner_name,
                        error_msg
                    );
                    failures.push((runner_name, error_msg));
                }
            }
        }

        // Clear list caches if any runners were updated
        if !updated.is_empty() {
            let _ = self
                .delete_cache_locked(&Self::find_all_list_cache_key(true))
                .await;
            let _ = self
                .delete_cache_locked(&Self::find_all_list_cache_key(false))
                .await;
        }

        Ok((updated, failures))
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
            .create(&RunnerRow {
                id: runner_id.value,
                name: name.to_string(),
                description: runner_data.runner_data.description.clone(),
                definition: format!("./target/debug/lib{name}.so"),
                r#type: RunnerType::Plugin as i32,
                created_at: command_utils::util::datetime::now_millis(),
            })
            .await?;
        self.store_proto_cache(runner_id, &runner_data).await;
        // clear memory cache
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key(false))
            .await;
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key(true))
            .await;
        Ok(runner_data)
    }
}

impl UseRdbRunnerRepository for RdbRunnerAppImpl {
    fn runner_repository(&self) -> &RdbRunnerRepositoryImpl {
        &self.repositories.runner_repository
    }
}

impl UseMokaCache<Arc<String>, Vec<RunnerWithSchema>> for RdbRunnerAppImpl {
    fn cache(&self) -> &Cache<Arc<String>, Vec<RunnerWithSchema>> {
        self.async_cache.cache()
    }
}
impl RunnerCacheHelper for RdbRunnerAppImpl {}
