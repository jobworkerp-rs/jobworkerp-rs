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
use proto::jobworkerp::data::{ResultOutputItem, StreamingOutputType};
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

/// Macro to implement as_any() and as_any_mut() for RunnerSpec
/// Usage: impl_runner_as_any!(RunnerImplType);
#[macro_export]
macro_rules! impl_runner_as_any {
    ($runner_type:ty) => {
        impl $runner_type {
            #[inline]
            pub fn as_any(&self) -> &dyn std::any::Any {
                self
            }

            #[inline]
            pub fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }
    };
}

pub trait RunnerSpec: Send + Sync + Any {
    fn name(&self) -> String;
    // only implement for stream runner (output_as_stream() == true)
    fn runner_settings_proto(&self) -> String;

    /// Returns the job arguments protobuf schema for normal runners
    /// For sub-method runners (MCP/Plugin), returns empty string
    fn job_args_proto(&self) -> String;

    /// Returns the job arguments protobuf schema map for sub-method runners
    /// Key: sub_method name, Value: protobuf schema string
    /// For normal runners, returns None
    fn job_args_proto_map(&self) -> Option<std::collections::HashMap<String, String>> {
        None
    }

    fn result_output_proto(&self) -> Option<String>;
    // run(), run_stream() availability
    fn output_type(&self) -> StreamingOutputType;
    // for json schema validation in the workflow API
    fn settings_schema(&self) -> String;
    fn arguments_schema(&self) -> String;
    fn output_schema(&self) -> Option<String>;

    /// Returns the JSON schema for a specific sub-method (for Function layer)
    /// Default implementation returns an error for runners that don't support sub-methods
    fn get_sub_method_json_schema(&self, _sub_method: &str) -> Result<String> {
        Err(anyhow::anyhow!("This runner does not support sub_method"))
    }

    /// Provides access to Any trait for downcasting
    /// Default implementation uses self reference
    fn as_any(&self) -> &dyn Any
    where
        Self: 'static + Sized,
    {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any
    where
        Self: 'static + Sized,
    {
        self
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
    /// * `sub_method` - Optional sub-method name for MCP/Plugin runners
    ///   - For normal runners: None or ignored if Some
    ///   - For MCP/Plugin: Required for multi-tool runners, auto-selected for single-tool
    ///
    /// # Returns
    /// Tuple of (Result<output_bytes>, updated_metadata)
    async fn run(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
        sub_method: Option<&str>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>);

    /// Execute job with streaming output
    ///
    /// # Arguments
    /// * `arg` - Protobuf binary arguments
    /// * `metadata` - Job metadata
    /// * `sub_method` - Optional sub-method name (same semantics as run())
    async fn run_stream(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
        sub_method: Option<&str>,
    ) -> Result<BoxStream<'static, ResultOutputItem>>;
}

// NOTE: SubMethodRunner trait has been removed.
// sub_method is now passed directly to RunnerTrait::run() and run_stream() as Option<&str>.
// Normal runners should ignore the sub_method parameter (use _sub_method).
// MCP/Plugin runners should use sub_method to select the appropriate tool/method.
