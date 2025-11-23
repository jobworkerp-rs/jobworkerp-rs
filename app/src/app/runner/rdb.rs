use super::{RunnerApp, RunnerCacheHelper, RunnerDataWithDescriptor, UseRunnerParserWithCache};
use crate::app::StorageConfig;
use anyhow::Result;
use async_trait::async_trait;
use debug_stub_derive::DebugStub;
use infra::infra::function_set::rdb::FunctionSetRepository;
use infra::infra::module::rdb::RdbChanRepositoryModule;
use infra::infra::runner::rdb::McpServerRegistrationResult;
use infra::infra::runner::rdb::RunnerRepository;
use infra::infra::runner::rdb::{RdbRunnerRepositoryImpl, UseRdbRunnerRepository};
use infra::infra::runner::rows::RunnerWithSchema;
use infra_utils::infra::rdb::UseRdbPool;
use jobworkerp_base::error::JobWorkerError;
use memory_utils::cache::moka::{MokaCacheConfig, MokaCacheImpl, UseMokaCache};
use moka::future::Cache;
use proto::jobworkerp::data::{RunnerId, RunnerType};
use proto::jobworkerp::function::data::{FunctionId, FunctionSetData};
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

    async fn add_runner(&self) -> Result<Vec<McpServerRegistrationResult>> {
        self.runner_repository()
            .add_from_plugins_from(self.plugin_dir.as_str())
            .await?;

        let registration_results = self.runner_repository().add_from_mcp_config_file().await?;

        self.load_runners_from_db().await;
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key(true))
            .await;

        Ok(registration_results)
    }

    /// Create FunctionSet for a registered MCP server
    ///
    /// Directly uses FunctionSetRepository instead of FunctionSetApp to avoid
    /// unnecessary layer indirection. Transaction management follows the same
    /// pattern as FunctionSetApp::create_function_set().
    ///
    /// # Arguments
    /// - `result`: Registration result from RunnerRepository
    ///
    /// # Returns
    /// - Ok(()): FunctionSet created or updated successfully
    /// - Err: FunctionSet creation failed (does NOT affect tool registration)
    async fn create_mcp_function_set(&self, result: &McpServerRegistrationResult) -> Result<()> {
        let function_set_data = FunctionSetData {
            name: result.name.clone(),
            description: result.description.clone().unwrap_or_else(|| {
                format!(
                    "MCP server '{}' providing {} tools",
                    result.name, result.tool_count
                )
            }),
            category: 2, // FunctionSetCategory::FUNCTION_SET_CATEGORY_MCP_SERVER
            targets: result
                .runner_ids
                .iter()
                .map(|id| FunctionId {
                    id: Some(
                        proto::jobworkerp::function::data::function_id::Id::RunnerId(RunnerId {
                            value: *id,
                        }),
                    ),
                })
                .collect(),
        };

        // FunctionSetRepositoryに直接アクセス
        let repo = &self.repositories.function_set_repository;

        // トランザクション管理（FunctionSetApp::create_function_set()と同じパターン）
        let db = repo.db_pool();
        let mut tx = db.begin().await?;

        // find_by_name で既存確認（トランザクション外で実行）
        let existing = repo.find_by_name(&result.name).await?;

        match existing {
            Some(existing_set) => {
                // 既存あり → update
                repo.update(
                    &mut tx,
                    &existing_set
                        .id
                        .ok_or_else(|| anyhow::anyhow!("FunctionSet has no ID"))?,
                    &function_set_data,
                )
                .await?;
                tx.commit().await?;

                tracing::info!(
                    "Updated FunctionSet for MCP server '{}' with {} tools",
                    result.name,
                    result.tool_count
                );
            }
            None => {
                // 新規 → create
                repo.create(&mut tx, &function_set_data).await?;
                tx.commit().await?;

                tracing::info!(
                    "Created FunctionSet for MCP server '{}' with {} tools",
                    result.name,
                    result.tool_count
                );
            }
        }

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
        let registration_results = self.add_runner().await?;

        // FunctionSet作成
        for result in registration_results {
            if let Err(e) = self.create_mcp_function_set(&result).await {
                tracing::warn!(
                    "Failed to create FunctionSet for '{}': {}. Tools are usable.",
                    result.name,
                    e
                );
            }
        }

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
