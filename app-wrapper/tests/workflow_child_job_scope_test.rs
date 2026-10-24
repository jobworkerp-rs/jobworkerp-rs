//! Integration tests for child job cancellation scopes.
//!
//! Child jobs are only cancelled at explicit decision points (workflow cancel,
//! task timeout, parallel `for` with `onError: break`, `fork` compete). Merely
//! dropping a workflow stream must leave them running.
//!
//! No worker runs inside the test process, so an enqueued COMMAND child job
//! stays Pending; a child is considered cancelled once it is neither Pending
//! nor Running.

use app::module::AppModule;
use app::module::test::create_rdb_chan_test_app;
use app_wrapper::modules::AppWrapperModule;
use app_wrapper::workflow::definition::workflow::WorkflowSchema;
use app_wrapper::workflow::execute::workflow::WorkflowExecutor;
use futures::StreamExt;
use infra::infra::job::status::JobProcessingStatusRepository;
use infra_utils::infra::test::TEST_RUNTIME;
use proto::jobworkerp::data::{JobId, JobProcessingStatus};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

fn sleep_run(seconds: u32) -> serde_json::Value {
    serde_json::json!({
        "run": { "runner": { "name": "COMMAND", "arguments": {
            "command": "sleep", "args": [seconds.to_string()]
        } } }
    })
}

fn workflow(do_: serde_json::Value) -> WorkflowSchema {
    serde_json::from_value(serde_json::json!({
        "document": { "dsl": "1.0.0", "namespace": "test", "name": "child-job-scope", "version": "1.0.0" },
        "do": do_
    }))
    .expect("valid workflow")
}

async fn executor(do_: serde_json::Value) -> (Arc<AppModule>, WorkflowExecutor) {
    executor_for_schema(workflow(do_)).await
}

async fn executor_for_schema(schema: WorkflowSchema) -> (Arc<AppModule>, WorkflowExecutor) {
    let app_module = Arc::new(create_rdb_chan_test_app(false, false).await.unwrap());
    let app_wrapper_module = Arc::new(AppWrapperModule::new_by_env(None));
    let executor = WorkflowExecutor::init(
        app_wrapper_module,
        app_module.clone(),
        Arc::new(schema),
        Arc::new(serde_json::json!({})),
        None,
        Arc::new(serde_json::json!({})),
        Arc::new(HashMap::new()),
        None,
    )
    .await
    .unwrap();
    (app_module, executor)
}

async fn active_child_jobs(app_module: &AppModule) -> Vec<JobId> {
    app_module
        .repositories
        .rdb_module
        .as_ref()
        .unwrap()
        .memory_job_processing_status_repository
        .find_status_all()
        .await
        .unwrap()
        .into_iter()
        .filter(|(_, status)| {
            matches!(
                status,
                JobProcessingStatus::Pending | JobProcessingStatus::Running
            )
        })
        .map(|(id, _)| id)
        .collect()
}

/// Drive the workflow for `budget`, then drop the stream. Returns the
/// collected items (errors included) seen before the budget ran out.
async fn run_for(executor: &WorkflowExecutor, budget: Duration) -> Vec<String> {
    let stream = executor.execute_workflow(Arc::new(opentelemetry::Context::current()));
    futures::pin_mut!(stream);
    let mut seen = Vec::new();
    let _ = tokio::time::timeout(budget, async {
        while let Some(item) = stream.next().await {
            seen.push(match item {
                Ok(_) => "ok".to_string(),
                Err(e) => format!("{}:{}", e.status, e.type_.0),
            });
        }
    })
    .await;
    seen
}

#[test]
fn dropping_workflow_stream_keeps_child_then_explicit_cancel_removes_it() {
    TEST_RUNTIME.block_on(async {
        let (app, executor) = executor(serde_json::json!([{ "sleep": sleep_run(30) }])).await;
        run_for(&executor, Duration::from_millis(500)).await;

        assert_eq!(
            active_child_jobs(&app).await.len(),
            1,
            "a dropped stream is not a decision to cancel"
        );

        executor.cancel().await;
        assert!(active_child_jobs(&app).await.is_empty());
    });
}

fn only_for_item(mut task: serde_json::Value, item: &str) -> serde_json::Value {
    task["if"] = serde_json::json!(format!("${{ $item == \"{item}\" }}"));
    task
}

fn with_timeout(mut task: serde_json::Value, millis: u64) -> serde_json::Value {
    task["timeout"] = serde_json::json!({ "after": { "milliseconds": millis } });
    task
}

#[test]
fn task_timeout_cancels_child_jobs_inside_the_task() {
    TEST_RUNTIME.block_on(async {
        let guarded = with_timeout(
            serde_json::json!({ "do": [ { "sleep": sleep_run(30) } ] }),
            300,
        );
        let (app, executor) = executor(serde_json::json!([{ "guarded": guarded }])).await;
        let seen = run_for(&executor, Duration::from_secs(5)).await;

        assert!(
            seen.iter()
                .any(|s| s.ends_with("/spec/1.0.0/errors/timeout") && s.starts_with("408")),
            "timeout error expected: {seen:?}"
        );
        assert!(active_child_jobs(&app).await.is_empty());
    });
}

