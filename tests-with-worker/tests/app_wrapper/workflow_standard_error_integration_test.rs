//! Integration tests for the Open Workflow standard error types raised by
//! tasks that need a worker to run their child jobs.
//!
//! Run with:
//! ```sh
//! cargo test --package tests-with-worker --test app_wrapper -- \
//!     workflow_standard_error_integration_test --test-threads=1 --nocapture
//! ```
//!
//! Prerequisites: Redis must be accessible when the test app is configured for
//! Hybrid/Scalable mode. The backend worker is started by `start_test_worker`.

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
use std::sync::Arc;
use tests_with_worker::start_test_worker;
use tokio::net::TcpListener;

/// A workflow whose only task runs `failing_task` inside a try that catches
/// errors of `error_kind` and outputs `{ "caught": true }`.
fn catching_workflow(failing_task: serde_json::Value, error_kind: StandardErrorType) -> String {
    catching_type(failing_task, error_kind.uri())
}

/// A workflow whose only task runs `failing_task` inside a try that catches
/// errors of `error_type` and outputs `caught` with the caught error's fields.
fn catching_type(failing_task: serde_json::Value, error_type: &str) -> String {
    serde_json::json!({
        "document": {
            "dsl": "1.0.0",
            "namespace": "test",
            "name": "standard-error-test",
            "version": "1.0.0"
        },
        "do": [ { "guarded": {
            "try": [ { "failing": failing_task } ],
            "catch": {
                "errors": { "with": { "type": error_type } },
                "do": [ { "fallback": { "set": {
                    "caught": true,
                    "type": "${ $error.type }",
                    "status": "${ $error.status }",
                    "instance": "${ $error.instance }"
                } } } ]
            }
        } } ]
    })
    .to_string()
}

/// A run task that executes a child workflow made of `tasks`.
fn child_workflow_run(tasks: serde_json::Value) -> serde_json::Value {
    let child = serde_json::json!({
        "document": {
            "dsl": "1.0.0",
            "namespace": "test",
            "name": "standard-error-child",
            "version": "1.0.0"
        },
        "do": tasks
    })
    .to_string();
    serde_json::json!({ "run": { "workflow": { "workflowData": child, "input": {} } } })
}

const CUSTOM_TYPE: &str = "https://example.com/errors/issue-339";

fn raise_custom() -> serde_json::Value {
    serde_json::json!([ { "boom": { "raise": { "error": {
        "type": CUSTOM_TYPE,
        "status": 418,
        "title": "Custom failure"
    } } } } ])
}

pub(super) async fn run_with_worker(workflow: String) -> Result<WorkflowResult> {
    let app_module = Arc::new(app::module::test::create_hybrid_test_app().await?);
    let worker_handle = start_test_worker(app_module.clone()).await?;
    let app_wrapper_module = Arc::new(AppWrapperModule::new_by_env(None));
    let mut runner = WorkflowUnifiedRunnerImpl::new(app_wrapper_module, app_module)?;
    let settings = WorkflowRunnerSettings {
        workflow_source: Some(WorkflowSource::WorkflowData(workflow)),
        workflow_context: None,
    };
    runner.load(settings.encode_to_vec()).await?;
    let args = WorkflowRunArgs {
        input: "{}".to_string(),
        ..Default::default()
    };
    let (result, _metadata) = runner
        .run(&args.encode_to_vec(), HashMap::new(), None)
        .await;
    worker_handle.shutdown().await;
    Ok(WorkflowResult::decode(&result?[..])?)
}

