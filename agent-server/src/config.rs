//! Process bootstrap configuration for the Agent Server.

use std::{
    collections::HashMap, env, error::Error, fmt, net::SocketAddr, path::PathBuf, time::Duration,
};

use infra_utils::infra::redis::RedisConfig;
use redis::IntoConnectionInfo;

use crate::http::HttpConfig;

const DEFAULT_BIND_ADDR: &str = "127.0.0.1:8181";
const DEFAULT_TOOL_REGISTRY_PATH: &str = "tool-registry.json";
const DEFAULT_APPROVAL_TTL_SECS: u64 = 15 * 60;
const MAX_APPROVAL_TTL_SECS: u64 = 24 * 60 * 60;
const MAX_SKILL_ROOTS: usize = 16;
const MAX_ALLOWLIST_ENTRIES: usize = 64;
const MAX_PATH_BYTES: usize = 4096;
const MAX_GRPC_ENDPOINT_BYTES: usize = 2048;
const MAX_GRPC_AUTH_TOKEN_BYTES: usize = 4096;
const MAX_TOKEN_BYTES: usize = 4096;
const MAX_REDIS_URL_BYTES: usize = 2048;

/// Redis-backed or process-local storage selected by `STORAGE_TYPE`.
pub enum AgentServerStorage {
    Standalone,
    Scalable { redis: RedisConfig },
}

impl AgentServerStorage {
    /// Build a Redis client without routing secret-bearing settings through infra-utils'
    /// debug-formatted pool errors. Its connection info applies separate ACL credentials.
    pub fn redis_client(&self) -> Result<redis::Client, AgentServerConfigError> {
        let Self::Scalable { redis } = self else {
            return Err(AgentServerConfigError::new(
                "Redis is not enabled in Standalone mode",
            ));
        };
        let mut info =
            redis.url.as_str().into_connection_info().map_err(|_| {
                AgentServerConfigError::new("REDIS_URL must be a valid Redis endpoint")
            })?;
        let mut settings = info.redis_settings().clone();
        if let Some(username) = &redis.username {
            settings = settings.set_username(username);
        }
        if let Some(password) = &redis.password {
            settings = settings.set_password(password);
        }
        info = info.set_redis_settings(settings);
        redis::Client::open(info)
            .map_err(|_| AgentServerConfigError::new("REDIS_URL must be a valid Redis endpoint"))
    }
}

/// Validated process settings. This type omits `Debug` because its HTTP policy
/// contains authentication material.
pub struct AgentServerConfig {
    pub grpc_endpoint: String,
    pub grpc_auth_token: Option<String>,
    pub skills_roots: Vec<PathBuf>,
    pub tool_registry_path: PathBuf,
    pub storage: AgentServerStorage,
    pub approval_ttl: Duration,
    http: HttpConfig,
}

impl AgentServerConfig {
    /// Read process environment variables and validate the complete bootstrap configuration.
    pub fn from_env() -> Result<Self, AgentServerConfigError> {
        let environment: HashMap<String, String> = env::vars_os()
            .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
            .collect();
        Self::parse(&environment)
    }

    /// Parse an injected environment map so bootstrap policy can be tested without process state.
    pub fn parse(environment: &HashMap<String, String>) -> Result<Self, AgentServerConfigError> {
        let bind_addr = parse_bind_addr(environment)?;
        let grpc_endpoint = required_value(
            environment,
            "AGENT_SERVER_GRPC_ENDPOINT",
            MAX_GRPC_ENDPOINT_BYTES,
        )?;
        let valid_endpoint =
            grpc_endpoint.parse::<http::Uri>().ok().is_some_and(|uri| {
                matches!(uri.scheme_str(), Some("http" | "https"))
                    && uri.authority().is_some_and(|authority| {
                        !authority.host().is_empty() && !authority.as_str().contains('@')
                    })
                    && (uri.path().is_empty() || uri.path() == "/")
                    && uri.query().is_none()
            }) && tonic::transport::Endpoint::from_shared(grpc_endpoint.to_owned()).is_ok();
        if grpc_endpoint.trim() != grpc_endpoint || !valid_endpoint {
            return Err(AgentServerConfigError::new(
                "AGENT_SERVER_GRPC_ENDPOINT must be a valid HTTP(S) gRPC endpoint",
            ));
        }
        let grpc_auth_token = parse_grpc_auth_token(environment)?;

        let skills_roots = parse_skills_roots(environment)?;
        let tool_registry_path = parse_path(
            environment,
            "AGENT_SERVER_TOOL_REGISTRY_PATH",
            DEFAULT_TOOL_REGISTRY_PATH,
        )?;
        let approval_ttl = parse_approval_ttl(environment)?;
        let storage = parse_storage(environment)?;
        let http = parse_http_config(environment, bind_addr)?;

        Ok(Self {
            grpc_endpoint: grpc_endpoint.to_owned(),
            grpc_auth_token,
            skills_roots,
            tool_registry_path,
            storage,
            approval_ttl,
            http,
        })
    }

