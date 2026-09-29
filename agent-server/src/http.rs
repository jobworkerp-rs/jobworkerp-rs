//! HTTP/JSON and SSE boundary for the Agent Server.
//!
//! This module deliberately depends only on the injected backend contract. The
//! backend is responsible for skills, chat orchestration, and jobworkerp access.

use std::{
    collections::HashSet,
    convert::Infallible,
    error::Error,
    fmt,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::Arc,
};

use async_trait::async_trait;
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Request, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header, uri::Authority},
    middleware::{self, Next},
    response::{
        IntoResponse, Response,
        sse::{Event, Sse},
    },
    routing::{get, post, put},
};
use futures_util::{Stream, StreamExt, stream};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

const DEFAULT_MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_CONFIGURED_BODY_BYTES: usize = 64 * 1024 * 1024;
const RESERVED_TOOL_NAMES: [&str; 3] = ["list_skills", "search_skills", "activate_skill"];
const CORS_METHODS: &str = "GET, POST, PUT, DELETE, OPTIONS";
const CORS_HEADERS: &str = "authorization, content-type";
const MAX_IMAGE_BASE64_CHARS: usize = 512 * 1024;

/// Authentication choices are explicit so an external listener cannot
/// accidentally inherit the local no-token behavior.
#[derive(Clone)]
pub enum HttpAuth {
    LocalNoToken,
    LocalSharedToken(String),
    External {
        chat_token: String,
        admin_token: String,
    },
}

/// Bind and browser-origin policy for the HTTP server.
#[derive(Clone)]
pub struct HttpConfig {
    pub bind_addr: SocketAddr,
    pub auth: HttpAuth,
    /// Host names or IP literals, without ports. Wildcards are never accepted.
    pub allowed_hosts: Vec<String>,
    /// Exact HTTP(S) origins. An empty list disables browser-origin requests.
    pub allowed_origins: Vec<String>,
    pub max_body_bytes: usize,
}

impl HttpConfig {
    /// Create an explicitly unauthenticated loopback-only configuration.
    pub fn local_no_token(bind_addr: SocketAddr) -> Self {
        Self::local(bind_addr, HttpAuth::LocalNoToken)
    }

    /// Create a loopback-only configuration whose single token can call both
    /// chat and administrator routes.
    pub fn local_shared_token(bind_addr: SocketAddr, token: impl Into<String>) -> Self {
        Self::local(bind_addr, HttpAuth::LocalSharedToken(token.into()))
    }

    /// Create an externally-bound configuration with role-separated tokens.
    pub fn external(
        bind_addr: SocketAddr,
        chat_token: impl Into<String>,
        admin_token: impl Into<String>,
        allowed_hosts: Vec<String>,
        allowed_origins: Vec<String>,
    ) -> Self {
        Self {
            bind_addr,
            auth: HttpAuth::External {
                chat_token: chat_token.into(),
                admin_token: admin_token.into(),
            },
            allowed_hosts,
            allowed_origins,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        }
    }

    /// Replace the exact browser-origin allowlist.
    pub fn with_allowed_origins(mut self, allowed_origins: Vec<String>) -> Self {
        self.allowed_origins = allowed_origins;
        self
    }

    /// Change the request-body cap, constrained to a finite positive size.
    pub fn with_max_body_bytes(mut self, max_body_bytes: usize) -> Self {
        self.max_body_bytes = max_body_bytes;
        self
    }

