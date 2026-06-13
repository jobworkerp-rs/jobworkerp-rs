pub mod python;

use super::TaskExecutorTrait;
use crate::workflow::{
    definition::{
        transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
        workflow::{self, RunJobRunner, RunJobWorker},
    },
    execute::{
        context::{TaskContext, WorkflowContext},
        expression::UseExpression,
    },
};
use anyhow::Result;
use app::app::{
    job::execute::{JobExecutorWrapper, UseJobExecutor},
    runner::UseRunnerApp,
};
use command_utils::trace::Tracing;
use proto::jobworkerp::data::{QueueType, ResponseType, StreamingType, WorkerData};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::RwLock;

pub struct RunTaskExecutor {
    workflow_context: Arc<RwLock<WorkflowContext>>,
    default_task_timeout: Duration,
    task: workflow::RunTask,
    job_executor_wrapper: Arc<JobExecutorWrapper>,
    metadata: Arc<HashMap<String, String>>,
}
impl UseExpression for RunTaskExecutor {}
impl UseJqAndTemplateTransformer for RunTaskExecutor {}
impl UseExpressionTransformer for RunTaskExecutor {}
impl Tracing for RunTaskExecutor {}

/// Convert a task's optional YAML `timeout` block to job timeout in seconds.
/// `max(1, ...)` prevents sub-second durations from immediately tripping the
/// receiver-side timeout. Shared by `RunTaskExecutor` and `stream::run` so the
/// two execution paths stay in lockstep with the receiver-side timeout
/// contract.
pub(crate) fn resolve_run_task_timeout_sec(
    timeout: Option<&workflow::TaskTimeout>,
    default_task_timeout: Duration,
) -> u32 {
    if let Some(workflow::TaskTimeout::Timeout(duration)) = timeout {
        std::cmp::max(1, duration.after.to_millis().div_ceil(1000) as u32)
    } else {
        default_task_timeout.as_secs() as u32
    }
}
impl RunTaskExecutor {
    pub fn new(
        workflow_context: Arc<RwLock<WorkflowContext>>,
        default_task_timeout: Duration,
        job_executor_wrapper: Arc<JobExecutorWrapper>,
        task: workflow::RunTask,
        metadata: Arc<HashMap<String, String>>,
    ) -> Self {
        Self {
            workflow_context,
            default_task_timeout,
            task,
            job_executor_wrapper,
            metadata,
        }
    }

    /// Convert workflow QueueType to proto QueueType
    fn convert_queue_type(qt: workflow::QueueType) -> i32 {
        match qt {
            workflow::QueueType::Normal => QueueType::Normal as i32,
            workflow::QueueType::WithBackup => QueueType::WithBackup as i32,
            workflow::QueueType::DbOnly => QueueType::DbOnly as i32,
        }
    }

    /// Convert workflow ResponseType to proto ResponseType
    fn convert_response_type(rt: workflow::ResponseType) -> i32 {
        match rt {
            workflow::ResponseType::NoResult => ResponseType::NoResult as i32,
            workflow::ResponseType::Direct => ResponseType::Direct as i32,
        }
    }

    fn function_options_to_worker_data(
        options: Option<workflow::WorkerOptions>,
        name: &str,
    ) -> Option<WorkerData> {
        if let Some(options) = options {
            // Initialize with struct syntax instead of default() + field assignments
            let worker_data = WorkerData {
                name: name.to_string(),
                description: String::new(),
                broadcast_results: options.broadcast_results.unwrap_or(false),
                store_failure: options.store_failure.unwrap_or(false),
                store_success: options.store_success.unwrap_or(false),
                use_static: options.use_static.unwrap_or(false),
                queue_type: options
                    .queue_type
                    .map(Self::convert_queue_type)
                    .unwrap_or(QueueType::Normal as i32),
                channel: options.channel,
                retry_policy: options.retry.map(|r| r.to_jobworkerp()),
                response_type: options
                    .response_type
                    .map(Self::convert_response_type)
                    .unwrap_or(ResponseType::Direct as i32),
                ..Default::default()
            };
            Some(worker_data)
        } else {
            None
        }
    }

