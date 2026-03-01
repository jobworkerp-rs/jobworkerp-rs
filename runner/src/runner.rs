//! This module provides traits and implementations for different types of job runners.
//!
//! # Overview
//!
//! The runner module defines the core interfaces for job execution in the jobworkerp system:
//!
//! * [`RunnerSpec`] - Defines the basic specification and metadata for a runner
//! * [`RunnerTrait`] - Extends the spec with execution capabilities
//!
//! # Available Runners
//!
//! The module contains several runner implementations for different execution environments:
//!
//! * [`command`] - Runs shell commands
//! * [`docker`] - Executes jobs in Docker containers
//! * [`grpc_unary`] - Runs jobs via gRPC unary calls
//! * [`k8s_job`] - Manages Kubernetes jobs
//! * [`llm`] - Integrates with LLM (Large Language Model) APIs
//! * [`mcp`] - Manages jobs in a multi-cluster environment
//! * [`plugins`] - Support for plugin-based runners
//! * [`python`] - Python script execution
//! * [`request`] - HTTP request-based jobs
//! * [`slack`] - Slack integration
//! * [`workflow`] - Reusable and inline workflow runners
//!
//! # Trait Documentation
//!
//! ## RunnerSpec
//!
//! Provides basic metadata about a runner implementation including its name,
//! serialization formats, and streaming capabilities.
//!
//! ## RunnerTrait
//!
//! Defines the core execution interface for job runners, including methods
//! for initialization, job execution (both streaming and non-streaming), and
//! job cancellation.
use std::any::Any;
use std::collections::HashMap;
use std::pin::Pin;

use anyhow::Result;
use futures::stream::BoxStream;
use proto::jobworkerp::data::ResultOutputItem;
use tokio::sync::mpsc;
use tonic::async_trait;

/// Feed data sent from client to a running streaming job
#[derive(Debug, Clone)]
pub struct FeedData {
    pub data: Vec<u8>,
    pub is_final: bool,
}

/// Type alias for the boxed future returned by `collect_stream`.
/// This reduces type complexity and improves readability.
pub type CollectStreamFuture =
    Pin<Box<dyn std::future::Future<Output = Result<(Vec<u8>, HashMap<String, String>)>> + Send>>;

pub mod cancellation;
pub mod cancellation_helper;
pub mod command;
pub mod create_workflow;
pub mod docker;
pub mod factory;
pub mod grpc_unary;
pub mod k8s_job;
pub mod llm;
pub mod llm_chat;
pub mod llm_unified;
pub mod mcp;
pub mod plugins;
pub mod python;
pub mod request;
pub mod slack;
pub mod timeout_config;
pub mod workflow;
pub mod workflow_unified;

#[cfg(any(test, feature = "test-utils"))]
pub mod test_common;

/// Macro to convert a Rust type to a JSON schema string
#[macro_export]
macro_rules! schema_to_json_string {
    ($type:ty, $method_name:expr_2021) => {{
        let schema = schemars::schema_for!($type);
        match serde_json::to_string(&schema) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("error in {}: {:?}", $method_name, e);
                "".to_string()
            }
        }
    }};
}

/// Macro to convert a Rust type to an Option<String> JSON schema
#[macro_export]
macro_rules! schema_to_json_string_option {
    ($type:ty, $method_name:expr_2021) => {{
        let schema = schemars::schema_for!($type);
        match serde_json::to_string(&schema) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::error!("error in {}: {:?}", $method_name, e);
                None
            }
        }
    }};
}

pub trait RunnerSpec: Send + Sync + Any {
    fn name(&self) -> String;
    // only implement for stream runner (output_as_stream() == true)
    fn runner_settings_proto(&self) -> String;

    /// Returns the method protobuf schema map for all runners
    /// - Key: method name (e.g., DEFAULT_METHOD_NAME ("run") for single-method runners, tool names for MCP/Plugin)
    /// - Value: MethodSchema (input schema, output schema, description, output_type)
    /// - Single-method runners: use default method name DEFAULT_METHOD_NAME ("run")
    /// - MCP/Plugin runners: use tool-specific method names
    fn method_proto_map(
        &self,
    ) -> std::collections::HashMap<String, proto::jobworkerp::data::MethodSchema>;

    /// Returns a map of method names to their JSON Schemas.
    /// - Key: method name (e.g., DEFAULT_METHOD_NAME ("run") for single-method runners, tool names for MCP/Plugin)
    /// - Value: MethodJsonSchema (args_schema, result_schema)
    ///
    /// **Default implementation**: Automatically converts method_proto_map() to JSON Schema.
    /// Plugin developers only need to implement method_proto_map(), and JSON Schema will be auto-generated.
    ///
    /// **Explicit implementation required for**:
    /// - MCP Server: Use existing JSON Schema from tools
    /// - Runners with custom JSON Schema optimization
    fn method_json_schema_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodJsonSchema> {
        // Default implementation: Convert method_proto_map() to JSON Schema
        proto::jobworkerp::data::MethodJsonSchema::from_proto_map(self.method_proto_map())
    }