    /// Validate bind scope, authentication, and origin/host restrictions.
    pub fn validate(&self) -> Result<(), HttpConfigError> {
        let loopback_bind = self.bind_addr.ip().is_loopback();
        match &self.auth {
            HttpAuth::LocalNoToken if !loopback_bind => {
                return Err(HttpConfigError::new(
                    "no-token mode is permitted only on a loopback bind address",
                ));
            }
            HttpAuth::LocalSharedToken(token) => {
                if !loopback_bind {
                    return Err(HttpConfigError::new(
                        "a local shared token is permitted only on a loopback bind address",
                    ));
                }
                validate_token("local shared token", token)?;
            }
            HttpAuth::External {
                chat_token,
                admin_token,
            } => {
                validate_token("chat token", chat_token)?;
                validate_token("admin token", admin_token)?;
                if constant_time_eq(chat_token.as_bytes(), admin_token.as_bytes()) {
                    return Err(HttpConfigError::new(
                        "external chat and admin tokens must be distinct",
                    ));
                }
            }
            HttpAuth::LocalNoToken => {}
        }

        if self.allowed_hosts.is_empty() {
            return Err(HttpConfigError::new(
                "at least one allowed host is required",
            ));
        }
        for host in &self.allowed_hosts {
            let normalized = normalize_host(host)
                .ok_or_else(|| HttpConfigError::new("an allowed host is invalid"))?;
            if normalized == "*" {
                return Err(HttpConfigError::new("wildcard hosts are not permitted"));
            }
            if loopback_bind_is_local(&self.auth) && !is_loopback_host(&normalized) {
                return Err(HttpConfigError::new(
                    "local mode may allow only localhost or loopback IP hosts",
                ));
            }
        }

        for origin in &self.allowed_origins {
            let parsed = parse_origin(origin)
                .ok_or_else(|| HttpConfigError::new("an allowed origin is invalid"))?;
            if origin.trim() == "*" || origin.eq_ignore_ascii_case("null") {
                return Err(HttpConfigError::new(
                    "wildcard and null origins are not permitted",
                ));
            }
            if loopback_bind_is_local(&self.auth)
                && (!is_loopback_host(&parsed.host)
                    || !self
                        .allowed_hosts
                        .iter()
                        .any(|host| normalize_host(host).as_deref() == Some(parsed.host.as_str())))
            {
                return Err(HttpConfigError::new(
                    "local mode may allow only origins on configured loopback hosts",
                ));
            }
        }

        if self.max_body_bytes == 0 || self.max_body_bytes > MAX_CONFIGURED_BODY_BYTES {
            return Err(HttpConfigError::new(
                "the HTTP body limit must be between 1 byte and 64 MiB",
            ));
        }
        Ok(())
    }

    fn local(bind_addr: SocketAddr, auth: HttpAuth) -> Self {
        let mut allowed_hosts = vec!["localhost".to_owned()];
        let bind_host = bind_addr.ip().to_string();
        if !allowed_hosts.iter().any(|host| host == &bind_host) {
            allowed_hosts.push(bind_host);
        }
        Self {
            bind_addr,
            auth,
            allowed_hosts,
            allowed_origins: Vec::new(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        }
    }
}

/// A startup-configuration error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpConfigError(String);

impl HttpConfigError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for HttpConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for HttpConfigError {}

/// The only entry point needed by the process integration layer. It fails
/// closed before a listener can be started with an unsafe authentication setup.
pub fn router(
    config: HttpConfig,
    backend: Arc<dyn HttpBackend>,
) -> Result<Router, HttpConfigError> {
    config.validate()?;

    let state = AppState::new(config, backend);
    Ok(Router::new()
        .route("/v1/chats", post(start_chat))
        .route("/v1/chats/stream", post(stream_chat))
        .route("/v1/chats/{chat_id}/resume", post(resume_chat))
        .route("/v1/chats/{chat_id}/cancel", post(cancel_chat))
        .route("/v1/skills", get(list_skills))
        .route("/v1/skills/reload", post(reload_skills))
        .route("/v1/skills/{name}", get(skill_detail))
        .route("/v1/tools", get(list_tools))
        .route("/v1/tools/{name}", put(upsert_tool).delete(delete_tool))
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(state.config.max_body_bytes))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security_middleware,
        ))
        .with_state(state))
}

