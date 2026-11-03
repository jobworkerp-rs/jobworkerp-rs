//! Integration test for Open Workflow `call: grpc`.
//!
//! Run with:
//! ```sh
//! cargo test --package tests-with-worker --test app_wrapper -- \
//!     call_grpc_workflow_integration_test --ignored --test-threads=1 --nocapture
//! ```
//!
//! Prerequisites:
//! - Redis must be accessible when the test app is configured for Hybrid/Scalable mode.
//! - Backend worker is started automatically by `start_test_worker`.
//!
//! The test starts a reflection-enabled echo gRPC server on an ephemeral port so
//! the GRPC runner can convert the JSON `arguments` body to protobuf and the
//! protobuf reply back to JSON, exercising the whole `call: grpc` path.

#![allow(clippy::uninlined_format_args)]

use anyhow::Result;
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
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;
use tonic::transport::Server;

mod echo {
    tonic::include_proto!("echo");
    pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("echo_descriptor");
}

use echo::echo_service_server::{EchoService, EchoServiceServer};
use echo::{EchoReply, EchoRequest};

// A request with this message is answered with a NOT_FOUND status, so tests can
// exercise a non-OK gRPC status without another RPC.
const NOT_FOUND_MESSAGE: &str = "missing";
// A request with this message is rejected as UNAUTHENTICATED with the received
// authorization metadata echoed in the status message.
const ECHO_AUTH_MESSAGE: &str = "echo-auth";

/// The authorization metadata a test server received, echoed back by the
/// rejecting endpoints so that tests can check it is masked.
fn received_authorization(metadata: &tonic::metadata::MetadataMap) -> String {
    metadata
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

#[derive(Default)]
struct EchoImpl {
    delay: Duration,
}

#[tonic::async_trait]
impl EchoService for EchoImpl {
    async fn echo(
        &self,
        request: tonic::Request<EchoRequest>,
    ) -> Result<tonic::Response<EchoReply>, tonic::Status> {
        tokio::time::sleep(self.delay).await;
        let authorization = received_authorization(request.metadata());
        let message = request.into_inner().message;
        if message == ECHO_AUTH_MESSAGE {
            return Err(tonic::Status::unauthenticated(format!(
                "rejected authorization: {authorization}"
            )));
        }
        if message == NOT_FOUND_MESSAGE {
            return Err(tonic::Status::not_found("no echo for missing"));
        }
        Ok(tonic::Response::new(EchoReply {
            message: format!("echo: {message}"),
        }))
    }
}

/// Start a reflection-enabled echo gRPC server on an ephemeral port that
/// answers after `delay`. Returns the bound address and a shutdown sender; the
/// server runs until the sender fires.
pub(super) async fn start_echo_grpc_server(
    delay: Duration,
) -> Result<(SocketAddr, oneshot::Sender<()>)> {
    start_echo_grpc_server_with(delay, false).await
}

/// Like [`start_echo_grpc_server`], but when `reject_reflection` is set the
/// reflection service rejects every call as UNAUTHENTICATED, echoing the
/// received authorization metadata in the status message.
async fn start_echo_grpc_server_with(
    delay: Duration,
    reject_reflection: bool,
) -> Result<(SocketAddr, oneshot::Sender<()>)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    let reflection = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(echo::FILE_DESCRIPTOR_SET)
        .build_v1()?;

    let reflection = tonic::service::interceptor::InterceptedService::new(
        reflection,
        move |request: tonic::Request<()>| {
            if !reject_reflection {
                return Ok(request);
            }
            Err(tonic::Status::unauthenticated(format!(
                "reflection rejected authorization: {}",
                received_authorization(request.metadata())
            )))
        },
    );

    let (tx, rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = Server::builder()
            .add_service(EchoServiceServer::new(EchoImpl { delay }))
            .add_service(reflection)
            .serve_with_incoming_shutdown(incoming, async {
                let _ = rx.await;
            })
            .await;
    });
    Ok((addr, tx))
}

fn echo_grpc_workflow(host: &str, port: u16) -> Vec<u8> {
    WorkflowRunnerSettings {
        workflow_source: Some(WorkflowSource::WorkflowData(echo_grpc_workflow_source(
            host, port, "world", None,
        ))),
        workflow_context: None,
    }
    .encode_to_vec()
}

/// An echo `call: grpc` workflow (JSON text). With `bearer_secret`, the
/// service authenticates with that declared secret.
fn echo_grpc_workflow_source(
    host: &str,
    port: u16,
    message: &str,
    bearer_secret: Option<&str>,
) -> String {
    let mut service = serde_json::json!({ "name": "echo.EchoService", "host": host, "port": port });
    let mut workflow = serde_json::json!({
        "document": { "dsl": "1.0.0", "namespace": "test-grpc", "name": "call-grpc-echo", "version": "1.0.0" },
        "do": []
    });
    if let Some(secret) = bearer_secret {
        service["authentication"] = serde_json::json!({ "bearer": { "use": secret } });
        workflow["use"] = serde_json::json!({ "secrets": [secret] });
    }
    workflow["do"] = serde_json::json!([{ "greet": { "call": "grpc", "with": {
        "service": service,
        "method": "Echo",
        "arguments": { "message": message }
    } } }]);
    workflow.to_string()
}

