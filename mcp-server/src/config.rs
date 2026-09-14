use anyhow::{Result, anyhow};
use serde::Deserialize;

use crate::proto_schema;

pub use proto_schema::DEFAULT_PROTO_SCHEMA_MAX_DEPTH;

/// Configuration for MCP Server
#[derive(Clone, Debug, Deserialize)]
pub struct McpServerConfig {
    /// Exclude runners from being exposed as tools
    pub exclude_runner_as_tool: bool,
    /// Exclude workers from being exposed as tools
    pub exclude_worker_as_tool: bool,
    /// Expose only tools from a specific FunctionSet
    pub set_name: Option<String>,
    /// Request timeout in seconds
    pub timeout_sec: u32,
    /// Enable streaming responses
    pub streaming: bool,
    /// Timeout for descriptor acquisition used while publishing fixed gRPC tools.
    pub grpc_schema_timeout_ms: u64,
    /// Maximum inline expansion depth for nested messages in tool JSON schemas.
    pub proto_schema_max_depth: usize,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            exclude_runner_as_tool: false,
            exclude_worker_as_tool: false,
            set_name: None,
            timeout_sec: 60,
            // Most runners are non-streaming; opt in explicitly via MCP_STREAMING.
            streaming: false,
            grpc_schema_timeout_ms: 5_000,
            proto_schema_max_depth: proto_schema::DEFAULT_PROTO_SCHEMA_MAX_DEPTH,
        }
    }
}

impl McpServerConfig {
    /// Create configuration from environment variables
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            exclude_runner_as_tool: std::env::var("MCP_EXCLUDE_RUNNER")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(false),
            exclude_worker_as_tool: std::env::var("MCP_EXCLUDE_WORKER")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(false),
            set_name: normalize_set_name(std::env::var("MCP_SET_NAME").ok()),
            timeout_sec: std::env::var("MCP_TIMEOUT_SEC")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(60),
            streaming: std::env::var("MCP_STREAMING")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(false),
            grpc_schema_timeout_ms: parse_grpc_schema_timeout(
                std::env::var("MCP_GRPC_SCHEMA_TIMEOUT_MS").ok(),
            )?,
            proto_schema_max_depth: parse_proto_schema_max_depth(
                std::env::var("MCP_PROTO_SCHEMA_MAX_DEPTH").ok(),
            )?,
        })
    }
}

fn parse_grpc_schema_timeout(value: Option<String>) -> Result<u64> {
    let Some(value) = value else {
        return Ok(5_000);
    };
    let timeout = value.parse::<u64>().map_err(|_| {
        anyhow!("MCP_GRPC_SCHEMA_TIMEOUT_MS must be a positive integer in milliseconds")
    })?;
    if timeout == 0 {
        return Err(anyhow!(
            "MCP_GRPC_SCHEMA_TIMEOUT_MS must be a positive integer in milliseconds"
        ));
    }
    Ok(timeout)
}

fn parse_proto_schema_max_depth(value: Option<String>) -> Result<usize> {
    let Some(value) = value else {
        return Ok(proto_schema::DEFAULT_PROTO_SCHEMA_MAX_DEPTH);
    };
    let depth = value
        .parse::<usize>()
        .map_err(|_| anyhow!("MCP_PROTO_SCHEMA_MAX_DEPTH must be a positive integer"))?;
    if depth == 0 {
        return Err(anyhow!(
            "MCP_PROTO_SCHEMA_MAX_DEPTH must be a positive integer"
        ));
    }
    Ok(depth)
}

fn normalize_set_name(value: Option<String>) -> Option<String> {
    value.filter(|name| !name.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = McpServerConfig::default();
        assert!(!config.exclude_runner_as_tool);
        assert!(!config.exclude_worker_as_tool);
        assert!(config.set_name.is_none());
        assert_eq!(config.timeout_sec, 60);
        assert!(!config.streaming);
    }

    #[test]
    fn empty_or_whitespace_set_name_does_not_restrict_tools() {
        assert_eq!(normalize_set_name(None), None);
        assert_eq!(normalize_set_name(Some(String::new())), None);
        assert_eq!(normalize_set_name(Some(" \t\n ".to_string())), None);
        assert_eq!(
            normalize_set_name(Some("public-tools".to_string())),
            Some("public-tools".to_string())
        );
    }

    #[test]
    fn grpc_schema_timeout_rejects_zero_and_non_numbers() {
        assert_eq!(parse_grpc_schema_timeout(None).unwrap(), 5_000);
        assert_eq!(parse_grpc_schema_timeout(Some("1".to_string())).unwrap(), 1);
        assert!(parse_grpc_schema_timeout(Some("0".to_string())).is_err());
        assert!(parse_grpc_schema_timeout(Some("bad".to_string())).is_err());
    }

    #[test]
    fn proto_schema_max_depth_uses_default_or_configured_value() {
        assert_eq!(
            parse_proto_schema_max_depth(None).unwrap(),
            proto_schema::DEFAULT_PROTO_SCHEMA_MAX_DEPTH
        );
        assert_eq!(
            parse_proto_schema_max_depth(Some("3".to_string())).unwrap(),
            3
        );
        assert!(parse_proto_schema_max_depth(Some("0".to_string())).is_err());
        assert!(parse_proto_schema_max_depth(Some("abc".to_string())).is_err());
        assert!(parse_proto_schema_max_depth(Some("-1".to_string())).is_err());
    }
}
