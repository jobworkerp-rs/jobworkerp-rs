use super::run::RunTaskExecutor;
use super::TaskExecutorTrait;
use crate::simple_workflow::definition::workflow::{
    self, RetryLimit, RetryLimitAttempt, RunTaskConfiguration,
};
use crate::simple_workflow::definition::workflow::{FunctionOptions, RunTask};
use crate::simple_workflow::definition::UseLoadUrlOrPath;
use crate::simple_workflow::execute::context::{TaskContext, WorkflowContext};
use crate::simple_workflow::execute::job::JobExecutorWrapper;
use anyhow::{anyhow, Result};
use infra_utils::infra::lock::RwLockWithKey;
use infra_utils::infra::memory::{MemoryCacheConfig, UseMemoryCache};
use infra_utils::infra::net::reqwest;
use serde_json::Map;
use std::sync::Arc;
use std::time::Duration;
use stretto::AsyncCache;
use tokio::sync::RwLock;

pub struct CallTaskExecutor<'a> {
    task: &'a workflow::CallTask,
    http_client: reqwest::ReqwestClient,
    job_executor_wrapper: Arc<JobExecutorWrapper>,
    memory_cache: infra_utils::infra::memory::MemoryCacheImpl<String, workflow::RunTask>,
}
impl<'a> CallTaskExecutor<'a> {
    const TIMEOUT_SEC: u64 = 300; // 5 minutes
    const FUNCTION_OPTIONS_METADATA_LABEL: &'static str = "options";
    const RUNNER_SETTINGS_METADATA_LABEL: &'static str = "settings";
    pub fn new(
        task: &'a workflow::CallTask,
        http_client: reqwest::ReqwestClient,
        job_executor_wrapper: Arc<JobExecutorWrapper>,
    ) -> Self {
        Self {
            task,
            http_client,
            job_executor_wrapper,
            memory_cache: infra_utils::infra::memory::MemoryCacheImpl::new(
                &MemoryCacheConfig::default(),
                Some(Duration::from_secs(Self::TIMEOUT_SEC)),
            ),
        }
    }
    // Apply metadata json values to FunctionOptions
    fn apply_options_from_json(
        options: &mut FunctionOptions,
        options_map: &serde_json::Map<String, serde_json::Value>,
    ) {
        if let Some(serde_json::Value::Bool(broadcast)) =
            options_map.get("broadcastResultsToListener")
        {
            options.broadcast_results_to_listener = Some(*broadcast);
        }

        if let Some(serde_json::Value::String(channel)) = options_map.get("channel") {
            options.channel = Some(channel.clone());
        }

        if let Some(serde_json::Value::Object(retry_options)) = options_map.get("retryPolicy") {
            // Clone existing retry_policy if present or create new one
            let mut retry = options.retry.clone().unwrap_or_default();

            // Update retry policy fields from metadata
            if let Some(serde_json::Value::String(interval)) = retry_options.get("backoff") {
                match interval.as_str() {
                    "exponential" => {
                        retry.backoff = Some(workflow::RetryBackoff::Exponential(Map::default()));
                    }
                    "linear" => {
                        retry.backoff = Some(workflow::RetryBackoff::Linear(Map::default()));
                    }
                    "constant" => {
                        retry.backoff = Some(workflow::RetryBackoff::Constant(Map::default()));
                    }
                    _ => {
                        retry.backoff = None;
                    }
                };
            }

            let mut retry_attempt = RetryLimitAttempt::default();
            if let Some(serde_json::Value::Number(max_count)) = retry_options.get("maxCount") {
                if let Some(count) = max_count.as_i64() {
                    retry_attempt.count = Some(count);
                }
            }
            if let Some(serde_json::Value::Number(max_interval)) = retry_options.get("maxInterval")
            {
                if let Some(interval) = max_interval.as_i64() {
                    retry_attempt.duration = Some(workflow::Duration::from_millis(interval as u64));
                }
            }
            if retry_attempt != RetryLimitAttempt::default() {
                retry.limit = Some(RetryLimit {
                    attempt: Some(retry_attempt),
                });
            }

            if let Some(serde_json::Value::Number(delay)) = retry_options.get("delay") {
                retry.delay = delay.as_u64().map(workflow::Duration::from_millis);
            }

            options.retry = Some(retry);
        }

        if let Some(serde_json::Value::Bool(store_failure)) = options_map.get("storeFailure") {
            options.store_failure = Some(*store_failure);
        }

        if let Some(serde_json::Value::Bool(store_success)) = options_map.get("storeSuccess") {
            options.store_success = Some(*store_success);
        }

        if let Some(serde_json::Value::Bool(use_static)) = options_map.get("useStatic") {
            options.use_static = Some(*use_static);
        }

        if let Some(serde_json::Value::Bool(with_backup)) = options_map.get("withBackup") {
            options.with_backup = Some(*with_backup);
        }
    }

