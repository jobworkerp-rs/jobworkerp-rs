use serde::Deserialize;

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
        }
    }
}

impl McpServerConfig {
    /// Create configuration from environment variables
    pub fn from_env() -> Self {
        Self {
            exclude_runner_as_tool: std::env::var("MCP_EXCLUDE_RUNNER")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(false),
            exclude_worker_as_tool: std::env::var("MCP_EXCLUDE_WORKER")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(false),
            set_name: std::env::var("MCP_SET_NAME").ok(),
            timeout_sec: std::env::var("MCP_TIMEOUT_SEC")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(60),
            streaming: std::env::var("MCP_STREAMING")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(false),
        }
    }
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
}