/// Sets an environment variable for the duration of a test and removes it on
/// drop, so a failing test cannot leak a secret into later tests.
pub(super) struct EnvVarGuard(&'static str);

impl EnvVarGuard {
    pub(super) fn set(name: &'static str, value: &str) -> Self {
        // SAFETY: tests run single-threaded (--test-threads=1).
        unsafe { std::env::set_var(name, value) };
        Self(name)
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: tests run single-threaded (--test-threads=1).
        unsafe { std::env::remove_var(self.0) };
    }
}

/// The problem a faulted WORKFLOW job reports with its classified failure.
pub(super) fn faulted_problem(
    result: Result<WorkflowResult>,
) -> proto::jobworkerp::data::ProblemDetails {
    let error = result.expect_err("the workflow must fault");
    let classified = error
        .downcast_ref::<proto::stream_error::ClassifiedJobError>()
        .unwrap_or_else(|| panic!("the failure carries its classification: {error:?}"));
    classified.error.problem.clone().expect("problem")
}

/// Asserts the error was caught and returns the catch block's output.
fn assert_caught(result: &WorkflowResult) -> Result<serde_json::Value> {
    assert_eq!(
        result.status,
        WorkflowStatus::Completed as i32,
        "the error must be caught: {:?}",
        result.error_message
    );
    let output: serde_json::Value = serde_json::from_str(&result.output)?;
    assert_eq!(output["caught"], serde_json::json!(true), "{output}");
    Ok(output)
}

/// An endpoint on a local port with nothing listening on it.
async fn closed_endpoint() -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(format!("http://127.0.0.1:{port}/unreachable"))
}

#[test]
fn call_http_connection_failure_is_a_communication_error() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let call = serde_json::json!({
            "call": "http",
            "with": { "method": "get", "endpoint": closed_endpoint().await? }
        });
        let result =
            run_with_worker(catching_workflow(call, StandardErrorType::Communication)).await?;
        assert_caught(&result).map(drop)
    })
}

#[test]
fn failing_run_child_job_is_a_runtime_error() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let run = serde_json::json!({
            "run": { "runner": {
                "name": "COMMAND",
                "arguments": { "command": "/nonexistent/command-for-issue-340" }
            } }
        });
        let result = run_with_worker(catching_workflow(run, StandardErrorType::Runtime)).await?;
        assert_caught(&result).map(drop)
    })
}

#[test]
fn child_workflow_error_type_and_status_reach_the_parent() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let result = run_with_worker(catching_type(
            child_workflow_run(raise_custom()),
            CUSTOM_TYPE,
        ))
        .await?;
        let output = assert_caught(&result)?;
        assert_eq!(output["type"], CUSTOM_TYPE, "{output}");
        assert_eq!(output["status"], 418, "{output}");
        // Reported at the parent task, not at the child's own position.
        let instance = output["instance"].as_str().unwrap_or_default();
        assert!(instance.contains("failing"), "{output}");
        assert!(!instance.contains("boom"), "{output}");
        Ok(())
    })
}

#[test]
fn child_workflow_task_timeout_is_caught_as_timeout() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let slow = serde_json::json!([ { "slow": {
            "wait": { "seconds": 5 },
            "timeout": { "after": { "milliseconds": 100 } }
        } } ]);
        let result = run_with_worker(catching_workflow(
            child_workflow_run(slow),
            StandardErrorType::Timeout,
        ))
        .await?;
        let output = assert_caught(&result)?;
        assert_eq!(output["status"], 408, "{output}");
        Ok(())
    })
}

#[test]
fn grandchild_workflow_error_reaches_the_parent() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let child = serde_json::json!([ { "nested": child_workflow_run(raise_custom()) } ]);
        let result = run_with_worker(catching_type(child_workflow_run(child), CUSTOM_TYPE)).await?;
        assert_caught(&result).map(drop)
    })
}

#[test]
fn streaming_child_workflow_error_reaches_the_parent() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let mut run = child_workflow_run(raise_custom());
        run["useStreaming"] = serde_json::json!(true);
        let result = run_with_worker(catching_type(run, CUSTOM_TYPE)).await?;
        let output = assert_caught(&result)?;
        assert_eq!(output["status"], 418, "{output}");
        Ok(())
    })
}