    // overwrite settings, options values with metadata if exists and return overwritten objects
    fn extract_metadata(
        metadata: &serde_json::Map<String, serde_json::Value>,
        mut settings: serde_json::Map<String, serde_json::Value>,
        options: Option<FunctionOptions>,
    ) -> (
        Option<FunctionOptions>,
        serde_json::Map<String, serde_json::Value>,
    ) {
        if let Some(serde_json::Value::Object(settings_map)) =
            metadata.get(Self::RUNNER_SETTINGS_METADATA_LABEL)
        {
            command_utils::util::json::merge_obj(&mut settings, settings_map.clone());
        }
        if let Some(serde_json::Value::Object(options_map)) =
            metadata.get(Self::FUNCTION_OPTIONS_METADATA_LABEL)
        {
            let mut options = options.unwrap_or_default();
            Self::apply_options_from_json(&mut options, options_map);
            (Some(options), settings)
        } else {
            (options, settings)
        }
    }
}
impl UseMemoryCache<String, workflow::RunTask> for CallTaskExecutor<'_> {
    fn cache(&self) -> &AsyncCache<std::string::String, workflow::RunTask> {
        self.memory_cache.cache()
    }

    #[doc = " default cache ttl"]
    fn default_ttl(&self) -> Option<&Duration> {
        self.memory_cache.default_ttl()
    }

    fn key_lock(&self) -> &RwLockWithKey<String> {
        self.memory_cache.key_lock()
    }
}
impl UseLoadUrlOrPath for CallTaskExecutor<'_> {
    fn http_client(&self) -> &infra_utils::infra::net::reqwest::ReqwestClient {
        &self.http_client
    }
}
impl TaskExecutorTrait for CallTaskExecutor<'_> {
    async fn execute(
        &self,
        task_name: &str,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        task_context: TaskContext,
    ) -> Result<TaskContext> {
        match self.task {
            // add other call task types
            workflow::CallTask {
                call,
                export: _export,
                if_: _if_,
                input: _input,
                metadata: call_metadata,
                output: call_output,
                then: _then,
                timeout: _timeout,
                with, // TODO return as argument for return value
            } => {
                // TODO name reference (inner yaml, public catalog)
                let fun = self
                    .with_cache_if_some(call, None, || self.load_url_or_path(call.as_str()))
                    .await?
                    .ok_or(anyhow!("not found: {}", call.as_str()))?;
                match fun {
                    // TODO: add other task types
                    RunTask {
                        export,
                        if_,
                        input,
                        metadata,
                        output,
                        run:
                            RunTaskConfiguration {
                                await_,
                                function:
                                    workflow::Function {
                                        mut arguments,
                                        options,
                                        runner_name,
                                        settings: loaded_settings,
                                    },
                                return_,
                            },
                        then,
                        timeout,
                    } => {
                        let (options, settings) =
                            Self::extract_metadata(call_metadata, loaded_settings, options);
                        let task = RunTask {
                            export,
                            if_,
                            input,
                            metadata,
                            output: call_output.clone().or(output), // TODO merge output
                            // XXX 1 pattern only
                            run: RunTaskConfiguration {
                                await_,
                                function: workflow::Function {
                                    arguments: {
                                        command_utils::util::json::merge_obj(
                                            &mut arguments,
                                            with.clone(),
                                        );
                                        arguments
                                    },
                                    options,
                                    runner_name,
                                    settings,
                                },
                                return_,
                            },
                            then,
                            timeout,
                        };
                        let executor =
                            RunTaskExecutor::new(self.job_executor_wrapper.clone(), &task);
                        executor
                            .execute(task_name, workflow_context, task_context)
                            .await
                    } // _ => {
                      //     tracing::error!("not supported the called function for now: {:?}", call);
                      //     Err(anyhow!(
                      //         "not supported the called function for now: {:?}",
                      //         call
                      //     ))
                      // }
                }
            } // _ => {
              //     tracing::error!("not supported the call for now: {:?}", &self.task);
              //     Err(anyhow!("not supported the call for now: {:?}", &self.task))
              // }
        }
    }
}
