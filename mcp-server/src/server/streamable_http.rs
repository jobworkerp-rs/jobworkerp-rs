use crate::handler::McpHandler;
use anyhow::Result;
use axum::{
    Router,
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    middleware::{self, Next},
    response::{Html, Response},
    routing::get,
};
use command_utils::util::shutdown::ShutdownLock;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

const INDEX_HTML: &str = r#"<!DOCTYPE html>
<html>
<head><title>jobworkerp MCP Server</title></head>
<body>
    <h1>jobworkerp MCP Server</h1>
    <p>MCP endpoint: <code>POST /mcp</code></p>
    <p>Health check: <code>GET /api/health</code></p>
</body>
</html>"#;

/// Token store for Bearer authentication.
///
/// NOTE: This is a simple implementation for development/demo purposes.
/// Production deployments should use proper token management with:
/// - Hashed token storage
/// - Constant-time comparison
/// - Rate limiting
/// - TLS/HTTPS requirement
pub struct TokenStore {
    valid_tokens: Vec<String>,
}

impl TokenStore {
    pub fn new(tokens: Vec<String>) -> Self {
        Self {
            valid_tokens: tokens,
        }
    }

    /// Create TokenStore from MCP_AUTH_TOKENS environment variable
    pub fn from_env() -> Self {
        let tokens = std::env::var("MCP_AUTH_TOKENS")
            .map(|s| s.split(',').map(|t| t.trim().to_string()).collect())
            .unwrap_or_else(|_| vec!["demo-token".to_string()]);
        Self::new(tokens)
    }

    pub fn is_valid(&self, token: &str) -> bool {
        self.valid_tokens.contains(&token.to_string())
    }
}

fn extract_bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|auth| auth.strip_prefix("Bearer ").map(String::from))
}

