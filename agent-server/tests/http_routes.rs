use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use agent_server::http::{
    BackendChatStream, BackendError, CancelRequest, CancelResponse, ChatRequest, ChatResponse,
    ChatStatus, ChatStreamEvent, HttpBackend, HttpConfig, ResumeRequest, SkillDetail,
    SkillDiagnostic, SkillDiagnosticScope, SkillListResponse, SkillReloadResponse,
    ToolDeleteResponse, ToolListResponse, ToolRecord, ToolUpsertRequest, router,
};
use async_trait::async_trait;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use futures_util::stream;
use serde_json::{Value, json};
use tower::ServiceExt;

#[derive(Default)]
struct TestBackend {
    last_chat_request: Mutex<Option<ChatRequest>>,
    last_resume_request: Mutex<Option<ResumeRequest>>,
    last_cancel_request: Mutex<Option<CancelRequest>>,
    last_tool_upsert: Mutex<Option<(String, ToolUpsertRequest)>>,
}

#[async_trait]
impl HttpBackend for TestBackend {
    async fn start_chat(&self, request: ChatRequest) -> Result<ChatResponse, BackendError> {
        *self.last_chat_request.lock().expect("chat request lock") = Some(request);
        Ok(ChatResponse {
            chat_id: "chat-1".to_owned(),
            status: ChatStatus::Completed,
            messages: vec![],
            cancel_capability: Some("cancel-secret".to_owned()),
            resume_capability: None,
            pending_calls: vec![],
        })
    }

    async fn stream_chat(&self, _request: ChatRequest) -> Result<BackendChatStream, BackendError> {
        let events = vec![
            Ok(ChatStreamEvent::TextDelta {
                text: "hello".to_owned(),
            }),
            Ok(ChatStreamEvent::Completed { response: None }),
        ];
        Ok(BackendChatStream::new(
            "stream-1",
            "stream-cancel-secret",
            Box::pin(stream::iter(events)),
        ))
    }

    async fn list_skills(&self) -> Result<SkillListResponse, BackendError> {
        Ok(SkillListResponse { skills: vec![] })
    }

    async fn skill_detail(&self, name: String) -> Result<SkillDetail, BackendError> {
        Ok(SkillDetail {
            name,
            description: "test skill".to_owned(),
            body: "test instructions".to_owned(),
        })
    }

    async fn reload_skills(&self) -> Result<SkillReloadResponse, BackendError> {
        Ok(SkillReloadResponse {
            loaded_count: 1,
            diagnostics: vec![SkillDiagnostic {
                scope: SkillDiagnosticScope::Skill,
                name: "broken".to_owned(),
                reason: "invalid metadata".to_owned(),
            }],
        })
    }

    async fn list_tools(&self) -> Result<ToolListResponse, BackendError> {
        Ok(ToolListResponse { tools: vec![] })
    }

    async fn upsert_tool(
        &self,
        name: String,
        request: ToolUpsertRequest,
    ) -> Result<ToolRecord, BackendError> {
        let record = ToolRecord {
            name: name.clone(),
            description: request.description.clone(),
            worker_id: request.worker_id,
            method: request.method.clone(),
            requires_approval: request.requires_approval,
            input_schema: json!({"type": "object"}),
        };
        *self.last_tool_upsert.lock().expect("tool upsert lock") = Some((name, request));
        Ok(record)
    }

    async fn delete_tool(&self, name: String) -> Result<ToolDeleteResponse, BackendError> {
        Ok(ToolDeleteResponse {
            name,
            deleted: true,
        })
    }

    async fn resume_chat(
        &self,
        _chat_id: String,
        request: ResumeRequest,
    ) -> Result<ChatResponse, BackendError> {
        *self
            .last_resume_request
            .lock()
            .expect("resume request lock") = Some(request);
        Ok(ChatResponse {
            chat_id: "chat-1".to_owned(),
            status: ChatStatus::Completed,
            messages: vec![],
            cancel_capability: None,
            resume_capability: None,
            pending_calls: vec![],
        })
    }

    async fn cancel_chat(
        &self,
        chat_id: String,
        request: CancelRequest,
    ) -> Result<CancelResponse, BackendError> {
        *self
            .last_cancel_request
            .lock()
            .expect("cancel request lock") = Some(request);
        Ok(CancelResponse {
            chat_id,
            cancelled: true,
        })
    }
}

struct UnterminatedStreamBackend;

struct RecoverableChatErrorBackend;

