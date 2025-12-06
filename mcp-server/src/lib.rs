//! MCP Server integration for jobworkerp-rs.
//!
//! This crate provides MCP (Model Context Protocol) server functionality,
//! allowing jobworkerp runners and workers to be exposed as MCP tools.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                      MCP Client                              │
//! │              (Claude Desktop, etc.)                          │
//! └─────────────────────┬───────────────────────────────────────┘
//!                       │ MCP Protocol (JSON-RPC)
//!                       │ via Stdio or Streamable HTTP
//! ┌─────────────────────▼───────────────────────────────────────┐
//! │                 mcp-server crate                             │
//! │  ┌─────────────────────────────────────────────────────────┐│
//! │  │ McpHandler (implements rmcp::ServerHandler)             ││
//! │  │  - list_tools() → FunctionApp::find_functions()         ││
//! │  │  - call_tool()  → FunctionApp::call_function_for_llm()  ││
//! │  └─────────────────────────────────────────────────────────┘│
//! └─────────────────────┬───────────────────────────────────────┘
//!                       │ Direct call (no gRPC overhead)
//! ┌─────────────────────▼───────────────────────────────────────┐
//! │                    app crate                                 │
//! │  ┌─────────────────────────────────────────────────────────┐│
//! │  │ FunctionAppImpl                                         ││
//! │  │  - find_functions()                                     ││
//! │  │  - call_function_for_llm()                              ││
//! │  └─────────────────────────────────────────────────────────┘│
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! ## Standalone Mode (all-in-one)
//!
//! ```ignore
//! use mcp_server::{McpHandler, McpServerConfig, boot_streamable_http_server};
//!
//! let handler = McpHandler::new(function_app, function_set_app, config);
//! boot_streamable_http_server(
//!     move || Ok(handler.clone()),
//!     "127.0.0.1:8000"
//! ).await?;
//! ```
//!
//! ## Environment Variables
//!
//! - `MCP_ENABLED`: Enable MCP server in all-in-one mode (default: false)
//! - `MCP_ADDR`: HTTP server bind address (default: 127.0.0.1:8000)
//! - `EXCLUDE_RUNNER_AS_TOOL`: Exclude runners from tools (default: false)
//! - `EXCLUDE_WORKER_AS_TOOL`: Exclude workers from tools (default: false)
//! - `TOOL_SET_NAME`: Expose only tools from specific FunctionSet
//! - `REQUEST_TIMEOUT_SEC`: Request timeout (default: 60)
//! - `MCP_STREAMING`: Enable streaming responses (default: true)
//! - `MCP_AUTH_ENABLED`: Enable Bearer authentication (default: false)
//! - `MCP_AUTH_TOKENS`: Valid tokens, comma-separated (default: demo-token)

pub mod config;
pub mod handler;
pub mod server;

pub use config::McpServerConfig;
pub use handler::McpHandler;
pub use server::{boot_stdio_server, boot_streamable_http_server};
