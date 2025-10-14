mod validation;

// Re-export validation constants for tests
pub use validation::MAX_RECURSIVE_DEPTH;

use super::super::TaskExecutorTrait;
use crate::workflow::{
    definition::{
        transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
        workflow::{self, supplement::PythonScriptSettings, supplement::ValidatedLanguage},
    },
    execute::{
        context::{TaskContext, WorkflowContext},
        expression::UseExpression,
    },
};
use anyhow::{anyhow, Context, Result};
use app::app::{
    job::execute::{JobExecutorWrapper, UseJobExecutor},
    runner::UseRunnerApp,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use jobworkerp_runner::jobworkerp::runner::{
    python_command_args, python_command_runner_settings, PythonCommandArgs, PythonCommandResult,
    PythonCommandRunnerSettings,
};
use proto::jobworkerp::data::{QueueType, ResponseType, WorkerData};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

/// Python script task executor implementing Serverless Workflow v1.0.0 script process
pub struct PythonTaskExecutor {
    workflow_context: Arc<RwLock<WorkflowContext>>,
    default_task_timeout: Duration,
    task: workflow::RunScript,
    job_executor_wrapper: Arc<JobExecutorWrapper>,
    metadata: Arc<HashMap<String, String>>,
}

impl UseExpression for PythonTaskExecutor {}
impl UseJqAndTemplateTransformer for PythonTaskExecutor {}
impl UseExpressionTransformer for PythonTaskExecutor {}

// Import the generic error handling macro from workflow::errors module
use crate::bail_with_position;

impl PythonTaskExecutor {
    pub fn new(
        workflow_context: Arc<RwLock<WorkflowContext>>,
        default_task_timeout: Duration,
        job_executor_wrapper: Arc<JobExecutorWrapper>,
        task: workflow::RunScript,
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

    /// Extract URI from ExternalResource
    fn extract_uri_from_external_resource(resource: &workflow::ExternalResource) -> Result<String> {
        match &resource.endpoint {
            workflow::Endpoint::UriTemplate(uri) => Ok(uri.0.clone()),
            workflow::Endpoint::EndpointConfiguration { uri, .. } => Ok(uri.0.clone()),
        }
    }

    /// Convert script configuration to PYTHON_COMMAND arguments
    ///
    /// This method evaluates runtime expressions in arguments and injects them
    /// as Python global variables, following Serverless Workflow v1.0.0 spec.
    async fn to_python_command_args(
        &self,
        script_config: &workflow::ScriptConfiguration,
        task_context: &TaskContext,
        expression: &std::collections::BTreeMap<String, Arc<serde_json::Value>>,
    ) -> Result<PythonCommandArgs> {
        let mut script_code = String::new();

        // Extract fields from enum variants
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
                Err(e) => {
                    return Err(anyhow!("Failed to evaluate runtime expressions: {:?}", e));
                }
            };
        tracing::debug!("Evaluated arguments: {:#?}", evaluated_args);

        // Step 2: Inject evaluated arguments as global variables via Base64 encoding (secure)
        if let serde_json::Value::Object(ref args_map) = evaluated_args {
            if !args_map.is_empty() {
                script_code.push_str("# Injected arguments (runtime expressions evaluated)\n");
                script_code.push_str("# Security: Base64 encoding prevents code injection\n");
                script_code.push_str("import json\n");
                script_code.push_str("import base64\n\n");

                for (key, value) in args_map {
                    // Security validation
                    validation::sanitize_python_variable(key, value)?;

                    // Serialize and Base64 encode (prevents all injection attacks)
                    let json_str = serde_json::to_string(value)
                        .context("Failed to serialize argument value")?;
                    let b64_encoded = STANDARD.encode(json_str.as_bytes());

                    // Inject as Python variable (secure)
                    script_code.push_str(&format!(
                        "{} = json.loads(base64.b64decode('{}').decode('utf-8'))\n",
                        key, b64_encoded
                    ));
                }
                script_code.push('\n');
            }
        }

        // Step 3: Append user's script or download external script
        match code_or_source {
            Ok(code) => {
                script_code.push_str(code);
            }
            Err(source) => {
                let uri = Self::extract_uri_from_external_resource(source)?;
                let external_code = validation::download_script_secure(&uri).await?;
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

impl TaskExecutorTrait<'_> for PythonTaskExecutor {
    async fn execute(
        &self,
        _cx: Arc<opentelemetry::Context>,
        task_name: &str,
        mut task_context: TaskContext,
    ) -> Result<TaskContext, Box<workflow::Error>> {
        let script_config = &self.task.script;

        // Extract language from enum variant
        let language_str = match script_config {
            workflow::ScriptConfiguration::Variant0 { language, .. } => language,
            workflow::ScriptConfiguration::Variant1 { language, .. } => language,
        };

        // Validate language support
        let language = match ValidatedLanguage::from_str(language_str) {
            Ok(lang) => lang,
            Err(e) => {
                let pos = task_context.position.read().await;
                return Err(workflow::errors::ErrorFactory::new().bad_argument(
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
                return Err(workflow::errors::ErrorFactory::new().not_implemented(
                    "JavaScript script execution".to_string(),
                    Some(pos.as_error_instance()),
                    Some("JavaScript support will be added in Phase 2".to_string()),
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

        // Extract Python-specific settings from metadata
        let python_settings = bail_with_position!(
            task_context,
            PythonScriptSettings::from_metadata(&self.metadata),
            bad_argument,
            "Failed to parse Python settings from metadata"
        );

        // Convert to PYTHON_COMMAND arguments (with runtime expression evaluation)
        let args = bail_with_position!(
            task_context,
            self.to_python_command_args(script_config, &task_context, &expression)
                .await,
            bad_argument,
            "Failed to prepare script arguments"
        );

        // Convert to PYTHON_COMMAND settings
        let settings = bail_with_position!(
            task_context,
            self.to_python_runner_settings(&python_settings),
            bad_argument,
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
                return Err(workflow::errors::ErrorFactory::new().service_unavailable(
                    format!("{} runner not found", runner_name),
                    Some(pos.as_error_instance()),
                    None,
                ));
            }
            Err(e) => {
                let pos = task_context.position.read().await;
                return Err(workflow::errors::ErrorFactory::new().service_unavailable(
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
            internal_error,
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
            internal_error,
            "Failed to encode script arguments to protobuf"
        );

        // Extract language-agnostic use_static setting from metadata
        let use_static = self
            .metadata
            .get("script.use_static")
            .and_then(|v| v.parse::<bool>().ok())
            .unwrap_or(false);

        // Create temporary worker for script execution
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

        // Find or create worker for script execution
        let worker_id = if worker_data.use_static {
            bail_with_position!(
                task_context,
                self.job_executor_wrapper
                    .find_or_create_worker(&worker_data)
                    .await,
                service_unavailable,
                "Failed to find or create worker"
            )
            .id
        } else {
            None
        };

        // Enqueue job with protobuf binary arguments directly
        let (_job_id, job_result, _stream) = bail_with_position!(
            task_context,
            self.job_executor_wrapper
                .enqueue_with_worker_or_temp(
                    self.metadata.clone(),
                    worker_id,
                    worker_data,
                    args_bytes, // Use protobuf binary directly, not JSON
                    None,       // No unique key
                    self.default_task_timeout.as_secs() as u32,
                    false, // No streaming
                )
                .await,
            service_unavailable,
            "Failed to enqueue script execution job"
        );

        // Extract job result output
        let output_bytes = bail_with_position!(
            task_context,
            job_result
                .ok_or_else(|| anyhow!("Job result not found"))
                .and_then(|res| self.job_executor_wrapper.extract_job_result_output(res)),
            internal_error,
            "Failed to extract job result output"
        );

        // Decode PYTHON_COMMAND result from protobuf binary
        let result: PythonCommandResult = bail_with_position!(
            task_context,
            prost::Message::decode(output_bytes.as_slice()),
            internal_error,
            "Failed to decode script result"
        );

        // Check exit code
        if result.exit_code != 0 {
            let error_detail = format!(
                "Script exited with code {}\nstdout: {}\nstderr: {}",
                result.exit_code,
                result.output,
                result.output_stderr.as_deref().unwrap_or("")
            );
            let pos = task_context.position.read().await;
            return Err(workflow::errors::ErrorFactory::new().internal_error(
                "Script execution failed".to_string(),
                Some(pos.as_error_instance()),
                Some(error_detail),
            ));
        }

        // Parse script output as JSON
        let script_output: serde_json::Value = serde_json::from_str(&result.output)
            .unwrap_or_else(|_| serde_json::Value::String(result.output.clone()));

        task_context.set_raw_output(script_output);

        // NOTE: Do NOT remove position here. The parent TaskExecutor (task.rs:390) will handle
        // the position cleanup after this method returns.
        // task_context.remove_position().await;

        Ok(task_context)
    }
}
