use crate::app::function::{FunctionApp, UseFunctionApp};
use crate::app::function::{UseRunnerApp, UseWorkerApp, helper::McpNameConverter};
use anyhow::Result;
use async_trait::async_trait;
use core::fmt;
use infra::infra::function_set::rdb::{
    FunctionSetRepository, FunctionSetRepositoryImpl, UseFunctionSetRepository,
};
use infra::infra::runner::rows::RunnerWithSchema;
use infra_utils::infra::rdb::UseRdbPool;
use jobworkerp_base::error::JobWorkerError;
use memory_utils::cache::moka::{MokaCache, MokaCacheImpl, UseMokaCache};
use proto::DEFAULT_METHOD_NAME;
use proto::jobworkerp::data::Worker;
use proto::jobworkerp::function::data::{
    FunctionSet, FunctionSetData, FunctionSetId, FunctionSpecs,
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

// Import for find_functions_by_set
use super::FunctionAppImpl;

/// The concrete target selected by one tool name in a FunctionSet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FunctionScopeTarget {
    Runner {
        runner_id: i64,
        runner_type: i32,
        using: String,
    },
    Worker {
        worker_id: i64,
        runner_id: i64,
        runner_type: i32,
        using: String,
    },
}

/// A current, authoritative view of one FunctionSet and its exposed tools.
///
/// The scope is a per-request snapshot. Callers must pass this same value when
/// executing tools selected from the snapshot so that execution can reject a
/// same-request retarget.
#[derive(Clone, Debug)]
pub struct FunctionSetScope {
    pub set_id: i64,
    pub set_name: String,
    pub functions: Vec<FunctionSpecs>,
    pub tools: HashMap<String, FunctionScopeTarget>,
    pub worker_snapshots: HashMap<String, Worker>,
}

/// A target revalidated immediately before dispatch, with the worker data that
/// must be used if the target is a Worker.
#[derive(Clone, Debug)]
pub struct ValidatedFunctionScopeTarget {
    pub target: FunctionScopeTarget,
    pub worker_snapshot: Option<Worker>,
}

