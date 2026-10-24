use std::net::IpAddr;

use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header, uri::Authority},
    middleware::Next,
    response::{IntoResponse, Response},
};

use super::{ApiError, AppState, HttpAuth, HttpConfigError};

const CORS_METHODS: &str = "GET, POST, PUT, DELETE, OPTIONS";
const CORS_HEADERS: &str = "authorization, content-type";

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct OriginKey {
    scheme: String,
    pub(super) host: String,
    port: u16,
}

pub(super) async fn security_middleware(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if !request_host_allowed(&state, &request) {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            "host_forbidden",
            "the request Host is not allowed",
        )
        .into_response();
    }

    let origin = match request_origin(&state, request.headers()) {
        Ok(origin) => origin,
        Err(error) => return error.into_response(),
    };

    if request.method() == Method::OPTIONS {
        return preflight_response(&request, origin.as_deref());
    }

    match authenticate(
        &state.config.auth,
        route_access(request.uri().path()),
        request.headers(),
    ) {
        AuthResult::Allowed => {}
        AuthResult::Unauthorized => {
            return policy_error(
                ApiError::new(
                    StatusCode::UNAUTHORIZED,
                    "unauthorized",
                    "a valid bearer token is required",
                ),
                origin.as_deref(),
            );
        }
        AuthResult::WrongRole => {
            return policy_error(
                ApiError::new(
                    StatusCode::FORBIDDEN,
                    "forbidden",
                    "the bearer token is not authorized for this route",
                ),
                origin.as_deref(),
            );
        }
    }

    if matches!(request.method().as_str(), "POST" | "PUT")
        && !is_json_content_type(request.headers())
    {
        return policy_error(
            ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_media_type",
                "request bodies must use application/json",
            ),
            origin.as_deref(),
        );
    }

    let mut response = next.run(request).await;
    apply_cors_headers(&mut response, origin.as_deref());
    response.headers_mut().insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    response
}

fn request_host_allowed(state: &AppState, request: &Request<Body>) -> bool {
    let host_values = request.headers().get_all(header::HOST);
    if host_values.iter().count() > 1 {
        return false;
    }
    let authority = match host_values.iter().next() {
        Some(value) => value
            .to_str()
            .ok()
            .and_then(|value| value.parse::<Authority>().ok()),
        None => request.uri().authority().cloned(),
    };
    let Some(host) = authority.and_then(|authority| normalize_host(authority.host())) else {
        return false;
    };
    state.allowed_hosts.contains(&host)
}

fn request_origin(state: &AppState, headers: &HeaderMap) -> Result<Option<String>, ApiError> {
    let values = headers.get_all(header::ORIGIN);
    if values.iter().count() > 1 {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "origin_forbidden",
            "multiple Origin headers are not allowed",
        ));
    }
    let Some(value) = values.iter().next() else {
        return Ok(None);
    };
    let origin = value.to_str().map_err(|_| {
        ApiError::new(
            StatusCode::FORBIDDEN,
            "origin_forbidden",
            "the request Origin is not allowed",
        )
    })?;
    if parse_origin(origin).is_some_and(|parsed| state.allowed_origins.contains(&parsed)) {
        return Ok(Some(origin.to_owned()));
    }
    Err(ApiError::new(
        StatusCode::FORBIDDEN,
        "origin_forbidden",
        "the request Origin is not allowed",
    ))
}

#[derive(Clone, Copy)]
enum AccessRole {
    Chat,
    Admin,
}

fn route_access(path: &str) -> AccessRole {
    if path == "/v1/skills"
        || path.starts_with("/v1/skills/")
        || path == "/v1/tools"
        || path.starts_with("/v1/tools/")
    {
        AccessRole::Admin
    } else {
        AccessRole::Chat
    }
}

enum AuthResult {
    Allowed,
    Unauthorized,
    WrongRole,
}