async fn auth_middleware(
    State(token_store): State<Arc<TokenStore>>,
    headers: HeaderMap,
    request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    // Check if auth is enabled via environment variable
    let auth_enabled = std::env::var("MCP_AUTH_ENABLED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(false);

    if !auth_enabled {
        return Ok(next.run(request).await);
    }

    match extract_bearer_token(&headers) {
        Some(token) if token_store.is_valid(&token) => Ok(next.run(request).await),
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn health_check() -> &'static str {
    "OK"
}

/// Resolved `MCP_ALLOWED_HOSTS` configuration for the Streamable HTTP server.
///
/// rmcp 2.x validates the inbound `Host` header to prevent DNS rebinding attacks and only
/// allows loopback hosts by default. Non-loopback deployments (e.g. behind a reverse proxy or
/// bound to a public interface) must declare their hostnames via `MCP_ALLOWED_HOSTS`
/// (comma-separated). Setting it to `*` disables Host validation (NOT recommended).
#[derive(Debug, PartialEq, Eq)]
enum AllowedHostsSetting {
    /// Keep rmcp's default (loopback-only) Host validation.
    Default,
    /// Disable Host validation entirely (`MCP_ALLOWED_HOSTS=*`).
    Disabled,
    /// Use the explicit list of allowed hosts.
    List(Vec<String>),
}

impl AllowedHostsSetting {
    /// Resolve the setting from the `MCP_ALLOWED_HOSTS` environment variable.
    fn from_env() -> Self {
        Self::parse(std::env::var("MCP_ALLOWED_HOSTS").ok().as_deref())
    }

    /// Parse the `MCP_ALLOWED_HOSTS` value.
    ///
    /// - `None` or empty/whitespace-only input keeps the default loopback-only validation.
    /// - `*` disables validation.
    /// - Otherwise, splits on commas and trims each entry, dropping empties.
    fn parse(value: Option<&str>) -> Self {
        let value = value.map(str::trim).unwrap_or("");
        if value.is_empty() {
            return Self::Default;
        }
        if value == "*" {
            return Self::Disabled;
        }
        let allowed: Vec<String> = value
            .split(',')
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty())
            .collect();
        if allowed.is_empty() {
            Self::Default
        } else {
            Self::List(allowed)
        }
    }
}

/// Boot the MCP Server with Streamable HTTP transport.
///
/// This creates an HTTP server using axum that:
/// - Serves MCP protocol at /mcp endpoint
/// - Provides health check at /api/health
/// - Optionally requires Bearer token authentication
///
/// # Arguments
/// * `handler_factory` - Factory function to create McpHandler instances (called per session)
/// * `bind_addr` - Address to bind the server to (e.g., "127.0.0.1:8000")
/// * `lock` - ShutdownLock for coordinated shutdown with other components
/// * `shutdown_signal` - Optional external shutdown signal. If None, uses internal ctrl_c handler.
pub async fn boot_streamable_http_server<F>(
    handler_factory: F,
    bind_addr: &str,
    lock: ShutdownLock,
    shutdown_signal: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
) -> Result<()>
where
    F: Fn() -> Result<McpHandler, std::io::Error> + Send + Sync + 'static,
{
    let token_store = Arc::new(TokenStore::from_env());
    let allowed_hosts = AllowedHostsSetting::from_env();

    // Create MCP service with StreamableHttpService.
    //
    // stateful_mode=false is a workaround for rmcp session management issues: with the
    // default stateful_mode=true, LocalSessionManager closes HTTP channels immediately after
    // the response, causing SSE reconnection failures ("Channel closed" errors).
    // Related issues:
    // - https://github.com/modelcontextprotocol/rust-sdk/issues/559
    // - https://github.com/modelcontextprotocol/rust-sdk/issues/572
    //
    // StreamableHttpServerConfig is #[non_exhaustive]; start from Default and override fields.
    let mut config = StreamableHttpServerConfig::default();
    config.stateful_mode = false;
    match allowed_hosts {
        AllowedHostsSetting::Default => {}
        AllowedHostsSetting::Disabled => {
            tracing::warn!(
                "MCP_ALLOWED_HOSTS=* disables Host header validation (DNS rebinding protection)"
            );
            config = config.disable_allowed_hosts();
        }
        AllowedHostsSetting::List(allowed) => {
            config = config.with_allowed_hosts(allowed);
        }
    }
    let mcp_service: StreamableHttpService<McpHandler, LocalSessionManager> =
        StreamableHttpService::new(
            handler_factory,
            LocalSessionManager::default().into(),
            config,
        );

    // API routes (public)
    let api_routes = Router::new().route("/health", get(health_check));

    // Protected MCP routes with optional auth middleware
    let protected_mcp =
        Router::new()
            .nest_service("/mcp", mcp_service)
            .layer(middleware::from_fn_with_state(
                token_store.clone(),
                auth_middleware,
            ));

    // Main router
    let app = Router::new()
        .route("/", get(index))
        .nest("/api", api_routes)
        .merge(protected_mcp);

    // Start server
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    tracing::info!("MCP Streamable HTTP Server started on {}", bind_addr);

    // Use provided shutdown signal or create internal one
    let shutdown_future: Pin<Box<dyn Future<Output = ()> + Send>> = match shutdown_signal {
        Some(signal) => signal,
        None => {
            // Standalone mode: setup internal shutdown signal handler (SIGINT/SIGTERM)
            let (tx, rx) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                command_utils::util::shutdown::shutdown_signal().await;
                tracing::info!("Shutting down MCP server...");
                let _ = tx.send(()).inspect_err(|e| {
                    tracing::error!("failed to send shutdown signal: {:?}", e);
                });
            });
            Box::pin(async move {
                rx.await.ok();
            })
        }
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_future)
        .await?;

    // Release shutdown lock when server stops
    lock.unlock();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_store() {
        let store = TokenStore::new(vec!["token1".to_string(), "token2".to_string()]);
        assert!(store.is_valid("token1"));
        assert!(store.is_valid("token2"));
        assert!(!store.is_valid("invalid"));
    }

    #[test]
    fn test_parse_allowed_hosts_default_when_unset() {
        assert_eq!(
            AllowedHostsSetting::parse(None),
            AllowedHostsSetting::Default
        );
    }

    #[test]
    fn test_parse_allowed_hosts_default_when_empty_or_whitespace() {
        assert_eq!(
            AllowedHostsSetting::parse(Some("")),
            AllowedHostsSetting::Default
        );
        assert_eq!(
            AllowedHostsSetting::parse(Some("   ")),
            AllowedHostsSetting::Default
        );
        // Only separators/empty entries also fall back to default.
        assert_eq!(
            AllowedHostsSetting::parse(Some(", ,")),
            AllowedHostsSetting::Default
        );
    }

    #[test]
    fn test_parse_allowed_hosts_wildcard_disables_validation() {
        assert_eq!(
            AllowedHostsSetting::parse(Some("*")),
            AllowedHostsSetting::Disabled
        );
        assert_eq!(
            AllowedHostsSetting::parse(Some("  *  ")),
            AllowedHostsSetting::Disabled
        );
    }

    #[test]
    fn test_parse_allowed_hosts_list_is_trimmed() {
        assert_eq!(
            AllowedHostsSetting::parse(Some("example.com, example.com:8080 ,localhost")),
            AllowedHostsSetting::List(vec![
                "example.com".to_string(),
                "example.com:8080".to_string(),
                "localhost".to_string(),
            ])
        );
    }

    #[test]
    fn test_parse_allowed_hosts_single_host() {
        assert_eq!(
            AllowedHostsSetting::parse(Some("example.com")),
            AllowedHostsSetting::List(vec!["example.com".to_string()])
        );
    }

    #[test]
    fn test_extract_bearer_token() {
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", "Bearer test-token".parse().unwrap());
        assert_eq!(
            extract_bearer_token(&headers),
            Some("test-token".to_string())
        );

        let mut headers_no_bearer = HeaderMap::new();
        headers_no_bearer.insert("Authorization", "Basic xyz".parse().unwrap());
        assert_eq!(extract_bearer_token(&headers_no_bearer), None);

        let empty_headers = HeaderMap::new();
        assert_eq!(extract_bearer_token(&empty_headers), None);
    }
}