/// Typed interface supplied by the process integration layer. Implementations
/// may connect to orchestration/storage code, but the HTTP module has no direct
/// jobworkerp dependency or concrete backend.
#[async_trait]
pub trait HttpBackend: Send + Sync + 'static {
    async fn start_chat(&self, _request: ChatRequest) -> Result<ChatResponse, BackendError> {
        Err(BackendError::unavailable())
    }

    async fn stream_chat(&self, _request: ChatRequest) -> Result<BackendChatStream, BackendError> {
        Err(BackendError::unavailable())
    }

    /// Validate and consume the supplied chat-scoped capability in the backend;
    /// it does not replace the HTTP chat bearer token.
    async fn resume_chat(
        &self,
        _chat_id: String,
        _request: ResumeRequest,
    ) -> Result<ChatResponse, BackendError> {
        Err(BackendError::unavailable())
    }

    /// Validate the capability against this chat's owned jobs before cancelling;
    /// the HTTP layer never treats it as route authentication.
    async fn cancel_chat(
        &self,
        _chat_id: String,
        _request: CancelRequest,
    ) -> Result<CancelResponse, BackendError> {
        Err(BackendError::unavailable())
    }

    async fn list_skills(&self) -> Result<SkillListResponse, BackendError> {
        Err(BackendError::unavailable())
    }

    async fn skill_detail(&self, _name: String) -> Result<SkillDetail, BackendError> {
        Err(BackendError::unavailable())
    }

    async fn reload_skills(&self) -> Result<SkillReloadResponse, BackendError> {
        Err(BackendError::unavailable())
    }

    async fn list_tools(&self) -> Result<ToolListResponse, BackendError> {
        Err(BackendError::unavailable())
    }

    async fn upsert_tool(
        &self,
        _name: String,
        _request: ToolUpsertRequest,
    ) -> Result<ToolRecord, BackendError> {
        Err(BackendError::unavailable())
    }

    async fn delete_tool(&self, _name: String) -> Result<ToolDeleteResponse, BackendError> {
        Err(BackendError::unavailable())
    }
}

/// A backend failure is intentionally opaque to HTTP clients; details can be
/// retained by the backend's own diagnostics without exposing internal paths.
#[derive(Clone)]
pub struct BackendError {
    message: String,
    cancel_chat_id: Option<String>,
    cancel_capability: Option<String>,
}

impl fmt::Debug for BackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BackendError")
            .field("message", &self.message)
            .field(
                "cancel_chat_id",
                &self.cancel_chat_id.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "cancel_capability",
                &self.cancel_capability.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

impl BackendError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cancel_chat_id: None,
            cancel_capability: None,
        }
    }

    /// Only a resumed request which actually started and retained a job may receive this bearer.
    pub fn with_cancel_capability(message: impl Into<String>, capability: String) -> Self {
        Self {
            message: message.into(),
            cancel_chat_id: None,
            cancel_capability: Some(capability),
        }
    }

    /// Only a chat with a still-owned job may disclose this cancellation context on failure.
    pub fn with_chat_cancel(
        message: impl Into<String>,
        chat_id: String,
        capability: String,
    ) -> Self {
        Self {
            message: message.into(),
            cancel_chat_id: Some(chat_id),
            cancel_capability: Some(capability),
        }
    }

    pub fn cancel_chat_id(&self) -> Option<&str> {
        self.cancel_chat_id.as_deref()
    }

    pub fn cancel_capability(&self) -> Option<&str> {
        self.cancel_capability.as_deref()
    }

    fn unavailable() -> Self {
        Self::new("the route is not implemented by the backend")
    }
}

impl fmt::Display for BackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for BackendError {}

pub type ChatEventStream =
    Pin<Box<dyn Stream<Item = Result<ChatStreamEvent, BackendError>> + Send>>;

/// Stream id and event source returned by a chat backend.
pub struct BackendChatStream {
    pub chat_id: String,
    /// Available before the first model job starts so the client can cancel in-flight work.
    pub cancel_capability: String,
    pub events: ChatEventStream,
}

impl BackendChatStream {
    pub fn new(
        chat_id: impl Into<String>,
        cancel_capability: impl Into<String>,
        events: ChatEventStream,
    ) -> Self {
        Self {
            chat_id: chat_id.into(),
            cancel_capability: cancel_capability.into(),
            events,
        }
    }
}

#[derive(Clone)]
struct AppState {
    config: HttpConfig,
    backend: Arc<dyn HttpBackend>,
    allowed_hosts: HashSet<String>,
    allowed_origins: HashSet<OriginKey>,
}