#[async_trait]
pub trait FunctionSetApp: // XXX 1 impl
    UseFunctionSetRepository
    + UseMokaCache<Arc<String>, FunctionSet>
    + UseFunctionApp
    + fmt::Debug
    + Send
    + Sync
    + 'static
{
    async fn create_function_set(&self, function_set: &FunctionSetData) -> Result<FunctionSetId> {
        // transaction example
        let db = self.function_set_repository().db_pool();
        let mut tx = db.begin().await.map_err(JobWorkerError::DBError)?;
        let id = self
            .function_set_repository()
            .create(&mut tx, function_set)
            .await?;
        tx.commit().await.map_err(JobWorkerError::DBError)?;
        Ok(id)
    }

    // only cache single instance
    async fn update_function_set(
        &self,
        id: &FunctionSetId,
        function_set: &Option<FunctionSetData>,
    ) -> Result<bool> {
        if let Some(w) = function_set {
            let Some(old_name) = self.find_function_set_name_by_id(id).await? else {
                return Ok(false);
            };
            let pool = self.function_set_repository().db_pool();
            let mut tx = pool.begin().await.map_err(JobWorkerError::DBError)?;
            let updated = self
                .function_set_repository()
                .update(&mut tx, id, w)
                .await?;
            tx.commit().await.map_err(JobWorkerError::DBError)?;
            self.delete_function_set_caches(
                id,
                Some(old_name.as_str()),
                Some(w.name.as_str()),
            )
            .await;
            Ok(updated)
        } else {
            // all empty, no update
            Ok(false)
        }
    }

    async fn delete_function_set(&self, id: &FunctionSetId) -> Result<bool> {
        let old_name = self.find_function_set_name_by_id(id).await?;
        let deleted = self.function_set_repository().delete(id).await?;
        self.delete_function_set_caches(id, old_name.as_deref(), None)
            .await;
        Ok(deleted)
    }

    async fn find_function_set_name_by_id(&self, id: &FunctionSetId) -> Result<Option<String>> {
        let Some(function_set) = self.function_set_repository().find(id).await? else {
            return Ok(None);
        };
        let name = function_set
            .data
            .ok_or_else(|| {
                JobWorkerError::NotFound(format!(
                    "FunctionSet {} data not found",
                    id.value
                ))
            })?
            .name;
        Ok(Some(name))
    }

    async fn delete_function_set_caches(
        &self,
        id: &FunctionSetId,
        old_name: Option<&str>,
        new_name: Option<&str>,
    ) {
        let id_key = Arc::new(self.find_cache_key(&id.value));
        let _ = self.delete_cache(&id_key).await;
        for name in [old_name, new_name].into_iter().flatten() {
            let name_key = Arc::new(self.find_by_name_cache_key(name));
            let _ = self.delete_cache(&name_key).await;
        }
    }

    fn find_cache_key(&self, id: &i64) -> String {
        ["function_set_id:", &id.to_string()].join("")
    }

    fn find_by_name_cache_key(&self, name: &str) -> String {
        ["function_set_id_name:", name].join("")
    }

    async fn find_function_set(
        &self,
        id: &FunctionSetId,
    ) -> Result<Option<FunctionSet>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(self.find_cache_key(&id.value));
        self.with_cache_if_some(&k, || async {
            self.function_set_repository().find(id).await
        })
        .await
    }

    async fn find_function_set_by_name(
        &self,
        name: &str,
    ) -> Result<Option<FunctionSet>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(self.find_by_name_cache_key(name));
        self.with_cache_if_some(&k, || async {
            self.function_set_repository().find_by_name(name).await
        })
        .await
    }

    /// Resolve by name directly from the repository, bypassing the name cache.
    /// This is used by skills-aware execution, where a stale cache must not
    /// preserve a deleted or retargeted FunctionSet.
    async fn find_function_set_by_name_authoritative(
        &self,
        name: &str,
    ) -> Result<Option<FunctionSet>> {
        self.function_set_repository().find_by_name(name).await
    }

    /// Read the current FunctionSet from storage, expose its functions, and
    /// reject names that resolve to any other Runner/Worker target.
    async fn resolve_current_function_scope(&self, set_name: &str) -> Result<FunctionSetScope> {
        let function_set = self
            .find_function_set_by_name_authoritative(set_name)
            .await?
            .ok_or_else(|| {
                JobWorkerError::NotFound(format!("FunctionSet '{set_name}' not found"))
            })?;
        let set_id = function_set
            .id
            .ok_or_else(|| JobWorkerError::NotFound("FunctionSet id not found".to_string()))?
            .value;
        let data = function_set.data.ok_or_else(|| {
            JobWorkerError::NotFound(format!("FunctionSet '{set_name}' data not found"))
        })?;
        let current_workers = self.find_current_workers().await?;
        let current_runners = self.find_current_runners().await?;
        let converted_functions = data
            .targets
            .iter()
            .map(|target| {
                self.convert_current_function_using(
                    target,
                    &current_workers,
                    &current_runners,
                    set_name,
                )
            })
            .collect::<Result<Vec<_>>>()?;

        let mut functions = Vec::new();
        let mut tools = HashMap::new();
        let mut worker_snapshots = HashMap::new();
        for (function, worker_snapshot) in converted_functions {
            let runner_id = function.runner_id.as_ref().ok_or_else(|| {
                JobWorkerError::InvalidParameter(format!(
                    "FunctionSet '{set_name}' contains a function without its Runner ID"
                ))
            })?;
            let target_id = if let Some(worker_id) = &function.worker_id {
                (true, worker_id.value, runner_id.value)
            } else {
                (false, runner_id.value, runner_id.value)
            };
            let runner_type = function.runner_type;

            let add_tool = |tool_name: String,
                            using: String,
                            tools: &mut HashMap<String, FunctionScopeTarget>,
                            worker_snapshots: &mut HashMap<String, Worker>| ->
             Result<bool, JobWorkerError> {
                let target = if target_id.0 {
                    FunctionScopeTarget::Worker {
                        worker_id: target_id.1,
                        runner_id: target_id.2,
                        runner_type,
                        using,
                    }
                } else {
                    FunctionScopeTarget::Runner {
                        runner_id: target_id.1,
                        runner_type,
                        using,
                    }
                };
                if let Some(existing) = tools.get(&tool_name) {
                    if existing != &target {
                        return Err(JobWorkerError::InvalidParameter(format!(
                            "FunctionSet '{set_name}' has an ambiguous tool name '{tool_name}'"
                        )));
                    }
                    Ok(false)
                } else {
                    if let Some(worker) = &worker_snapshot {
                        worker_snapshots.insert(tool_name.clone(), worker.clone());
                    }
                    tools.insert(tool_name, target);
                    Ok(true)
                }
            };

            let mut exposed_function = function;
            if let Some(methods) = &mut exposed_function.methods {
                let mut exposed_methods = HashSet::new();
                for method_name in methods.schemas.keys() {
                    let tool_name = if method_name == DEFAULT_METHOD_NAME {
                        exposed_function.name.clone()
                    } else {
                        <FunctionAppImpl as McpNameConverter>::combine_names(
                            &exposed_function.name,
                            method_name,
                        )
                    };
                    if add_tool(
                        tool_name.clone(),
                        method_name.clone(),
                        &mut tools,
                        &mut worker_snapshots,
                    )? {
                        exposed_methods.insert(method_name.clone());
                    }
                }
                methods
                    .schemas
                    .retain(|method_name, _| exposed_methods.contains(method_name));
                if exposed_methods.is_empty() {
                    continue;
                }
            } else {
                if !add_tool(
                    exposed_function.name.clone(),
                    DEFAULT_METHOD_NAME.to_string(),
                    &mut tools,
                    &mut worker_snapshots,
                )? {
                    continue;
                }
            }
            functions.push(exposed_function);
        }

        for (tool_name, target) in &tools {
            let resolutions = self.resolve_function_name_targets(
                tool_name,
                &current_workers,
                &current_runners,
            )?;
            if resolutions.len() != 1 || resolutions.first() != Some(target) {
                return Err(JobWorkerError::InvalidParameter(format!(
                    "FunctionSet tool name '{tool_name}' is not uniquely resolvable"
                ))
                .into());
            }
        }

        Ok(FunctionSetScope {
            set_id,
            set_name: data.name,
            functions,
            tools,
            worker_snapshots,
        })
    }

    /// Re-resolve the selected public name from the current FunctionSet.
    /// A stale per-request scope is rejected if the name now selects a
    /// different ID, target kind, or method.
    async fn validate_function_scope_tool(
        &self,
        scope: &FunctionSetScope,
        tool_name: &str,
    ) -> Result<FunctionScopeTarget> {
        Ok(self
            .validate_function_scope_target_for_dispatch(scope, tool_name)
            .await?
            .target)
    }

    async fn validate_function_scope_target_for_dispatch(
        &self,
        scope: &FunctionSetScope,
        tool_name: &str,
    ) -> Result<ValidatedFunctionScopeTarget> {
        let expected = scope.tools.get(tool_name).ok_or_else(|| {
            JobWorkerError::InvalidParameter(format!(
                "Tool '{tool_name}' was not exposed by FunctionSet '{}'",
                scope.set_name
            ))
        })?;
        let current = self.resolve_current_function_scope(&scope.set_name).await?;
        let actual = current.tools.get(tool_name).ok_or_else(|| {
            JobWorkerError::InvalidParameter(format!(
                "Tool '{tool_name}' is no longer exposed by FunctionSet '{}'",
                scope.set_name
            ))
        })?;
        if current.set_id != scope.set_id || actual != expected {
            return Err(JobWorkerError::InvalidParameter(format!(
                "Tool '{tool_name}' changed after FunctionSet '{}' was listed",
                scope.set_name
            ))
            .into());
        }
        let worker_snapshot = current.worker_snapshots.get(tool_name).cloned();
        if matches!(actual, FunctionScopeTarget::Worker { .. }) {
            let listed_snapshot = scope.worker_snapshots.get(tool_name).ok_or_else(|| {
                JobWorkerError::NotFound(format!(
                    "Listed worker snapshot for FunctionSet tool '{tool_name}' is unavailable"
                ))
            })?;
            if worker_snapshot.as_ref() != Some(listed_snapshot) {
                return Err(JobWorkerError::InvalidParameter(format!(
                    "Worker target for FunctionSet tool '{tool_name}' changed after listing"
                ))
                .into());
            }
        }
        Ok(ValidatedFunctionScopeTarget {
            target: actual.clone(),
            worker_snapshot,
        })
    }

    /// Execute a selected tool after fresh scope validation, using its ID and
    /// method rather than resolving its public name again.
    async fn call_function_for_llm_in_scope(
        &self,
        meta: Arc<HashMap<String, String>>,
        scope: &FunctionSetScope,
        tool_name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
        timeout_sec: u32,
    ) -> Result<serde_json::Value> {
        let validated = self
            .validate_function_scope_target_for_dispatch(scope, tool_name)
            .await?;
        self.function_app()
            .call_function_for_llm_target(
                meta,
                &validated.target,
                validated.worker_snapshot,
                arguments,
                timeout_sec,
            )
            .await
    }

    /// Enqueue a selected tool after fresh scope validation. Streaming-capable
    /// targets retain the existing internal-stream/listener behavior.
    async fn enqueue_function_for_llm_in_scope(
        &self,
        meta: Arc<HashMap<String, String>>,
        scope: &FunctionSetScope,
        tool_name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
        timeout_sec: u32,
    ) -> Result<super::EnqueuedFunction> {
        let validated = self
            .validate_function_scope_target_for_dispatch(scope, tool_name)
            .await?;
        self.function_app()
            .enqueue_function_for_llm_target(
                meta,
                &validated.target,
                validated.worker_snapshot,
                arguments,
                timeout_sec,
            )
            .await
    }

    async fn find_current_runners(&self) -> Result<Vec<RunnerWithSchema>> {
        self.function_app()
            .runner_app()
            .find_runner_list_by(vec![], None, Some(i32::MAX), Some(0), None, None)
            .await
    }

    async fn find_current_workers(&self) -> Result<Vec<Worker>> {
        self.function_app().worker_app().find_current_list().await
    }

    fn convert_current_function_using(
        &self,
        function_using: &proto::jobworkerp::function::data::FunctionUsing,
        workers: &[Worker],
        runners: &[RunnerWithSchema],
        context: &str,
    ) -> Result<(FunctionSpecs, Option<Worker>)> {
        use proto::jobworkerp::function::data::function_id;
        use crate::app::function::converter::FunctionSpecConverter;

        let id = function_using
            .function_id
            .as_ref()
            .and_then(|function_id| function_id.id.as_ref())
            .ok_or_else(|| {
                JobWorkerError::InvalidParameter(format!(
                    "FunctionSet '{context}' contains a target without an ID"
                ))
            })?;

        match id {
            function_id::Id::RunnerId(runner_id) => {
                let runner = runners
                    .iter()
                    .find(|runner| runner.id == Some(*runner_id))
                    .cloned()
                    .ok_or_else(|| {
                        JobWorkerError::NotFound(format!(
                            "Runner {} from FunctionSet '{context}' no longer exists",
                            runner_id.value
                        ))
                    })?;
                let function = if let Some(using) = &function_using.using {
                    FunctionAppImpl::convert_runner_using_to_function_specs(runner, using)?
                } else {
                    FunctionAppImpl::convert_runner_to_function_specs(runner)
                };
                Ok((function, None))
            }
            function_id::Id::WorkerId(worker_id) => {
                let worker = workers
                    .iter()
                    .find(|worker| worker.id == Some(*worker_id))
                    .cloned()
                    .ok_or_else(|| {
                        JobWorkerError::NotFound(format!(
                            "Worker {} from FunctionSet '{context}' no longer exists",
                            worker_id.value
                        ))
                    })?;
                let worker_data = worker.data.clone().ok_or_else(|| {
                    JobWorkerError::NotFound(format!(
                        "Worker {} from FunctionSet '{context}' has no data",
                        worker_id.value
                    ))
                })?;
                let runner_id = worker_data.runner_id.ok_or_else(|| {
                    JobWorkerError::InvalidParameter(format!(
                        "Worker {} from FunctionSet '{context}' has no Runner ID",
                        worker_id.value
                    ))
                })?;
                let runner = runners
                    .iter()
                    .find(|runner| runner.id == Some(runner_id))
                    .cloned()
                    .ok_or_else(|| {
                        JobWorkerError::NotFound(format!(
                            "Runner {} for Worker {} from FunctionSet '{context}' no longer exists",
                            runner_id.value, worker_id.value
                        ))
                    })?;
                let function = if let Some(using) = &function_using.using {
                    FunctionAppImpl::convert_worker_using_to_function_specs(
                        *worker_id,
                        worker_data,
                        runner,
                        using,
                    )?
                } else {
                    FunctionAppImpl::convert_worker_to_function_specs(
                        *worker_id,
                        worker_data,
                        runner,
                    )?
                };
                Ok((function, Some(worker)))
            }
        }
    }

    fn resolve_function_name_targets(
        &self,
        name: &str,
        workers: &[Worker],
        runners: &[RunnerWithSchema],
    ) -> Result<Vec<FunctionScopeTarget>> {
        let mut targets = Vec::new();

        if let Some(runner) = runner_named(runners, name) {
            let (runner_id, runner_data) = current_runner_identity(runner, name)?;
            push_distinct_target(
                &mut targets,
                FunctionScopeTarget::Runner {
                    runner_id,
                    runner_type: runner_data,
                    using: DEFAULT_METHOD_NAME.to_string(),
                },
            );
        }

        if let Some(worker) = worker_named(workers, name) {
            let worker_target =
                self.worker_target_by_runner(worker, runners, DEFAULT_METHOD_NAME)?;
            push_distinct_target(&mut targets, worker_target);
        }

        if let Some((base_name, using)) = <FunctionAppImpl as McpNameConverter>::divide_names(name)
        {
            if let Some(runner) = runner_named(runners, &base_name) {
                let (runner_id, runner_type) = current_runner_identity(runner, &base_name)?;
                push_distinct_target(
                    &mut targets,
                    FunctionScopeTarget::Runner {
                        runner_id,
                        runner_type,
                        using: using.clone(),
                    },
                );
            }

            if let Some(worker) = worker_named(workers, &base_name) {
                let worker_target = self.worker_target_by_runner(worker, runners, &using)?;
                push_distinct_target(&mut targets, worker_target);
            }
        }

        Ok(targets)
    }

    fn worker_target_by_runner(
        &self,
        worker: &Worker,
        runners: &[RunnerWithSchema],
        using: &str,
    ) -> Result<FunctionScopeTarget> {
        let worker_id = worker.id.ok_or_else(|| {
            JobWorkerError::InvalidParameter("Worker name resolves without an ID".to_string())
        })?;
        let worker_data = worker.data.as_ref().ok_or_else(|| {
            JobWorkerError::InvalidParameter(format!(
                "Worker {} resolves without data",
                worker_id.value
            ))
        })?;
        let runner_id = worker_data.runner_id.as_ref().ok_or_else(|| {
            JobWorkerError::InvalidParameter(format!(
                "Worker {} resolves without a Runner ID",
                worker_id.value
            ))
        })?;
        let runner = runners
            .iter()
            .find(|runner| runner.id.as_ref() == Some(runner_id))
            .ok_or_else(|| {
                JobWorkerError::InvalidParameter(format!(
                    "Worker {} resolves to a missing Runner {}",
                    worker_id.value, runner_id.value
                ))
            })?;
        let runner_data = runner.data.as_ref().ok_or_else(|| {
            JobWorkerError::InvalidParameter(format!(
                "Worker {} resolves to a Runner without data",
                worker_id.value
            ))
        })?;
        Ok(FunctionScopeTarget::Worker {
            worker_id: worker_id.value,
            runner_id: runner_id.value,
            runner_type: runner_data.runner_type,
            using: using.to_string(),
        })
    }

    async fn find_function_set_list(
        &self,
        limit: Option<&i32>,
        offset: Option<&i64>,
        _ttl: Option<&Duration>,
    ) -> Result<Vec<FunctionSet>>
    where
        Self: Send + 'static,
    {
        // TODO list cache
        self.function_set_repository()
            .find_list(limit, offset)
            .await
    }

    async fn find_function_set_all_list(&self, _ttl: Option<&Duration>) -> Result<Vec<FunctionSet>>
    where
        Self: Send + 'static,
    {
        // TODO list cache
        self.function_set_repository().find_list(None, None).await
    }

    async fn count(&self) -> Result<i64>
    where
        Self: Send + 'static,
    {
        // TODO cache
        self.function_set_repository()
            .count_list_tx(self.function_set_repository().db_pool())
            .await
    }

    async fn find_functions_by_set(&self, set_name: &str) -> Result<Vec<FunctionSpecs>> {
        let function_set = self.find_function_set_by_name(set_name).await?;

        if let Some(set) = function_set {
            if let Some(data) = set.data {
                self.function_app()
                    .convert_function_usings_to_specs(&data.targets, set_name)
                    .await
            } else {
                Ok(Vec::new())
            }
        } else {
            Ok(Vec::new())
        }
    }

}

