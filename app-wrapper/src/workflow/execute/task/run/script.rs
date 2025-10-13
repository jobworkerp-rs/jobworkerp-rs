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
use jobworkerp_runner::jobworkerp::runner::{
    python_command_args, python_command_runner_settings, PythonCommandArgs, PythonCommandResult,
    PythonCommandRunnerSettings,
};
use proto::jobworkerp::data::{QueueType, ResponseType, WorkerData};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

/// Script task executor implementing Serverless Workflow v1.0.0 script process
pub struct ScriptTaskExecutor {
    workflow_context: Arc<RwLock<WorkflowContext>>,
    default_task_timeout: Duration,
    task: workflow::RunScript,
    job_executor_wrapper: Arc<JobExecutorWrapper>,
    metadata: Arc<HashMap<String, String>>,
}

impl UseExpression for ScriptTaskExecutor {}
impl UseJqAndTemplateTransformer for ScriptTaskExecutor {}
impl UseExpressionTransformer for ScriptTaskExecutor {}

impl ScriptTaskExecutor {
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

    /// Validate Python variable name against keywords and identifier rules
    fn is_valid_python_identifier(s: &str) -> bool {
        if s.is_empty() {
            return false;
        }

        // Must not start with digit
        if s.chars().next().unwrap().is_numeric() {
            return false;
        }

        // Must be alphanumeric or underscore
        if !s.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return false;
        }

