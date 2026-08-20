// MCP authentication configuration is implemented here.
use anyhow::{Result, anyhow};
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Resolved external MCP authentication configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpAuthConfig {
    Disabled,
    Enabled { tokens: Vec<String> },
    Unavailable { reason: String },
}

impl McpAuthConfig {
    pub fn from_env() -> Self {
        let enabled = match std::env::var("MCP_AUTH_ENABLED") {
            Ok(value) => match parse_auth_enabled(Some(&value)) {
                Ok(enabled) => enabled,
                Err(error) => {
                    return Self::Unavailable {
                        reason: error.to_string(),
                    };
                }
            },
            Err(std::env::VarError::NotPresent) => false,
            Err(error) => {
                return Self::Unavailable {
                    reason: format!("failed to read MCP_AUTH_ENABLED: {error}"),
                };
            }
        };
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

    pub fn into_usable(self) -> Result<Self> {
        match self {
            Self::Unavailable { reason } => Err(anyhow!(
                "MCP authentication configuration is invalid: {reason}"
            )),
            config => Ok(config),
        }
    }
}

fn parse_auth_enabled(value: Option<&str>) -> Result<bool> {
    value
        .map(|value| {
            value
                .parse::<bool>()
                .map_err(|_| anyhow!("MCP_AUTH_ENABLED must be true or false, got {value:?}"))
        })
        .transpose()
        .map(|value| value.unwrap_or(false))
}

pub fn resolve_mcp_auth_config_from_env() -> Result<McpAuthConfig> {
    McpAuthConfig::from_env().into_usable()
}

fn read_and_delete_token(path: &Path) -> Result<String> {
    let mut file = open_private_token_file(path)?;
    let mut token = String::new();
    file.read_to_string(&mut token)
        .map_err(|error| anyhow!("failed to read MCP auth token file: {error}"))?;
    drop(file);
    std::fs::remove_file(path)
        .map_err(|error| anyhow!("failed to remove MCP auth token file: {error}"))?;

    let token = token.trim().to_string();
    if token.is_empty() {
        return Err(anyhow!("MCP auth token file must not be empty"));
    }
    Ok(token)
}

#[cfg(unix)]
fn open_private_token_file(path: &Path) -> Result<File> {
    use nix::fcntl::{OFlag, open};
    use nix::sys::stat::Mode;
    use std::os::unix::fs::MetadataExt;

    let fd = open(
        path,
        OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK,
        Mode::empty(),
    )
    .map_err(|error| anyhow!("failed to open MCP auth token file: {error}"))?;
    let file = File::from(fd);
    let metadata = file
        .metadata()
        .map_err(|error| anyhow!("failed to inspect MCP auth token file: {error}"))?;

    if !metadata.file_type().is_file() {
        return Err(anyhow!("MCP auth token file must be a regular file"));
    }
    if metadata.mode() & 0o777 != 0o600 {
        return Err(anyhow!("MCP auth token file must have mode 0600"));
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_private_token_file(path: &Path) -> Result<File> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| anyhow!("failed to inspect MCP auth token file: {error}"))?;

    if metadata.file_type().is_symlink() {
        return Err(anyhow!("MCP auth token file must not be a symbolic link"));
    }
    if !metadata.file_type().is_file() {
        return Err(anyhow!("MCP auth token file must be a regular file"));
    }
    File::open(path).map_err(|error| anyhow!("failed to open MCP auth token file: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn private_token_file(contents: &str) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(file.path(), contents).unwrap();
        #[cfg(unix)]
        fs::set_permissions(
            file.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .unwrap();
        file
    }

    #[test]
    fn enabled_auth_reads_and_removes_a_private_token_file() {
        let file = private_token_file("external-token\n");

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
        let file = private_token_file("  \n");

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
    fn unavailable_auth_configuration_fails_before_server_startup() {
        let auth = McpAuthConfig::from_enabled_and_sources(true, None, None);

        let error = auth.into_usable().unwrap_err();
        assert!(error.to_string().contains("MCP_AUTH_TOKEN_FILE"));
    }

    #[test]
    fn invalid_auth_enabled_value_is_rejected() {
        let error = parse_auth_enabled(Some("treu")).unwrap_err();

        assert!(error.to_string().contains("MCP_AUTH_ENABLED"));
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
        let file = private_token_file("file-token\n");

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
}
