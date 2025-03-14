use super::run::RunTaskExecutor;
use super::TaskExecutorTrait;
use crate::simple_workflow::definition::workflow::RunTask;
use crate::simple_workflow::definition::workflow::{self, RunTaskConfiguration, Script};
use crate::simple_workflow::definition::UseLoadYaml;
use crate::simple_workflow::execute::context::{TaskContext, WorkflowContext};
use crate::simple_workflow::execute::job::JobExecutorWrapper;
use anyhow::{anyhow, Result};
use infra_utils::infra::lock::RwLockWithKey;
use infra_utils::infra::memory::{MemoryCacheConfig, UseMemoryCache};
use infra_utils::infra::net::reqwest;
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
impl UseLoadYaml for CallTaskExecutor<'_> {
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
            workflow::CallTask::Function {
                call,
                export: _export,
                if_: _if_,
                input: _input,
                metadata,
                output: call_output,
                then: _then,
                timeout: _timeout,
                with, // TODO return as argument for return value
            } => {
                // TODO name reference (inner yaml, public catalog)
                let fun = self
                    .with_cache_if_some(call, None, || self.load_yaml(call.as_str()))
                    .await?
                    .ok_or(anyhow!("not found: {}", call.as_str()))?;
                match fun {
                    RunTask {
                        export,
                        if_,
                        input,
                        metadata: mut loaded_metadata,
                        output,
                        run:
                            RunTaskConfiguration::Variant1 {
                                await_,
                                script:
                                    Script::Variant0 {
                                        // XXX 1 pattern only
                                        mut arguments,
                                        code,
                                        environment,
                                        language,
                                    },
                            },
                        then,
                        timeout,
                    } => {
                        let task = RunTask {
                            export,
                            if_,
                            input,
                            metadata: {
                                command_utils::util::json::merge_obj(
                                    &mut loaded_metadata,
                                    metadata.clone(),
                                );
                                loaded_metadata
                            },
                            output: call_output.clone().or(output), // TODO merge output
                            // XXX 1 pattern only
                            run: RunTaskConfiguration::Variant1 {
                                await_,
                                script: Script::Variant0 {
                                    arguments: {
                                        command_utils::util::json::merge_obj(
                                            &mut arguments,
                                            with.clone(),
                                        );
                                        arguments
                                    }, // replace
                                    code,
                                    environment,
                                    language,
                                },
                            },
                            then,
                            timeout,
                        };
                        let executor =
                            RunTaskExecutor::new(self.job_executor_wrapper.clone(), &task);
                        executor
                            .execute(task_name, workflow_context, task_context)
                            .await
                    }
                    _ => {
                        tracing::error!("not supported the called function for now: {:?}", call);
                        Err(anyhow!(
                            "not supported the called function for now: {:?}",
                            call
                        ))
                    }
                }
            }
            _ => {
                tracing::error!("not supported the call for now: {:?}", &self.task);
                Err(anyhow!("not supported the call for now: {:?}", &self.task))
            }
        }
    }
}
