use std::time::Duration;

#[derive(Debug, Clone)]
pub struct RunnerTimeoutConfig {
    // Primitive Layer - Plugin
    pub plugin_load: Duration,
    pub plugin_instantiate: Duration,

    // Primitive Layer - MCP
    pub mcp_connection: Duration,
    pub mcp_transport_start: Duration,

    // Application Layer - Schema (Runner created後の処理)
    pub mcp_tools_load: Duration,
    pub plugin_schema_load: Duration,
}

impl Default for RunnerTimeoutConfig {
    fn default() -> Self {
        Self {
            plugin_load: Duration::from_secs(10),
            plugin_instantiate: Duration::from_secs(5),
            mcp_connection: Duration::from_secs(15),
            mcp_transport_start: Duration::from_secs(15),
            mcp_tools_load: Duration::from_secs(10),
            plugin_schema_load: Duration::from_secs(5),
        }
    }
}

impl RunnerTimeoutConfig {
    pub fn from_env() -> Self {
        Self {
            plugin_load: Self::parse_env("PLUGIN_LOAD_TIMEOUT_SECS", 10),
            plugin_instantiate: Self::parse_env("PLUGIN_INSTANTIATE_TIMEOUT_SECS", 5),
            mcp_connection: Self::parse_env("MCP_CONNECTION_TIMEOUT_SECS", 15),
            mcp_transport_start: Self::parse_env("MCP_TRANSPORT_START_TIMEOUT_SECS", 15),
            mcp_tools_load: Self::parse_env("MCP_TOOLS_LOAD_TIMEOUT_SECS", 10),
            plugin_schema_load: Self::parse_env("PLUGIN_SCHEMA_LOAD_TIMEOUT_SECS", 5),
        }
    }

    fn parse_env(key: &str, default: u64) -> Duration {
        let value = std::env::var(key)
            .ok()
            .and_then(|s| {
                s.parse::<u64>().ok().and_then(|v| {
                    if v == 0 {
                        tracing::warn!("{} is 0, using default {}s", key, default);
                        None
                    } else if v > 600 {
                        tracing::warn!("{} is too large ({}s), capping at 600s", key, v);
                        Some(600)
                    } else {
                        Some(v)
                    }
                })
            })
            .unwrap_or_else(|| {
                tracing::debug!("{} not set or invalid, using default {}s", key, default);
                default
            });
        Duration::from_secs(value)
    }

    // Singleton pattern for global access
    pub fn global() -> &'static Self {
        use std::sync::OnceLock;
        static CONFIG: OnceLock<RunnerTimeoutConfig> = OnceLock::new();
        CONFIG.get_or_init(Self::from_env)
    }

    // Test-only constructor for unit tests
    #[cfg(test)]
    pub fn for_test(
        plugin_load_secs: u64,
        plugin_instantiate_secs: u64,
        mcp_connection_secs: u64,
        mcp_transport_start_secs: u64,
        mcp_tools_load_secs: u64,
        plugin_schema_load_secs: u64,
    ) -> Self {
        Self {
            plugin_load: Duration::from_secs(plugin_load_secs),
            plugin_instantiate: Duration::from_secs(plugin_instantiate_secs),
            mcp_connection: Duration::from_secs(mcp_connection_secs),
            mcp_transport_start: Duration::from_secs(mcp_transport_start_secs),
            mcp_tools_load: Duration::from_secs(mcp_tools_load_secs),
            plugin_schema_load: Duration::from_secs(plugin_schema_load_secs),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = RunnerTimeoutConfig::default();
        assert_eq!(config.plugin_load, Duration::from_secs(10));
        assert_eq!(config.plugin_instantiate, Duration::from_secs(5));
        assert_eq!(config.mcp_connection, Duration::from_secs(15));
        assert_eq!(config.mcp_transport_start, Duration::from_secs(15));
        assert_eq!(config.mcp_tools_load, Duration::from_secs(10));
        assert_eq!(config.plugin_schema_load, Duration::from_secs(5));
    }

    #[test]
    fn test_for_test_constructor() {
        let config = RunnerTimeoutConfig::for_test(1, 2, 3, 4, 5, 6);
        assert_eq!(config.plugin_load, Duration::from_secs(1));
        assert_eq!(config.plugin_instantiate, Duration::from_secs(2));
        assert_eq!(config.mcp_connection, Duration::from_secs(3));
        assert_eq!(config.mcp_transport_start, Duration::from_secs(4));
        assert_eq!(config.mcp_tools_load, Duration::from_secs(5));
        assert_eq!(config.plugin_schema_load, Duration::from_secs(6));
    }

    #[test]
    fn test_global_singleton() {
        let config1 = RunnerTimeoutConfig::global();
        let config2 = RunnerTimeoutConfig::global();
        // Same instance
        assert!(std::ptr::eq(config1, config2));
    }
}