impl AppState {
    fn new(config: HttpConfig, backend: Arc<dyn HttpBackend>) -> Self {
        let allowed_hosts = config
            .allowed_hosts
            .iter()
            .filter_map(|host| normalize_host(host))
            .collect();
        let allowed_origins = config
            .allowed_origins
            .iter()
            .filter_map(|origin| parse_origin(origin))
            .collect();
        Self {
            config,
            backend,
            allowed_hosts,
            allowed_origins,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatRequest {
    pub llm_worker_id: u64,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub options: Option<ChatOptions>,
    /// Optional consistency check; route selection remains authoritative.
    #[serde(default)]
    pub stream: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatOptions {
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub content: ChatContent,
    #[serde(default)]
    pub tool_call_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ChatContent {
    Text(TextContent),
    ToolCalls(ToolCallsContent),
    ToolResults(ToolResultsContent),
    Image(ImageContent),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TextContent {
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResultsContent {
    #[serde(rename = "toolResults", alias = "tool_results")]
    pub tool_results: Vec<ToolResult>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCallsContent {
    #[serde(rename = "toolCalls", alias = "tool_calls")]
    pub tool_calls: Vec<AssistantToolCall>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssistantToolCall {
    pub call_id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImageContent {
    pub image: ImagePayload,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImagePayload {
    pub content_type: String,
    pub source: ImageSource,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ImageSource {
    Url(String),
    Base64(String),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolResult {
    pub call_id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub result: Value,
    #[serde(default)]
    pub is_error: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatStatus {
    Completed,
    ApprovalRequired,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatResponse {
    pub chat_id: String,
    pub status: ChatStatus,
    #[serde(default)]
    pub messages: Vec<ChatMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_capability: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_capability: Option<String>,
    #[serde(default)]
    pub pending_calls: Vec<PendingCall>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PendingCall {
    pub call_id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResumeRequest {
    pub resume_capability: String,
    pub decisions: Vec<ToolDecision>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolDecision {
    pub call_id: String,
    pub decision: ApprovalDecision,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approve,
    Reject,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CancelRequest {
    pub cancel_capability: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CancelResponse {
    pub chat_id: String,
    pub cancelled: bool,
}

/// Backend-to-SSE events. The route adds a monotonically increasing sequence
/// and the backend chat id, and guarantees exactly one terminal frame.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatStreamEvent {
    Started {
        #[serde(rename = "cancelCapability")]
        cancel_capability: String,
    },
    TextDelta {
        text: String,
    },
    ToolCall {
        call_id: String,
        name: String,
        arguments: Value,
    },
    ToolResult {
        call_id: String,
        result: Value,
    },
    ApprovalRequired {
        pending_calls: Vec<PendingCall>,
        resume_capability: String,
    },
    Completed {
        response: Option<ChatResponse>,
    },
    Error {
        code: String,
        message: String,
    },
}

impl ChatStreamEvent {
    fn event_name(&self) -> &'static str {
        match self {
            Self::Started { .. } => "started",
            Self::TextDelta { .. } => "text_delta",
            Self::ToolCall { .. } => "tool_call",
            Self::ToolResult { .. } => "tool_result",
            Self::ApprovalRequired { .. } => "approval_required",
            Self::Completed { .. } => "completed",
            Self::Error { .. } => "error",
        }
    }

    fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::ApprovalRequired { .. } | Self::Completed { .. } | Self::Error { .. }
        )
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillListResponse {
    pub skills: Vec<SkillSummary>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillDetail {
    pub name: String,
    pub description: String,
    pub body: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillReloadResponse {
    pub loaded_count: usize,
    pub diagnostics: Vec<SkillDiagnostic>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillDiagnostic {
    pub scope: SkillDiagnosticScope,
    pub name: String,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillDiagnosticScope {
    Root,
    Skill,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolListResponse {
    pub tools: Vec<ToolRecord>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolRecord {
    pub name: String,
    pub description: String,
    pub worker_id: u64,
    pub method: String,
    pub requires_approval: bool,
    pub input_schema: Value,
}

/// Input intentionally omits `input_schema`; schema authority stays with the
/// backend's worker-method lookup, not an HTTP caller.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolUpsertRequest {
    pub description: String,
    pub worker_id: u64,
    pub method: String,
    pub requires_approval: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolDeleteResponse {
    pub name: String,
    pub deleted: bool,
}

#[derive(Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
}

struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        Self {
            status,
            code,
            message,
        }
    }

    fn bad_request(code: &'static str, message: &'static str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    fn backend() -> Self {
        Self::new(
            StatusCode::BAD_GATEWAY,
            "backend_error",
            "the Agent Server backend could not complete the request",
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorEnvelope {
                error: ErrorBody {
                    code: self.code,
                    message: self.message,
                },
            }),
        )
            .into_response()
    }
}

async fn start_chat(
    State(state): State<AppState>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Response {
    let request = match decode_chat_json(payload) {
        Ok(request) => match prepare_chat_request(request, false) {
            Ok(request) => request,
            Err(error) => return error.into_response(),
        },
        Err(error) => return error.into_response(),
    };
    match state.backend.start_chat(request).await {
        Ok(response) => no_store(Json(response).into_response()),
        Err(error) => {
            if let (Some(chat_id), Some(capability)) =
                (error.cancel_chat_id(), error.cancel_capability())
                && valid_capability(chat_id)
                && valid_capability(capability)
            {
                return no_store(
                    (
                        StatusCode::BAD_GATEWAY,
                        Json(serde_json::json!({
                            "error": {
                                "code": "backend_error",
                                "message": "the Agent Server backend could not complete the request"
                            },
                            "chatId": chat_id,
                            "cancelCapability": capability,
                        })),
                    )
                        .into_response(),
                );
            }
            ApiError::backend().into_response()
        }
    }
}

async fn stream_chat(
    State(state): State<AppState>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Response {
    let request = match decode_chat_json(payload) {
        Ok(request) => match prepare_chat_request(request, true) {
            Ok(request) => request,
            Err(error) => return error.into_response(),
        },
        Err(error) => return error.into_response(),
    };
    let backend_stream = match state.backend.stream_chat(request).await {
        Ok(stream)
            if !stream.chat_id.trim().is_empty() && valid_capability(&stream.cancel_capability) =>
        {
            stream
        }
        Ok(_) => return ApiError::backend().into_response(),
        Err(_) => return ApiError::backend().into_response(),
    };
    let chat_id = backend_stream.chat_id.clone();
    let stream = sse_events(
        chat_id,
        backend_stream.cancel_capability,
        backend_stream.events,
    );
    let mut response = Sse::new(stream).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-transform"),
    );
    response.headers_mut().insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    response
}

async fn resume_chat(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Response {
    let request = match decode_json::<ResumeRequest>(payload) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    if chat_id.trim().is_empty()
        || !valid_capability(&request.resume_capability)
        || request.decisions.len() != 1
    {
        return ApiError::bad_request("invalid_request", "the resume request is invalid")
            .into_response();
    }
    let mut seen_call_ids = HashSet::new();
    if request.decisions.iter().any(|decision| {
        decision.call_id.trim().is_empty() || !seen_call_ids.insert(decision.call_id.as_str())
    }) {
        return ApiError::bad_request("invalid_request", "resume call ids must be unique")
            .into_response();
    }
    match state.backend.resume_chat(chat_id, request).await {
        Ok(response) => no_store(Json(response).into_response()),
        Err(error) => {
            if let Some(capability) = error
                .cancel_capability()
                .filter(|value| valid_capability(value))
            {
                // This bearer is returned only when a valid resume claimed its stored proposal,
                // started a job, and failed to cancel that job. Never reveal backend error text.
                let response = (
                    StatusCode::BAD_GATEWAY,
                    Json(serde_json::json!({
                        "error": {
                            "code": "backend_error",
                            "message": "the Agent Server backend could not complete the request"
                        },
                        "cancelCapability": capability,
                    })),
                )
                    .into_response();
                return no_store(response);
            }
            ApiError::backend().into_response()
        }
    }
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn cancel_chat(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Response {
    let request = match decode_json::<CancelRequest>(payload) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    if chat_id.trim().is_empty() || !valid_capability(&request.cancel_capability) {
        return ApiError::bad_request("invalid_request", "the cancel request is invalid")
            .into_response();
    }
    match state.backend.cancel_chat(chat_id, request).await {
        Ok(response) => Json(response).into_response(),
        Err(_) => ApiError::backend().into_response(),
    }
}

async fn list_skills(State(state): State<AppState>) -> Response {
    match state.backend.list_skills().await {
        Ok(response) => Json(response).into_response(),
        Err(_) => ApiError::backend().into_response(),
    }
}

async fn skill_detail(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    if !valid_public_name(&name) {
        return ApiError::bad_request("invalid_name", "the skill name is invalid").into_response();
    }
    match state.backend.skill_detail(name).await {
        Ok(response) => Json(response).into_response(),
        Err(_) => ApiError::backend().into_response(),
    }
}

async fn reload_skills(State(state): State<AppState>) -> Response {
    match state.backend.reload_skills().await {
        Ok(response) => Json(response).into_response(),
        Err(_) => ApiError::backend().into_response(),
    }
}

async fn list_tools(State(state): State<AppState>) -> Response {
    match state.backend.list_tools().await {
        Ok(response) => Json(response).into_response(),
        Err(_) => ApiError::backend().into_response(),
    }
}

async fn upsert_tool(
    State(state): State<AppState>,
    Path(name): Path<String>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Response {
    let request = match decode_json::<ToolUpsertRequest>(payload) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    if !valid_public_name(&name)
        || RESERVED_TOOL_NAMES.contains(&name.as_str())
        || request.worker_id == 0
        || request.method.trim().is_empty()
        || request.method.len() > 128
        || request.description.len() > 16 * 1024
    {
        return ApiError::bad_request("invalid_tool", "the tool definition is invalid")
            .into_response();
    }
    match state.backend.upsert_tool(name, request).await {
        Ok(response) => Json(response).into_response(),
        Err(_) => ApiError::backend().into_response(),
    }
}

async fn delete_tool(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    if !valid_public_name(&name) || RESERVED_TOOL_NAMES.contains(&name.as_str()) {
        return ApiError::bad_request("invalid_tool", "the tool name is invalid").into_response();
    }
    match state.backend.delete_tool(name).await {
        Ok(response) => Json(response).into_response(),
        Err(_) => ApiError::backend().into_response(),
    }
}

async fn not_found() -> Response {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "not_found",
        "the route does not exist",
    )
    .into_response()
}

fn prepare_chat_request(
    mut request: ChatRequest,
    streaming_route: bool,
) -> Result<ChatRequest, ApiError> {
    if request.llm_worker_id == 0 || request.messages.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_request",
            "llmWorkerId and at least one message are required",
        ));
    }
    if request
        .stream
        .is_some_and(|stream| stream != streaming_route)
    {
        return Err(ApiError::bad_request(
            "invalid_request",
            "the stream option conflicts with the selected route",
        ));
    }
    validate_image_sources(&request.messages)?;
    if let Some(options) = &request.options
        && (options
            .temperature
            .is_some_and(|v| !(0.0..=2.0).contains(&v))
            || options.top_p.is_some_and(|v| !(0.0..=1.0).contains(&v))
            || options.max_tokens.is_some_and(|v| v == 0))
    {
        return Err(ApiError::bad_request(
            "invalid_request",
            "one or more chat options are outside the supported range",
        ));
    }

    request
        .messages
        .retain(|message| message.role != MessageRole::System);
    if request.messages.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_request",
            "at least one non-system message is required",
        ));
    }
    validate_chat_messages(&request.messages)?;
    request.stream = None;
    Ok(request)
}

fn decode_json<T: DeserializeOwned>(
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<T, ApiError> {
    let value = decode_json_value(payload)?;
    serde_json::from_value(value).map_err(|_| {
        ApiError::bad_request(
            "invalid_request",
            "the request body does not match the supported schema",
        )
    })
}

fn decode_chat_json(payload: Result<Json<Value>, JsonRejection>) -> Result<ChatRequest, ApiError> {
    let value = decode_json_value(payload)?;
    if value
        .get("options")
        .and_then(Value::as_object)
        .is_some_and(|options| options.contains_key("stop"))
    {
        return Err(ApiError::bad_request(
            "unsupported_option",
            "the stop option is not supported by the current chat backend",
        ));
    }
    serde_json::from_value(value).map_err(|_| {
        ApiError::bad_request(
            "invalid_request",
            "the request body does not match the supported schema",
        )
    })
}

fn decode_json_value(payload: Result<Json<Value>, JsonRejection>) -> Result<Value, ApiError> {
    let value = payload
        .map_err(|_| ApiError::bad_request("invalid_json", "the request body must be valid JSON"))?
        .0;
    if contains_tool_execution_requests(&value) {
        return Err(ApiError::bad_request(
            "tool_execution_requests_not_allowed",
            "tool_execution_requests are execution directives and are not accepted from clients",
        ));
    }
    Ok(value)
}

fn validate_chat_messages(messages: &[ChatMessage]) -> Result<(), ApiError> {
    for (index, message) in messages.iter().enumerate() {
        let combination_allowed = match &message.role {
            MessageRole::User => matches!(
                &message.content,
                ChatContent::Text(_) | ChatContent::Image(_)
            ),
            MessageRole::Assistant => matches!(
                &message.content,
                ChatContent::Text(_) | ChatContent::ToolCalls(_)
            ),
            MessageRole::Tool => {
                matches!(&message.content, ChatContent::ToolResults(_))
            }
            MessageRole::System => false,
        };
        if !combination_allowed {
            return Err(ApiError::bad_request(
                "invalid_message",
                "the message role does not support this content type",
            ));
        }

        if let ChatContent::ToolCalls(calls) = &message.content
            && !valid_tool_call_history(calls, messages.get(index + 1))
        {
            return Err(ApiError::bad_request(
                "invalid_tool_history",
                "assistant tool calls must be followed by matching tool results",
            ));
        }

        if let ChatContent::ToolResults(results) = &message.content {
            let immediately_follows_calls =
                index > 0 && matches!(&messages[index - 1].content, ChatContent::ToolCalls(_));
            if !immediately_follows_calls || results.tool_results.is_empty() {
                return Err(ApiError::bad_request(
                    "invalid_tool_history",
                    "tool results must immediately follow assistant tool calls",
                ));
            }
        }
    }
    Ok(())
}

fn validate_image_sources(messages: &[ChatMessage]) -> Result<(), ApiError> {
    for message in messages {
        if let ChatContent::Image(ImageContent { image }) = &message.content
            && !valid_image_payload(image)
        {
            return Err(ApiError::bad_request(
                "invalid_image",
                "image content type or source is invalid or exceeds its size limit",
            ));
        }
    }
    Ok(())
}

fn valid_tool_call_history(calls: &ToolCallsContent, next_message: Option<&ChatMessage>) -> bool {
    if calls.tool_calls.is_empty() {
        return false;
    }
    let Some(next_message) = next_message else {
        return false;
    };
    if next_message.role != MessageRole::Tool {
        return false;
    }
    let ChatContent::ToolResults(results) = &next_message.content else {
        return false;
    };
    if results.tool_results.is_empty() {
        return false;
    }

    let call_ids: HashSet<&str> = calls
        .tool_calls
        .iter()
        .map(|call| call.call_id.as_str())
        .collect();
    let result_ids: HashSet<&str> = results
        .tool_results
        .iter()
        .map(|result| result.call_id.as_str())
        .collect();
    call_ids.len() == calls.tool_calls.len()
        && result_ids.len() == results.tool_results.len()
        && calls
            .tool_calls
            .iter()
            .all(|call| !call.call_id.trim().is_empty() && !call.name.trim().is_empty())
        && results
            .tool_results
            .iter()
            .all(|result| !result.call_id.trim().is_empty())
        && call_ids == result_ids
}

fn valid_image_payload(image: &ImagePayload) -> bool {
    let Some((media_type, media_subtype)) = image.content_type.split_once('/') else {
        return false;
    };
    if image.content_type.len() > 128
        || !media_type.eq_ignore_ascii_case("image")
        || media_subtype.is_empty()
        || !media_subtype
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'.' | b'-'))
    {
        return false;
    }

    match &image.source {
        // Fetching a client-selected URL from a Worker is an SSRF and unbounded-download
        // primitive. Reject the source explicitly rather than accepting and silently dropping it.
        ImageSource::Url(_) => false,
        ImageSource::Base64(encoded) => valid_base64_image_source(encoded),
    }
}

fn valid_base64_image_source(encoded: &str) -> bool {
    let bytes = encoded.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BASE64_CHARS || !encoded.is_ascii() {
        return false;
    }
    let padding = bytes.iter().rev().take_while(|byte| **byte == b'=').count();
    if padding > 2 || (padding > 0 && !bytes.len().is_multiple_of(4)) {
        return false;
    }
    let data_len = bytes.len() - padding;
    if padding == 0 && data_len % 4 == 1 {
        return false;
    }
    bytes[..data_len]
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'+' | b'/'))
}

fn contains_tool_execution_requests(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            object
                .keys()
                .any(|key| key == "tool_execution_requests" || key == "toolExecutionRequests")
                || object.values().any(contains_tool_execution_requests)
        }
        Value::Array(items) => items.iter().any(contains_tool_execution_requests),
        _ => false,
    }
}

fn valid_capability(capability: &str) -> bool {
    !capability.trim().is_empty()
        && capability.len() <= 2048
        && !capability.chars().any(char::is_whitespace)
}

fn valid_public_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        && name.as_bytes()[0].is_ascii_alphanumeric()
}

fn sse_events(
    chat_id: String,
    cancel_capability: String,
    events: ChatEventStream,
) -> impl Stream<Item = Result<Event, Infallible>> {
    struct StreamState {
        chat_id: String,
        cancel_capability: String,
        events: ChatEventStream,
        sequence: u64,
        started: bool,
        terminal: bool,
    }

    let state = StreamState {
        chat_id,
        cancel_capability,
        events,
        sequence: 0,
        started: false,
        terminal: false,
    };
    stream::unfold(state, |mut state| async move {
        if state.terminal {
            return None;
        }
        if !state.started {
            state.started = true;
            return Some((
                Ok(sse_event(
                    &state.chat_id,
                    &mut state.sequence,
                    ChatStreamEvent::Started {
                        cancel_capability: state.cancel_capability.clone(),
                    },
                )),
                state,
            ));
        }

        loop {
            match state.events.next().await {
                Some(Ok(ChatStreamEvent::Started { .. })) => continue,
                Some(Ok(event)) => {
                    state.terminal = event.is_terminal();
                    let frame = sse_event(&state.chat_id, &mut state.sequence, event);
                    return Some((Ok(frame), state));
                }
                Some(Err(_)) | None => {
                    state.terminal = true;
                    let event = ChatStreamEvent::Error {
                        code: "stream_failed".to_owned(),
                        message: "the chat stream ended before a terminal event".to_owned(),
                    };
                    let frame = sse_event(&state.chat_id, &mut state.sequence, event);
                    return Some((Ok(frame), state));
                }
            }
        }
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SseEnvelope<'a> {
    chat_id: &'a str,
    sequence: u64,
    event: ChatStreamEvent,
}

fn sse_event(chat_id: &str, sequence: &mut u64, event: ChatStreamEvent) -> Event {
    *sequence += 1;
    let event_name = event.event_name();
    let data = serde_json::to_string(&SseEnvelope {
        chat_id,
        sequence: *sequence,
        event,
    })
    .unwrap_or_else(|_| "{}".to_owned());
    Event::default()
        .event(event_name)
        .id(sequence.to_string())
        .data(data)
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct OriginKey {
    scheme: String,
    host: String,
    port: u16,
}

async fn security_middleware(
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

fn validate_token(name: &str, token: &str) -> Result<(), HttpConfigError> {
    if token.trim().is_empty() || token != token.trim() || token.chars().any(char::is_whitespace) {
        return Err(HttpConfigError::new(format!(
            "{name} must be a non-empty bearer token"
        )));
    }
    Ok(())
}

fn loopback_bind_is_local(auth: &HttpAuth) -> bool {
    matches!(auth, HttpAuth::LocalNoToken | HttpAuth::LocalSharedToken(_))
}

fn normalize_host(host: &str) -> Option<String> {
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

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn parse_origin(origin: &str) -> Option<OriginKey> {
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

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let max_len = left.len().max(right.len());
    let mut difference = left.len() ^ right.len();
    for index in 0..max_len {
        difference |= usize::from(*left.get(index).unwrap_or(&0) ^ *right.get(index).unwrap_or(&0));
    }
    difference == 0
}
