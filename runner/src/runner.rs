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
//! * [`plugins`] - Support for plugin-based runners
//! * [`python`] - Python script execution
//! * [`request`] - HTTP request-based jobs
//! * [`simple_workflow`] - simple workflow runner
//! * [`slack`] - Slack integration
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
use anyhow::Result;
use futures::stream::BoxStream;
use proto::jobworkerp::data::{ResultOutputItem, StreamingOutputType};
use tonic::async_trait;

pub mod command;
pub mod docker;
pub mod factory;
pub mod grpc_unary;
pub mod k8s_job;
pub mod plugins;
pub mod python;
pub mod request;
pub mod simple_workflow;
pub mod slack;

pub trait RunnerSpec: Send + Sync {
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
    async fn run(&mut self, arg: &[u8]) -> Result<Vec<Vec<u8>>>;
    // only implement for stream runner (output_as_stream() == true)
    async fn run_stream(&mut self, arg: &[u8]) -> Result<BoxStream<'static, ResultOutputItem>>;
    async fn cancel(&mut self);
}