#[derive(Debug)]
pub struct FunctionSetAppImpl {
    function_set_repository: Arc<FunctionSetRepositoryImpl>,
    memory_cache: MokaCacheImpl<Arc<String>, FunctionSet>,
    function_app: Arc<FunctionAppImpl>,
}

impl FunctionSetAppImpl {
    pub fn new(
        function_set_repository: Arc<FunctionSetRepositoryImpl>,
        mc_config: &memory_utils::cache::moka::MokaCacheConfig,
        function_app: Arc<FunctionAppImpl>,
    ) -> Self {
        let memory_cache = MokaCacheImpl::new(mc_config);
        Self {
            function_set_repository,
            memory_cache,
            function_app,
        }
    }
}

impl UseFunctionSetRepository for FunctionSetAppImpl {
    fn function_set_repository(&self) -> &FunctionSetRepositoryImpl {
        &self.function_set_repository
    }
}

impl UseFunctionApp for FunctionSetAppImpl {
    fn function_app(&self) -> &FunctionAppImpl {
        &self.function_app
    }
}

impl FunctionSetApp for FunctionSetAppImpl {}

impl UseMokaCache<Arc<String>, FunctionSet> for FunctionSetAppImpl {
    fn cache(&self) -> &MokaCache<Arc<String>, FunctionSet> {
        self.memory_cache.cache()
    }
}

