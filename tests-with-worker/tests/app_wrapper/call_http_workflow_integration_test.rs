//! Integration test for Open Workflow `call: http`.
//!
//! Run with:
//! ```sh
//! cargo test --package tests-with-worker --test app_wrapper -- \
//!     call_http_workflow_integration_test --test-threads=1 --nocapture
//! ```
//!
//! Prerequisites:
//! - Redis must be accessible when the test app is configured for Hybrid/Scalable mode.
//! - Backend worker is started automatically by `start_test_worker`.

#![allow(clippy::uninlined_format_args)]

use super::workflow_standard_error_integration_test::{
    EnvVarGuard, faulted_problem, run_with_worker,
};
use anyhow::{Context, Result};
use app_wrapper::modules::AppWrapperModule;
use app_wrapper::workflow::runner::unified::WorkflowUnifiedRunnerImpl;
use infra::workflow::definition::workflow::errors::StandardErrorType;
use infra_utils::infra::test::TEST_RUNTIME;
use jobworkerp_runner::jobworkerp::runner::workflow_result::WorkflowStatus;
use jobworkerp_runner::jobworkerp::runner::workflow_runner_settings::WorkflowSource;
use jobworkerp_runner::jobworkerp::runner::{
    WorkflowResult, WorkflowRunArgs, WorkflowRunnerSettings,
};
use jobworkerp_runner::runner::RunnerTrait;
use prost::Message;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tests_with_worker::start_test_worker;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const CALL_HTTP_WORKFLOW: &str =
    include_str!("../../../app-wrapper/test-files/workflow-call-http-test.yaml");

const NO_CONTENT_RESPONSE: &str =
    "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

/// HTTP server bound to an ephemeral port that answers one connection per
/// entry of `responses`, in order, with that raw HTTP response. The endpoint
/// uses `path`; the JoinHandle yields the captured raw requests.
async fn start_scripted_http_server(
    path: &str,
    responses: Vec<String>,
) -> Result<(String, tokio::task::JoinHandle<Result<Vec<String>>>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let endpoint = format!("http://{addr}{path}");
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for response in responses {
            let (mut socket, _) = listener.accept().await?;
            let mut request = vec![0_u8; 8192];
            let read = socket.read(&mut request).await?;
            requests.push(String::from_utf8_lossy(&request[..read]).to_string());
            socket.write_all(response.as_bytes()).await?;
        }
        Ok(requests)
    });
    Ok((endpoint, server))
}

/// Single-shot HTTP server that captures one request and replies 204.
async fn start_capturing_http_server(
    path: &str,
) -> Result<(String, tokio::task::JoinHandle<Result<String>>)> {
    let (endpoint, server) =
        start_scripted_http_server(path, vec![NO_CONTENT_RESPONSE.to_string()]).await?;
    let server = tokio::spawn(async move {
        let mut requests = server.await??;
        Ok(requests.remove(0))
    });
    Ok((endpoint, server))
}

/// A raw HTTP response with the given status line, extra headers and body.
fn http_response(status_line: &str, extra_headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status_line}\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Single-shot server that also asserts the request line, for tests that only
/// need to confirm the request was made.
async fn start_single_response_http_server() -> Result<(String, tokio::task::JoinHandle<Result<()>>)>
{
    let (endpoint, server) = start_capturing_http_server("/call-test").await?;
    let server = tokio::spawn(async move {
        let request = server.await??;
        assert!(
            request.starts_with("GET /call-test HTTP/1.1"),
            "unexpected HTTP request line: {request}"
        );
        Ok(())
    });
    Ok((endpoint, server))
}

fn workflow_settings() -> Vec<u8> {
    WorkflowRunnerSettings {
        workflow_source: Some(WorkflowSource::WorkflowData(CALL_HTTP_WORKFLOW.to_string())),
        workflow_context: None,
    }
    .encode_to_vec()
}

