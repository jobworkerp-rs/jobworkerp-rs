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
    Enabled { tokens: Vec<String> },
    Unavailable { reason: String },
}

impl McpAuthConfig {
    pub fn from_env() -> Self {
        let enabled = std::env::var("MCP_AUTH_ENABLED")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(false);
        let path = std::env::var("MCP_AUTH_TOKEN_FILE").ok();
        let tokens = std::env::var("MCP_AUTH_TOKENS").ok();
        Self::from_enabled_and_sources(enabled, path.as_deref(), tokens.as_deref())
    }

    fn from_enabled_and_sources(enabled: bool, path: Option<&str>, tokens: Option<&str>) -> Self {
        if !enabled {
            return Self::Disabled;
        }

        if let Some(path) = path.filter(|path| !path.trim().is_empty()) {
            return match read_and_delete_token(Path::new(path)) {
                Ok(token) => Self::Enabled {
                    tokens: vec![token],
                },
                Err(error) => Self::Unavailable {
                    reason: error.to_string(),
                },
            };
        }

        let tokens = tokens
            .into_iter()
            .flat_map(|tokens| tokens.split(','))
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        if tokens.is_empty() {
            Self::Unavailable {
                reason:
                    "MCP_AUTH_TOKEN_FILE or MCP_AUTH_TOKENS is required when MCP_AUTH_ENABLED=true"
                        .to_string(),
            }
        } else {
            Self::Enabled { tokens }
        }
    }

    pub fn is_usable(&self) -> bool {
        !matches!(self, Self::Unavailable { .. })
    }

    /// Deferred activation intentionally requires an authenticated endpoint.
    /// A disabled server must never become externally reachable through the
    /// activation control plane.
    pub fn is_enabled(&self) -> bool {
        matches!(self, Self::Enabled { .. })
    }
}

fn read_and_delete_token(path: &Path) -> Result<String> {
    #[cfg(unix)]
    {
        read_and_delete_token_unix(path)
    }

    #[cfg(not(unix))]
    {
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|error| anyhow!("failed to inspect MCP auth token file: {error}"))?;

        if metadata.file_type().is_symlink() {
            return Err(anyhow!("MCP auth token file must not be a symbolic link"));
        }
        if !metadata.file_type().is_file() {
            return Err(anyhow!("MCP auth token file must be a regular file"));
        }

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
}

#[cfg(unix)]
fn read_and_delete_token_unix(path: &Path) -> Result<String> {
    use nix::fcntl::{OFlag, open};
    use nix::sys::stat::Mode;
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;

    let fd = open(
        path,
        OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK,
        Mode::empty(),
    )
    .map_err(|error| anyhow!("failed to open MCP auth token file: {error}"))?;
    let mut file = std::fs::File::from(fd);
    let metadata = file
        .metadata()
        .map_err(|error| anyhow!("failed to inspect MCP auth token file: {error}"))?;

    if !metadata.file_type().is_file() {
        return Err(anyhow!("MCP auth token file must be a regular file"));
    }
    if metadata.mode() & 0o777 != 0o600 {
        return Err(anyhow!("MCP auth token file must have mode 0600"));
    }

    let mut token = String::new();
    file.read_to_string(&mut token)
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
        if !self.auth_config.is_enabled() {
            return Err(anyhow!(
                "deferred MCP activation requires enabled authentication"
            ));
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
        let listener_addr = listener
            .local_addr()
            .map_err(|error| anyhow!("failed to determine MCP listener address: {error}"))?;
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
        Ok(listener_addr.to_string())
    }

    pub fn auth_config(&self) -> &McpAuthConfig {
        &self.auth_config
    }
}

