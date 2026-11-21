//! Protocol-level tests for the MCP handler.
//!
//! These tests drive `McpHandler` through a real rmcp client over an in-memory
//! duplex stream, so the wire encoding of `tools/list` and `tools/call`
//! (including request `_meta`) is exercised end to end rather than through
//! direct handler method calls.
//!
//! Requires: SQLite database and Redis (`TEST_REDIS_URL`), as provided by
//! `create_hybrid_test_app`.

use anyhow::Result;
use infra_utils::infra::test::TEST_RUNTIME;
use mcp_server::{McpHandler, McpServerConfig};
use rmcp::model::{CallToolRequestParams, RequestMetaObject};
use rmcp::service::{RoleClient, RunningService, ServiceError};
use rmcp::{ServiceExt, model::JsonObject};

const DUPLEX_BUFFER_SIZE: usize = 1024 * 1024;
// Without the request `_meta` the call would be enqueued as a real GRPC job
// and wait for a worker that never runs, so bound it to fail fast instead.
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

async fn create_handler() -> Result<McpHandler> {
    let app_module = app::module::test::create_hybrid_test_app().await?;
    let config = McpServerConfig {
        exclude_worker_as_tool: true,
        timeout_sec: 30,
        ..Default::default()
    };
    Ok(McpHandler::new(
        app_module.function_app.clone(),
        app_module.function_set_app.clone(),
        config,
    ))
}

/// Serve `handler` on one end of a duplex stream and connect a client to the other.
async fn connect(handler: McpHandler) -> Result<RunningService<RoleClient, ()>> {
    let (server_io, client_io) = tokio::io::duplex(DUPLEX_BUFFER_SIZE);
    tokio::spawn(async move {
        match handler.serve(server_io).await {
            Ok(server) => {
                let _ = server.waiting().await;
            }
            Err(error) => tracing::error!(%error, "test MCP server failed to start"),
        }
    });
    Ok(().serve(client_io).await?)
}

#[test]
fn list_tools_over_protocol_returns_runner_tools() {
    TEST_RUNTIME.block_on(async {
        let client = connect(create_handler().await.unwrap()).await.unwrap();

        let tools = client.list_all_tools().await.unwrap();

        assert!(
            tools.iter().any(|tool| tool.name.starts_with("COMMAND")),
            "COMMAND runner tool should be listed. Found: {:?}",
            tools.iter().map(|tool| &tool.name).collect::<Vec<_>>()
        );
        client.cancel().await.unwrap();
    });
}

#[test]
fn call_tool_over_protocol_rejects_unpublished_tool() {
    TEST_RUNTIME.block_on(async {
        let client = connect(create_handler().await.unwrap()).await.unwrap();

        let error = client
            .call_tool(CallToolRequestParams::new("NO_SUCH_TOOL___nothing"))
            .await
            .expect_err("calling an unpublished tool must fail");

        assert!(
            matches!(error, ServiceError::McpError(_)),
            "expected an MCP error response, got {error:?}"
        );
        client.cancel().await.unwrap();
    });
}

#[test]
fn call_tool_over_protocol_forwards_request_meta() {
    TEST_RUNTIME.block_on(async {
        let client = connect(create_handler().await.unwrap()).await.unwrap();
        let tools = client.list_all_tools().await.unwrap();
        let grpc_tool = tools
            .iter()
            .find(|tool| tool.name.starts_with("GRPC"))
            .expect("generic GRPC runner tool should be listed");

        // `_meta.jobworkerp/runner` is only valid for fixed gRPC worker tools, so a
        // generic GRPC runner tool reports an input-stage error. Observing that
        // error proves the request `_meta` reached the handler intact.
        let mut params =
            CallToolRequestParams::new(grpc_tool.name.clone()).with_arguments(JsonObject::new());
        params.meta = Some(RequestMetaObject::from(rmcp::object!({
            "jobworkerp/runner": {}
        })));
        let result = tokio::time::timeout(CALL_TIMEOUT, client.call_tool(params))
            .await
            .expect("tools/call should be answered without running a job")
            .unwrap();

        assert_eq!(result.is_error, Some(true));
        let error = result
            .structured_content
            .as_ref()
            .and_then(|content| content.get("error"))
            .expect("structured error payload");
        assert_eq!(error["stage"], "input");
        assert!(
            error["message"]
                .as_str()
                .is_some_and(|message| message.contains("jobworkerp/runner")),
            "unexpected error payload: {error}"
        );
        client.cancel().await.unwrap();
    });
}