fn bearer_secret_workflow(endpoint: &str) -> Vec<u8> {
    // Named bearer auth (`use.authentications`) whose token is a `$secrets`
    // expression. This exercises the `use:` reference path, where the named
    // policy must be expanded with the call's expression context (not sent as
    // the literal `${ ... }` string).
    //
    // `UNUSED_SECRET` is declared but never referenced, and `unusedAuth` is a
    // named policy this call never selects (it references `UNUSED_SECRET`).
    // Neither env var is set: the workflow must still run because only the
    // referenced policy's referenced secrets are resolved.
    let yaml = format!(
        r#"document:
  dsl: 1.0.0
  namespace: test-auth
  name: call-http-bearer-secret
  version: 1.0.0
use:
  secrets: [API_TOKEN, UNUSED_SECRET]
  authentications:
    apiAuth:
      bearer:
        token: "${{ $secrets.API_TOKEN }}"
    unusedAuth:
      bearer:
        token: "${{ $secrets.UNUSED_SECRET }}"
do:
  - fetch:
      call: http
      with:
        method: GET
        endpoint:
          uri: {endpoint}
          authentication:
            use: apiAuth
"#
    );
    WorkflowRunnerSettings {
        workflow_source: Some(WorkflowSource::WorkflowData(yaml)),
        workflow_context: None,
    }
    .encode_to_vec()
}

fn bearer_use_secret_workflow(endpoint: &str) -> Vec<u8> {
    // Official secretBasedAuthenticationPolicy: `bearer: { use: <secretName> }`.
    // The runtime resolves the value from WORKFLOW_SECRET_API_TOKEN.
    let yaml = format!(
        r#"document:
  dsl: 1.0.0
  namespace: test-auth
  name: call-http-bearer-use-secret
  version: 1.0.0
use:
  secrets: [API_TOKEN]
do:
  - fetch:
      call: http
      with:
        method: GET
        endpoint:
          uri: {endpoint}
          authentication:
            bearer:
              use: API_TOKEN
"#
    );
    WorkflowRunnerSettings {
        workflow_source: Some(WorkflowSource::WorkflowData(yaml)),
        workflow_context: None,
    }
    .encode_to_vec()
}

#[test]
fn test_call_http_workflow_executes_with_worker() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app_module = Arc::new(app::module::test::create_hybrid_test_app().await?);
        let worker_handle = start_test_worker(app_module.clone()).await?;
        let app_wrapper_module = Arc::new(AppWrapperModule::new_by_env(None));
        let mut runner = WorkflowUnifiedRunnerImpl::new(app_wrapper_module, app_module)?;
        runner.load(workflow_settings()).await?;

        let (endpoint, server) = start_single_response_http_server().await?;
        let args = WorkflowRunArgs {
            input: serde_json::json!({ "endpoint": &endpoint }).to_string(),
            ..Default::default()
        };
        let (result, _metadata) = runner
            .run(&args.encode_to_vec(), HashMap::new(), None)
            .await;
        worker_handle.shutdown().await;

        if result.is_ok() {
            tokio::time::timeout(Duration::from_secs(5), server)
                .await
                .context("HTTP test server did not receive the workflow request")?
                .context("HTTP test server task panicked")??;
        } else {
            server.abort();
        }

        let output = result?;
        let workflow_result = WorkflowResult::decode(&output[..])?;
        assert_eq!(
            workflow_result.status,
            WorkflowStatus::Completed as i32,
            "workflow failed: {:?}",
            workflow_result.error_message
        );

        let output: serde_json::Value = serde_json::from_str(&workflow_result.output)?;
        assert_eq!(output["statusCode"], serde_json::json!(204));
        assert_eq!(output["content"], serde_json::json!(""));
        assert_eq!(output["request"]["method"], serde_json::json!("GET"));
        assert_eq!(output["request"]["uri"], serde_json::json!(endpoint));
        assert_eq!(
            output["request"]["headers"]["Accept"],
            serde_json::json!("*/*")
        );

        Ok(())
    })
}

