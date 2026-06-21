//! Integration test for Serverless Workflow `call: grpc`.
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
use tokio::sync::oneshot;
use tonic::transport::Server;

mod echo {
    tonic::include_proto!("echo");
    pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("echo_descriptor");
}

use echo::echo_service_server::{EchoService, EchoServiceServer};
use echo::{EchoReply, EchoRequest};

#[derive(Default)]
struct EchoImpl;

#[tonic::async_trait]
impl EchoService for EchoImpl {
    async fn echo(
        &self,
        request: tonic::Request<EchoRequest>,
    ) -> Result<tonic::Response<EchoReply>, tonic::Status> {
        let message = request.into_inner().message;
        Ok(tonic::Response::new(EchoReply {
            message: format!("echo: {message}"),
        }))
    }
}

/// Start a reflection-enabled echo gRPC server on an ephemeral port. Returns the
/// bound address and a shutdown sender; the server runs until the sender fires.
async fn start_echo_grpc_server() -> Result<(SocketAddr, oneshot::Sender<()>)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    let reflection = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(echo::FILE_DESCRIPTOR_SET)
        .build_v1()?;

    let (tx, rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = Server::builder()
            .add_service(EchoServiceServer::new(EchoImpl))
            .add_service(reflection)
            .serve_with_incoming_shutdown(incoming, async {
                let _ = rx.await;
            })
            .await;
    });
    Ok((addr, tx))
}

fn echo_grpc_workflow(host: &str, port: u16) -> Vec<u8> {
    let yaml = format!(
        r#"document:
  dsl: 1.0.0
  namespace: test-grpc
  name: call-grpc-echo
  version: 1.0.0
do:
  - greet:
      call: grpc
      with:
        service:
          name: echo.EchoService
          host: {host}
          port: {port}
        method: Echo
        arguments:
          message: world
"#
    );
    WorkflowRunnerSettings {
        workflow_source: Some(WorkflowSource::WorkflowData(yaml)),
        workflow_context: None,
    }
    .encode_to_vec()
}

#[test]
#[ignore = "requires Redis and starts a local gRPC server"]
fn test_call_grpc_unary_workflow_executes_with_worker() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let (addr, shutdown) = start_echo_grpc_server().await?;

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