    /// Return the already-validated HTTP policy for the listener integration layer.
    pub fn http_config(&self) -> HttpConfig {
        self.http.clone()
    }
}

fn parse_grpc_auth_token(
    environment: &HashMap<String, String>,
) -> Result<Option<String>, AgentServerConfigError> {
    let Some(token) = optional_value(
        environment,
        "AGENT_SERVER_GRPC_AUTH_TOKEN",
        MAX_GRPC_AUTH_TOKEN_BYTES,
    )?
    else {
        return Ok(None);
    };
    if token.is_empty() || !token.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
        return Err(AgentServerConfigError::new(
            "AGENT_SERVER_GRPC_AUTH_TOKEN must be a non-empty ASCII token without whitespace",
        ));
    }
    Ok(Some(token.to_owned()))
}

/// Configuration errors contain only fixed messages, never user-supplied values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentServerConfigError(&'static str);

impl AgentServerConfigError {
    const fn new(message: &'static str) -> Self {
        Self(message)
    }
}

impl fmt::Display for AgentServerConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl Error for AgentServerConfigError {}

fn parse_bind_addr(
    environment: &HashMap<String, String>,
) -> Result<SocketAddr, AgentServerConfigError> {
    let value = optional_value(environment, "AGENT_SERVER_ADDR", 64)?.unwrap_or(DEFAULT_BIND_ADDR);
    value.parse::<SocketAddr>().map_err(|_| {
        AgentServerConfigError::new("AGENT_SERVER_ADDR must be a valid socket address")
    })
}

fn parse_skills_roots(
    environment: &HashMap<String, String>,
) -> Result<Vec<PathBuf>, AgentServerConfigError> {
    let Some(value) = optional_value(environment, "AGENT_SERVER_SKILLS_ROOTS", 64 * 1024)? else {
        return Ok(Vec::new());
    };
    let roots = parse_csv(
        value,
        MAX_SKILL_ROOTS,
        MAX_PATH_BYTES,
        "AGENT_SERVER_SKILLS_ROOTS",
    )?;
    Ok(roots.into_iter().map(PathBuf::from).collect())
}

fn parse_path(
    environment: &HashMap<String, String>,
    key: &'static str,
    default: &str,
) -> Result<PathBuf, AgentServerConfigError> {
    let value = optional_value(environment, key, MAX_PATH_BYTES)?.unwrap_or(default);
    let value = value.trim();
    if value.is_empty() {
        return Err(AgentServerConfigError::new(
            "configured filesystem path must not be empty",
        ));
    }
    Ok(PathBuf::from(value))
}

fn parse_approval_ttl(
    environment: &HashMap<String, String>,
) -> Result<Duration, AgentServerConfigError> {
    let seconds = match optional_value(environment, "AGENT_SERVER_APPROVAL_TTL_SECS", 20)? {
        Some(value) => value.parse::<u64>().map_err(|_| {
            AgentServerConfigError::new("AGENT_SERVER_APPROVAL_TTL_SECS must be a positive integer")
        })?,
        None => DEFAULT_APPROVAL_TTL_SECS,
    };
    if !(1..=MAX_APPROVAL_TTL_SECS).contains(&seconds) {
        return Err(AgentServerConfigError::new(
            "AGENT_SERVER_APPROVAL_TTL_SECS must be between 1 second and 24 hours",
        ));
    }
    Ok(Duration::from_secs(seconds))
}

fn parse_storage(
    environment: &HashMap<String, String>,
) -> Result<AgentServerStorage, AgentServerConfigError> {
    match optional_value(environment, "STORAGE_TYPE", 32)?.unwrap_or("Standalone") {
        "Standalone" => Ok(AgentServerStorage::Standalone),
        "Scalable" => {
            parse_redis_config(environment).map(|redis| AgentServerStorage::Scalable { redis })
        }
        _ => Err(AgentServerConfigError::new(
            "STORAGE_TYPE must be either Standalone or Scalable",
        )),
    }
}