#[test]
fn test_call_http_bearer_authentication_from_secret() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        // Secret value is supplied via env, never written in the workflow.
        let _secret = EnvVarGuard::set("WORKFLOW_SECRET_API_TOKEN", "tok-abc-123");

        let app_module = Arc::new(app::module::test::create_hybrid_test_app().await?);
        let worker_handle = start_test_worker(app_module.clone()).await?;
        let app_wrapper_module = Arc::new(AppWrapperModule::new_by_env(None));
        let mut runner = WorkflowUnifiedRunnerImpl::new(app_wrapper_module, app_module)?;

        let (endpoint, server) = start_capturing_http_server("/auth-test").await?;
        runner.load(bearer_secret_workflow(&endpoint)).await?;

        let args = WorkflowRunArgs {
            input: serde_json::json!({}).to_string(),
            ..Default::default()
        };
        let (result, _metadata) = runner
            .run(&args.encode_to_vec(), HashMap::new(), None)
            .await;
        worker_handle.shutdown().await;

        let captured = if result.is_ok() {
            tokio::time::timeout(Duration::from_secs(5), server)
                .await
                .context("HTTP test server did not receive the workflow request")?
                .context("HTTP test server task panicked")??
        } else {
            server.abort();
            let output = result?;
            let workflow_result = WorkflowResult::decode(&output[..])?;
            panic!("workflow failed: {:?}", workflow_result.error_message);
        };

        // The resolved secret reached the server as a bearer Authorization header.
        assert!(
            captured.contains("authorization: Bearer tok-abc-123")
                || captured.contains("Authorization: Bearer tok-abc-123"),
            "expected bearer Authorization header, got request:\n{captured}"
        );

        let output = result?;
        let workflow_result = WorkflowResult::decode(&output[..])?;
        assert_eq!(
            workflow_result.status,
            WorkflowStatus::Completed as i32,
            "workflow failed: {:?}",
            workflow_result.error_message
        );

        Ok(())
    })
}

#[test]
fn test_call_http_bearer_use_secret_reference() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        // Official `bearer: { use: API_TOKEN }` secret reference, resolved from env.
        let _secret = EnvVarGuard::set("WORKFLOW_SECRET_API_TOKEN", "use-ref-token");

        let app_module = Arc::new(app::module::test::create_hybrid_test_app().await?);
        let worker_handle = start_test_worker(app_module.clone()).await?;
        let app_wrapper_module = Arc::new(AppWrapperModule::new_by_env(None));
        let mut runner = WorkflowUnifiedRunnerImpl::new(app_wrapper_module, app_module)?;

        let (endpoint, server) = start_capturing_http_server("/use-secret-test").await?;
        runner.load(bearer_use_secret_workflow(&endpoint)).await?;

        let args = WorkflowRunArgs {
            input: serde_json::json!({}).to_string(),
            ..Default::default()
        };
        let (result, _metadata) = runner
            .run(&args.encode_to_vec(), HashMap::new(), None)
            .await;
        worker_handle.shutdown().await;

        let captured = if result.is_ok() {
            tokio::time::timeout(Duration::from_secs(5), server)
                .await
                .context("HTTP test server did not receive the workflow request")?
                .context("HTTP test server task panicked")??
        } else {
            server.abort();
            let output = result?;
            let workflow_result = WorkflowResult::decode(&output[..])?;
            panic!("workflow failed: {:?}", workflow_result.error_message);
        };

        assert!(
            captured.contains("authorization: Bearer use-ref-token")
                || captured.contains("Authorization: Bearer use-ref-token"),
            "expected bearer Authorization header, got request:\n{captured}"
        );

        let output = result?;
        let workflow_result = WorkflowResult::decode(&output[..])?;
        assert_eq!(
            workflow_result.status,
            WorkflowStatus::Completed as i32,
            "workflow failed: {:?}",
            workflow_result.error_message
        );

        Ok(())
    })
}

const COMMUNICATION_TYPE: &str = StandardErrorType::Communication.uri();

fn workflow_json(name: &str, tasks: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "document": { "dsl": "1.0.0", "namespace": "test", "name": name, "version": "1.0.0" },
        "do": tasks
    })
}

fn workflow_document(name: &str, tasks: serde_json::Value) -> String {
    workflow_json(name, tasks).to_string()
}

