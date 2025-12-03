// WorkflowLoader and related functionality for loading and validating workflows
// Moved from app-wrapper to resolve circular dependency (app -> app-wrapper -> app)

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;

use anyhow::{anyhow, Result};
use jobworkerp_runner::jobworkerp::runner::inline_workflow_args::WorkflowSource;
use net_utils::net::reqwest::{self, ReqwestClient};
use serde::de::DeserializeOwned;
use url::Url;

// Submodule declarations (using parent.rs + parent/ directory structure, not mod.rs)
pub mod definition;
pub mod position;

// Re-export main types for convenience
pub use definition::transform;
pub use definition::workflow::errors::{ErrorCode, ErrorFactory};
pub use definition::workflow::{DoTask, Document, Error, Task, WorkflowSchema};
pub use position::WorkflowPosition;

// Constants
pub const RUNNER_SETTINGS_METADATA_LABEL: &str = "settings";
pub const WORKER_PARAMS_METADATA_LABEL: &str = "options";
pub const WORKER_NAME_METADATA_LABEL: &str = "name";

#[derive(Debug, Clone)]
pub struct WorkflowLoader {
    http_client: Option<reqwest::ReqwestClient>, // Private to prevent security setting bypass; None for local-only mode
}

// JSON Schema validator initialization
// Benchmark results (see tests/workflow_schema_validation_benchmark.rs and actual_workflow_validation_benchmark.rs):
// - Validator initialization: ~450ms (acceptable)
// - Simple workflow validation: ~500ms (acceptable)
// - Complex workflow validation: 55+ seconds (UNACCEPTABLE)
//
// Problem: jsonschema crate has exponential performance degradation with complex nested workflows
// containing loops, conditionals, and dynamic expressions.
//
// Solution: Keep validator initialized for future improvements in jsonschema crate,
// but use lightweight validation by default. Full schema validation can be enabled
// via environment variable for testing/debugging.
//
// Security: Plugin developers MUST validate their inputs independently.
// See CLAUDE.md for security guidelines.
static WORKFLOW_VALIDATOR: LazyLock<Option<jsonschema::Validator>> = LazyLock::new(|| {
    let schema_content = include_str!("../../runner/schema/workflow.yaml");
    let schema = serde_yaml::from_str(schema_content)
        .inspect_err(|e| tracing::error!("Failed to parse workflow schema: {:?}", e))
        .ok()?;

    // Direct initialization (no thread spawning overhead)
    jsonschema::draft202012::new(&schema)
        .inspect_err(|e| tracing::warn!("Failed to create workflow schema validator: {:?}", e))
        .ok()
});

// Flag to enable/disable full JSON Schema validation
// Default: false (use lightweight validation only)
// Set WORKFLOW_ENABLE_FULL_SCHEMA_VALIDATION=true to enable full validation
static ENABLE_FULL_SCHEMA_VALIDATION: LazyLock<bool> = LazyLock::new(|| {
    std::env::var("WORKFLOW_ENABLE_FULL_SCHEMA_VALIDATION")
        .map(|v| v.to_lowercase() == "true" || v == "1")
        .unwrap_or(false)
});

// Show security warning only once per process
static VALIDATION_WARNING_SHOWN: AtomicBool = AtomicBool::new(false);

impl WorkflowLoader {
    /// Create a WorkflowLoader with HTTP client for loading workflows from URLs
    pub fn new(http_client: ReqwestClient) -> Result<Self> {
        Ok(Self {
            http_client: Some(http_client),
        })
    }

    /// Create a WorkflowLoader without HTTP client (local file loading only)
    /// Attempting to load from URL will return an error
    pub fn new_local_only() -> Self {
        Self { http_client: None }
    }

    /// Get HTTP client if available
    /// Returns None if loader is in local-only mode
    pub fn http_client(&self) -> Option<&reqwest::ReqwestClient> {
        self.http_client.as_ref()
    }

    pub async fn load_workflow_source(
        &self,
        source: &WorkflowSource,
    ) -> Result<definition::workflow::WorkflowSchema> {
        match source {
            WorkflowSource::WorkflowUrl(url) => self
                .load_workflow(Some(url.as_str()), None, true)
                .await
                .map_err(|e| anyhow!("Failed to load workflow from url={} , err: {}", url, e)),
            WorkflowSource::WorkflowData(data) => self
                .load_workflow(None, Some(data.as_str()), true)
                .await
                .map_err(|e| anyhow!("Failed to load workflow from json={} , err: {}", data, e)),
        }
    }

