// Deferred MCP activation support is implemented here.
use crate::McpServerConfig;
use anyhow::{Result, anyhow};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, mpsc};

/// Resolved external MCP authentication configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpAuthConfig {
    Disabled,
    Enabled { token: String },
    Unavailable { reason: String },
}

impl McpAuthConfig {
    pub fn from_env() -> Self {
        let enabled = std::env::var("MCP_AUTH_ENABLED")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(false);
        let path = std::env::var("MCP_AUTH_TOKEN_FILE").ok();
        Self::from_enabled_and_path(enabled, path.as_deref())
    }

    fn from_enabled_and_path(enabled: bool, path: Option<&str>) -> Self {
        if !enabled {
            return Self::Disabled;
        }

        let Some(path) = path.filter(|path| !path.trim().is_empty()) else {
            return Self::Unavailable {
                reason: "MCP_AUTH_TOKEN_FILE is required when MCP_AUTH_ENABLED=true".to_string(),
            };
        };

        match read_and_delete_token(Path::new(path)) {
            Ok(token) => Self::Enabled { token },
            Err(error) => Self::Unavailable {
                reason: error.to_string(),
            },
        }
    }

    pub fn is_usable(&self) -> bool {
        !matches!(self, Self::Unavailable { .. })
    }
}

fn read_and_delete_token(path: &Path) -> Result<String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| anyhow!("failed to inspect MCP auth token file: {error}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o777 != 0o600 {
            return Err(anyhow!("MCP auth token file must have mode 0600"));
        }
    }

    let token = std::fs::read_to_string(path)
        .map_err(|error| anyhow!("failed to read MCP auth token file: {error}"))?;
    std::fs::remove_file(path)
        .map_err(|error| anyhow!("failed to remove MCP auth token file: {error}"))?;
    let token = token.trim().to_string();
    if token.is_empty() {
        return Err(anyhow!("MCP auth token file must not be empty"));
    }
    Ok(token)
}

/// Read the per-process activation secret from the private file supplied by
/// the local launcher. This deliberately shares the token-file semantics so
/// neither credential survives in the child environment or data root.
pub fn read_deferred_activation_secret_from_env() -> Result<String> {
    let path = std::env::var("MCP_ACTIVATION_SECRET_FILE")
        .map_err(|_| anyhow!("MCP_ACTIVATION_SECRET_FILE is required for deferred MCP"))?;
    read_and_delete_token(Path::new(&path))
}

pub struct DeferredMcpStartRequest {
    pub listener: TcpListener,
    pub config: McpServerConfig,
    pub auth_config: McpAuthConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActivationState {
    Pending,
    Activated,
    Failed,
}

/// Coordinates the single deferred MCP listener activation.
pub struct DeferredMcpActivation {
    secret: Vec<u8>,
    auth_config: McpAuthConfig,
    bind_addr: String,
    state: Mutex<ActivationState>,
    start_tx: mpsc::Sender<DeferredMcpStartRequest>,
}

static DEFERRED_MCP_ACTIVATION: OnceLock<Arc<DeferredMcpActivation>> = OnceLock::new();

/// Install the process-local controller before the gRPC front starts.
/// The controller is intentionally one-shot because a running MCP listener
/// must never switch its visible FunctionSet at runtime.
pub fn install_deferred_mcp_activation(activation: Arc<DeferredMcpActivation>) -> Result<()> {
    DEFERRED_MCP_ACTIVATION
        .set(activation)
        .map_err(|_| anyhow!("deferred MCP activation is already installed"))
}

/// Activate the listener through the loopback gRPC control surface.
pub async fn activate_deferred_mcp(secret: &str, function_set_name: String) -> Result<String> {
    let activation = DEFERRED_MCP_ACTIVATION
        .get()
        .ok_or_else(|| anyhow!("deferred MCP activation is not enabled"))?;
    activation.activate(secret, function_set_name).await
}

impl DeferredMcpActivation {
    pub fn new(
        secret: String,
        auth_config: McpAuthConfig,
        bind_addr: String,
        start_tx: mpsc::Sender<DeferredMcpStartRequest>,
    ) -> Self {
        Self {
            secret: secret.into_bytes(),
            auth_config,
            bind_addr,
            state: Mutex::new(ActivationState::Pending),
            start_tx,
        }
    }

