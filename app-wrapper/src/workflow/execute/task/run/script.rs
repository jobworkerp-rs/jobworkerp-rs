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
use once_cell::sync::Lazy;
use proto::jobworkerp::data::{QueueType, ResponseType, WorkerData};
use regex::Regex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

// Compile regex patterns once at startup for performance
static DANGEROUS_FUNC_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(eval|exec|compile|__import__|open|input|execfile)\s*\(")
        .expect("Invalid regex pattern for dangerous functions")
});

static SHELL_COMMAND_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(os\.system|subprocess\.|commands\.|popen)")
        .expect("Invalid regex pattern for shell commands")
});

static DUNDER_ACCESS_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"__[a-zA-Z_]+__").expect("Invalid regex pattern for dunder attributes")
});

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

/// Macro to reduce repetitive error handling in execute() method
///
/// Usage:
/// ```
/// let result = bail_with_position!(
///     task_context,
///     some_operation(),
///     bad_argument,
///     "Operation failed"
/// );
/// ```
macro_rules! bail_with_position {
    ($task_context:expr, $result:expr, $error_type:ident, $message:expr) => {
        match $result {
            Ok(val) => val,
            Err(e) => {
                let pos = $task_context.position.read().await;
                return Err(workflow::errors::ErrorFactory::new().$error_type(
                    $message.to_string(),
                    Some(pos.as_error_instance()),
                    Some(format!("{:?}", e)),
                ));
            }
        }
    };
    ($task_context:expr, $result:expr, $error_type:ident, $message:expr, $detail:expr) => {
        match $result {
            Ok(val) => val,
            Err(e) => {
                let pos = $task_context.position.read().await;
                return Err(workflow::errors::ErrorFactory::new().$error_type(
                    $message.to_string(),
                    Some(pos.as_error_instance()),
                    Some($detail),
                ));
            }
        }
    };
}

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

    /// Sanitize arguments to prevent code injection with enhanced security validation
    fn sanitize_python_variable(key: &str, value: &serde_json::Value) -> Result<()> {
        // Validate variable name
        if !Self::is_valid_python_identifier(key) {
            return Err(anyhow!(
                "Invalid Python variable name: '{}'. Must be alphanumeric with underscores only.",
                key
            ));
        }

        // Recursively validate all string values in the JSON structure
        Self::validate_value_recursive(value, 0)?;

        Ok(())
    }

    /// Recursively validate JSON values for security threats
    fn validate_value_recursive(value: &serde_json::Value, depth: usize) -> Result<()> {
        const MAX_DEPTH: usize = 10;
        if depth > MAX_DEPTH {
            return Err(anyhow!("Maximum nesting depth exceeded"));
        }

        match value {
            serde_json::Value::String(s) => Self::validate_string_content(s),
            serde_json::Value::Array(arr) => {
                for item in arr {
                    Self::validate_value_recursive(item, depth + 1)?;
                }
                Ok(())
            }
            serde_json::Value::Object(obj) => {
                for (k, v) in obj {
                    // Validate nested keys as potential Python identifiers
                    if !Self::is_valid_python_identifier(k) {
                        return Err(anyhow!(
                            "Invalid Python identifier in nested object key: '{}'",
                            k
                        ));
                    }
                    Self::validate_value_recursive(v, depth + 1)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Validate string content for dangerous patterns using regex
    fn validate_string_content(s: &str) -> Result<()> {
        // 1. Dangerous function calls (eval, exec, etc.) with flexible whitespace
        if DANGEROUS_FUNC_REGEX.is_match(s) {
            return Err(anyhow!(
                "Dangerous function call detected in argument value: matches pattern for eval/exec/compile/__import__/open/input/execfile"
            ));
        }

        // 2. Dunder attribute access (excluding safe common ones)
        if let Some(matched) = DUNDER_ACCESS_REGEX.find(s) {
            let dunder = matched.as_str();
            // Allow common safe patterns
            const SAFE_DUNDERS: &[&str] = &["__name__", "__doc__", "__version__", "__file__"];
            if !SAFE_DUNDERS.contains(&dunder) {
                return Err(anyhow!(
                    "Dunder attribute access not allowed in argument value: {}",
                    dunder
                ));
            }
        }

        // 3. Shell command execution patterns
        if SHELL_COMMAND_REGEX.is_match(s) {
            return Err(anyhow!(
                "Shell command execution pattern detected in argument value: matches os.system/subprocess/commands/popen"
            ));
        }

        Ok(())
    }

    /// Download script from external URL with comprehensive security validation
    async fn download_script_secure(uri: &str) -> Result<String> {
        // 1. URL schema validation
        let url = reqwest::Url::parse(uri).context("Invalid URL format")?;

        if url.scheme() != "https" {
            return Err(anyhow!(
                "Only HTTPS URLs are allowed for external scripts (got: {})",
                url.scheme()
            ));
        }

        // 2. Download with size limit and timeout
        const MAX_SCRIPT_SIZE: usize = 1024 * 1024; // 1MB
        const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);

        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(false) // Explicitly enable TLS verification
            .timeout(DOWNLOAD_TIMEOUT)
            .build()
            .context("Failed to build HTTP client")?;

        let response = client
            .get(uri)
            .send()
            .await
            .context(format!("Failed to download script from: {}", uri))?;

        if !response.status().is_success() {
            return Err(anyhow!(
                "Failed to download script: HTTP {} from {}",
                response.status(),
                uri
            ));
        }

        // 3. Content-Type validation (optional but recommended)
        if let Some(content_type) = response.headers().get("content-type") {
            let ct_str = content_type
                .to_str()
                .context("Invalid Content-Type header")?;

            if !ct_str.starts_with("text/")
                && !ct_str.contains("python")
                && !ct_str.contains("plain")
            {
                tracing::warn!(
                    "Unexpected Content-Type for script: {} (expected text/* or application/x-python)",
                    ct_str
                );
            }
        }

        // 4. Stream download with size limit
        let bytes = response
            .bytes()
            .await
            .context("Failed to read response body")?;

        if bytes.len() > MAX_SCRIPT_SIZE {
            return Err(anyhow!(
                "Script size exceeds limit: {} bytes (max: {} bytes)",
                bytes.len(),
                MAX_SCRIPT_SIZE
            ));
        }

        let content = String::from_utf8(bytes.to_vec()).context("Script contains invalid UTF-8")?;

        tracing::info!(
            "Downloaded external script from {} ({} bytes)",
            uri,
            content.len()
        );

        Ok(content)
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

        // Step 2: Inject evaluated arguments as global variables via Base64 encoding (secure)
        if let serde_json::Value::Object(ref args_map) = evaluated_args {
            if !args_map.is_empty() {
                script_code.push_str("# Injected arguments (runtime expressions evaluated)\n");
                script_code.push_str("# Security: Base64 encoding prevents code injection\n");
                script_code.push_str("import json\n");
                script_code.push_str("import base64\n\n");

                for (key, value) in args_map {
                    // Security validation
                    Self::sanitize_python_variable(key, value)?;

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
                let external_code = Self::download_script_secure(&uri).await?;
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

        let settings_value = bail_with_position!(
            task_context,
            serde_json::to_value(&settings),
            internal_error,
            "Failed to serialize runner settings"
        );

        let settings_bytes = bail_with_position!(
            task_context,
            self.job_executor_wrapper
                .setup_runner_and_settings(&runner, Some(settings_value))
                .await,
            service_unavailable,
            "Failed to setup runner settings"
        );

        let args_value = bail_with_position!(
            task_context,
            serde_json::to_value(&args),
            internal_error,
            "Failed to serialize script arguments"
        );

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

        let output = bail_with_position!(
            task_context,
            self.job_executor_wrapper
                .setup_worker_and_enqueue_with_json(
                    Arc::new(metadata),
                    runner_name,
                    worker_data,
                    args_value,
                    None, // No unique key
                    self.default_task_timeout.as_secs() as u32,
                    false, // No streaming
                )
                .await,
            service_unavailable,
            "Failed to execute script"
        );

        // Parse PYTHON_COMMAND result and extract output
        // Note: output is serde_json::Value, not Vec<u8>
        let output_bytes = bail_with_position!(
            task_context,
            serde_json::to_vec(&output),
            internal_error,
            "Failed to serialize output for decoding"
        );

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

        task_context.remove_position().await;

        Ok(task_context)
    }
}
