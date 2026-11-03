mod validation;

// Re-export validation constants for tests
pub use validation::MAX_RECURSIVE_DEPTH;

use super::super::TaskExecutorTrait;
use crate::workflow::execute::expression::ExpressionContext;
use crate::workflow::{
    definition::{
        transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
        workflow::{
            self, errors::StandardErrorType, supplement::PythonScriptSettings,
            supplement::ValidatedLanguage,
        },
    },
    execute::{
        context::{TaskContext, WorkflowContext},
        expression::UseExpression,
        task::{
            ChildJobTracking,
            child_job::{self, ChildJobStage},
            enqueue_child_job,
        },
    },
};
use anyhow::{Context, Result};
use app::app::{
    job::execute::{JobExecutorWrapper, UseJobExecutor},
    runner::UseRunnerApp,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use jobworkerp_runner::jobworkerp::runner::{
    PythonCommandArgs, PythonCommandResult, PythonCommandRunnerSettings, python_command_args,
    python_command_runner_settings,
};
use proto::jobworkerp::data::{QueueType, ResponseType, StreamingType, WorkerData};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

/// Python script task executor implementing Open Workflow v1.0.0 script process
pub struct PythonTaskExecutor {
    workflow_context: Arc<RwLock<WorkflowContext>>,
    task_timeout: Duration,
    task: workflow::RunScript,
    job_executor_wrapper: Arc<JobExecutorWrapper>,
    metadata: Arc<HashMap<String, String>>,
}

impl UseExpression for PythonTaskExecutor {}
impl UseJqAndTemplateTransformer for PythonTaskExecutor {}
impl UseExpressionTransformer for PythonTaskExecutor {}

// Import the generic error handling macro from infra::workflow::errors module
use infra::bail_with_position;

impl PythonTaskExecutor {
    pub fn new(
        workflow_context: Arc<RwLock<WorkflowContext>>,
        task_timeout: Duration,
        job_executor_wrapper: Arc<JobExecutorWrapper>,
        task: workflow::RunScript,
        metadata: Arc<HashMap<String, String>>,
    ) -> Self {
        Self {
            workflow_context,
            task_timeout,
            task,
            job_executor_wrapper,
            metadata,
        }
    }

    /// Extract URI from ExternalResource
    fn extract_uri_from_external_resource(resource: &workflow::ExternalResource) -> &str {
        match &resource.endpoint {
            workflow::Endpoint::UriTemplate(uri) => &uri.0,
            workflow::Endpoint::EndpointConfiguration { uri, .. } => &uri.0,
        }
    }

    /// Convert script configuration to PYTHON_COMMAND arguments
    ///
    /// This method evaluates runtime expressions in arguments and injects them
    /// as Python global variables, following Open Workflow v1.0.0 spec.
    async fn to_python_command_args(
        &self,
        script_config: &workflow::ScriptConfiguration,
        task_context: &TaskContext,
        expression: &ExpressionContext,
    ) -> Result<PythonCommandArgs, Box<workflow::Error>> {
        let factory = workflow::errors::ErrorFactory::new();
        let position = task_context.position.read().await.clone();
        let instance = || Some(position.as_error_instance());
        let mut script_code = String::new();

        let (arguments, environment, code_or_source) = match script_config {
            workflow::ScriptConfiguration::Variant0 {
                code,
                arguments,
                environment,
                ..
            } => (arguments, environment, Ok(code.as_str())),
            workflow::ScriptConfiguration::Variant1 {
                source,
                arguments,
                environment,
                ..
            } => (arguments, environment, Err(source)),
        };

        // Step 1: Evaluate runtime expressions in arguments
        tracing::debug!("Raw arguments from workflow: {:#?}", arguments);
        tracing::debug!("Task context input: {:#?}", task_context.input);
        let evaluated_args =
            match Self::transform_map(task_context.input.clone(), arguments.clone(), expression) {
                Ok(args) => args,
                Err(mut e) => {
                    let mut arguments_position = position.clone();
                    arguments_position.push("arguments".to_string());
                    e.position(&arguments_position);
                    return Err(e);
                }
            };
        tracing::debug!("Evaluated arguments: {:#?}", evaluated_args);

        // Step 2: Inject evaluated arguments as global variables via Base64 encoding (secure)
        if let serde_json::Value::Object(ref args_map) = evaluated_args
            && !args_map.is_empty()
        {
            script_code.push_str("# Injected arguments (runtime expressions evaluated)\n");
            script_code.push_str("# Security: Base64 encoding prevents code injection\n");
            script_code.push_str("import json\n");
            script_code.push_str("import base64\n\n");

            for (key, value) in args_map {
                // Security validation
                validation::sanitize_python_variable(key, value).map_err(|e| {
                    factory.validation(
                        format!("Invalid script argument: {key}"),
                        instance(),
                        Some(e.to_string()),
                    )
                })?;

                // Serialize and Base64 encode (prevents all injection attacks)
                let json_str = serde_json::to_string(value).map_err(|e| {
                    factory.runtime(
                        format!("Failed to serialize script argument: {key}"),
                        instance(),
                        Some(e.to_string()),
                    )
                })?;
                let b64_encoded = STANDARD.encode(json_str.as_bytes());

                // Inject as Python variable (secure)
                script_code.push_str(&format!(
                    "{} = json.loads(base64.b64decode('{}').decode('utf-8'))\n",
                    key, b64_encoded
                ));
            }
            script_code.push('\n');
        }

        // Step 3: Append user's script or download external script
        match code_or_source {
            Ok(code) => {
                script_code.push_str(code);
            }
            Err(source) => {
                let uri = Self::extract_uri_from_external_resource(source);
                let url = validation::validate_script_url(uri).map_err(|e| {
                    factory.configuration(
                        "Invalid external script URL".to_string(),
                        instance(),
                        Some(e.to_string()),
                    )
                })?;
                let external_code =
                    validation::download_script_secure(&url)
                        .await
                        .map_err(|e| {
                            factory.communication(
                                "Failed to download external script".to_string(),
                                instance(),
                                Some(e.to_string()),
                            )
                        })?;
                script_code.push_str(&external_code);
            }
        }

        tracing::debug!("Generated Python script:\n{}", script_code);

        Ok(PythonCommandArgs {
            script: Some(python_command_args::Script::ScriptContent(script_code)),
            input_data: None, // Arguments are injected as globals
            env_vars: environment.clone(),
            with_stderr: true, // Capture stderr for error reporting
        })
    }

    /// Convert script configuration to PYTHON_COMMAND runner settings
    fn to_python_runner_settings(
        &self,
        python_settings: &PythonScriptSettings,
    ) -> Result<PythonCommandRunnerSettings> {
        let requirements_spec = if !python_settings.packages.is_empty() {
            Some(python_command_runner_settings::RequirementsSpec::Packages(
                python_command_runner_settings::PackagesList {
                    list: python_settings.packages.clone(),
                },
            ))
        } else {
            python_settings.requirements_url.as_ref().map(|url| {
                python_command_runner_settings::RequirementsSpec::RequirementsUrl(url.clone())
            })
        };

        Ok(PythonCommandRunnerSettings {
            python_version: python_settings.version.clone(),
            uv_path: None, // Use default uv path
            requirements_spec,
        })
    }
}

/// Shape a `PythonCommandResult` into the Open Workflow v1.0.0 process
/// result `{code, stdout, stderr}` and apply `return`, reusing the same adapter
/// as run.shell / run.container so all run process tasks behave identically.
/// `return: stdout` (the default) yields the raw stdout string — script output
/// is not JSON-parsed.
fn adapt_script_return_output(
    result: PythonCommandResult,
    return_type: workflow::ProcessReturnType,
    input: &serde_json::Value,
) -> Result<serde_json::Value> {
    let process_result = serde_json::json!({
        "code": result.exit_code,
        "stdout": result.output,
        "stderr": result.output_stderr.unwrap_or_default(),
    });
    super::alias::adapt_process_return_output(process_result, return_type, input)
}

impl TaskExecutorTrait<'_> for PythonTaskExecutor {
    async fn execute(
        &self,
        _cx: Arc<opentelemetry::Context>,
        task_name: &str,
        mut task_context: TaskContext,
    ) -> Result<TaskContext, Box<workflow::Error>> {
        let script_config = &self.task.script;

        let language_str = match script_config {
            workflow::ScriptConfiguration::Variant0 { language, .. } => language,
            workflow::ScriptConfiguration::Variant1 { language, .. } => language,
        };

        let language = match ValidatedLanguage::from_str(language_str) {
            Ok(lang) => lang,
            Err(e) => {
                let pos = task_context.position.read().await;
                return Err(workflow::errors::ErrorFactory::new().configuration(
                    e,
                    Some(pos.as_error_instance()),
                    None,
                ));
            }
        };

        match language {
            ValidatedLanguage::Python => { /* Supported */ }
            ValidatedLanguage::Javascript => {
                let pos = task_context.position.read().await;
                return Err(workflow::errors::ErrorFactory::new().configuration(
                    "JavaScript script execution".to_string(),
                    Some(pos.as_error_instance()),
                    Some("JavaScript support is not yet implemented".to_string()),
                ));
            }
        }

        // Prepare runtime expression evaluator
        let expression = match Self::expression(
            &*(self.workflow_context.read().await),
            Arc::new(task_context.clone()),
        )
        .await
        {
            Ok(e) => e,
            Err(mut e) => {
                let pos = task_context.position.read().await;
                e.position(&pos);
                return Err(e);
            }
        };

        let python_settings = bail_with_position!(
            task_context,
            PythonScriptSettings::from_metadata(&self.metadata),
            configuration,
            "Failed to parse Python settings from metadata"
        );

        let args = self
            .to_python_command_args(script_config, &task_context, &expression)
            .await?;

        let settings = bail_with_position!(
            task_context,
            self.to_python_runner_settings(&python_settings),
            configuration,
            "Failed to prepare script settings"
        );

        // Execute via PYTHON_COMMAND runner
        let runner_name = "PYTHON_COMMAND";
        let runner = match self
            .job_executor_wrapper
            .runner_app()
            .find_runner_by_name(runner_name)
            .await
        {
            Ok(Some(runner)) => runner,
            Ok(None) => {
                let pos = task_context.position.read().await;
                return Err(workflow::errors::ErrorFactory::new().configuration(
                    format!("{} runner not found", runner_name),
                    Some(pos.as_error_instance()),
                    None,
                ));
            }
            Err(e) => {
                let pos = task_context.position.read().await;
                return Err(workflow::errors::ErrorFactory::new().configuration(
                    format!("Failed to find {} runner", runner_name),
                    Some(pos.as_error_instance()),
                    Some(format!("{:?}", e)),
                ));
            }
        };

        // Encode PythonCommandRunnerSettings directly to protobuf binary
        // This is necessary because serde_json::to_value() encodes Rust enum variants
        // as {"VariantName": {...}}, which violates protobuf JSON encoding rules
        // where oneof fields should use the field name directly (e.g., "packages" not "Packages")
        let settings_bytes = bail_with_position!(
            task_context,
            {
                let mut buf = Vec::new();
                prost::Message::encode(&settings, &mut buf)
                    .context("Failed to encode PythonCommandRunnerSettings")
                    .map(|_| buf)
            },
            runtime,
            "Failed to encode runner settings to protobuf"
        );

        // Encode PythonCommandArgs directly to protobuf binary instead of using JSON
        // This bypasses the JSON serialization issue with oneof fields
        let args_bytes = bail_with_position!(
            task_context,
            {
                let mut buf = Vec::new();
                prost::Message::encode(&args, &mut buf)
                    .context("Failed to encode PythonCommandArgs")
                    .map(|_| buf)
            },
            runtime,
            "Failed to encode script arguments to protobuf"
        );

        let use_static = self
            .metadata
            .get("script.use_static")
            .and_then(|v| v.parse::<bool>().ok())
            .unwrap_or(false);

        // await:false runs the script fire-and-forget. NoResult is applied as a
        // per-job override (not on worker_data) because use_static reuses an
        // existing persisted worker whose response_type would otherwise win.
        let await_completion = self.task.await_;
        let worker_data = WorkerData {
            name: format!("python_{}", task_name),
            description: format!("Script task: {}", task_name),
            runner_id: runner.id,
            runner_settings: settings_bytes,
            periodic_interval: 0,
            channel: None,
            queue_type: QueueType::Normal as i32,
            response_type: ResponseType::Direct as i32,
            store_success: false,
            store_failure: true,
            use_static, // Language-agnostic setting
            retry_policy: None,
            broadcast_results: false,
        };

        let worker = bail_with_position!(
            task_context,
            self.job_executor_wrapper
                .worker_for_enqueue(worker_data)
                .await,
            runtime,
            "Failed to find or create worker"
        );

        // Enqueue job with protobuf binary arguments directly. Awaited scripts
        // are tracked so a task timeout or workflow cancel can reach them.
        let tracking = if await_completion {
            ChildJobTracking::Tracked
        } else {
            ChildJobTracking::Untracked
        };
        let child_job_scope = task_context.child_job_scope.clone();
        let enqueued = enqueue_child_job(
            &self.workflow_context,
            &self.job_executor_wrapper,
            child_job_scope.as_ref(),
            self.job_executor_wrapper
                .enqueue_with_worker_or_temp_channel(
                    self.metadata.clone(),
                    worker,
                    args_bytes, // Use protobuf binary directly, not JSON
                    None,       // No unique key
                    self.task_timeout.as_secs() as u32,
                    StreamingType::None,
                    None, // using not used for Python script execution
                    super::no_result_override_if(!await_completion),
                ),
            tracking,
        )
        .await
        .context(ChildJobStage::Enqueue);
        let (result_fut, running_job_guard) = match enqueued {
            Ok(enqueued) => enqueued,
            Err(e) => {
                return Err(child_job::fail(
                    &task_context,
                    e,
                    StandardErrorType::Runtime,
                    "Failed to enqueue script execution job",
                )
                .await);
            }
        };

        if !await_completion {
            // Fire-and-forget: continue with the current task input as output.
            drop(result_fut);
            task_context.set_raw_output(task_context.input.as_ref().clone());
            return Ok(task_context);
        }

        let wait_result = result_fut.await;
        if let Some(guard) = running_job_guard {
            guard.mark_completed();
        }
        let output_bytes = match child_job::finished_output(&self.job_executor_wrapper, wait_result)
        {
            Ok(output) => output,
            Err(e) => {
                return Err(child_job::fail(
                    &task_context,
                    e,
                    StandardErrorType::Runtime,
                    "Failed to execute script job",
                )
                .await);
            }
        };

        // Decode PYTHON_COMMAND result from protobuf binary
        let result: PythonCommandResult = bail_with_position!(
            task_context,
            prost::Message::decode(output_bytes.as_slice()),
            runtime,
            "Failed to decode script result"
        );

        if result.exit_code != 0 {
            let error_detail = format!(
                "Script exited with code {}\nstdout: {}\nstderr: {}",
                result.exit_code,
                result.output,
                result.output_stderr.as_deref().unwrap_or("")
            );
            let pos = task_context.position.read().await;
            return Err(workflow::errors::ErrorFactory::new().runtime(
                "Script execution failed".to_string(),
                Some(pos.as_error_instance()),
                Some(error_detail),
            ));
        }

        let script_output = bail_with_position!(
            task_context,
            adapt_script_return_output(result, self.task.return_, task_context.input.as_ref()),
            runtime,
            "Failed to adapt script process output"
        );

        task_context.set_raw_output(script_output);

        // NOTE: Do NOT remove position here. The parent TaskExecutor (task.rs:390) will handle
        // the position cleanup after this method returns.
        // task_context.remove_position().await;

        Ok(task_context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn result(exit_code: i32, stdout: &str, stderr: Option<&str>) -> PythonCommandResult {
        PythonCommandResult {
            exit_code,
            output: stdout.to_string(),
            output_stderr: stderr.map(str::to_string),
        }
    }

    #[test]
    fn adapts_each_return_type() {
        use workflow::ProcessReturnType::{All, Code, Stderr, Stdout};
        let cases: Vec<(
            &str,
            PythonCommandResult,
            workflow::ProcessReturnType,
            Value,
        )> = vec![
            // Default `stdout` returns the raw stdout string — JSON-looking
            // stdout must NOT be parsed (guards the dropped JSON-parse default).
            (
                "stdout not parsed",
                result(0, "{\"a\": 1}", None),
                Stdout,
                json!("{\"a\": 1}"),
            ),
            (
                "stdout raw",
                result(0, "hello\n", Some("warn")),
                Stdout,
                json!("hello\n"),
            ),
            (
                "stderr",
                result(0, "out", Some("boom")),
                Stderr,
                json!("boom"),
            ),
            (
                "stderr absent -> empty",
                result(0, "out", None),
                Stderr,
                json!(""),
            ),
            ("code", result(0, "out", None), Code, json!(0)),
            (
                "all",
                result(0, "out", Some("err")),
                All,
                json!({"code": 0, "stdout": "out", "stderr": "err"}),
            ),
        ];
        for (label, res, return_type, expected) in cases {
            let out = adapt_script_return_output(res, return_type, &json!({})).unwrap();
            assert_eq!(out, expected, "case: {label}");
        }
    }

    #[test]
    fn return_none_returns_task_input() {
        let input = json!({"keep": "me"});
        let out = adapt_script_return_output(
            result(0, "ignored stdout", Some("ignored stderr")),
            workflow::ProcessReturnType::None,
            &input,
        )
        .unwrap();
        assert_eq!(out, input);
    }
}