    pub async fn activate(&self, secret: &str, function_set_name: String) -> Result<String> {
        if !constant_time_eq(&self.secret, secret.as_bytes()) {
            return Err(anyhow!("invalid MCP activation secret"));
        }
        if !self.auth_config.is_usable() {
            return Err(anyhow!("MCP authentication is unavailable"));
        }

        let mut state = self.state.lock().await;
        match *state {
            ActivationState::Activated => return Err(anyhow!("MCP is already activated")),
            ActivationState::Failed => return Err(anyhow!("MCP activation previously failed")),
            ActivationState::Pending => {}
        }

        let listener = match TcpListener::bind(&self.bind_addr).await {
            Ok(listener) => listener,
            Err(error) => {
                *state = ActivationState::Failed;
                return Err(anyhow!("failed to bind MCP listener: {error}"));
            }
        };
        let config = McpServerConfig {
            set_name: Some(function_set_name),
            ..McpServerConfig::from_env()
        };
        if self
            .start_tx
            .send(DeferredMcpStartRequest {
                listener,
                config,
                auth_config: self.auth_config.clone(),
            })
            .await
            .is_err()
        {
            *state = ActivationState::Failed;
            return Err(anyhow!("MCP listener startup is unavailable"));
        }
        *state = ActivationState::Activated;
        Ok(self.bind_addr.clone())
    }

    pub fn auth_config(&self) -> &McpAuthConfig {
        &self.auth_config
    }
}

fn constant_time_eq(expected: &[u8], actual: &[u8]) -> bool {
    let mut difference = expected.len() ^ actual.len();
    for index in 0..expected.len().max(actual.len()) {
        difference |= usize::from(*expected.get(index).unwrap_or(&0) ^ *actual.get(index).unwrap_or(&0));
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn enabled_auth_reads_and_removes_a_private_token_file() {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(file.path(), "external-token\n").unwrap();
        #[cfg(unix)]
        fs::set_permissions(file.path(), std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();

        let auth = McpAuthConfig::from_enabled_and_path(true, file.path().to_str());

        assert_eq!(auth, McpAuthConfig::Enabled { token: "external-token".to_string() });
        assert!(!file.path().exists());
    }

    #[test]
    fn enabled_auth_rejects_a_missing_token_file() {
        let auth = McpAuthConfig::from_enabled_and_path(true, Some("/tmp/jobworkerp-missing-mcp-token"));
        assert!(matches!(auth, McpAuthConfig::Unavailable { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn enabled_auth_rejects_a_non_private_token_file() {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(file.path(), "external-token").unwrap();
        fs::set_permissions(file.path(), std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();

        let auth = McpAuthConfig::from_enabled_and_path(true, file.path().to_str());

        assert!(matches!(auth, McpAuthConfig::Unavailable { .. }));
        assert!(file.path().exists());
    }

    #[tokio::test]
    async fn activation_rejects_invalid_secret_without_starting_listener() {
        let (tx, mut rx) = mpsc::channel(1);
        let activation = DeferredMcpActivation::new(
            "expected".to_string(), McpAuthConfig::Disabled, "127.0.0.1:0".to_string(), tx,
        );

        assert!(activation.activate("wrong", "set".to_string()).await.is_err());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn activation_is_one_time() {
        let (tx, mut rx) = mpsc::channel(1);
        let activation = DeferredMcpActivation::new(
            "expected".to_string(), McpAuthConfig::Disabled, "127.0.0.1:0".to_string(), tx,
        );

        assert!(activation.activate("expected", "set".to_string()).await.is_ok());
        let _request = rx.recv().await.unwrap();
        assert!(activation.activate("expected", "set".to_string()).await.is_err());
    }
}
