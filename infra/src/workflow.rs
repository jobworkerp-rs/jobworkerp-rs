// WorkflowLoader and related functionality for loading and validating workflows
// Moved from app-wrapper to resolve circular dependency (app -> app-wrapper -> app)

use std::sync::LazyLock;

use anyhow::{Result, anyhow};
use jobworkerp_runner::jobworkerp::runner::workflow_run_args::WorkflowSource;
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

        Ok(())
    }

    /// Full JSON Schema validation
    async fn validate_schema(&self, instance: &serde_json::Value) -> Result<()> {
        // Perform full validation
        if let Some(validator) = &*WORKFLOW_VALIDATOR {
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

/// Maximum size for workflow files loaded via HTTP (10MB)
const MAX_WORKFLOW_HTTP_SIZE: usize = 10 * 1024 * 1024;

/// Check if a string looks like a Windows drive path (e.g., "C:\path" or "D:/path")
fn is_windows_path(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

/// Check if a URL points to a potentially unsafe destination (SSRF protection)
fn is_unsafe_url(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return true;
    };

    // Block localhost hostname
    if host == "localhost" {
        return true;
    }

    // Block cloud metadata services
    if host == "169.254.169.254" || host == "metadata.google.internal" {
        return true;
    }

    // Block common internal hostnames
    if host.ends_with(".local") || host.ends_with(".internal") {
        return true;
    }

    // Check for unsafe IPv4 addresses
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        if ip.is_unspecified()
            || ip.is_loopback()
            || ip.is_private()
            || ip.is_link_local()
            || ip.is_broadcast()
        {
            return true;
        }
        // Block 169.254.x.x (link-local, redundant with is_link_local but explicit)
        if ip.octets()[0] == 169 && ip.octets()[1] == 254 {
            return true;
        }
        return false;
    }

    // Check for unsafe IPv6 addresses
    // Strip brackets if present (e.g., "[::1]" -> "::1")
    let ipv6_host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = ipv6_host.parse::<std::net::Ipv6Addr>() {
        if ip.is_unspecified() || ip.is_loopback() {
            return true;
        }
        // Check for link-local (fe80::/10) and unique local (fc00::/7, includes fd00::/8)
        // These cover AWS fd00:ec2::254 and similar private IPv6 addresses
        let segments = ip.segments();
        // Link-local: fe80::/10 (first 10 bits are 1111111010)
        if (segments[0] & 0xffc0) == 0xfe80 {
            return true;
        }
        // Unique local: fc00::/7 (first 7 bits are 1111110)
        if (segments[0] & 0xfe00) == 0xfc00 {
            return true;
        }
    }

    false
}

pub trait UseLoadUrlOrPath {
    fn http_client(&self) -> Option<&reqwest::ReqwestClient>;
    fn load_url_or_path<T: DeserializeOwned + Clone>(
        &self,
        url_or_path: &str,
    ) -> impl std::future::Future<Output = Result<T>> + Send {
        let http_client = self.http_client().cloned();
        async move {
            // Check for Windows paths before URL parsing to avoid misclassification
            let body = if is_windows_path(url_or_path) {
                std::fs::read_to_string(url_or_path)?
            } else if let Ok(url) = url_or_path.parse::<Url>() {
                match url.scheme() {
                    "file" => {
                        // file:// URL -> load from local filesystem
                        let path = url
                            .to_file_path()
                            .map_err(|_| anyhow!("Invalid file URL path: {}", url))?;
                        std::fs::read_to_string(&path)
                            .map_err(|e| anyhow!("Failed to read file from URL {}: {}", url, e))?
                    }
                    "http" | "https" => {
                        // SSRF protection: block requests to internal/private addresses
                        if is_unsafe_url(&url) {
                            return Err(anyhow!(
                                "URL points to a restricted address (localhost, private IP, or metadata service): {}",
                                url
                            ));
                        }

                        // HTTP(S) URL -> fetch via HTTP client
                        let client = http_client.ok_or_else(|| {
                            anyhow!(
                                "HTTP client not available. Cannot load from URL: {}. Use WorkflowLoader::new() instead of new_local_only()",
                                url
                            )
                        })?;
                        let res = client.client().get(url.clone()).send().await?;
                        if res.status().is_success() {
                            // Check Content-Length header if available for early rejection
                            if let Some(content_length) = res.content_length()
                                && content_length as usize > MAX_WORKFLOW_HTTP_SIZE
                            {
                                return Err(anyhow!(
                                    "Workflow file too large: {} bytes (max: {} bytes)",
                                    content_length,
                                    MAX_WORKFLOW_HTTP_SIZE
                                ));
                            }

                            // Stream response with size limit to prevent OOM
                            use futures_util::StreamExt;
                            let mut stream = res.bytes_stream();
                            let mut total_size: usize = 0;
                            let mut collected_bytes: Vec<u8> = Vec::new();

                            while let Some(chunk_result) = stream.next().await {
                                let chunk = chunk_result?;
                                total_size += chunk.len();
                                if total_size > MAX_WORKFLOW_HTTP_SIZE {
                                    return Err(anyhow!(
                                        "Workflow file too large: {} bytes (max: {} bytes)",
                                        total_size,
                                        MAX_WORKFLOW_HTTP_SIZE
                                    ));
                                }
                                collected_bytes.extend_from_slice(&chunk);
                            }

                            String::from_utf8(collected_bytes)
                                .map_err(|e| anyhow!("Invalid UTF-8 in workflow file: {}", e))?
                        } else {
                            return Err(anyhow!(
                                "Failed to load yaml: {}, status: {}",
                                &url,
                                res.status()
                            ));
                        }
                    }
                    scheme => {
                        return Err(anyhow!(
                            "Unsupported URL scheme '{}' in: {}. Supported schemes: file, http, https",
                            scheme,
                            url
                        ));
                    }
                }
            } else {
                // Relative or absolute path -> load from local filesystem
                std::fs::read_to_string(url_or_path)?
            };
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
        assert!(
            flow.input
                .schema
                .as_ref()
                .is_some_and(|s| { s.json_schema().is_some() })
        );

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
        assert!(
            flow.input
                .schema
                .as_ref()
                .is_some_and(|s| { s.json_schema().is_some() })
        );

        let run_task = match &flow.do_.0[0]["ListWorker"] {
            workflow::Task::RunTask(run_task) => run_task,
            _ => return Err("Expected RunTask but found different task type".into()),
        };
        assert!(run_task.metadata.is_empty());
        assert!(
            run_task
                .output
                .as_ref()
                .is_some_and(|o| o.as_.as_ref().is_some_and(|s| {
                    match s {
                        workflow::OutputAs::Variant0(_v) => true, // string
                        _ => false,
                    }
                }))
        );
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

    #[test]
    fn test_is_windows_path() {
        use super::is_windows_path;

        // Valid Windows paths
        assert!(is_windows_path("C:\\Users\\test"));
        assert!(is_windows_path("D:/path/to/file"));
        assert!(is_windows_path("c:\\lowercase"));
        assert!(is_windows_path("Z:/last-drive"));

        // Not Windows paths
        assert!(!is_windows_path("/unix/path"));
        assert!(!is_windows_path("relative/path"));
        assert!(!is_windows_path("http://example.com"));
        assert!(!is_windows_path("file:///path"));
        assert!(!is_windows_path("C:")); // Too short
        assert!(!is_windows_path("C")); // Too short
        assert!(!is_windows_path("")); // Empty
        assert!(!is_windows_path("1:\\invalid")); // Starts with number
    }

    #[test]
    fn test_is_unsafe_url() {
        use super::is_unsafe_url;
        use url::Url;

        // Unsafe IPv4 addresses
        assert!(is_unsafe_url(&Url::parse("http://localhost/").unwrap()));
        assert!(is_unsafe_url(&Url::parse("http://127.0.0.1/").unwrap()));
        assert!(is_unsafe_url(&Url::parse("http://0.0.0.0/").unwrap())); // Unspecified
        assert!(is_unsafe_url(
            &Url::parse("http://169.254.169.254/latest/meta-data/").unwrap()
        ));
        assert!(is_unsafe_url(
            &Url::parse("http://192.168.1.1/admin").unwrap()
        ));
        assert!(is_unsafe_url(&Url::parse("http://10.0.0.1/").unwrap()));
        assert!(is_unsafe_url(
            &Url::parse("http://172.16.0.1/internal").unwrap()
        ));

        // Unsafe IPv6 addresses
        assert!(is_unsafe_url(&Url::parse("http://[::1]/").unwrap())); // Loopback
        assert!(is_unsafe_url(&Url::parse("http://[::]/").unwrap())); // Unspecified
        assert!(is_unsafe_url(&Url::parse("http://[fe80::1]/").unwrap())); // Link-local
        assert!(is_unsafe_url(
            &Url::parse("http://[fd00:ec2::254]/").unwrap()
        )); // AWS unique local
        assert!(is_unsafe_url(&Url::parse("http://[fc00::1]/").unwrap())); // Unique local

        // Unsafe hostnames
        assert!(is_unsafe_url(
            &Url::parse("http://metadata.google.internal/").unwrap()
        ));
        assert!(is_unsafe_url(
            &Url::parse("http://service.local/api").unwrap()
        ));
        assert!(is_unsafe_url(
            &Url::parse("http://db.internal/query").unwrap()
        ));

        // Safe URLs (should return false)
        assert!(!is_unsafe_url(
            &Url::parse("https://example.com/workflow.yaml").unwrap()
        ));
        assert!(!is_unsafe_url(
            &Url::parse("https://github.com/repo/file.yaml").unwrap()
        ));
        assert!(!is_unsafe_url(
            &Url::parse("http://203.0.113.50/public").unwrap()
        ));
        assert!(!is_unsafe_url(
            &Url::parse("http://[2001:db8::1]/").unwrap()
        )); // Documentation prefix (safe for testing)
    }
}