#[test]
#[ignore = "requires Redis and starts a local gRPC server"]
fn test_call_grpc_unary_workflow_executes_with_worker() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let (addr, shutdown) = start_echo_grpc_server(Duration::ZERO).await?;

        let app_module = Arc::new(app::module::test::create_hybrid_test_app().await?);
        let worker_handle = tests_with_worker::start_test_worker(app_module.clone()).await?;
        let app_wrapper_module = Arc::new(AppWrapperModule::new_by_env(None));
        let mut runner = WorkflowUnifiedRunnerImpl::new(app_wrapper_module, app_module)?;
        runner
            .load(echo_grpc_workflow(&addr.ip().to_string(), addr.port()))
            .await?;

        let args = WorkflowRunArgs {
            input: serde_json::json!({}).to_string(),
            ..Default::default()
        };
        let (result, _metadata) = runner
            .run(&args.encode_to_vec(), HashMap::new(), None)
            .await;
        worker_handle.shutdown().await;
        let _ = shutdown.send(());

        let output = result?;
        let workflow_result = WorkflowResult::decode(&output[..])?;
        assert_eq!(
            workflow_result.status,
            WorkflowStatus::Completed as i32,
            "workflow failed: {:?}",
            workflow_result.error_message
        );

        // The adapter parses the runner's JSON response body into `body` and the
        // echo server replied with `echo: world`.
        let output: serde_json::Value = serde_json::from_str(&workflow_result.output)?;
        assert_eq!(output["code"], serde_json::json!(0));
        assert_eq!(output["body"]["message"], serde_json::json!("echo: world"));

        Ok(())
    })
}

#[test]
#[ignore = "requires Redis and starts a local gRPC server"]
fn test_call_grpc_not_found_status_faults_with_communication_error() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let (addr, shutdown) = start_echo_grpc_server(Duration::ZERO).await?;
        let workflow =
            echo_grpc_workflow_source(&addr.ip().to_string(), addr.port(), NOT_FOUND_MESSAGE, None);

        let result =
            super::workflow_standard_error_integration_test::run_with_worker(workflow).await;
        let _ = shutdown.send(());

        let problem = super::workflow_standard_error_integration_test::faulted_problem(result);
        assert_eq!(problem.r#type, StandardErrorType::Communication.uri());
        assert_eq!(problem.status, 404);
        assert_eq!(problem.title.as_deref(), Some("gRPC NOT_FOUND"));
        assert_eq!(problem.detail.as_deref(), Some("no echo for missing"));
        Ok(())
    })
}

/// A bearer-authenticated echo call against a server that echoes the received
/// authorization back in its error (from the Echo RPC, or from reflection when
/// `reject_reflection` is set); the resulting error must not carry the token.
async fn assert_service_token_masked(
    token: &str,
    reject_reflection: bool,
    message: &str,
    expected_text: &str,
) -> Result<()> {
    let _secret = super::workflow_standard_error_integration_test::EnvVarGuard::set(
        "WORKFLOW_SECRET_API_TOKEN",
        token,
    );
    let (addr, shutdown) = start_echo_grpc_server_with(Duration::ZERO, reject_reflection).await?;
    let workflow = echo_grpc_workflow_source(
        &addr.ip().to_string(),
        addr.port(),
        message,
        Some("API_TOKEN"),
    );

    let result = super::workflow_standard_error_integration_test::run_with_worker(workflow).await;
    let _ = shutdown.send(());

    let problem = super::workflow_standard_error_integration_test::faulted_problem(result);
    let text = format!("{:?} {:?}", problem.title, problem.detail);
    assert!(!text.contains(token), "{text}");
    assert!(text.contains(expected_text), "{text}");
    Ok(())
}

#[test]
#[ignore = "requires Redis and starts a local gRPC server"]
fn test_call_grpc_error_detail_masks_service_token() -> Result<()> {
    TEST_RUNTIME.block_on(assert_service_token_masked(
        "grpc-token-456",
        false,
        ECHO_AUTH_MESSAGE,
        "rejected authorization",
    ))
}

#[test]
#[ignore = "requires Redis and starts a local gRPC server"]
fn test_call_grpc_reflection_failure_detail_masks_service_token() -> Result<()> {
    TEST_RUNTIME.block_on(assert_service_token_masked(
        "grpc-reflection-token-789",
        true,
        "world",
        "reflection rejected",
    ))
}
