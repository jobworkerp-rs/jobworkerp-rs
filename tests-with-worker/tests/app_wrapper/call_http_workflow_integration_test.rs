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

async fn start_single_response_http_server() -> Result<(String, tokio::task::JoinHandle<Result<()>>)>
{
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let endpoint = format!("http://{addr}/call-test");
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        let mut request = vec![0_u8; 4096];
        let read = socket.read(&mut request).await?;
        let request = String::from_utf8_lossy(&request[..read]);
        assert!(
            request.starts_with("GET /call-test HTTP/1.1"),
            "unexpected HTTP request line: {request}"
        );
        socket
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await?;
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