    /// Lightweight structural validation for workflow
    /// Checks essential fields without full JSON Schema validation
    fn validate_lightweight(wf: &definition::workflow::WorkflowSchema) -> Result<()> {
        if wf.document.namespace.is_empty() {
            return Err(anyhow!("Workflow document.namespace must not be empty"));
        }
        if wf.document.name.is_empty() {
            return Err(anyhow!("Workflow document.name must not be empty"));
        }

        if wf.do_.0.is_empty() {
            return Err(anyhow!(
                "Workflow must have at least one task in 'do' section"
            ));
        }

        // Show warning only on first invocation (improved from original implementation)
        if !VALIDATION_WARNING_SHOWN.swap(true, Ordering::SeqCst) {
            tracing::warn!(
                "⚠️  SECURITY WARNING: Full JSON Schema validation is disabled for performance. \
                 Plugin developers MUST validate their inputs independently to prevent security issues. \
                 Complex workflows may contain malicious inputs that bypass lightweight validation. \
                 See documentation for input validation best practices. \
                 (This warning is shown only once per process)"
            );
        }

        Ok(())
    }

    /// Full JSON Schema validation (expensive, disabled by default)
    async fn validate_schema(&self, instance: &serde_json::Value) -> Result<()> {
        if !*ENABLE_FULL_SCHEMA_VALIDATION {
            tracing::debug!(
                "Full JSON Schema validation is disabled. \
                 Set WORKFLOW_ENABLE_FULL_SCHEMA_VALIDATION=true to enable. \
                 Using lightweight validation instead."
            );
            return Ok(());
        }

        // Perform full validation if enabled
        if let Some(validator) = &*WORKFLOW_VALIDATOR {
            tracing::warn!(
                "Full JSON Schema validation is ENABLED and may take significant time for complex workflows. \
                 Consider disabling for production use."
            );

            let mut error_details = Vec::new();
            for error in validator.iter_errors(instance) {
                error_details.push(format!("Path: {}, Message: {}", error.instance_path, error));
            }
            if error_details.is_empty() {
                Ok(())
            } else {
                tracing::error!(
                    "Workflow schema validation failed: {}",
                    error_details.join("; ")
                );
                Err(anyhow!(
                    "Failed to validate workflow schema: errors: {}",
                    error_details.join("; ")
                ))
            }
        } else {
            tracing::warn!("Workflow schema validator is not initialized, skipping validation");
            Ok(())
        }
    }

    pub async fn load_workflow(
        &self,
        url_or_path: Option<&str>,
        json_or_yaml_data: Option<&str>,
        validate: bool,
    ) -> Result<definition::workflow::WorkflowSchema> {
        let wf = if let Some(url_or_path) = url_or_path {
            tracing::debug!("workflow url_or_path: {}", url_or_path);
            let json = self
                .load_url_or_path::<serde_json::Value>(url_or_path)
                .await?;

            // validate schema
            if validate {
                let _ = self.validate_schema(&json).await;
            }
            // convert to workflow schema
            serde_json::from_value(json).map_err(|e| {
                anyhow!(
                    "Failed to parse workflow schema from url_or_path: {}, error: {}",
                    url_or_path,
                    e
                )
            })
        } else if let Some(data) = json_or_yaml_data {
            tracing::debug!("workflow string_data: {}", data);
            // Try to parse as complete WorkflowSchema first
            match serde_json::from_str::<definition::workflow::WorkflowSchema>(data)
                .or_else(|_| serde_yaml::from_str::<definition::workflow::WorkflowSchema>(data))
            {
                Ok(wf) => {
                    // Successfully parsed as WorkflowSchema
                    tracing::debug!(
                        "Parsed as complete WorkflowSchema with name: {}",
                        wf.document.name.as_str()
                    );
                    Ok(wf)
                }
                Err(e) => {
                    // Fallback: load as do_ task list (json or yaml) for backward compatibility
                    tracing::debug!(
                        "Falling back to DoTask parsing. WorkflowSchema parse error: {}",
                        e
                    );
                    let do_task: definition::workflow::DoTask =
                        serde_json::from_str(data).or_else(|_| serde_yaml::from_str(data))?;
                    Ok(definition::workflow::WorkflowSchema {
                        checkpointing: None,
                        do_: do_task.do_,
                        input: do_task.input.unwrap_or_default(),
                        output: do_task.output,
                        document: definition::workflow::Document::default(),
                    })
                }
            }
        } else {
            Err(anyhow!("url_or_path or json_or_yaml_data is required"))
        }?;

        // Perform lightweight structural validation
        Self::validate_lightweight(&wf)?;

        Ok(wf)
    }
}

pub trait UseLoadUrlOrPath {
    fn http_client(&self) -> Option<&reqwest::ReqwestClient>;
    fn load_url_or_path<T: DeserializeOwned + Clone>(
        &self,
        url_or_path: &str,
    ) -> impl std::future::Future<Output = Result<T>> + Send {
        let http_client = self.http_client().cloned();
        async move {
            let body = if let Ok(url) = url_or_path.parse::<Url>() {
                // HTTP client is required for URL loading
                let client = http_client.ok_or_else(|| {
                    anyhow!(
                        "HTTP client not available. Cannot load from URL: {}. Use WorkflowLoader::new() instead of new_local_only()",
                        url
                    )
                })?;
                let res = client.client().get(url.clone()).send().await?;
                if res.status().is_success() {
                    let body = res.text().await?;
                    Ok(body)
                } else {
                    Err(anyhow!(
                        "Failed to load yaml: {}, status: {}",
                        &url,
                        res.status()
                    ))
                }
            } else {
                // TODO name reference (inner yaml, public catalog)
                let body = std::fs::read_to_string(url_or_path)?;
                Ok(body)
            }?;
            let yaml: T = serde_json::from_str(&body).or_else(|e1| {
                serde_yaml::from_str(&body).map_err(|e2| {
                    anyhow!(
                        "Failed to parse data as yaml or json: {}\nas json: {:?}\nas yaml: {:?}",
                        url_or_path,
                        e1,
                        e2
                    )
                })
            })?;
            Ok(yaml)
        }
    }
}

