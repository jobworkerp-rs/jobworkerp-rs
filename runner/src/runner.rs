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

use anyhow::Result;
use futures::stream::BoxStream;
use proto::jobworkerp::data::ResultOutputItem;
use tonic::async_trait;

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
pub mod mcp;
pub mod plugins;
pub mod python;
pub mod request;
pub mod slack;
pub mod timeout_config;
pub mod workflow;

#[cfg(any(test, feature = "test-utils"))]
pub mod test_common;

/// Macro to convert a Rust type to a JSON schema string
#[macro_export]
macro_rules! schema_to_json_string {
    ($type:ty, $method_name:expr) => {{
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
    ($type:ty, $method_name:expr) => {{
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

    /// Returns the method protobuf schema map for all runners (REQUIRED in Phase 6.6.4+)
    /// - Key: method name (e.g., "run" for single-method runners, tool names for MCP/Plugin)
    /// - Value: MethodSchema (input schema, output schema, description, output_type)
    /// - Single-method runners: use default method name "run"
    /// - MCP/Plugin runners: use tool-specific method names
    fn method_proto_map(
        &self,
    ) -> std::collections::HashMap<String, proto::jobworkerp::data::MethodSchema>;

    /// JSON schema methods for Workflow API validation
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
    fn arguments_schema(&self) -> String;
    fn output_schema(&self) -> Option<String>;
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
}

// NOTE: UsingRunner trait has been removed.
// using is now passed directly to RunnerTrait::run() and run_stream() as Option<&str>.
// Normal runners should ignore the using parameter (use _using).
// MCP/Plugin runners should use using to select the appropriate tool/method.