fn parse_redis_config(
    environment: &HashMap<String, String>,
) -> Result<RedisConfig, AgentServerConfigError> {
    // Approval state uses the Redis client directly. Reject settings whose apparent connection
    // limits would otherwise be silently ignored under load.
    if [
        "REDIS_POOL_SIZE",
        "REDIS_POOL_MIN_IDLE",
        "REDIS_POOL_CONNECTION_TIMEOUT_MSEC",
        "REDIS_POOL_IDLE_TIMEOUT_MSEC",
        "REDIS_POOL_MAX_LIFETIME_MSEC",
    ]
    .iter()
    .any(|key| environment.contains_key(*key))
    {
        return Err(AgentServerConfigError::new(
            "REDIS_POOL_* settings are not supported by Agent Server",
        ));
    }
    let url = required_value(environment, "REDIS_URL", MAX_REDIS_URL_BYTES)?;
    if redis::Client::open(url).is_err() {
        return Err(AgentServerConfigError::new(
            "REDIS_URL must be a valid Redis endpoint",
        ));
    }

    Ok(RedisConfig {
        username: optional_nonempty(environment, "REDIS_USERNAME", 256)?.map(str::to_owned),
        password: optional_nonempty(environment, "REDIS_PASSWORD", MAX_TOKEN_BYTES)?
            .map(str::to_owned),
        url: url.to_owned(),
        pool_connection_timeout_msec: None,
        pool_idle_timeout_msec: None,
        pool_max_lifetime_msec: None,
        pool_size: 1,
        pool_min_idle: None,
        blocking: false,
    })
}

fn parse_http_config(
    environment: &HashMap<String, String>,
    bind_addr: SocketAddr,
) -> Result<HttpConfig, AgentServerConfigError> {
    let mode = parse_auth_mode(environment, bind_addr)?;
    let allowed_hosts = optional_csv(
        environment,
        "AGENT_SERVER_ALLOWED_HOSTS",
        MAX_ALLOWLIST_ENTRIES,
        253,
    )?;
    let allowed_origins = optional_csv(
        environment,
        "AGENT_SERVER_ALLOWED_ORIGINS",
        MAX_ALLOWLIST_ENTRIES,
        512,
    )?;

    let mut config = match mode {
        AuthMode::LocalNoToken => {
            reject_present(environment, "AGENT_SERVER_TOKEN", MAX_TOKEN_BYTES)?;
            reject_present(environment, "AGENT_SERVER_CHAT_TOKEN", MAX_TOKEN_BYTES)?;
            reject_present(environment, "AGENT_SERVER_ADMIN_TOKEN", MAX_TOKEN_BYTES)?;
            HttpConfig::local_no_token(bind_addr)
        }
        AuthMode::LocalSharedToken => {
            reject_present(environment, "AGENT_SERVER_CHAT_TOKEN", MAX_TOKEN_BYTES)?;
            reject_present(environment, "AGENT_SERVER_ADMIN_TOKEN", MAX_TOKEN_BYTES)?;
            let token = required_value(environment, "AGENT_SERVER_TOKEN", MAX_TOKEN_BYTES)?;
            HttpConfig::local_shared_token(bind_addr, token)
        }
        AuthMode::External => {
            reject_present(environment, "AGENT_SERVER_TOKEN", MAX_TOKEN_BYTES)?;
            let chat_token =
                required_value(environment, "AGENT_SERVER_CHAT_TOKEN", MAX_TOKEN_BYTES)?;
            let admin_token =
                required_value(environment, "AGENT_SERVER_ADMIN_TOKEN", MAX_TOKEN_BYTES)?;
            let hosts = allowed_hosts.as_ref().ok_or_else(|| {
                AgentServerConfigError::new("external binds require AGENT_SERVER_ALLOWED_HOSTS")
            })?;
            let origins = allowed_origins.as_ref().ok_or_else(|| {
                AgentServerConfigError::new("external binds require AGENT_SERVER_ALLOWED_ORIGINS")
            })?;
            if hosts.is_empty() || origins.is_empty() {
                return Err(AgentServerConfigError::new(
                    "external binds require non-empty host and origin allowlists",
                ));
            }
            HttpConfig::external(
                bind_addr,
                chat_token,
                admin_token,
                hosts.clone(),
                origins.clone(),
            )
        }
    };

    if let Some(hosts) = allowed_hosts {
        config.allowed_hosts = hosts;
    }
    if let Some(origins) = allowed_origins {
        config.allowed_origins = origins;
    }
    config.validate().map_err(|_| {
        AgentServerConfigError::new("HTTP authentication or allowlist configuration is invalid")
    })?;
    Ok(config)
}

