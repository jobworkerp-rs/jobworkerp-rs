//! Configuration for AG-UI Front server.

use serde::Deserialize;
use std::collections::HashSet;

/// A server-owned endpoint binding to a registered worker.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgUiProfile {
    pub path: String,
    pub worker_name: String,
    pub mode: AgUiProfileMode,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgUiProfileMode {
    Streaming,
    Workflow,
}

impl AgUiProfile {
    pub fn local_route_path(&self) -> Option<&str> {
        self.path.strip_prefix("/ag-ui")
    }

    pub fn validate_all(profiles: &[Self]) -> anyhow::Result<()> {
        let mut paths = HashSet::new();
        for profile in profiles {
            let parts: Vec<_> = profile.path.split('/').collect();
            anyhow::ensure!(
                parts.len() == 5
                    && parts[0].is_empty()
                    && parts[1] == "ag-ui"
                    && parts[2] == "agents"
                    && !parts[3].is_empty()
                    && parts[4] == "run",
                "invalid AG-UI profile path: {}",
                profile.path
            );
            anyhow::ensure!(
                !profile.worker_name.trim().is_empty(),
                "workerName must not be empty"
            );
            anyhow::ensure!(
                paths.insert(&profile.path),
                "duplicate AG-UI profile path: {}",
                profile.path
            );
        }
        Ok(())
    }
}

pub fn profile_for_path<'a>(profiles: &'a [AgUiProfile], path: &str) -> Option<&'a AgUiProfile> {
    profiles.iter().find(|profile| profile.path == path)
}

impl AgUiServerConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if let Some(error) = &self.profile_config_error {
            anyhow::bail!("{error}");
        }
        AgUiProfile::validate_all(&self.profiles)
    }
}

/// Configuration for AG-UI Server
#[derive(Clone, Debug, Deserialize)]
pub struct AgUiServerConfig {
    /// Request timeout in seconds
    pub timeout_sec: u32,

    /// Maximum events to store per run (for replay)
    pub max_events_per_run: usize,

    /// Session time-to-live in seconds
    pub session_ttl_sec: u64,

    /// Server-owned endpoint bindings. Empty by default; configure AG_UI_PROFILES to enable runs.
    #[serde(default)]
    pub profiles: Vec<AgUiProfile>,

    #[serde(skip)]
    pub profile_config_error: Option<String>,
}

impl Default for AgUiServerConfig {
    fn default() -> Self {
        Self {
            timeout_sec: 60,
            max_events_per_run: 1000,
            session_ttl_sec: 3600, // 1 hour
            profiles: vec![],
            profile_config_error: None,
        }
    }
}

impl AgUiServerConfig {
    /// Create configuration from environment variables
    pub fn from_env() -> Self {
        let (profiles, profile_config_error) = profile_env();
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
            profiles,
            profile_config_error,
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

fn parse_profiles(value: Option<&str>) -> (Vec<AgUiProfile>, Option<String>) {
    match value {
        Some(value) => match serde_json::from_str(value) {
            Ok(profiles) => (profiles, None),
            Err(error) => (
                vec![],
                Some(format!("invalid AG_UI_PROFILES JSON: {error}")),
            ),
        },
        None => (vec![], None),
    }
}

fn profile_env() -> (Vec<AgUiProfile>, Option<String>) {
    parse_profiles(std::env::var("AG_UI_PROFILES").ok().as_deref())
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
        assert!(config.profiles.is_empty());
    }

    #[test]
    fn profile_paths_must_be_unique_and_scoped() {
        let make = |path: &str| AgUiProfile {
            path: path.to_string(),
            worker_name: "worker".to_string(),
            mode: AgUiProfileMode::Workflow,
        };
        assert!(AgUiProfile::validate_all(&[make("/ag-ui/agents/guarded/run")]).is_ok());
        assert!(AgUiProfile::validate_all(&[make("/ag-ui/run")]).is_err());
        assert!(
            AgUiProfile::validate_all(&[
                make("/ag-ui/agents/live/run"),
                make("/ag-ui/agents/live/run"),
            ])
            .is_err()
        );
    }

    #[test]
    fn profile_json_uses_wire_field_names_and_rejects_unknown_fields() {
        let profile: AgUiProfile = serde_json::from_str(
            r#"{"path":"/ag-ui/agents/live/run","workerName":"llm","mode":"streaming"}"#,
        )
        .unwrap();
        assert_eq!(profile.mode, AgUiProfileMode::Streaming);
        assert!(serde_json::from_str::<AgUiProfile>(
            r#"{"path":"/ag-ui/agents/live/run","workerName":"llm","mode":"streaming","extra":true}"#
        ).is_err());
    }

    #[test]
    fn malformed_profile_json_is_not_treated_as_an_empty_configuration() {
        let (profiles, error) = parse_profiles(Some("{"));
        assert!(profiles.is_empty());
        assert!(error.is_some());
        assert!(
            AgUiServerConfig {
                profile_config_error: error,
                ..AgUiServerConfig::default()
            }
            .validate()
            .is_err()
        );
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
