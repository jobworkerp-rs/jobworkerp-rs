//! Integration tests for WORKFLOW runner cancellation.
//!
//! Covers the cancellation path added so that cancelling a WORKFLOW job
//! actively cancels its in-flight child jobs (instead of waiting for each to
//! finish) and unwinds the execution loop promptly. See
//! `WorkflowExecutor::cancel` (child-job fan-out) and
//! `WorkflowUnifiedRunnerImpl::execute_run{,_stream}` (token watch).
//!
//! These use the rdb_chan (memory + SQLite) app so they run without Redis,
//! matching the "Memory environment" the original issue was reported against.
//!
//! Tests that need a worker to actually execute the child job (to observe
//! end-to-end propagation through delete_job -> broadcast -> result publish)
//! are marked `#[ignore]`: no worker runs inside the test process, so a child
//! job would never complete. The pre-cancellation test needs no worker.

#![allow(clippy::uninlined_format_args)]

use anyhow::Result;
use app::module::test::create_rdb_chan_test_app;
use app_wrapper::modules::AppWrapperModule;
use app_wrapper::workflow::runner::unified::WorkflowUnifiedRunnerImpl;
use infra_utils::infra::test::TEST_RUNTIME;
use jobworkerp_runner::jobworkerp::runner::workflow_runner_settings::WorkflowSource as SettingsWorkflowSource;
use jobworkerp_runner::jobworkerp::runner::{WorkflowRunArgs, WorkflowRunnerSettings};
use jobworkerp_runner::runner::RunnerTrait;
use jobworkerp_runner::runner::cancellation_helper::CancelMonitoringHelper;
use jobworkerp_runner::runner::test_common::mock::MockCancellationManager;
use prost::Message;
use proto::jobworkerp::data::{QueueType, ResponseType, RunnerId, WorkerData};
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Built-in COMMAND runner ID.
const COMMAND_RUNNER_ID: i64 = 1;

/// Workflow whose single task runs a long `sleep` via the COMMAND runner, so
/// the child job stays Running long enough to be cancelled mid-flight.
fn sleep_workflow_json() -> String {
    r#"{
        "document": {
            "dsl": "0.0.1",
            "namespace": "test",
            "name": "sleep-workflow",
            "version": "1.0.0"
        },
        "do": [
            {
                "sleep-task": {
                    "run": {
                        "runner": {
                            "name": "COMMAND",
                            "arguments": {
                                "command": "sleep",
                                "args": ["30"]
                            }
                        }
                    }
                }
            }
        ]
    }"#
    .to_string()
}

/// Same long-running child job as `sleep_workflow_json`, but dispatched via a
/// named worker. This mirrors `thread-summary-single.yaml`'s
/// `generateSummary` shape (`workerName: memories-llm`) rather than the
/// runnerName path.
fn worker_name_sleep_workflow_json(worker_name: &str) -> String {
    format!(
        r#"{{
        "document": {{
            "dsl": "0.0.1",
            "namespace": "test",
            "name": "worker-name-sleep-workflow",
            "version": "1.0.0"
        }},
        "do": [
            {{
                "sleep-task": {{
                    "run": {{
                        "function": {{
                            "workerName": "{worker_name}",
                            "arguments": {{
                                "command": "sleep",
                                "args": ["30"]
                            }}
                        }}
                    }}
                }}
            }}
        ]
    }}"#
    )
}

fn workflow_settings(workflow_json: &str) -> Vec<u8> {
    WorkflowRunnerSettings {
        workflow_source: Some(SettingsWorkflowSource::WorkflowData(
            workflow_json.to_string(),
        )),
        workflow_context: None,
    }
    .encode_to_vec()
}

/// Build a unified runner wired with a cancel monitor backed by `token`, so
/// the runner's `get_cancellation_token()` returns the same token the test can
/// fire.
async fn runner_with_token(token: CancellationToken) -> Result<WorkflowUnifiedRunnerImpl> {
    let app_module = Arc::new(create_rdb_chan_test_app(false, false).await?);
    let app_wrapper_module = Arc::new(AppWrapperModule::new_by_env(None));
    let mock = MockCancellationManager::new_with_token(token);
    let helper = CancelMonitoringHelper::new(Box::new(mock));
    WorkflowUnifiedRunnerImpl::new_with_cancel_monitoring(app_wrapper_module, app_module, helper)
}

async fn runner_with_app_and_token(
    token: CancellationToken,
) -> Result<(Arc<app::module::AppModule>, WorkflowUnifiedRunnerImpl)> {
    let app_module = Arc::new(create_rdb_chan_test_app(false, false).await?);
    let app_wrapper_module = Arc::new(AppWrapperModule::new_by_env(None));
    let mock = MockCancellationManager::new_with_token(token);
    let helper = CancelMonitoringHelper::new(Box::new(mock));
    let runner = WorkflowUnifiedRunnerImpl::new_with_cancel_monitoring(
        app_wrapper_module,
        app_module.clone(),
        helper,
    )?;
    Ok((app_module, runner))
}