        // Must not be Python keyword
        const PYTHON_KEYWORDS: &[&str] = &[
            "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
            "elif", "else", "except", "False", "finally", "for", "from", "global", "if", "import",
            "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return",
            "True", "try", "while", "with", "yield",
        ];
        !PYTHON_KEYWORDS.contains(&s)
    }

    /// Sanitize arguments to prevent code injection
    fn sanitize_python_variable(key: &str, value: &serde_json::Value) -> Result<()> {
        // Validate variable name
        if !Self::is_valid_python_identifier(key) {
            return Err(anyhow!(
                "Invalid Python variable name: '{}'. Must be alphanumeric with underscores only.",
                key
            ));
        }

        // Check for dangerous patterns in string values
        if let serde_json::Value::String(s) = value {
            const DANGEROUS_PATTERNS: &[&str] = &[
                "__import__",
                "eval(",
                "exec(",
                "compile(",
                "open(",
                "input(",
                "execfile(",
            ];

            for pattern in DANGEROUS_PATTERNS {
                if s.contains(pattern) {
                    return Err(anyhow!(
                        "Potentially malicious code detected in argument '{}': {}",
                        key,
                        pattern
                    ));
                }
            }
        }

        Ok(())
    }

    /// Download script from external URL
    async fn download_script(uri: &str) -> Result<String> {
        let response = reqwest::get(uri)
            .await
            .context(format!("Failed to download script from URL: {}", uri))?;

        if !response.status().is_success() {
            return Err(anyhow!(
                "Failed to download script: HTTP status {}",
                response.status()
            ));
        }

        response
            .text()
            .await
            .context("Failed to read script response as text")
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
        let evaluated_args =
            match Self::transform_map(task_context.input.clone(), arguments.clone(), expression) {
                Ok(args) => args,
                Err(e) => {
                    return Err(anyhow!("Failed to evaluate runtime expressions: {:?}", e));
                }
            };

        // Step 2: Inject evaluated arguments as global variables
        if let serde_json::Value::Object(ref args_map) = evaluated_args {
            if !args_map.is_empty() {
                script_code.push_str("# Injected arguments (runtime expressions evaluated)\n");
                script_code.push_str("import json\n");

                for (key, value) in args_map {
                    // Security validation
                    Self::sanitize_python_variable(key, value)?;

                    // Use triple-quoted string to safely embed JSON
                    let json_str = serde_json::to_string(value)?;
                    script_code.push_str(&format!(
                        "{} = json.loads('''{}''')\n",
                        key,
                        json_str.replace('\\', "\\\\").replace("'''", "\\'\\'\\'")
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
                let external_code = Self::download_script(&uri).await?;
                script_code.push_str(&external_code);
            }
        }

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
            python_settings
                .requirements_url
                .as_ref()
                .map(|url| python_command_runner_settings::RequirementsSpec::RequirementsUrl(url.clone()))
        };

        Ok(PythonCommandRunnerSettings {
            python_version: python_settings.version.clone(),
            uv_path: None, // Use default uv path
            requirements_spec,
        })
    }
}

impl TaskExecutorTrait<'_> for ScriptTaskExecutor {
    async fn execute(
        &self,
        cx: Arc<opentelemetry::Context>,
        task_name: &str,
        mut task_context: TaskContext,
    ) -> Result<TaskContext, Box<workflow::Error>> {
        task_context.add_position_name("script".to_string()).await;

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
        let python_settings = match PythonScriptSettings::from_metadata(&self.metadata) {
            Ok(settings) => settings,
            Err(e) => {
                let pos = task_context.position.read().await;
                return Err(workflow::errors::ErrorFactory::new().bad_argument(
                    "Failed to parse Python settings from metadata".to_string(),
                    Some(pos.as_error_instance()),
                    Some(format!("{:?}", e)),
                ));
            }
        };

        // Convert to PYTHON_COMMAND arguments (with runtime expression evaluation)
        let args = match self
            .to_python_command_args(script_config, &task_context, &expression)
            .await
        {
            Ok(args) => args,
            Err(e) => {
                let pos = task_context.position.read().await;
                return Err(workflow::errors::ErrorFactory::new().bad_argument(
                    "Failed to prepare script arguments".to_string(),
                    Some(pos.as_error_instance()),
                    Some(format!("{:?}", e)),
                ));
            }
        };

        // Convert to PYTHON_COMMAND settings
        let settings = match self.to_python_runner_settings(&python_settings) {
            Ok(settings) => settings,
            Err(e) => {
                let pos = task_context.position.read().await;
                return Err(workflow::errors::ErrorFactory::new().bad_argument(
                    "Failed to prepare script settings".to_string(),
                    Some(pos.as_error_instance()),
                    Some(format!("{:?}", e)),
                ));
            }
        };

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

        let settings_value = match serde_json::to_value(&settings) {
            Ok(v) => v,
            Err(e) => {
                let pos = task_context.position.read().await;
                return Err(workflow::errors::ErrorFactory::new().internal_error(
                    "Failed to serialize runner settings".to_string(),
                    Some(pos.as_error_instance()),
                    Some(format!("{:?}", e)),
                ));
            }
        };

        let settings_bytes = match self
            .job_executor_wrapper
            .setup_runner_and_settings(&runner, Some(settings_value))
            .await
        {
            Ok(bytes) => bytes,
            Err(e) => {
                let pos = task_context.position.read().await;
                return Err(workflow::errors::ErrorFactory::new().service_unavailable(
                    "Failed to setup runner settings".to_string(),
                    Some(pos.as_error_instance()),
                    Some(format!("{:?}", e)),
                ));
            }
        };

        let args_value = match serde_json::to_value(&args) {
            Ok(v) => v,
            Err(e) => {
                let pos = task_context.position.read().await;
                return Err(workflow::errors::ErrorFactory::new().internal_error(
                    "Failed to serialize script arguments".to_string(),
                    Some(pos.as_error_instance()),
                    Some(format!("{:?}", e)),
                ));
            }
        };

        // Extract language-agnostic use_static setting from metadata
        let use_static = self
            .metadata
            .get("script.use_static")
            .and_then(|v| v.parse::<bool>().ok())
            .unwrap_or(false);

        // Create temporary worker for script execution
        let worker_data = WorkerData {
            name: format!("script_{}", task_name),
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

        // Inject metadata from opentelemetry context
        let mut metadata = (*self.metadata).clone();
        super::RunTaskExecutor::inject_metadata_from_context(&mut metadata, &cx);

        let output = match self
            .job_executor_wrapper
            .setup_worker_and_enqueue_with_json(
                Arc::new(metadata),
                runner_name,
                worker_data,
                args_value,
                None, // No unique key
                self.default_task_timeout.as_secs() as u32,
                false, // No streaming
            )
            .await
        {
            Ok(output) => output,
            Err(e) => {
                let pos = task_context.position.read().await;
                return Err(workflow::errors::ErrorFactory::new().service_unavailable(
                    "Failed to execute script".to_string(),
                    Some(pos.as_error_instance()),
                    Some(format!("{:?}", e)),
                ));
            }
        };

        // Parse PYTHON_COMMAND result and extract output
        // Note: output is serde_json::Value, not Vec<u8>
        let output_bytes = match serde_json::to_vec(&output) {
            Ok(bytes) => bytes,
            Err(e) => {
                let pos = task_context.position.read().await;
                return Err(workflow::errors::ErrorFactory::new().internal_error(
                    "Failed to serialize output for decoding".to_string(),
                    Some(pos.as_error_instance()),
                    Some(format!("{:?}", e)),
                ));
            }
        };

        let result: PythonCommandResult = match prost::Message::decode(output_bytes.as_slice()) {
            Ok(result) => result,
            Err(e) => {
                let pos = task_context.position.read().await;
                return Err(workflow::errors::ErrorFactory::new().internal_error(
                    "Failed to decode script result".to_string(),
                    Some(pos.as_error_instance()),
                    Some(format!("{:?}", e)),
                ));
            }
        };

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

        task_context.remove_position().await;

        Ok(task_context)
    }
}
