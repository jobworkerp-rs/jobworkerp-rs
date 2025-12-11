//! Authentication middleware for AG-UI HTTP server.

use axum::{
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    middleware::Next,
    response::Response,
};
use std::sync::Arc;

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
    /// Create a new TokenStore with the given valid tokens.
    pub fn new(tokens: Vec<String>) -> Self {
        Self {
            valid_tokens: tokens,
        }
    }

    /// Create TokenStore from AG_UI_AUTH_TOKENS environment variable.
    pub fn from_env() -> Self {
        let tokens = std::env::var("AG_UI_AUTH_TOKENS")
            .map(|s| s.split(',').map(|t| t.trim().to_string()).collect())
            .unwrap_or_else(|_| vec!["demo-token".to_string()]);
        Self::new(tokens)
    }

    /// Check if a token is valid.
    pub fn is_valid(&self, token: &str) -> bool {
        self.valid_tokens.contains(&token.to_string())
    }
}

/// Extract Bearer token from Authorization header.
pub fn extract_bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|auth| auth.strip_prefix("Bearer ").map(String::from))
}

/// Authentication middleware for axum.
///
/// Checks AG_UI_AUTH_ENABLED environment variable to determine if auth is required.
pub async fn auth_middleware(
    State(token_store): State<Arc<TokenStore>>,
    headers: HeaderMap,
    request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    // Check if auth is enabled via environment variable
    let auth_enabled = std::env::var("AG_UI_AUTH_ENABLED")
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
    fn test_token_store_empty() {
        let store = TokenStore::new(vec![]);
        assert!(!store.is_valid("any"));
    }

    #[test]
    fn test_extract_bearer_token() {
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", "Bearer test-token".parse().unwrap());
        assert_eq!(
            extract_bearer_token(&headers),
            Some("test-token".to_string())
        );
    }

    #[test]
    fn test_extract_bearer_token_no_bearer() {
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", "Basic xyz".parse().unwrap());
        assert_eq!(extract_bearer_token(&headers), None);
    }

    #[test]
    fn test_extract_bearer_token_empty() {
        let headers = HeaderMap::new();
        assert_eq!(extract_bearer_token(&headers), None);
    }

    #[test]
    fn test_extract_bearer_token_with_spaces() {
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", "Bearer  token-with-space".parse().unwrap());
        // Note: "Bearer " prefix is stripped, leaving " token-with-space"
        assert_eq!(
            extract_bearer_token(&headers),
            Some(" token-with-space".to_string())
        );
    }
}