fn get_call(endpoint: &str) -> serde_json::Value {
    serde_json::json!({ "call": "http", "with": { "method": "get", "endpoint": endpoint } })
}

fn decode_completed(result: Result<WorkflowResult>) -> Result<serde_json::Value> {
    let workflow_result = result?;
    assert_eq!(
        workflow_result.status,
        WorkflowStatus::Completed as i32,
        "workflow failed: {:?}",
        workflow_result.error_message
    );
    Ok(serde_json::from_str(&workflow_result.output)?)
}

#[test]
fn test_call_http_not_found_faults_with_communication_error() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let (endpoint, server) = start_scripted_http_server(
            "/missing",
            vec![http_response(
                "404 Not Found",
                "Content-Type: text/plain\r\n",
                "no such item",
            )],
        )
        .await?;
        let workflow = workflow_document(
            "call-http-404",
            serde_json::json!([{ "fetch": get_call(&endpoint) }]),
        );

        let result = run_with_worker(workflow).await;
        server.abort();

        let problem = faulted_problem(result);
        assert_eq!(problem.r#type, COMMUNICATION_TYPE);
        assert_eq!(problem.status, 404);
        assert_eq!(problem.title.as_deref(), Some("HTTP 404 Not Found"));
        assert_eq!(problem.detail.as_deref(), Some("no such item"));
        Ok(())
    })
}

#[test]
fn test_call_http_error_status_is_caught_by_status_filter() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let (endpoint, server) = start_scripted_http_server(
            "/missing",
            vec![http_response(
                "404 Not Found",
                "Content-Type: application/json\r\n",
                r#"{"error":"no such item"}"#,
            )],
        )
        .await?;
        let workflow = workflow_document(
            "call-http-404-caught",
            serde_json::json!([{ "guarded": {
                "try": [ { "fetch": get_call(&endpoint) } ],
                "catch": {
                    "errors": { "with": { "type": COMMUNICATION_TYPE, "status": 404 } },
                    "do": [ { "fallback": { "set": {
                        "caught": true,
                        "status": "${ $error.status }",
                        "detail": "${ $error.detail | fromjson }"
                    } } } ]
                }
            } }]),
        );

        let result = run_with_worker(workflow).await;
        server.abort();

        let output = decode_completed(result)?;
        assert_eq!(output["caught"], serde_json::json!(true), "{output}");
        assert_eq!(output["status"], serde_json::json!(404), "{output}");
        assert_eq!(
            output["detail"]["error"],
            serde_json::json!("no such item"),
            "{output}"
        );
        Ok(())
    })
}

#[test]
fn test_call_http_service_unavailable_is_retried_until_success() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let (endpoint, server) = start_scripted_http_server(
            "/flaky",
            vec![
                http_response("503 Service Unavailable", "", "busy"),
                http_response(
                    "200 OK",
                    "Content-Type: application/json\r\n",
                    r#"{"ok":true}"#,
                ),
            ],
        )
        .await?;
        let workflow = workflow_document(
            "call-http-retry",
            serde_json::json!([{ "guarded": {
                "try": [ { "fetch": get_call(&endpoint) } ],
                "catch": {
                    "errors": { "with": { "type": COMMUNICATION_TYPE, "status": 503 } },
                    "retry": {
                        "delay": { "milliseconds": 100 },
                        "backoff": { "constant": {} },
                        "limit": { "attempt": { "count": 3 } }
                    }
                }
            } }]),
        );

        let result = run_with_worker(workflow).await;
        let requests = tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .context("HTTP test server did not receive both requests")???;

        let output = decode_completed(result)?;
        assert_eq!(output, serde_json::json!({"ok": true}));
        assert_eq!(requests.len(), 2);
        Ok(())
    })
}

/// A 3xx the client does not follow (no `Location`), answered to a call with
/// the given `redirect` setting.
fn unfollowed_redirect_workflow(endpoint: &str, redirect: bool) -> String {
    workflow_document(
        "call-http-redirect",
        serde_json::json!([{ "fetch": { "call": "http", "with": {
            "method": "get",
            "endpoint": endpoint,
            "redirect": redirect,
            "output": "response"
        } } }]),
    )
}