    /// Transform a string field that may contain jq expression or Liquid template
    fn transform_string_field(
        input: Arc<serde_json::Value>,
        field: &str,
        expression: &std::collections::BTreeMap<String, Arc<serde_json::Value>>,
    ) -> Result<String, Box<workflow::Error>> {
        let transformed = Self::execute_transform(input, field, expression)?;
        match transformed {
            serde_json::Value::String(s) => Ok(s),
            other => Ok(other.to_string()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_by_jobworkerp(
        &self,
        cx: Arc<opentelemetry::Context>,
        runner_name: &str,
        settings: Option<serde_json::Value>,
        options: Option<workflow::WorkerOptions>,
        job_args: serde_json::Value,
        worker_name: &str,
        using: Option<String>,
        // Effective task timeout in seconds (YAML `timeout` block, else default).
        // Threaded through so RunRunner / RunFunction paths honor it instead of
        // silently capping at `default_task_timeout`.
        timeout_sec: u32,
    ) -> Result<serde_json::Value> {
        let runner = self
            .job_executor_wrapper
            .runner_app()
            .find_runner_by_name(runner_name)
            .await
            .map_err(|e| {
                anyhow::anyhow!("Failed to find runner by name '{}': {:#?}", runner_name, e)
            })?
            .ok_or(anyhow::anyhow!("Runner '{}' not found", runner_name))?;
        let settings = self
            .job_executor_wrapper
            .setup_runner_and_settings(&runner, settings)
            .await?;
        let mut worker_data = Self::function_options_to_worker_data(options, worker_name)
            .unwrap_or(WorkerData {
                name: worker_name.to_string().clone(),
                description: "".to_string(),
                runner_id: None,         // replace after
                runner_settings: vec![], // replace after
                periodic_interval: 0,
                channel: None,
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::Direct as i32,
                store_success: false,
                store_failure: true, //
                use_static: false,
                retry_policy: None, //TODO
                broadcast_results: true,
            });
        worker_data.runner_id = runner.id;
        worker_data.runner_settings = settings;
        // inject metadata from opentelemetry context for remote job
        let mut metadata = (*self.metadata).clone();
        Self::inject_metadata_from_context(&mut metadata, &cx);

        // Enqueue via the channel API so we learn the child JobId before the
        // result is ready. Register it on the workflow context so that, if the
        // workflow is cancelled while this child is running, WorkflowExecutor
        // can actively cancel it (delete_job) instead of waiting for it to
        // finish. Deregister on completion (success or error).
        let (rid, rdata) = match (runner.id, runner.data) {
            (Some(rid), Some(rdata)) => (rid, rdata),
            _ => return Err(anyhow::anyhow!("Runner '{}' missing id/data", runner_name)),
        };
        let job_args = self
            .job_executor_wrapper
            .transform_job_args(&rid, &rdata, &job_args, using.as_deref())
            .await?;
        let wid = if worker_data.use_static {
            self.job_executor_wrapper
                .find_or_create_worker(&worker_data)
                .await?
                .id
        } else {
            None
        };

        let (job_id, result_fut) = self
            .job_executor_wrapper
            .enqueue_with_worker_or_temp_channel(
                Arc::new(metadata),
                wid,
                worker_data,
                job_args,
                None, // XXX no uniq_key,
                timeout_sec,
                StreamingType::None,
                using.clone(),
            )
            .await?;

        self.workflow_context
            .read()
            .await
            .register_running_job(&job_id)
            .await;
        let wait_result = result_fut.await;
        self.workflow_context
            .read()
            .await
            .unregister_running_job(&job_id)
            .await;

        let (res, _stream) = wait_result?;
        let output = res
            .map(|r| self.job_executor_wrapper.extract_job_result_output(r))
            .ok_or(anyhow::anyhow!(
                "Failed to enqueue job or job result not found"
            ))
            .and_then(|output| output)?;
        self.job_executor_wrapper
            .transform_raw_output(&rid, &rdata, &output, using.as_deref())
            .await
    }

    async fn execute_by_worker_name(
        &self,
        metadata: Arc<HashMap<String, String>>,
        worker_name: &str,
        job_args: &serde_json::Value,
        using: Option<String>,
        timeout_sec: u32,
    ) -> Result<serde_json::Value> {
        let child = self
            .job_executor_wrapper
            .enqueue_with_worker_name_channel(
                metadata,
                worker_name,
                job_args,
                None,
                timeout_sec,
                StreamingType::None,
                using,
            )
            .await?;

        self.workflow_context
            .read()
            .await
            .register_running_job(&child.job_id)
            .await;
        let wait_result = child.result_fut.await;
        self.workflow_context
            .read()
            .await
            .unregister_running_job(&child.job_id)
            .await;

        let (res, _stream) = wait_result?;
        let output = res
            .map(|r| self.job_executor_wrapper.extract_job_result_output(r))
            .ok_or(anyhow::anyhow!(
                "Failed to enqueue job or job result not found"
            ))
            .and_then(|output| output)?;
        self.job_executor_wrapper
            .transform_raw_output(
                &child.runner_id,
                &child.runner_data,
                output.as_slice(),
                child.using.as_deref(),
            )
            .await
    }
}
impl TaskExecutorTrait<'_> for RunTaskExecutor {
    async fn execute(
        &self,
        cx: Arc<opentelemetry::Context>,
        task_name: &str,
        mut task_context: TaskContext,
    ) -> Result<TaskContext, Box<workflow::Error>> {
        // TODO: add other task types
        let workflow::RunTask {
            // TODO
            // export,   //: ::std::option::Option<Export>,
            metadata: task_metadata, //: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            // output,   //: ::std::option::Option<Output>,
            timeout, //: ::std::option::Option<TaskTimeout>,
            // then, //  ::std::option::Option<FlowDirective>
            run,
            ..
        } = &self.task;
        let timeout_sec = resolve_run_task_timeout_sec(timeout.as_ref(), self.default_task_timeout);

        // merge metadata: create new metadata with both self.metadata and task metadata
        let mut metadata = (*self.metadata).clone();
        for (k, v) in task_metadata {
            // if key already exists, append value
            if metadata.contains_key(k) {
                continue; // skip if key already exists
            } else if let Some(v) = v.as_str() {
                // add only if value is a string
                metadata.insert(k.clone(), v.to_string());
            }
        }
        let metadata = Arc::new(metadata);

        // setup task context
        task_context.add_position_name("run".to_string()).await;

        let expression = Self::expression(
            &*(self.workflow_context.read().await),
            Arc::new(task_context.clone()),
        )
        .await;

        // tracing::debug!("expression: {:#?}", expression);

        let expression = match expression {
            Ok(e) => e,
            Err(mut e) => {
                let pos = task_context.position.clone();
                let pos = pos.read().await;
                e.position(&pos);
                return Err(e);
            }
        };

        // Log workflow.input for debugging input value propagation
        if let Some(workflow_val) = expression.get("workflow")
            && let Some(input_val) = workflow_val.get("input")
        {
            tracing::debug!(
                "[DEBUG] task={} workflow.input in expression: {}",
                task_name,
                serde_json::to_string(input_val).unwrap_or_else(|_| "serialize error".to_string())
            );
        }

        // TODO: add other task types
        // currently support only RunTaskConfiguration
        match run {
            workflow::RunTaskConfiguration::Worker(workflow::RunWorker {
                worker:
                    RunJobWorker {
                        arguments,
                        name: worker_name,
                        using,
                    },
            }) => {
                task_context.add_position_name("worker".to_string()).await;

                tracing::debug!("raw arguments: {:#?}", arguments);
                let args = match Self::transform_map(
                    task_context.input.clone(),
                    arguments.clone(),
                    &expression,
                ) {
                    Ok(args) => args,
                    Err(mut e) => {
                        let pos = task_context.position.clone();
                        let mut pos = pos.write().await;
                        pos.push("arguments".to_string());
                        e.position(&pos);
                        return Err(e);
                    }
                };
                // let args = serde_json::Value::Object(arguments.clone());
                tracing::debug!("transformed arguments: {:#?}", args);

                let output = match self
                    .execute_by_worker_name(
                        metadata,
                        worker_name,
                        &args,
                        using.clone(),
                        timeout_sec,
                    )
                    .await
                {
                    Ok(output) => Ok(output),
                    Err(e) => {
                        let pos = task_context.position.clone();
                        let pos = pos.read().await.as_error_instance();
                        tracing::error!(error = ?e, position = %pos, "Failed to execute by jobworkerp (function)");
                        Err(workflow::errors::ErrorFactory::new().service_unavailable(
                            "Failed to execute by jobworkerp".to_string(),
                            Some(pos),
                            Some(e.to_string()),
                        ))
                    }
                }?;
                task_context.set_raw_output(output);

                // out of run.function
                task_context.remove_position().await;
                task_context.remove_position().await;

                Ok(task_context)
            }
            workflow::RunTaskConfiguration::Runner(workflow::RunRunner {
                runner:
                    RunJobRunner {
                        arguments,
                        name: runner_name,
                        options,
                        settings,
                        using,
                    },
            }) => {
                task_context.add_position_name("runner".to_string()).await;

                // Transform runner_name (may contain jq expression or Liquid template)
                let transformed_runner_name = match Self::transform_string_field(
                    task_context.input.clone(),
                    runner_name,
                    &expression,
                ) {
                    Ok(name) => name,
                    Err(mut e) => {
                        let pos = task_context.position.clone();
                        let mut pos = pos.write().await;
                        pos.push("name".to_string());
                        e.position(&pos);
                        return Err(e);
                    }
                };
                tracing::debug!("transformed runner_name: {}", transformed_runner_name);

                // Transform using (may contain jq expression or Liquid template)
                let transformed_using = match using.as_ref() {
                    Some(u) => match Self::transform_string_field(
                        task_context.input.clone(),
                        u,
                        &expression,
                    ) {
                        Ok(name) => Some(name),
                        Err(mut e) => {
                            let pos = task_context.position.clone();
                            let mut pos = pos.write().await;
                            pos.push("using".to_string());
                            e.position(&pos);
                            return Err(e);
                        }
                    },
                    None => None,
                };
                tracing::debug!("transformed using: {:?}", transformed_using);

                tracing::debug!("raw arguments: {:#?}", arguments);
                let args = match Self::transform_map(
                    task_context.input.clone(),
                    arguments.clone(),
                    &expression,
                ) {
                    Ok(args) => args,
                    Err(mut e) => {
                        let pos = task_context.position.clone();
                        let mut pos = pos.write().await;
                        pos.push("arguments".to_string());
                        e.position(&pos);
                        return Err(e);
                    }
                };
                // let args = serde_json::Value::Object(arguments.clone());
                tracing::debug!("transformed arguments: {:#?}", args);

                // enter run.function
                let transformed_settings = match Self::transform_map(
                    task_context.input.clone(),
                    settings.clone(),
                    &expression,
                ) {
                    Ok(settings) => settings,
                    Err(mut e) => {
                        let pos = task_context.position.clone();
                        let mut pos = pos.write().await;
                        pos.push("settings".to_string());
                        e.position(&pos);
                        return Err(e);
                    }
                };

                let output = match self
                    .execute_by_jobworkerp(
                        cx.clone(),
                        &transformed_runner_name,
                        Some(transformed_settings),
                        options.clone(),
                        args,
                        task_name,
                        transformed_using,
                        timeout_sec,
                    )
                    .await
                {
                    Ok(output) => Ok(output),
                    Err(e) => {
                        let pos = task_context.position.clone();
                        let pos = pos.read().await.as_error_instance();
                        tracing::error!(error = ?e, position = %pos, "Failed to execute by jobworkerp (runner)");
                        Err(workflow::errors::ErrorFactory::new().service_unavailable(
                            "Failed to execute by jobworkerp".to_string(),
                            Some(pos),
                            Some(e.to_string()),
                        ))
                    }
                }?;
                task_context.set_raw_output(output);

                // out of run.function
                task_context.remove_position().await;
                task_context.remove_position().await;

                Ok(task_context)
            }
            workflow::RunTaskConfiguration::Function(workflow::RunFunction {
                function:
                    workflow::RunJobFunction::WorkerFunction {
                        arguments,
                        using,
                        worker_name,
                    },
            }) => {
                task_context.add_position_name("function".to_string()).await;

                // Transform worker_name (may contain jq expression or Liquid template)
                let transformed_worker_name = match Self::transform_string_field(
                    task_context.input.clone(),
                    worker_name,
                    &expression,
                ) {
                    Ok(name) => name,
                    Err(mut e) => {
                        let pos = task_context.position.clone();
                        let mut pos = pos.write().await;
                        pos.push("worker_name".to_string());
                        e.position(&pos);
                        return Err(e);
                    }
                };
                tracing::debug!("transformed worker_name: {}", transformed_worker_name);

                // Transform using (may contain jq expression or Liquid template)
                let transformed_using = match using.as_ref() {
                    Some(u) => match Self::transform_string_field(
                        task_context.input.clone(),
                        u,
                        &expression,
                    ) {
                        Ok(name) => Some(name),
                        Err(mut e) => {
                            let pos = task_context.position.clone();
                            let mut pos = pos.write().await;
                            pos.push("using".to_string());
                            e.position(&pos);
                            return Err(e);
                        }
                    },
                    None => None,
                };
                tracing::debug!("transformed using: {:?}", transformed_using);

                tracing::debug!("raw arguments: {:#?}", arguments);
                let args = match Self::transform_map(
                    task_context.input.clone(),
                    arguments.clone(),
                    &expression,
                ) {
                    Ok(args) => args,
                    Err(mut e) => {
                        let pos = task_context.position.clone();
                        let mut pos = pos.write().await;
                        pos.push("arguments".to_string());
                        e.position(&pos);
                        return Err(e);
                    }
                };
                // let args = serde_json::Value::Object(arguments.clone());
                tracing::debug!("transformed arguments: {:#?}", args);

                let output = match self
                    .execute_by_worker_name(
                        metadata,
                        &transformed_worker_name,
                        &args,
                        transformed_using,
                        timeout_sec,
                    )
                    .await
                {
                    Ok(output) => Ok(output),
                    Err(e) => {
                        let pos = task_context.position.clone();
                        let pos = pos.read().await.as_error_instance();
                        tracing::error!(error = ?e, position = %pos, "Failed to execute by jobworkerp (worker function)");
                        Err(workflow::errors::ErrorFactory::new().service_unavailable(
                            "Failed to execute by jobworkerp".to_string(),
                            Some(pos),
                            Some(e.to_string()),
                        ))
                    }
                }?;
                task_context.set_raw_output(output);

                // out of run.function
                task_context.remove_position().await;
                task_context.remove_position().await;

                Ok(task_context)
            }
            workflow::RunTaskConfiguration::Function(workflow::RunFunction {
                function:
                    workflow::RunJobFunction::RunnerFunction {
                        arguments,
                        options,
                        runner_name,
                        settings,
                        using,
                    },
            }) => {
                task_context.add_position_name("function".to_string()).await;

                // Transform runner_name (may contain jq expression or Liquid template)
                let transformed_runner_name = match Self::transform_string_field(
                    task_context.input.clone(),
                    runner_name,
                    &expression,
                ) {
                    Ok(name) => name,
                    Err(mut e) => {
                        let pos = task_context.position.clone();
                        let mut pos = pos.write().await;
                        pos.push("runner_name".to_string());
                        e.position(&pos);
                        return Err(e);
                    }
                };
                tracing::debug!("transformed runner_name: {}", transformed_runner_name);

                // Transform using (may contain jq expression or Liquid template)
                let transformed_using = match using.as_ref() {
                    Some(u) => match Self::transform_string_field(
                        task_context.input.clone(),
                        u,
                        &expression,
                    ) {
                        Ok(name) => Some(name),
                        Err(mut e) => {
                            let pos = task_context.position.clone();
                            let mut pos = pos.write().await;
                            pos.push("using".to_string());
                            e.position(&pos);
                            return Err(e);
                        }
                    },
                    None => None,
                };
                tracing::debug!("transformed using: {:?}", transformed_using);

                tracing::debug!("raw arguments: {:#?}", arguments);
                let args = match Self::transform_map(
                    task_context.input.clone(),
                    arguments.clone(),
                    &expression,
                ) {
                    Ok(args) => args,
                    Err(mut e) => {
                        let pos = task_context.position.clone();
                        let mut pos = pos.write().await;
                        pos.push("arguments".to_string());
                        e.position(&pos);
                        return Err(e);
                    }
                };
                // let args = serde_json::Value::Object(arguments.clone());
                tracing::debug!("transformed arguments: {:#?}", args);

                // enter run.function
                let transformed_settings = match Self::transform_map(
                    task_context.input.clone(),
                    settings.clone(),
                    &expression,
                ) {
                    Ok(settings) => settings,
                    Err(mut e) => {
                        let pos = task_context.position.clone();
                        let mut pos = pos.write().await;
                        pos.push("settings".to_string());
                        e.position(&pos);
                        return Err(e);
                    }
                };
                tracing::debug!("transformed settings: {:#?}", &settings);

                let output = match self
                    .execute_by_jobworkerp(
                        cx.clone(),
                        &transformed_runner_name,
                        Some(transformed_settings),
                        options.clone(),
                        args,
                        task_name,
                        transformed_using,
                        timeout_sec,
                    )
                    .await
                {
                    Ok(output) => Ok(output),
                    Err(e) => {
                        let pos = task_context.position.clone();
                        let pos = pos.read().await.as_error_instance();
                        tracing::error!(error = ?e, position = %pos, "Failed to execute by jobworkerp (runner function)");
                        Err(workflow::errors::ErrorFactory::new().service_unavailable(
                            "Failed to execute by jobworkerp".to_string(),
                            Some(pos),
                            Some(e.to_string()),
                        ))
                    }
                }?;
                task_context.set_raw_output(output);

                // out of run.function
                task_context.remove_position().await;
                task_context.remove_position().await;

                Ok(task_context)
            }
            workflow::RunTaskConfiguration::Script(run_script) => {
                task_context.add_position_name("script".to_string()).await;

                let executor = python::PythonTaskExecutor::new(
                    self.workflow_context.clone(),
                    Duration::from_secs(timeout_sec as u64),
                    self.job_executor_wrapper.clone(),
                    run_script.clone(),
                    metadata.clone(),
                );
                let result = executor.execute(cx, task_name, task_context).await;

                match result {
                    Ok(tc) => {
                        tc.remove_position().await; // Remove "script"
                        tc.remove_position().await; // Remove "run"
                        Ok(tc)
                    }
                    Err(e) => Err(e),
                }
            } // _ => {
              //     let pos = task_context.position.clone();
              //     let mut pos = pos.write().await;
              //     pos.push("run".to_string());
              //     Err(workflow::errors::ErrorFactory::new().not_implemented(
              //         "RunTaskConfiguration".to_string(),
              //         Some(pos.as_error_instance()),
              //         Some(format!("Not implemented now: {:#?}", run)),
              //     ))
              // }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RunTaskExecutor;
    use crate::workflow::definition::workflow;
    use proto::jobworkerp::data::{QueueType, ResponseType};

    fn worker_options_with_queue_and_response(
        queue_type: workflow::QueueType,
        response_type: workflow::ResponseType,
    ) -> workflow::WorkerOptions {
        workflow::WorkerOptions {
            broadcast_results: Some(false),
            channel: None,
            queue_type: Some(queue_type),
            response_type: Some(response_type),
            retry: None,
            store_failure: Some(true),
            store_success: Some(false),
            use_static: Some(false),
        }
    }

    #[test]
    fn worker_data_respects_queue_and_response_type() {
        let options = worker_options_with_queue_and_response(
            workflow::QueueType::DbOnly,
            workflow::ResponseType::NoResult,
        );

        let worker_data =
            RunTaskExecutor::function_options_to_worker_data(Some(options), "test_worker")
                .expect("worker data should be generated");

        assert_eq!(worker_data.queue_type, QueueType::DbOnly as i32);
        assert_eq!(worker_data.response_type, ResponseType::NoResult as i32);
    }

    #[test]
    fn worker_data_defaults_to_normal_queue_and_direct_response() {
        let options = workflow::WorkerOptions {
            broadcast_results: None,
            channel: None,
            queue_type: None,
            response_type: None,
            retry: None,
            store_failure: None,
            store_success: None,
            use_static: None,
        };

        let worker_data =
            RunTaskExecutor::function_options_to_worker_data(Some(options), "legacy_worker")
                .expect("worker data should be generated");

        assert_eq!(worker_data.queue_type, QueueType::Normal as i32);
        assert_eq!(worker_data.response_type, ResponseType::Direct as i32);
    }

    fn task_timeout_hours(h: i64) -> workflow::TaskTimeout {
        workflow::TaskTimeout::Timeout(workflow::Timeout {
            after: workflow::Duration::Inline {
                days: None,
                hours: Some(h),
                minutes: None,
                seconds: None,
                milliseconds: None,
            },
        })
    }

    // Regression: a YAML task-level `timeout: hours: 24` used to be dropped
    // on the run.function / run.runner paths because execute_by_jobworkerp
    // hard-coded `default_task_timeout` (3600s) as the job timeout. This
    // test pins the conversion contract so the bug cannot silently come back.
    #[test]
    fn timeout_sec_honors_24_hour_yaml_timeout() {
        let t = task_timeout_hours(24);
        let resolved = super::resolve_run_task_timeout_sec(
            Some(&t),
            std::time::Duration::from_secs(3600), // fallback that must NOT win
        );
        assert_eq!(
            resolved, 86_400,
            "24h must resolve to 86400s, not the default"
        );
    }

    #[test]
    fn timeout_sec_uses_default_when_yaml_omits_timeout() {
        let resolved =
            super::resolve_run_task_timeout_sec(None, std::time::Duration::from_secs(3600));
        assert_eq!(resolved, 3600);
    }

    #[test]
    fn timeout_sec_rounds_sub_second_durations_up_to_one() {
        let t = workflow::TaskTimeout::Timeout(workflow::Timeout {
            after: workflow::Duration::Inline {
                days: None,
                hours: None,
                minutes: None,
                seconds: None,
                milliseconds: Some(250),
            },
        });
        let resolved =
            super::resolve_run_task_timeout_sec(Some(&t), std::time::Duration::from_secs(3600));
        assert_eq!(
            resolved, 1,
            "sub-second durations must round up to 1s, not 0"
        );
    }
}