impl UseLoadUrlOrPath for WorkflowLoader {
    fn http_client(&self) -> Option<&reqwest::ReqwestClient> {
        self.http_client.as_ref()
    }
}

// Dependency injection trait for WorkflowLoader
pub trait UseWorkflowLoader {
    fn workflow_loader(&self) -> &WorkflowLoader;
}

#[cfg(test)]
mod test {
    use crate::workflow::definition::workflow::{self, RetryPolicy};

    // parse example flow yaml
    // Note: These tests require test-files/ directory which is in app-wrapper
    // They are kept for documentation but ignored in infra crate
    #[tokio::test]
    #[ignore]
    async fn test_parse_example_switch_yaml() -> Result<(), Box<dyn std::error::Error>> {
        // command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
        let loader = super::WorkflowLoader::new_local_only();
        let flow = loader
            .load_workflow(Some("test-files/switch.yaml"), None, true)
            .await?;
        println!("{flow:#?}");
        assert_eq!(
            flow.document.title,
            Some("Workflow test (switch)".to_string())
        );
        assert_eq!(flow.document.name.as_str(), "switch-test");
        assert!(flow
            .input
            .schema
            .as_ref()
            .is_some_and(|s| { s.json_schema().is_some() }));

        Ok(())
    }

    // parse example flow yaml
    #[tokio::test]
    #[ignore]
    async fn test_parse_example_flow_yaml() -> Result<(), Box<dyn std::error::Error>> {
        // command_utils::util::tracing::tracing_init_test(Level::DEBUG);
        let loader = super::WorkflowLoader::new_local_only();
        let flow = loader
            .load_workflow(Some("test-files/ls-test.yaml"), None, true)
            .await?;
        println!("{flow:#?}");
        assert_eq!(flow.document.title, Some("Workflow test (ls)".to_string()));
        assert_eq!(flow.document.name.as_str(), "ls-test");
        assert!(flow
            .input
            .schema
            .as_ref()
            .is_some_and(|s| { s.json_schema().is_some() }));

        let run_task = match &flow.do_.0[0]["ListWorker"] {
            workflow::Task::RunTask(run_task) => run_task,
            _ => return Err("Expected RunTask but found different task type".into()),
        };
        assert!(run_task.metadata.is_empty());
        assert!(run_task
            .output
            .as_ref()
            .is_some_and(|o| o.as_.as_ref().is_some_and(|s| {
                match s {
                    workflow::OutputAs::Variant0(_v) => true, // string
                    _ => false,
                }
            })));
        assert!(run_task.input.as_ref().is_none());
        if let workflow::RunTaskConfiguration::Runner(workflow::RunRunner {
            runner:
                workflow::RunJobRunner {
                    arguments,
                    name: runner_name,
                    options,
                    settings,
                    ..
                },
            ..
        }) = run_task.run.clone()
        {
            assert_eq!(runner_name, "COMMAND".to_string());
            assert_eq!(
                serde_json::Value::Object(arguments),
                serde_json::json!({
                    "command": "ls",
                    "args": ["${.}"]
                })
            );
            assert_eq!(serde_json::Value::Object(settings), serde_json::json!({}));
            let opts = workflow::WorkerOptions {
                channel: Some("workflow".to_string()),
                store_failure: Some(true),
                store_success: Some(true),
                use_static: Some(false),
                retry: Some(RetryPolicy {
                    backoff: Some(workflow::RetryBackoff::Exponential(serde_json::Map::new())),
                    delay: Some(workflow::Duration::Inline {
                        seconds: Some(2),
                        days: None,
                        hours: None,
                        milliseconds: None,
                        minutes: None,
                    }),
                    limit: Some(workflow::RetryLimit {
                        attempt: Some(workflow::RetryLimitAttempt {
                            count: Some(3),
                            duration: None,
                        }),
                    }),
                }),
                ..Default::default()
            };
            assert_eq!(options, Some(opts));
        } else {
            return Err(
                "Expected RunTaskConfiguration::Variant1 but found different configuration".into(),
            );
        }
        let _for_task = match &flow.do_.0[1]["EachFileIteration"] {
            workflow::Task::ForTask(for_task) => for_task,
            f => return Err(format!("unexpected task type: {f:#?}").into()),
        };
        // println!("====FOR: {:#?}", _for_task);

        Ok(())
    }
}