pub trait UseFunctionSetApp {
    fn function_set_app(&self) -> &FunctionSetAppImpl;
}

fn push_distinct_target(targets: &mut Vec<FunctionScopeTarget>, target: FunctionScopeTarget) {
    if !targets.contains(&target) {
        targets.push(target);
    }
}

fn runner_named<'a>(runners: &'a [RunnerWithSchema], name: &str) -> Option<&'a RunnerWithSchema> {
    runners
        .iter()
        .find(|runner| runner.data.as_ref().is_some_and(|data| data.name == name))
}

fn worker_named<'a>(workers: &'a [Worker], name: &str) -> Option<&'a Worker> {
    workers
        .iter()
        .find(|worker| worker.data.as_ref().is_some_and(|data| data.name == name))
}

fn current_runner_identity(runner: &RunnerWithSchema, name: &str) -> Result<(i64, i32)> {
    let runner_id = runner.id.ok_or_else(|| {
        JobWorkerError::InvalidParameter(format!("Runner name '{name}' resolves without an ID"))
    })?;
    let runner_type = runner
        .data
        .as_ref()
        .ok_or_else(|| {
            JobWorkerError::InvalidParameter(format!("Runner name '{name}' resolves without data"))
        })?
        .runner_type;
    Ok((runner_id.value, runner_type))
}