#[test]
fn workflow_level_timeout_cancels_child_jobs() {
    TEST_RUNTIME.block_on(async {
        let mut schema = workflow(serde_json::json!([{ "sleep": sleep_run(30) }]));
        schema.timeout =
            serde_json::from_value(serde_json::json!({ "after": { "milliseconds": 300 } }))
                .unwrap();
        let (app_module, executor) = executor_for_schema(schema).await;
        run_for(&executor, Duration::from_secs(5)).await;
        assert!(active_child_jobs(&app_module).await.is_empty());
    });
}

#[test]
fn task_timeout_does_not_cancel_parallel_sibling_items() {
    TEST_RUNTIME.block_on(async {
        let short_run = with_timeout(only_for_item(sleep_run(30), "short"), 300);
        let long_run = only_for_item(sleep_run(30), "long");
        let (app, executor) = executor(serde_json::json!([{ "each": {
            "for": { "each": "item", "in": "${ [\"short\", \"long\"] }" },
            "inParallel": true,
            "onError": "continue",
            "do": [ { "shortRun": short_run }, { "longRun": long_run } ]
        } }]))
        .await;
        run_for(&executor, Duration::from_millis(800)).await;

        assert_eq!(
            active_child_jobs(&app).await.len(),
            1,
            "only the timed-out item's child may be cancelled"
        );
    });
}

/// Parallel `for` where the "fail" item raises after a short wait while the
/// "run" item has a child job in flight.
fn parallel_for_with_failing_item(on_error: &str) -> serde_json::Value {
    let wait = only_for_item(
        serde_json::json!({ "wait": { "milliseconds": 300 } }),
        "fail",
    );
    let raise = only_for_item(
        serde_json::json!({ "raise": { "error": {
            "type": "https://example.invalid/errors/item", "status": 500
        } } }),
        "fail",
    );
    let run = only_for_item(sleep_run(30), "run");
    serde_json::json!([{ "each": {
        "for": { "each": "item", "in": "${ [\"fail\", \"run\"] }" },
        "inParallel": true,
        "onError": on_error,
        "do": [ { "pause": wait }, { "fail": raise }, { "child": run } ]
    } }])
}

#[test]
fn parallel_for_break_cancels_child_jobs_of_other_items() {
    TEST_RUNTIME.block_on(async {
        let (app, executor) = executor(parallel_for_with_failing_item("break")).await;
        let seen = run_for(&executor, Duration::from_secs(3)).await;

        assert!(seen.iter().any(|s| s.starts_with("500")), "{seen:?}");
        assert!(active_child_jobs(&app).await.is_empty());
    });
}

#[test]
fn parallel_for_continue_keeps_child_jobs_of_other_items() {
    TEST_RUNTIME.block_on(async {
        let (app, executor) = executor(parallel_for_with_failing_item("continue")).await;
        run_for(&executor, Duration::from_millis(600)).await;

        assert_eq!(active_child_jobs(&app).await.len(), 1);
    });
}

#[test]
fn fork_compete_cancels_child_jobs_of_losing_branches() {
    TEST_RUNTIME.block_on(async {
        let (app, executor) = executor(serde_json::json!([{ "race": { "fork": {
            "compete": true,
            "branches": [
                { "fast": { "wait": { "milliseconds": 300 } } },
                { "slow": sleep_run(30) }
            ]
        } } }]))
        .await;
        let seen = run_for(&executor, Duration::from_secs(3)).await;

        assert!(seen.iter().all(|s| s == "ok"), "{seen:?}");
        assert!(active_child_jobs(&app).await.is_empty());
    });
}

#[test]
fn streaming_child_survives_drop_and_is_cancelled_explicitly() {
    TEST_RUNTIME.block_on(async {
        let mut streaming = sleep_run(30);
        streaming["useStreaming"] = serde_json::json!(true);
        let (app, executor) = executor(serde_json::json!([{ "stream": streaming }])).await;
        run_for(&executor, Duration::from_millis(500)).await;

        assert_eq!(active_child_jobs(&app).await.len(), 1);

        executor.cancel().await;
        assert!(active_child_jobs(&app).await.is_empty());
    });
}

#[test]
fn task_timeout_cancels_streaming_child() {
    TEST_RUNTIME.block_on(async {
        let mut streaming = with_timeout(sleep_run(30), 300);
        streaming["useStreaming"] = serde_json::json!(true);
        let (app, executor) = executor(serde_json::json!([{ "stream": streaming }])).await;
        run_for(&executor, Duration::from_secs(3)).await;

        assert!(active_child_jobs(&app).await.is_empty());
    });
}

#[test]
fn script_child_is_cancelled_explicitly() {
    TEST_RUNTIME.block_on(async {
        let script = serde_json::json!({ "run": { "script": {
            "language": "python",
            "code": "import time\ntime.sleep(30)"
        } } });
        let (app, executor) = executor(serde_json::json!([{ "script": script }])).await;
        run_for(&executor, Duration::from_millis(500)).await;

        assert_eq!(active_child_jobs(&app).await.len(), 1);

        executor.cancel().await;
        assert!(active_child_jobs(&app).await.is_empty());
    });
}