#[test]
fn faulted_workflow_job_reports_its_error_and_keeps_the_diagnostic() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let workflow = serde_json::json!({
            "document": {
                "dsl": "1.0.0",
                "namespace": "test",
                "name": "standard-error-faulted",
                "version": "1.0.0"
            },
            "do": raise_custom()
        })
        .to_string();
        let error = run_with_worker(workflow).await.unwrap_err();
        // The output text of a failed WORKFLOW job is unchanged.
        assert!(
            error
                .to_string()
                .starts_with("Failed to execute workflow: "),
            "{error}"
        );
        let classified = error
            .downcast_ref::<proto::stream_error::ClassifiedJobError>()
            .expect("the failure carries its classification");
        assert_eq!(classified.error.origin, "WORKFLOW");
        let problem = classified.error.problem.as_ref().expect("problem");
        assert_eq!(problem.r#type, CUSTOM_TYPE);
        assert_eq!(problem.status, 418);
        assert_eq!(problem.title.as_deref(), Some("Custom failure"));
        Ok(())
    })
}

#[test]
fn run_child_job_exceeding_the_task_timeout_is_a_timeout_error() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        // The child job's own timeout is derived from the task timeout and
        // starts later, so the task timeout is what fires.
        let run = serde_json::json!({
            "run": { "runner": {
                "name": "COMMAND",
                "arguments": { "command": "sleep", "args": ["10"] }
            } },
            "timeout": { "after": { "seconds": 1 } }
        });
        let result = run_with_worker(catching_workflow(run, StandardErrorType::Timeout)).await?;
        let output = assert_caught(&result)?;
        assert_eq!(output["status"], 408, "{output}");
        Ok(())
    })
}

#[test]
fn grpc_runner_deadline_is_a_timeout_error() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let (addr, shutdown) = super::call_grpc_workflow_integration_test::start_echo_grpc_server(
            std::time::Duration::from_secs(5),
        )
        .await?;
        let run = serde_json::json!({ "run": { "runner": {
            "name": "GRPC",
            "using": "unary",
            "settings": {
                "host": addr.ip().to_string(),
                "port": addr.port(),
                "use_reflection": true
            },
            "arguments": {
                "method": "echo.EchoService/Echo",
                "json_body": "{\"message\":\"slow\"}",
                "timeout": 200,
                "as_json": true
            }
        } } });
        let result = run_with_worker(catching_workflow(run, StandardErrorType::Timeout)).await;
        let _ = shutdown.send(());
        assert_caught(&result?).map(drop)
    })
}

#[test]
fn run_arguments_violating_the_runner_schema_are_a_validation_error() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let run = serde_json::json!({ "run": { "runner": {
            "name": "COMMAND",
            "arguments": { "command": "echo", "args": "not-a-list" }
        } } });
        let result = run_with_worker(catching_workflow(run, StandardErrorType::Validation)).await?;
        let output = assert_caught(&result)?;
        assert_eq!(output["status"], 400, "{output}");
        Ok(())
    })
}

#[test]
fn streaming_run_deadline_before_the_stream_starts_is_a_timeout_error() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let (addr, shutdown) = super::call_grpc_workflow_integration_test::start_echo_grpc_server(
            std::time::Duration::from_secs(5),
        )
        .await?;
        // The deadline expires while opening the call, before any stream exists,
        // so the only failure the parent sees is the End the app layer synthesizes.
        let run = serde_json::json!({
            "useStreaming": true,
            "run": { "runner": {
                "name": "GRPC",
                "using": "streaming",
                "settings": {
                    "host": addr.ip().to_string(),
                    "port": addr.port(),
                    "use_reflection": true
                },
                "arguments": {
                    "method": "echo.EchoService/Echo",
                    "json_body": "{\"message\":\"slow\"}",
                    "timeout": 200,
                    "as_json": true
                }
            } }
        });
        let result = run_with_worker(catching_workflow(run, StandardErrorType::Timeout)).await;
        let _ = shutdown.send(());
        assert_caught(&result?).map(drop)
    })
}