#[test]
fn test_call_http_redirect_true_accepts_unfollowed_3xx() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let (endpoint, server) = start_scripted_http_server(
            "/choices",
            vec![http_response("300 Multiple Choices", "", "")],
        )
        .await?;

        let result = run_with_worker(unfollowed_redirect_workflow(&endpoint, true)).await;
        server.abort();

        let output = decode_completed(result)?;
        assert_eq!(output["statusCode"], serde_json::json!(300), "{output}");
        Ok(())
    })
}

#[test]
fn test_call_http_redirect_false_rejects_unfollowed_3xx() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let (endpoint, server) = start_scripted_http_server(
            "/choices",
            vec![http_response("300 Multiple Choices", "", "")],
        )
        .await?;

        let result = run_with_worker(unfollowed_redirect_workflow(&endpoint, false)).await;
        server.abort();

        let problem = faulted_problem(result);
        assert_eq!(problem.r#type, COMMUNICATION_TYPE);
        assert_eq!(problem.status, 300);
        Ok(())
    })
}

// Mirrors the comment-posting steps of the PR auto-review workflow: a failed
// post is recorded from the caught error and later tasks keep running.
#[test]
fn test_call_http_error_status_recorded_in_catch_is_visible_to_later_tasks() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let (endpoint, server) = start_scripted_http_server(
            "/comments",
            vec![http_response("403 Forbidden", "", "forbidden")],
        )
        .await?;
        let workflow = workflow_document(
            "call-http-record-status",
            serde_json::json!([
                { "post": {
                    "try": [ { "postRequest": {
                        "call": "http",
                        "with": { "method": "post", "endpoint": endpoint, "body": {}, "output": "response" },
                        "export": { "as": { "http_status": "${ .statusCode // 0 }" } }
                    } } ],
                    "catch": {
                        "errors": { "with": { "type": COMMUNICATION_TYPE } },
                        "as": "post_error",
                        "do": [ { "recordStatus": { "set": {
                            "http_status": "${ $post_error.status // 0 }"
                        } } } ]
                    }
                } },
                { "report": { "set": { "recorded": "${ $http_status }" } } }
            ]),
        );

        let result = run_with_worker(workflow).await;
        server.abort();

        let output = decode_completed(result)?;
        assert_eq!(output["recorded"], serde_json::json!(403), "{output}");
        Ok(())
    })
}

const API_KEY_ENV: &str = "WORKFLOW_SECRET_API_KEY";
// Contains `/` so that the URI carries it percent-encoded.
const API_KEY_VALUE: &str = "k3y/value";
const API_KEY_VALUE_ENCODED: &str = "k3y%2Fvalue";
const SESSION_COOKIE_VALUE: &str = "abc123";

fn api_key_call_workflow(endpoint: &str) -> String {
    let mut workflow = workflow_json(
        "call-http-masking",
        serde_json::json!([{ "fetch": { "call": "http", "with": {
            "method": "get",
            "endpoint": endpoint,
            "query": { "api_key": "${ $secrets.API_KEY }" },
            "headers": {
                "X-Api-Key": "${ $secrets.API_KEY }",
                "Cookie": format!("session={SESSION_COOKIE_VALUE}")
            },
            "output": "response"
        } } }]),
    );
    workflow["use"] = serde_json::json!({ "secrets": ["API_KEY"] });
    workflow.to_string()
}

#[test]
fn test_call_http_response_output_masks_credentials_in_request() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let _secret = EnvVarGuard::set(API_KEY_ENV, API_KEY_VALUE);
        let (endpoint, server) = start_scripted_http_server(
            "/items",
            vec![http_response(
                "200 OK",
                "Content-Type: application/json\r\n",
                "{}",
            )],
        )
        .await?;

        let result = run_with_worker(api_key_call_workflow(&endpoint)).await;
        let requests = tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .context("HTTP test server did not receive the workflow request")???;

        // The real secret was sent to the service...
        assert!(
            requests[0].contains(&format!("api_key={API_KEY_VALUE_ENCODED}")),
            "{}",
            requests[0]
        );
        // ...but the task output does not carry it.
        let output = decode_completed(result)?;
        let request = output["request"].to_string();
        assert!(!request.contains(API_KEY_VALUE), "{request}");
        assert!(!request.contains(API_KEY_VALUE_ENCODED), "{request}");
        assert!(!request.contains(SESSION_COOKIE_VALUE), "{request}");
        assert_eq!(
            output["request"]["headers"]["Cookie"],
            serde_json::json!("***")
        );
        Ok(())
    })
}