    /// JSON schema methods for Workflow API validation
    ///
    /// **DEPRECATED**: Use method_json_schema_map() instead.
    ///
    /// **IMPORTANT**: These methods are for **normal runners only** (non-using runners).
    /// For using-based runners (MCP Server, Plugin with multiple methods):
    /// - Return empty string "{}" or minimal schema
    /// - Actual tool-specific schemas are provided via RunnerWithSchema.tools field
    ///
    /// Example:
    /// - Normal runner (COMMAND): returns actual schema
    /// - MCP Server: returns "{}" (schemas in tools field)
    /// - Plugin with using: returns "{}" (schemas in tools field)
    fn settings_schema(&self) -> String;

    /// Collect streaming output into a single result
    ///
    /// This method collects all items from a streaming output and combines them
    /// into a single result. The default implementation concatenates all data chunks.
    ///
    /// # Arguments
    /// * `stream` - BoxStream of ResultOutputItem from run_stream()
    /// * `using` - Optional method name for multi-method runners (LLM, WORKFLOW)
    ///
    /// # Returns
    /// Tuple of (collected_bytes, metadata_from_trailer)
    ///
    /// # Override
    /// Runners should override this method if they need custom collection logic:
    /// - COMMAND: Merge stdout/stderr/exit_code from CommandResult chunks
    /// - HTTP_REQUEST: Merge body chunks with final status/headers
    /// - MCP_SERVER: Merge McpServerResult contents (TextContent concatenation)
    /// - LLM: Merge text tokens with final tool_calls/usage (method-specific)
    /// - WORKFLOW: Keep last result for run, non-streaming for create
    ///
    /// Default implementation: keeps only the last Data item (memory efficient).
    /// Override this method for runners that need to merge/concatenate stream data.
    fn collect_stream(
        &self,
        stream: BoxStream<'static, ResultOutputItem>,
        _using: Option<&str>,
    ) -> CollectStreamFuture {
        use futures::StreamExt;
        use proto::jobworkerp::data::result_output_item;

        Box::pin(async move {
            let mut last_data: Option<Vec<u8>> = None;
            let mut metadata = HashMap::new();
            let mut stream = stream;

            while let Some(item) = stream.next().await {
                match item.item {
                    Some(result_output_item::Item::Data(data)) => {
                        // Keep only the last data (memory efficient for most runners)
                        last_data = Some(data);
                    }
                    Some(result_output_item::Item::End(trailer)) => {
                        metadata = trailer.metadata;
                        break;
                    }
                    Some(result_output_item::Item::FinalCollected(data)) => {
                        // FinalCollected already contains the collected data
                        return Ok((data, metadata));
                    }
                    None => {}
                }
            }
            Ok((last_data.unwrap_or_default(), metadata))
        })
    }
}

#[async_trait]
pub trait RunnerTrait: RunnerSpec + Send + Sync {
    async fn load(&mut self, settings: Vec<u8>) -> Result<()>;

    /// Execute job with optional sub-method specification
    ///
    /// # Arguments
    /// * `arg` - Protobuf binary arguments
    /// * `metadata` - Job metadata
    /// * `using` - Optional sub-method name for MCP/Plugin runners
    ///   - For normal runners: None or ignored if Some
    ///   - For MCP/Plugin: Required for multi-tool runners, auto-selected for single-tool
    ///
    /// # Returns
    /// Tuple of (Result<output_bytes>, updated_metadata)
    async fn run(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
        using: Option<&str>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>);

    /// Execute job with streaming output
    ///
    /// # Arguments
    /// * `arg` - Protobuf binary arguments
    /// * `metadata` - Job metadata
    /// * `using` - Optional sub-method name (same semantics as run())
    async fn run_stream(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
        using: Option<&str>,
    ) -> Result<BoxStream<'static, ResultOutputItem>>;

    /// Whether this runner supports feed data for the given method
    fn supports_feed(&self, _using: Option<&str>) -> bool {
        false
    }

    /// Set up a feed channel for receiving data during streaming execution.
    /// Returns a Sender that the infrastructure layer will use to deliver feed data.
    fn setup_feed_channel(&mut self, _using: Option<&str>) -> Option<mpsc::Sender<FeedData>> {
        None
    }
}

// NOTE: UsingRunner trait has been removed.
// using is now passed directly to RunnerTrait::run() and run_stream() as Option<&str>.
// Normal runners should ignore the using parameter (use _using).
// MCP/Plugin runners should use using to select the appropriate tool/method.
