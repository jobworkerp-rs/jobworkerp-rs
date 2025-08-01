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
    fn job_args_proto(&self) -> String;
    fn result_output_proto(&self) -> Option<String>;
    // run(), run_stream() availability
    fn output_type(&self) -> StreamingOutputType;
    // for json schema validation in the workflow API
    fn settings_schema(&self) -> String;
    fn arguments_schema(&self) -> String;
    fn output_schema(&self) -> Option<String>;
}

#[async_trait]
pub trait RunnerTrait: RunnerSpec + Send + Sync {
    async fn load(&mut self, settings: Vec<u8>) -> Result<()>;
    async fn run(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>);
    // only implement for stream runner (output_as_stream() == true)
    async fn run_stream(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
    ) -> Result<BoxStream<'static, ResultOutputItem>>;
}