#[test]
fn test_call_http_error_detail_masks_secret_echoed_by_service() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let _secret = EnvVarGuard::set(API_KEY_ENV, API_KEY_VALUE);
        let (endpoint, server) = start_scripted_http_server(
            "/items",
            vec![http_response(
                "403 Forbidden",
                "Content-Type: text/plain\r\n",
                &format!("invalid api key {API_KEY_VALUE}"),
            )],
        )
        .await?;

        let result = run_with_worker(api_key_call_workflow(&endpoint)).await;
        server.abort();

        let problem = faulted_problem(result);
        assert_eq!(problem.status, 403);
        let detail = problem.detail.unwrap_or_default();
        assert!(!detail.contains(API_KEY_VALUE), "{detail}");
        assert!(detail.contains("invalid api key"), "{detail}");
        Ok(())
    })
}

/// A plain-text 401 response with the given body.
fn unauthorized_text(body: &str) -> String {
    http_response("401 Unauthorized", "Content-Type: text/plain\r\n", body)
}

const API_TOKEN_ENV: &str = "WORKFLOW_SECRET_API_TOKEN";
const API_TOKEN_VALUE: &str = "tok-from-policy-123";
const API_USER_ENV: &str = "WORKFLOW_SECRET_API_USER";
const API_USER_VALUE: &str = "svc-user-789";

/// A call whose credential is resolved by the authentication step (not by
/// `with`), against a service that answers 401 echoing `echoed` back. The
/// secret `secret_env` is set, `use` is the workflow's `use` block, and
/// `authentication` the endpoint authentication.
async fn assert_auth_value_masked_in_error_detail(
    secret_env: (&'static str, &str),
    use_block: serde_json::Value,
    authentication: serde_json::Value,
    echoed: &str,
) -> Result<()> {
    let _secret = EnvVarGuard::set(secret_env.0, secret_env.1);
    let (endpoint, server) = start_scripted_http_server(
        "/items",
        vec![unauthorized_text(&format!("rejected credential {echoed}"))],
    )
    .await?;
    let mut workflow = workflow_json(
        "call-http-auth-masking",
        serde_json::json!([{ "fetch": { "call": "http", "with": {
            "method": "get",
            "endpoint": { "uri": endpoint, "authentication": authentication }
        } } }]),
    );
    workflow["use"] = use_block;

    let result = run_with_worker(workflow.to_string()).await;
    server.abort();

    let problem = faulted_problem(result);
    assert_eq!(problem.status, 401);
    let detail = problem.detail.unwrap_or_default();
    assert!(!detail.contains(echoed), "{detail}");
    assert!(detail.contains("rejected credential"), "{detail}");
    Ok(())
}

fn bearer_policy_use_block() -> serde_json::Value {
    serde_json::json!({
        "secrets": ["API_TOKEN"],
        "authentications": { "apiAuth": { "bearer": { "token": "${ $secrets.API_TOKEN }" } } }
    })
}

fn basic_policy_use_block(username: &str) -> serde_json::Value {
    serde_json::json!({
        "secrets": ["API_USER"],
        "authentications": { "basicAuth": { "basic": { "username": username, "password": "pw" } } }
    })
}

#[test]
fn test_call_http_error_detail_masks_token_from_named_policy() -> Result<()> {
    TEST_RUNTIME.block_on(assert_auth_value_masked_in_error_detail(
        (API_TOKEN_ENV, API_TOKEN_VALUE),
        bearer_policy_use_block(),
        serde_json::json!({ "use": "apiAuth" }),
        &format!("Bearer {API_TOKEN_VALUE}"),
    ))
}

#[test]
fn test_call_http_error_detail_masks_token_from_secret_reference() -> Result<()> {
    TEST_RUNTIME.block_on(assert_auth_value_masked_in_error_detail(
        (API_TOKEN_ENV, API_TOKEN_VALUE),
        bearer_policy_use_block(),
        serde_json::json!({ "bearer": { "use": "API_TOKEN" } }),
        &format!("Bearer {API_TOKEN_VALUE}"),
    ))
}

#[test]
fn test_call_http_error_detail_masks_basic_username_from_named_policy() -> Result<()> {
    TEST_RUNTIME.block_on(assert_auth_value_masked_in_error_detail(
        (API_USER_ENV, API_USER_VALUE),
        basic_policy_use_block("${ $secrets.API_USER }"),
        serde_json::json!({ "use": "basicAuth" }),
        API_USER_VALUE,
    ))
}

#[test]
fn test_call_http_error_detail_masks_transformed_basic_username() -> Result<()> {
    TEST_RUNTIME.block_on(assert_auth_value_masked_in_error_detail(
        (API_USER_ENV, API_USER_VALUE),
        basic_policy_use_block("${ $secrets.API_USER | ascii_upcase }"),
        serde_json::json!({ "use": "basicAuth" }),
        &API_USER_VALUE.to_ascii_uppercase(),
    ))
}

#[test]
fn test_call_http_runner_failure_detail_masks_secret_in_url() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let _secret = EnvVarGuard::set(API_KEY_ENV, API_KEY_VALUE);
        // Nothing listens on the port, so the runner fails and its error text
        // (which names the request URL) becomes the error detail.
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}/items", listener.local_addr()?);
        drop(listener);

        let result = run_with_worker(api_key_call_workflow(&endpoint)).await;

        let problem = faulted_problem(result);
        assert_eq!(problem.r#type, COMMUNICATION_TYPE);
        let text = format!("{:?} {:?}", problem.title, problem.detail);
        assert!(!text.contains(API_KEY_VALUE), "{text}");
        assert!(!text.contains(API_KEY_VALUE_ENCODED), "{text}");
        Ok(())
    })
}

