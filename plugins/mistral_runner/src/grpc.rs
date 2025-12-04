//! gRPC client for FunctionService
//!
//! Provides tool calling functionality via gRPC

pub mod client;

pub use client::generated;
pub use client::FunctionServiceClient;
