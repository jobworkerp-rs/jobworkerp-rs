//! Integration test for Serverless Workflow `call: http`.
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

use anyhow::{Context, Result};
use app_wrapper::modules::AppWrapperModule;
use app_wrapper::workflow::runner::unified::WorkflowUnifiedRunnerImpl;
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

/// Single-shot HTTP server bound to an ephemeral port. Accepts one request,
/// captures its raw text, and replies 204. The endpoint uses `path`; the
/// JoinHandle yields the captured request so callers can assert on it.
async fn start_capturing_http_server(
    path: &str,
) -> Result<(String, tokio::task::JoinHandle<Result<String>>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let endpoint = format!("http://{addr}{path}");
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        let mut request = vec![0_u8; 8192];
        let read = socket.read(&mut request).await?;
        let request = String::from_utf8_lossy(&request[..read]).to_string();
        socket
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await?;
        Ok(request)
    });
    Ok((endpoint, server))
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
            input: serde_json::json!({ "endpoint": endpoint }).to_string(),
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
        assert_eq!(output["body"], serde_json::json!(""));

        Ok(())
    })
}

#[test]
fn test_call_http_bearer_authentication_from_secret() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        // Secret value is supplied via env, never written in the workflow.
        // SAFETY: tests run single-threaded (--test-threads=1).
        unsafe {
            std::env::set_var("WORKFLOW_SECRET_API_TOKEN", "tok-abc-123");
        }

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
            unsafe {
                std::env::remove_var("WORKFLOW_SECRET_API_TOKEN");
            }
            let output = result?;
            let workflow_result = WorkflowResult::decode(&output[..])?;
            panic!("workflow failed: {:?}", workflow_result.error_message);
        };
        unsafe {
            std::env::remove_var("WORKFLOW_SECRET_API_TOKEN");
        }

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
        // SAFETY: tests run single-threaded (--test-threads=1).
        unsafe {
            std::env::set_var("WORKFLOW_SECRET_API_TOKEN", "use-ref-token");
        }

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
            unsafe {
                std::env::remove_var("WORKFLOW_SECRET_API_TOKEN");
            }
            let output = result?;
            let workflow_result = WorkflowResult::decode(&output[..])?;
            panic!("workflow failed: {:?}", workflow_result.error_message);
        };
        unsafe {
            std::env::remove_var("WORKFLOW_SECRET_API_TOKEN");
        }

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