fn authenticate(auth: &HttpAuth, role: AccessRole, headers: &HeaderMap) -> AuthResult {
    match auth {
        HttpAuth::LocalNoToken => AuthResult::Allowed,
        HttpAuth::LocalSharedToken(expected) => match bearer_token(headers) {
            Some(token) if constant_time_eq(token.as_bytes(), expected.as_bytes()) => {
                AuthResult::Allowed
            }
            _ => AuthResult::Unauthorized,
        },
        HttpAuth::External {
            chat_token,
            admin_token,
        } => {
            let Some(token) = bearer_token(headers) else {
                return AuthResult::Unauthorized;
            };
            let required = match role {
                AccessRole::Chat => chat_token,
                AccessRole::Admin => admin_token,
            };
            if constant_time_eq(token.as_bytes(), required.as_bytes()) {
                AuthResult::Allowed
            } else {
                let other = match role {
                    AccessRole::Chat => admin_token,
                    AccessRole::Admin => chat_token,
                };
                if constant_time_eq(token.as_bytes(), other.as_bytes()) {
                    AuthResult::WrongRole
                } else {
                    AuthResult::Unauthorized
                }
            }
        }
    }
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let values = headers.get_all(header::AUTHORIZATION);
    if values.iter().count() != 1 {
        return None;
    }
    let value = values.iter().next()?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("Bearer")
        || token.is_empty()
        || token.trim() != token
        || token.chars().any(char::is_whitespace)
    {
        return None;
    }
    Some(token)
}

fn is_json_content_type(headers: &HeaderMap) -> bool {
    let values = headers.get_all(header::CONTENT_TYPE);
    if values.iter().count() != 1 {
        return false;
    }
    values
        .iter()
        .next()
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"))
}

fn preflight_response(request: &Request<Body>, origin: Option<&str>) -> Response {
    let Some(origin) = origin else {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            "origin_forbidden",
            "CORS preflight requires an allowed Origin",
        )
        .into_response();
    };
    let requested_method = request
        .headers()
        .get("access-control-request-method")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !matches!(requested_method, "GET" | "POST" | "PUT" | "DELETE") {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            "cors_method_forbidden",
            "the requested CORS method is not allowed",
        )
        .into_response();
    }
    if let Some(requested_headers) = request
        .headers()
        .get("access-control-request-headers")
        .and_then(|value| value.to_str().ok())
    {
        let headers_allowed = requested_headers.split(',').all(|header| {
            matches!(
                header.trim().to_ascii_lowercase().as_str(),
                "authorization" | "content-type"
            )
        });
        if !headers_allowed {
            return ApiError::new(
                StatusCode::FORBIDDEN,
                "cors_header_forbidden",
                "the requested CORS headers are not allowed",
            )
            .into_response();
        }
    }

    let mut response = StatusCode::NO_CONTENT.into_response();
    apply_cors_headers(&mut response, Some(origin));
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static(CORS_METHODS),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static(CORS_HEADERS),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("600"),
    );
    response
}

fn apply_cors_headers(response: &mut Response, origin: Option<&str>) {
    if let Some(origin) = origin.and_then(|origin| HeaderValue::from_str(origin).ok()) {
        response
            .headers_mut()
            .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        response
            .headers_mut()
            .insert(header::VARY, HeaderValue::from_static("Origin"));
    }
}

fn policy_error(error: ApiError, origin: Option<&str>) -> Response {
    let mut response = error.into_response();
    apply_cors_headers(&mut response, origin);
    response.headers_mut().insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    response
}

pub(super) fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

pub(super) fn validate_token(name: &str, token: &str) -> Result<(), HttpConfigError> {
    if token.trim().is_empty() || token != token.trim() || token.chars().any(char::is_whitespace) {
        return Err(HttpConfigError::new(format!(
            "{name} must be a non-empty bearer token"
        )));
    }
    Ok(())
}

pub(super) fn loopback_bind_is_local(auth: &HttpAuth) -> bool {
    matches!(auth, HttpAuth::LocalNoToken | HttpAuth::LocalSharedToken(_))
}

pub(super) fn normalize_host(host: &str) -> Option<String> {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    if host.is_empty()
        || host.contains('/')
        || host.contains('@')
        || host.contains(':') && host.parse::<IpAddr>().is_err()
    {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

pub(super) fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

pub(super) fn parse_origin(origin: &str) -> Option<OriginKey> {
    let uri = origin.parse::<Uri>().ok()?;
    let scheme = uri.scheme_str()?.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let authority = uri.authority()?;
    let raw_authority = origin.split_once("://")?.1;
    if raw_authority != authority.as_str()
        || uri.query().is_some()
        || authority.as_str().contains('@')
    {
        return None;
    }
    let host = normalize_host(authority.host())?;
    let port = authority.port_u16().or(match scheme.as_str() {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    })?;
    Some(OriginKey { scheme, host, port })
}

pub(super) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let max_len = left.len().max(right.len());
    let mut difference = left.len() ^ right.len();
    for index in 0..max_len {
        difference |= usize::from(*left.get(index).unwrap_or(&0) ^ *right.get(index).unwrap_or(&0));
    }
    difference == 0
}
