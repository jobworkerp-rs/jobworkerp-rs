//! Configuration for AG-UI Front server.

use serde::Deserialize;

/// Configuration for AG-UI Server
#[derive(Clone, Debug, Deserialize)]
pub struct AgUiServerConfig {
    /// Request timeout in seconds
    pub timeout_sec: u32,

    /// Maximum events to store per run (for replay)
    pub max_events_per_run: usize,

    /// Session time-to-live in seconds
    pub session_ttl_sec: u64,
}

impl Default for AgUiServerConfig {
    fn default() -> Self {
        Self {
            timeout_sec: 60,
            max_events_per_run: 1000,
            session_ttl_sec: 3600, // 1 hour
        }
    }
}

impl AgUiServerConfig {
    /// Create configuration from environment variables
    pub fn from_env() -> Self {
        Self {
            timeout_sec: std::env::var("AG_UI_REQUEST_TIMEOUT_SEC")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(60),
            max_events_per_run: std::env::var("AG_UI_MAX_EVENTS_PER_RUN")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1000),
            session_ttl_sec: std::env::var("AG_UI_SESSION_TTL_SEC")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(3600),
        }
    }
}

/// Authentication configuration for AG-UI Server
#[derive(Clone, Debug)]
pub struct AuthConfig {
    /// Whether authentication is enabled
    pub enabled: bool,

    /// Valid tokens (comma-separated in env var)
    pub tokens: Vec<String>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            tokens: vec!["demo-token".to_string()],
        }
    }
}

impl AuthConfig {
    /// Create auth configuration from environment variables
    pub fn from_env() -> Self {
        let enabled = std::env::var("AG_UI_AUTH_ENABLED")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(false);

        let tokens = std::env::var("AG_UI_AUTH_TOKENS")
            .ok()
            .map(|s| s.split(',').map(|t| t.trim().to_string()).collect())
            .unwrap_or_else(|| vec!["demo-token".to_string()]);

        Self { enabled, tokens }
    }

    /// Check if a token is valid
    pub fn is_valid_token(&self, token: &str) -> bool {
        if !self.enabled {
            return true;
        }
        self.tokens.contains(&token.to_string())
    }
}

/// Combined server configuration
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// Server bind address
    pub bind_addr: String,

    /// AG-UI specific config
    pub ag_ui: AgUiServerConfig,

    /// Authentication config
    pub auth: AuthConfig,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:8001".to_string(),
            ag_ui: AgUiServerConfig::default(),
            auth: AuthConfig::default(),
        }
    }
}

impl ServerConfig {
    /// Create configuration from environment variables
    pub fn from_env() -> Self {
        Self {
            bind_addr: std::env::var("AG_UI_ADDR").unwrap_or_else(|_| "127.0.0.1:8001".to_string()),
            ag_ui: AgUiServerConfig::from_env(),
            auth: AuthConfig::from_env(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = AgUiServerConfig::default();
        assert_eq!(config.timeout_sec, 60);
        assert_eq!(config.max_events_per_run, 1000);
        assert_eq!(config.session_ttl_sec, 3600);
    }

    #[test]
    fn test_auth_config_default() {
        let auth = AuthConfig::default();
        assert!(!auth.enabled);
        assert!(auth.tokens.contains(&"demo-token".to_string()));
    }

    #[test]
    fn test_auth_validation_disabled() {
        let auth = AuthConfig {
            enabled: false,
            tokens: vec![],
        };
        // When disabled, any token is valid
        assert!(auth.is_valid_token("any-token"));
    }

    #[test]
    fn test_auth_validation_enabled() {
        let auth = AuthConfig {
            enabled: true,
            tokens: vec!["valid-token".to_string()],
        };
        assert!(auth.is_valid_token("valid-token"));
        assert!(!auth.is_valid_token("invalid-token"));
    }

    #[test]
    fn test_server_config_default() {
        let config = ServerConfig::default();
        assert_eq!(config.bind_addr, "127.0.0.1:8001");
    }
}