#[derive(Clone, Copy)]
enum AuthMode {
    LocalNoToken,
    LocalSharedToken,
    External,
}

fn parse_auth_mode(
    environment: &HashMap<String, String>,
    bind_addr: SocketAddr,
) -> Result<AuthMode, AgentServerConfigError> {
    let selected = match optional_value(environment, "AGENT_SERVER_AUTH_MODE", 32)? {
        Some(mode) => match mode {
            "local-no-token" => AuthMode::LocalNoToken,
            "local-shared-token" => AuthMode::LocalSharedToken,
            "external" => AuthMode::External,
            _ => {
                return Err(AgentServerConfigError::new(
                    "AGENT_SERVER_AUTH_MODE must be local-no-token, local-shared-token, or external",
                ));
            }
        },
        None if environment.contains_key("AGENT_SERVER_CHAT_TOKEN")
            || environment.contains_key("AGENT_SERVER_ADMIN_TOKEN") =>
        {
            AuthMode::External
        }
        None if environment.contains_key("AGENT_SERVER_TOKEN") => AuthMode::LocalSharedToken,
        None if bind_addr.ip().is_loopback() => {
            return Err(AgentServerConfigError::new(
                "loopback no-token access must be explicitly selected with AGENT_SERVER_AUTH_MODE",
            ));
        }
        None => AuthMode::External,
    };
    Ok(selected)
}

fn optional_csv(
    environment: &HashMap<String, String>,
    key: &'static str,
    max_entries: usize,
    max_entry_bytes: usize,
) -> Result<Option<Vec<String>>, AgentServerConfigError> {
    optional_value(environment, key, 64 * 1024)?
        .map(|value| parse_csv(value, max_entries, max_entry_bytes, key))
        .transpose()
}

fn parse_csv(
    value: &str,
    max_entries: usize,
    max_entry_bytes: usize,
    key: &'static str,
) -> Result<Vec<String>, AgentServerConfigError> {
    if value.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for entry in value.split(',') {
        let entry = entry.trim();
        if entry.is_empty() || entry.len() > max_entry_bytes || entries.len() == max_entries {
            return Err(AgentServerConfigError::new(match key {
                "AGENT_SERVER_SKILLS_ROOTS" => {
                    "AGENT_SERVER_SKILLS_ROOTS contains an empty, oversized, or excess entry"
                }
                _ => "an allowlist contains an empty, oversized, or excess entry",
            }));
        }
        entries.push(entry.to_owned());
    }
    Ok(entries)
}

fn optional_nonempty<'a>(
    environment: &'a HashMap<String, String>,
    key: &'static str,
    max_bytes: usize,
) -> Result<Option<&'a str>, AgentServerConfigError> {
    let Some(value) = optional_value(environment, key, max_bytes)? else {
        return Ok(None);
    };
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}

fn reject_present(
    environment: &HashMap<String, String>,
    key: &'static str,
    max_bytes: usize,
) -> Result<(), AgentServerConfigError> {
    if optional_value(environment, key, max_bytes)?.is_some() {
        return Err(AgentServerConfigError::new(
            "authentication variables conflict with the selected AGENT_SERVER_AUTH_MODE",
        ));
    }
    Ok(())
}

fn required_value<'a>(
    environment: &'a HashMap<String, String>,
    key: &'static str,
    max_bytes: usize,
) -> Result<&'a str, AgentServerConfigError> {
    let value = optional_value(environment, key, max_bytes)?
        .ok_or_else(|| AgentServerConfigError::new("a required environment setting is missing"))?;
    if value.trim().is_empty() {
        return Err(AgentServerConfigError::new(
            "a required environment setting must not be empty",
        ));
    }
    Ok(value)
}

fn optional_value<'a>(
    environment: &'a HashMap<String, String>,
    key: &'static str,
    max_bytes: usize,
) -> Result<Option<&'a str>, AgentServerConfigError> {
    let Some(value) = environment.get(key) else {
        return Ok(None);
    };
    if value.len() > max_bytes {
        return Err(AgentServerConfigError::new(
            "an environment setting exceeds its configured input limit",
        ));
    }
    Ok(Some(value))
}