/// A call to an endpoint carrying `userinfo`, against a service that answers
/// 401 echoing the Basic value `sent_basic` that the client derives from it.
/// Also checks that the client really sends that value.
async fn assert_userinfo_basic_masked_in_error_detail(
    userinfo: &str,
    sent_basic: &str,
) -> Result<()> {
    let (endpoint, server) = start_scripted_http_server(
        "/items",
        vec![unauthorized_text(&format!(
            "rejected Authorization: Basic {sent_basic}"
        ))],
    )
    .await?;
    let endpoint = endpoint.replacen("http://", &format!("http://{userinfo}@"), 1);
    let workflow = workflow_document(
        "call-http-userinfo-masking",
        serde_json::json!([{ "fetch": get_call(&endpoint) }]),
    );

    let result = run_with_worker(workflow).await;
    let requests = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .context("HTTP test server did not receive the workflow request")???;

    assert!(
        requests[0].contains(&format!("Basic {sent_basic}\r\n")),
        "{}",
        requests[0]
    );
    let detail = faulted_problem(result).detail.unwrap_or_default();
    assert!(!detail.contains(sent_basic), "{detail}");
    Ok(())
}

#[test]
fn test_call_http_error_detail_masks_basic_value_from_uri_userinfo() -> Result<()> {
    // base64("alice:pa ss"): the client sends the decoded userinfo as Basic auth.
    TEST_RUNTIME.block_on(assert_userinfo_basic_masked_in_error_detail(
        "alice:pa%20ss",
        "YWxpY2U6cGEgc3M=",
    ))
}

#[test]
fn test_call_http_error_detail_masks_basic_value_from_non_utf8_userinfo_password() -> Result<()> {
    // The client drops a password that is not UTF-8 and sends base64("alice:").
    TEST_RUNTIME.block_on(assert_userinfo_basic_masked_in_error_detail(
        "alice:pa%FFss",
        "YWxpY2U6",
    ))
}