fn constant_time_eq(expected: &[u8], actual: &[u8]) -> bool {
    let mut difference = expected.len() ^ actual.len();
    for index in 0..expected.len().max(actual.len()) {
        difference |=
            usize::from(*expected.get(index).unwrap_or(&0) ^ *actual.get(index).unwrap_or(&0));
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
        fs::set_permissions(
            file.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .unwrap();

        let auth = McpAuthConfig::from_enabled_and_sources(true, file.path().to_str(), None);

        assert_eq!(
            auth,
            McpAuthConfig::Enabled {
                tokens: vec!["external-token".to_string()]
            }
        );
        assert!(!file.path().exists());
    }

    #[test]
    fn enabled_auth_rejects_an_empty_token_file() {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(file.path(), "  \n").unwrap();
        #[cfg(unix)]
        fs::set_permissions(
            file.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .unwrap();

        let auth = McpAuthConfig::from_enabled_and_sources(true, file.path().to_str(), None);
        assert!(matches!(auth, McpAuthConfig::Unavailable { .. }));
        assert!(!file.path().exists());
    }

    #[test]
    fn enabled_auth_rejects_a_missing_token_file() {
        let auth = McpAuthConfig::from_enabled_and_sources(
            true,
            Some("/tmp/jobworkerp-missing-mcp-token"),
            None,
        );
        assert!(matches!(auth, McpAuthConfig::Unavailable { .. }));
    }

    #[test]
    fn enabled_auth_accepts_legacy_comma_separated_tokens_without_a_token_file() {
        let auth = McpAuthConfig::from_enabled_and_sources(
            true,
            None,
            Some(" local-token , second-token ,, "),
        );

        assert_eq!(
            auth,
            McpAuthConfig::Enabled {
                tokens: vec!["local-token".to_string(), "second-token".to_string()]
            }
        );
    }

    #[test]
    fn enabled_auth_uses_legacy_tokens_when_token_file_path_is_blank() {
        let auth = McpAuthConfig::from_enabled_and_sources(true, Some("  "), Some("legacy-token"));

        assert_eq!(
            auth,
            McpAuthConfig::Enabled {
                tokens: vec!["legacy-token".to_string()]
            }
        );
    }

    #[test]
    fn enabled_auth_does_not_fall_back_when_the_configured_token_file_is_invalid() {
        let auth = McpAuthConfig::from_enabled_and_sources(
            true,
            Some("/tmp/jobworkerp-missing-mcp-token"),
            Some("legacy-token"),
        );

        assert!(matches!(auth, McpAuthConfig::Unavailable { .. }));
    }

    #[test]
    fn enabled_auth_prefers_a_token_file_over_legacy_tokens() {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(file.path(), "file-token\n").unwrap();
        #[cfg(unix)]
        fs::set_permissions(
            file.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .unwrap();

        let auth = McpAuthConfig::from_enabled_and_sources(
            true,
            file.path().to_str(),
            Some("legacy-token"),
        );

        assert_eq!(
            auth,
            McpAuthConfig::Enabled {
                tokens: vec!["file-token".to_string()]
            }
        );
        assert!(!file.path().exists());
    }

    #[cfg(unix)]
    #[test]
    fn enabled_auth_rejects_a_non_private_token_file() {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(file.path(), "external-token").unwrap();
        fs::set_permissions(
            file.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o644),
        )
        .unwrap();

        let auth = McpAuthConfig::from_enabled_and_sources(true, file.path().to_str(), None);

        assert!(matches!(auth, McpAuthConfig::Unavailable { .. }));
        assert!(file.path().exists());
    }

    #[cfg(unix)]
    #[test]
    fn enabled_auth_rejects_a_non_regular_private_path() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), PermissionsExt::from_mode(0o600)).unwrap();

        let auth = McpAuthConfig::from_enabled_and_sources(true, directory.path().to_str(), None);

        assert!(matches!(
            auth,
            McpAuthConfig::Unavailable { reason }
                if reason == "MCP auth token file must be a regular file"
        ));
        assert!(directory.path().exists());
    }

    #[cfg(unix)]
    #[test]
    fn enabled_auth_rejects_a_symbolic_link_to_a_private_token_file() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let directory = tempfile::tempdir().unwrap();
        let token_path = directory.path().join("token");
        let link_path = directory.path().join("token-link");
        fs::write(&token_path, "external-token").unwrap();
        fs::set_permissions(&token_path, PermissionsExt::from_mode(0o600)).unwrap();
        symlink(&token_path, &link_path).unwrap();

        let auth = McpAuthConfig::from_enabled_and_sources(true, link_path.to_str(), None);

        assert!(matches!(auth, McpAuthConfig::Unavailable { .. }));
        assert!(token_path.exists());
        assert!(std::fs::symlink_metadata(&link_path).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn enabled_auth_rejects_a_private_fifo_without_blocking() {
        use nix::sys::stat::Mode;
        use nix::unistd::mkfifo;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mcp-auth.fifo");
        mkfifo(&path, Mode::from_bits_truncate(0o600)).unwrap();

        let auth = McpAuthConfig::from_enabled_and_sources(true, path.to_str(), None);

        assert!(matches!(
            auth,
            McpAuthConfig::Unavailable { reason }
                if reason == "MCP auth token file must be a regular file"
        ));
        assert!(path.exists());
    }

    #[tokio::test]
    async fn activation_rejects_invalid_secret_without_starting_listener() {
        let (tx, mut rx) = mpsc::channel(1);
        let activation = DeferredMcpActivation::new(
            "expected".to_string(),
            McpAuthConfig::Disabled,
            "127.0.0.1:0".to_string(),
            tx,
        );

        assert!(
            activation
                .activate("wrong", "set".to_string())
                .await
                .is_err()
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn activation_is_one_time() {
        let (tx, mut rx) = mpsc::channel(1);
        let activation = DeferredMcpActivation::new(
            "expected".to_string(),
            McpAuthConfig::Disabled,
            "127.0.0.1:0".to_string(),
            tx,
        );

        assert!(
            activation
                .activate("expected", "set".to_string())
                .await
                .is_err()
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn activation_is_one_time_when_authentication_is_enabled() {
        let (tx, mut rx) = mpsc::channel(1);
        let activation = DeferredMcpActivation::new(
            "expected".to_string(),
            McpAuthConfig::Enabled {
                tokens: vec!["token".to_string()],
            },
            "127.0.0.1:0".to_string(),
            tx,
        );

        let address = activation
            .activate("expected", "set".to_string())
            .await
            .unwrap();
        assert_ne!(address, "127.0.0.1:0");
        let request = rx.recv().await.unwrap();
        assert_eq!(address, request.listener.local_addr().unwrap().to_string());
        assert!(
            activation
                .activate("expected", "set".to_string())
                .await
                .is_err()
        );
    }
}