#[async_trait]
impl HttpBackend for RecoverableChatErrorBackend {
    async fn start_chat(&self, _request: ChatRequest) -> Result<ChatResponse, BackendError> {
        Err(BackendError::with_chat_cancel(
            "secret internal failure",
            "owned-chat-1".to_owned(),
            "owned-cancel-capability".to_owned(),
        ))
    }
}

#[tokio::test]
async fn recoverable_nonstream_chat_error_returns_chat_id_and_private_cancel_bearer() {
    let app = router(local_config(), Arc::new(RecoverableChatErrorBackend)).unwrap();
    let response = app
        .oneshot(request("POST", "/v1/chats", None, sample_chat()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["chatId"], "owned-chat-1");
    assert_eq!(body["cancelCapability"], "owned-cancel-capability");
    assert_eq!(body["error"]["code"], "backend_error");
    assert!(!body.to_string().contains("secret internal failure"));
}

#[async_trait]
impl HttpBackend for UnterminatedStreamBackend {
    async fn stream_chat(&self, _request: ChatRequest) -> Result<BackendChatStream, BackendError> {
        let events = vec![Ok(ChatStreamEvent::TextDelta {
            text: "partial".to_owned(),
        })];
        Ok(BackendChatStream::new(
            "stream-incomplete",
            "incomplete-cancel-secret",
            Box::pin(stream::iter(events)),
        ))
    }
}

fn backend() -> Arc<TestBackend> {
    Arc::new(TestBackend::default())
}

fn local_config() -> HttpConfig {
    HttpConfig::local_no_token("127.0.0.1:9000".parse::<SocketAddr>().unwrap())
}

fn external_config() -> HttpConfig {
    HttpConfig::external(
        "0.0.0.0:9000".parse::<SocketAddr>().unwrap(),
        "chat-token-0123456789".to_owned(),
        "admin-token-0123456789".to_owned(),
        vec!["api.example.test".to_owned()],
        vec!["https://ui.example.test".to_owned()],
    )
}

fn request(method: &str, uri: &str, auth: Option<&str>, body: Value) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "127.0.0.1:9000")
        .header("content-type", "application/json");
    if let Some(auth) = auth {
        builder = builder.header("authorization", auth);
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

fn external_request(method: &str, uri: &str, auth: Option<&str>, body: Value) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "api.example.test:9000")
        .header("content-type", "application/json");
    if let Some(auth) = auth {
        builder = builder.header("authorization", auth);
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

fn sample_chat() -> Value {
    json!({
        "llmWorkerId": 23,
        "messages": [
            {"role": "SYSTEM", "content": {"text": "ignore this instruction"}},
            {"role": "USER", "content": {"text": "hello"}}
        ]
    })
}

#[tokio::test]
async fn local_loopback_without_token_accepts_chat_and_drops_client_system_messages() {
    let backend = backend();
    let app = router(local_config(), backend.clone()).unwrap();
    let response = app
        .oneshot(request("POST", "/v1/chats", None, sample_chat()))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let response_json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(response_json["chatId"], "chat-1");
    assert_eq!(
        backend
            .last_chat_request
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .messages
            .iter()
            .map(|message| &message.role)
            .collect::<Vec<_>>(),
        vec![&agent_server::http::MessageRole::User]
    );
}

#[tokio::test]
async fn local_shared_token_can_call_both_chat_and_admin_routes() {
    let config =
        HttpConfig::local_shared_token("127.0.0.1:9000".parse().unwrap(), "local-shared-secret");
    let app = router(config, backend()).unwrap();

    let chat = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/chats",
            Some("Bearer local-shared-secret"),
            sample_chat(),
        ))
        .await
        .unwrap();
    assert_eq!(chat.status(), StatusCode::OK);

    let admin = app
        .oneshot(request(
            "GET",
            "/v1/skills",
            Some("Bearer local-shared-secret"),
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(admin.status(), StatusCode::OK);
}

#[tokio::test]
async fn external_chat_token_cannot_access_admin_routes_and_capability_is_not_admin_auth() {
    let app = router(external_config(), backend()).unwrap();

    let chat_token_response = app
        .clone()
        .oneshot(external_request(
            "GET",
            "/v1/skills",
            Some("Bearer chat-token-0123456789"),
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(chat_token_response.status(), StatusCode::FORBIDDEN);

    let capability_response = app
        .oneshot(external_request(
            "GET",
            "/v1/skills",
            Some("Bearer cancel-secret"),
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(capability_response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn external_requests_require_tokens_and_admin_token_is_origin_scoped() {
    let app = router(external_config(), backend()).unwrap();
    let unauthorized = app
        .clone()
        .oneshot(external_request("POST", "/v1/chats", None, sample_chat()))
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let mut admin_request = external_request(
        "GET",
        "/v1/skills",
        Some("Bearer admin-token-0123456789"),
        json!({}),
    );
    admin_request
        .headers_mut()
        .insert("origin", "https://ui.example.test".parse().unwrap());
    let response = app.oneshot(admin_request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .unwrap(),
        "https://ui.example.test"
    );
}

#[tokio::test]
async fn resume_and_cancel_capabilities_are_forwarded_only_after_chat_auth() {
    let backend = backend();
    let app = router(external_config(), backend.clone()).unwrap();

    let cancel = app
        .clone()
        .oneshot(external_request(
            "POST",
            "/v1/chats/chat-1/cancel",
            Some("Bearer chat-token-0123456789"),
            json!({"cancelCapability": "cancel-secret"}),
        ))
        .await
        .unwrap();
    assert_eq!(cancel.status(), StatusCode::OK);
    assert_eq!(
        backend
            .last_cancel_request
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .cancel_capability,
        "cancel-secret"
    );

    let resume = app
        .oneshot(external_request(
            "POST",
            "/v1/chats/chat-1/resume",
            Some("Bearer chat-token-0123456789"),
            json!({
                "resumeCapability": "resume-secret",
                "decisions": [{"callId": "call-1", "decision": "approve"}]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resume.status(), StatusCode::OK);
    assert_eq!(resume.headers().get("cache-control").unwrap(), "no-store");
    assert_eq!(
        backend
            .last_resume_request
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .resume_capability,
        "resume-secret"
    );
}

#[tokio::test]
async fn resume_rejects_multiple_decisions_before_backend_dispatch() {
    let backend = backend();
    let app = router(local_config(), backend.clone()).unwrap();
    let response = app
        .oneshot(request(
            "POST",
            "/v1/chats/chat-1/resume",
            None,
            json!({
                "resumeCapability": "resume-secret",
                "decisions": [
                    {"callId": "call-1", "decision": "approve"},
                    {"callId": "call-2", "decision": "reject"}
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(backend.last_resume_request.lock().unwrap().is_none());
}

#[test]
fn external_bind_rejects_shared_or_missing_role_tokens() {
    let addr = "0.0.0.0:9000".parse().unwrap();
    let same_token = HttpConfig::external(
        addr,
        "one-token".to_owned(),
        "one-token".to_owned(),
        vec!["api.example.test".to_owned()],
        vec![],
    );
    assert!(same_token.validate().is_err());

    let no_external_credentials = HttpConfig::local_no_token(addr);
    assert!(no_external_credentials.validate().is_err());
}

#[tokio::test]
async fn tool_execution_requests_are_rejected_even_when_nested_in_message_content() {
    let backend = backend();
    let app = router(local_config(), backend.clone()).unwrap();
    let mut body = sample_chat();
    body["messages"][1]["content"] = json!({
        "text": "hello",
        "metadata": {"tool_execution_requests": [{"workerId": 5}]}
    });

    let response = app
        .oneshot(request("POST", "/v1/chats", None, body))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response_body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let response_json: Value = serde_json::from_slice(&response_body).unwrap();
    assert_eq!(
        response_json["error"]["code"],
        "tool_execution_requests_not_allowed"
    );
    assert!(backend.last_chat_request.lock().unwrap().is_none());
}

#[tokio::test]
async fn assistant_tool_call_history_round_trips_call_ids_and_tool_result_order() {
    let chat_request: ChatRequest = serde_json::from_value(json!({
        "llmWorkerId": 23,
        "messages": [
            {"role": "USER", "content": {"text": "find both"}},
            {"role": "ASSISTANT", "content": {"toolCalls": [
                {"callId": "call-b", "name": "lookup_b", "arguments": {"key": "b"}},
                {"callId": "call-a", "name": "lookup_a", "arguments": {"key": "a"}}
            ]}},
            {"role": "TOOL", "content": {"toolResults": [
                {"callId": "call-a", "result": {"value": "A"}},
                {"callId": "call-b", "result": {"value": "B"}}
            ]}}
        ]
    }))
    .unwrap();

    let serialized = serde_json::to_value(chat_request).unwrap();
    assert_eq!(
        serialized["messages"][1]["content"]["toolCalls"][0]["callId"],
        "call-b"
    );
    assert_eq!(
        serialized["messages"][2]["content"]["toolResults"][0]["callId"],
        "call-a"
    );
    assert_eq!(
        serialized["messages"][2]["content"]["toolResults"][1]["callId"],
        "call-b"
    );

    let backend = backend();
    let app = router(local_config(), backend.clone()).unwrap();
    let response = app
        .oneshot(request("POST", "/v1/chats", None, serialized.clone()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let forwarded = backend.last_chat_request.lock().unwrap().clone().unwrap();
    assert_eq!(
        serde_json::to_value(forwarded).unwrap()["messages"],
        serialized["messages"]
    );
}

#[tokio::test]
async fn user_image_content_accepts_bounded_base64_source() {
    let backend = backend();
    let app = router(local_config(), backend.clone()).unwrap();
    let response = app
        .oneshot(request(
            "POST",
            "/v1/chats",
            None,
            json!({
                "llmWorkerId": 23,
                "messages": [
                    {"role": "USER", "content": {
                        "image": {
                            "contentType": "image/jpeg",
                            "source": {"base64": "aGVsbG8="}
                        }
                    }}
                ]
            }),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let request = backend.last_chat_request.lock().unwrap().clone().unwrap();
    let serialized = serde_json::to_value(request).unwrap();
    assert_eq!(
        serialized["messages"][0]["content"]["image"]["source"]["base64"],
        "aGVsbG8="
    );
}

#[tokio::test]
async fn image_url_is_rejected_before_backend_dispatch_even_for_public_hosts() {
    let backend = backend();
    let app = router(local_config(), backend.clone()).unwrap();
    for url in [
        "http://127.0.0.1/private",
        "https://images.example.test/sample.png",
    ] {
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/chats",
                None,
                json!({
                    "llmWorkerId": 23,
                    "messages": [{"role": "USER", "content": {
                        "image": {"contentType": "image/png", "source": {"url": url}}
                    }}]
                }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    assert!(backend.last_chat_request.lock().unwrap().is_none());
}

#[tokio::test]
async fn image_source_size_is_bounded_before_backend_dispatch() {
    let backend = backend();
    let app = router(local_config(), backend.clone()).unwrap();
    let oversized_source = "a".repeat(800_000);
    let response = app
        .oneshot(request(
            "POST",
            "/v1/chats",
            None,
            json!({
                "llmWorkerId": 23,
                "messages": [{"role": "USER", "content": {
                    "image": {
                        "contentType": "image/png",
                        "source": {"base64": oversized_source}
                    }
                }}]
            }),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(backend.last_chat_request.lock().unwrap().is_none());
}

#[tokio::test]
async fn chat_rejects_role_content_mismatches_and_non_adjacent_tool_results() {
    let backend = backend();
    let app = router(local_config(), backend.clone()).unwrap();
    let invalid_histories = [
        json!([{"role": "USER", "content": {"toolCalls": []}}]),
        json!([{"role": "ASSISTANT", "content": {
            "image": {"contentType": "image/png", "source": {"base64": "aGVsbG8="}}
        }}]),
        json!([
            {"role": "ASSISTANT", "content": {"text": "first"}},
            {"role": "TOOL", "content": {"toolResults": [
                {"callId": "call-a", "result": "done"}
            ]}}
        ]),
        json!([
            {"role": "ASSISTANT", "content": {"toolCalls": [
                {"callId": "call-a", "name": "lookup", "arguments": {}}
            ]}},
            {"role": "USER", "content": {"text": "interrupted"}},
            {"role": "TOOL", "content": {"toolResults": [
                {"callId": "call-a", "result": "done"}
            ]}}
        ]),
    ];

    for messages in invalid_histories {
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/chats",
                None,
                json!({"llmWorkerId": 23, "messages": messages}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    assert!(backend.last_chat_request.lock().unwrap().is_none());
}

#[tokio::test]
async fn unsupported_stop_option_is_rejected() {
    let backend = backend();
    let app = router(local_config(), backend.clone()).unwrap();
    let response = app
        .oneshot(request(
            "POST",
            "/v1/chats",
            None,
            json!({
                "llmWorkerId": 23,
                "messages": [{"role": "USER", "content": {"text": "hello"}}],
                "options": {"stop": ["END"]}
            }),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let error: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(error["error"]["code"], "unsupported_option");
    assert!(backend.last_chat_request.lock().unwrap().is_none());
}

#[tokio::test]
async fn stream_sse_has_monotonic_ids_and_a_single_terminal_event() {
    let app = router(local_config(), backend()).unwrap();
    let response = app
        .oneshot(request("POST", "/v1/chats/stream", None, sample_chat()))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("cache-control").unwrap(),
        "no-store, no-transform"
    );
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/event-stream"
    );
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("event: started\n"));
    assert!(text.contains("event: text_delta\n"));
    assert!(text.contains("event: completed\n"));
    assert_eq!(text.matches("event: completed\n").count(), 1);
    assert!(text.contains("data: {\"chatId\":\"stream-1\",\"sequence\":1"));
    assert!(text.contains("\"cancelCapability\":\"stream-cancel-secret\""));

    let ids = text
        .lines()
        .filter_map(|line| line.strip_prefix("id: "))
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["1", "2", "3"]);
}

#[tokio::test]
async fn stream_synthesizes_an_error_terminal_if_backend_ends_early() {
    let app = router(local_config(), Arc::new(UnterminatedStreamBackend)).unwrap();
    let response = app
        .oneshot(request("POST", "/v1/chats/stream", None, sample_chat()))
        .await
        .unwrap();
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();

    assert_eq!(text.matches("event: error\n").count(), 1);
    assert_eq!(text.matches("event: completed\n").count(), 0);
    assert!(text.contains("stream_failed"));
    assert!(text.contains("id: 3\n"));
}

#[tokio::test]
async fn host_and_origin_allowlists_are_enforced() {
    let config = local_config().with_allowed_origins(vec!["http://localhost:3000".to_owned()]);
    let app = router(config, backend()).unwrap();
    let blocked = Request::builder()
        .method("POST")
        .uri("/v1/chats")
        .header("host", "localhost:9000")
        .header("origin", "https://evil.example")
        .header("content-type", "application/json")
        .body(Body::from(sample_chat().to_string()))
        .unwrap();
    let response = app.clone().oneshot(blocked).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let malformed_host = Request::builder()
        .method("POST")
        .uri("http://localhost:9000/v1/chats")
        .header("host", "not a valid host")
        .header("content-type", "application/json")
        .body(Body::from(sample_chat().to_string()))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(malformed_host).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    for malformed_origin in ["http://localhost:3000/", "http://user@localhost:3000"] {
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chats")
            .header("host", "localhost:9000")
            .header("origin", malformed_origin)
            .header("content-type", "application/json")
            .body(Body::from(sample_chat().to_string()))
            .unwrap();
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
}

#[tokio::test]
async fn skills_and_tool_registry_admin_routes_use_typed_backend_operations() {
    let backend = backend();
    let app = router(local_config(), backend.clone()).unwrap();

    let detail = app
        .clone()
        .oneshot(request("GET", "/v1/skills/skill-a", None, json!({})))
        .await
        .unwrap();
    assert_eq!(detail.status(), StatusCode::OK);

    let reload = app
        .clone()
        .oneshot(request("POST", "/v1/skills/reload", None, json!({})))
        .await
        .unwrap();
    assert_eq!(reload.status(), StatusCode::OK);

    let listing = app
        .clone()
        .oneshot(request("GET", "/v1/tools", None, json!({})))
        .await
        .unwrap();
    assert_eq!(listing.status(), StatusCode::OK);

    let upsert = app
        .clone()
        .oneshot(request(
            "PUT",
            "/v1/tools/weather",
            None,
            json!({
                "description": "look up weather",
                "workerId": 7,
                "method": "lookup",
                "requiresApproval": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(upsert.status(), StatusCode::OK);
    assert_eq!(
        backend.last_tool_upsert.lock().unwrap().as_ref().unwrap().0,
        "weather"
    );

    let delete = app
        .clone()
        .oneshot(request("DELETE", "/v1/tools/weather", None, json!({})))
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::OK);

    let reserved = app
        .oneshot(request(
            "PUT",
            "/v1/tools/activate_skill",
            None,
            json!({
                "description": "reserved",
                "workerId": 7,
                "method": "lookup",
                "requiresApproval": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(reserved.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn chat_mutations_reject_non_json_content_types() {
    let app = router(local_config(), backend()).unwrap();
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chats")
        .header("host", "127.0.0.1:9000")
        .header("content-type", "text/plain")
        .body(Body::from(sample_chat().to_string()))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
}