async fn create_command_worker(app_module: &app::module::AppModule, name: &str) -> Result<()> {
    app_module
        .worker_app
        .create(&WorkerData {
            name: name.to_string(),
            description: "workflow cancellation test worker".to_string(),
            runner_id: Some(RunnerId {
                value: COMMAND_RUNNER_ID,
            }),
            runner_settings: Vec::new(),
            periodic_interval: 0,
            channel: None,
            queue_type: QueueType::Normal as i32,
            response_type: ResponseType::Direct as i32,
            store_success: false,
            store_failure: true,
            use_static: false,
            retry_policy: None,
            broadcast_results: true,
        })
        .await?;
    Ok(())
}

/// A token already cancelled before `run` starts must short-circuit: the
/// runner checks `token.is_cancelled()` up front and returns an error without
/// ever launching the workflow. Needs no worker.
#[test]
fn test_workflow_run_pre_cancelled_returns_immediately() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let token = CancellationToken::new();
        token.cancel(); // cancelled before run

        let mut runner = runner_with_token(token).await?;
        runner
            .load(workflow_settings(&sleep_workflow_json()))
            .await?;

        let args = WorkflowRunArgs {
            workflow_source: None,
            input: "{}".to_string(),
            ..Default::default()
        };
        let (result, _meta) = runner
            .run(&args.encode_to_vec(), HashMap::new(), None)
            .await;

        let err = result.expect_err("pre-cancelled run must return an error");
        assert!(
            err.to_string().contains("canceled by user"),
            "expected a cancellation error, got: {err}"
        );
        Ok(())
    })
}

/// End-to-end: start the workflow, let the child `sleep` job begin, then cancel
/// the token. The execution loop must observe the token, call
/// `executor.cancel()` (which fans out delete_job to the running child), and
/// return promptly — well before the 30s sleep would finish.
///
/// Ignored: no worker runs inside the test, so the child `sleep` job is never
/// actually executed and its Direct-response wait would not complete. Run
/// against a deployment with a worker on the same backend.
#[test]
#[ignore = "needs a worker on the same backend to execute the child job"]
fn test_workflow_run_cancel_propagates_to_child() -> Result<()> {
    use std::time::Duration;
    use tokio::time::Instant;

    TEST_RUNTIME.block_on(async {
        let token = CancellationToken::new();
        let mut runner = runner_with_token(token.clone()).await?;
        runner
            .load(workflow_settings(&sleep_workflow_json()))
            .await?;

        let args = WorkflowRunArgs {
            workflow_source: None,
            input: "{}".to_string(),
            ..Default::default()
        };
        let args_bytes = args.encode_to_vec();

        // Fire cancellation shortly after the child job is enqueued.
        let canceller = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            canceller.cancel();
        });

        let started = Instant::now();
        let (_result, _meta) = runner.run(&args_bytes, HashMap::new(), None).await;
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(10),
            "cancellation should unwind well before the 30s sleep; took {:?}",
            elapsed
        );
        Ok(())
    })
}

/// End-to-end equivalent of `test_workflow_run_cancel_propagates_to_child`,
/// but the child job is launched through `run.function.workerName`.
///
/// This is the regression shape for `thread-summary-single.yaml`:
/// `generateSummary` calls a named LLM worker, so the workflow must register
/// that child job before awaiting its Direct result.
#[test]
#[ignore = "needs a worker on the same backend to execute the child job"]
fn test_workflow_run_cancel_propagates_to_worker_name_child() -> Result<()> {
    use std::time::Duration;
    use tokio::time::Instant;

    TEST_RUNTIME.block_on(async {
        let token = CancellationToken::new();
        let worker_name = "workflow-cancel-worker-name-command";
        let (app_module, mut runner) = runner_with_app_and_token(token.clone()).await?;
        create_command_worker(&app_module, worker_name).await?;
        runner
            .load(workflow_settings(&worker_name_sleep_workflow_json(
                worker_name,
            )))
            .await?;

        let args = WorkflowRunArgs {
            workflow_source: None,
            input: "{}".to_string(),
            ..Default::default()
        };
        let args_bytes = args.encode_to_vec();

        let canceller = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            canceller.cancel();
        });

        let started = Instant::now();
        let (_result, _meta) = runner.run(&args_bytes, HashMap::new(), None).await;
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(10),
            "workerName cancellation should unwind well before the 30s sleep; took {:?}",
            elapsed
        );
        Ok(())
    })
}
