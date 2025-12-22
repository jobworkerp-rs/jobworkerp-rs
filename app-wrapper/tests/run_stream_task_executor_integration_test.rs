/// Integration tests for RunStreamTaskExecutor
///
/// These tests verify that RunStreamTaskExecutor correctly executes jobs
/// with streaming support using various configurations (Worker, Runner, Function).
///
/// Run with: cargo test --package app-wrapper --test run_stream_task_executor_integration_test -- --ignored --test-threads=1 --nocapture
///
/// Prerequisites:
/// - Redis must be accessible (for Hybrid/Scalable mode)
/// - Worker process must be running to execute jobs
use anyhow::Result;
use app::app::job::execute::JobExecutorWrapper;
use app::module::test::create_hybrid_test_app;
use app_wrapper::workflow::{
    definition::{
        workflow::{self, RunJobRunner, RunJobWorker, RunTaskConfiguration},
        WorkflowLoader,
    },
    execute::{
        context::{TaskContext, WorkflowContext, WorkflowStatus, WorkflowStreamEvent},
        task::{stream::run::RunStreamTaskExecutor, StreamTaskExecutorTrait},
    },
};
use futures::StreamExt;
use infra_utils::infra::test::TEST_RUNTIME;
use proto::jobworkerp::data::{QueueType, ResponseType, RunnerId, WorkerData};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};

/// Helper to create a COMMAND worker for testing
async fn create_command_worker(
    app_module: &app::module::AppModule,
    name: &str,
) -> Result<proto::jobworkerp::data::WorkerId> {
    let worker_data = WorkerData {
        name: name.to_string(),
        description: "Test worker for RunStreamTaskExecutor".to_string(),
        runner_id: Some(RunnerId { value: 1 }), // COMMAND runner
        runner_settings: vec![],
        retry_policy: None,
        periodic_interval: 0,
        channel: None,
        queue_type: QueueType::WithBackup as i32,
        response_type: ResponseType::Direct as i32,
        store_success: true,
        store_failure: true,
        use_static: false,
        broadcast_results: true,
    };
    app_module.worker_app.create(&worker_data).await
}

/// Test: RunStreamTaskExecutor executes Worker configuration with streaming
///
/// This test verifies that RunStreamTaskExecutor can execute a job using
/// Worker configuration (pre-registered worker) and collect streaming results.
///
/// NOTE: This test requires a FRESH worker process restart before running.
/// The worker process caches runner information by worker ID. If a worker ID
/// was previously used by a different runner (e.g., CREATE_WORKFLOW), the
/// cached runner info will be used instead of the new worker's runner,
/// causing test failures.
///
/// Prerequisites:
/// 1. Restart the worker process (to clear runner cache)
/// 2. Ensure no existing worker with ID 1 in the database, or use a fresh test DB
#[test]
#[ignore = "Requires fresh worker process restart, Redis, and same RDB as worker process"]
fn test_run_stream_task_executor_worker_config() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    TEST_RUNTIME.block_on(async {
        let app_module = Arc::new(create_hybrid_test_app().await?);
        let job_executors = Arc::new(JobExecutorWrapper::new(app_module.clone()));

        // Create a worker for the test
        let worker_name = "test-stream-executor-worker";
        let worker_id = create_command_worker(&app_module, worker_name).await?;
        eprintln!("✅ Created worker '{}': {:?}", worker_name, worker_id);

        // Wait for worker registration
        eprintln!("⏳ Waiting for worker registration (2s)...");
        tokio::time::sleep(Duration::from_secs(2)).await;

        // Load a workflow for context
        let loader = WorkflowLoader::new_local_only();
        let flow = loader
            .load_workflow(Some("test-files/ls-test.yaml"), None, false)
            .await?;

        let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
            &flow,
            Arc::new(serde_json::json!({})),
            Arc::new(serde_json::json!({})),
            None,
        )));
        workflow_context.write().await.status = WorkflowStatus::Running;

        // Create RunTask with Worker configuration
        let run_task = workflow::RunTask {
            metadata: serde_json::Map::new(),
            timeout: None,
            run: RunTaskConfiguration::Worker(workflow::RunWorker {
                worker: RunJobWorker {
                    name: worker_name.to_string(),
                    arguments: {
                        let mut args = serde_json::Map::new();
                        args.insert("command".to_string(), serde_json::json!("echo"));
                        args.insert(
                            "args".to_string(),
                            serde_json::json!(["stream", "worker", "test"]),
                        );
                        args
                    },
                    using: None,
                },
            }),
            export: None,
            if_: None,
            checkpoint: false,
            input: None,
            output: None,
            then: None,
            use_streaming: true,
        };

        let executor = RunStreamTaskExecutor::new(
            workflow_context.clone(),
            Duration::from_secs(30),
            job_executors.clone(),
            run_task,
            Arc::new(HashMap::new()),
        );

        let task_context = TaskContext::new(
            None,
            Arc::new(serde_json::json!({})),
            Arc::new(Mutex::new(Default::default())),
        );

        eprintln!("\n📤 Executing RunStreamTaskExecutor with Worker config...");
        let cx = Arc::new(opentelemetry::Context::new());
        let stream =
            executor.execute_stream(cx, Arc::new("test_worker_config".to_string()), task_context);
        futures::pin_mut!(stream);

        let mut started = false;
        let mut completed = false;
        let mut final_context: Option<TaskContext> = None;

        while let Some(event_result) = stream.next().await {
            match event_result {
                Ok(WorkflowStreamEvent::StreamingJobStarted { job_id, .. }) => {
                    eprintln!("📥 StreamingJobStarted: job_id={}", job_id.value);
                    started = true;
                }
                Ok(WorkflowStreamEvent::StreamingJobCompleted {
                    job_id, context, ..
                }) => {
                    eprintln!("📥 StreamingJobCompleted: job_id={}", job_id.value);
                    eprintln!("   Output: {:?}", context.raw_output);
                    completed = true;
                    final_context = Some(context);
                }
                Ok(other) => {
                    eprintln!("📥 Other event: {:?}", other);
                }
                Err(e) => {
                    eprintln!("❌ Execution failed: {:?}", e);
                    let _ = app_module.worker_app.delete(&worker_id).await;
                    return Err(anyhow::anyhow!("RunStreamTaskExecutor failed: {:?}", e));
                }
            }
        }

        // Cleanup
        let _ = app_module.worker_app.delete(&worker_id).await;

        assert!(started, "StreamingJobStarted event should be emitted");
        assert!(completed, "StreamingJobCompleted event should be emitted");
        assert!(final_context.is_some(), "Final context should be set");
        eprintln!("✅ Execution succeeded!");

        eprintln!("\n✅ test_run_stream_task_executor_worker_config passed");
        Ok(())
    })
}

/// Test: RunStreamTaskExecutor executes Runner configuration with streaming
///
/// This test verifies that RunStreamTaskExecutor can execute a job using
/// Runner configuration (direct runner invocation) and collect streaming results.
#[test]
#[ignore = "Requires Redis, running worker process, and same RDB as worker process"]
fn test_run_stream_task_executor_runner_config() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app_module = Arc::new(create_hybrid_test_app().await?);
        let job_executors = Arc::new(JobExecutorWrapper::new(app_module.clone()));

        // Load a workflow for context
        let loader = WorkflowLoader::new_local_only();
        let flow = loader
            .load_workflow(Some("test-files/ls-test.yaml"), None, false)
            .await?;

        let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
            &flow,
            Arc::new(serde_json::json!({})),
            Arc::new(serde_json::json!({})),
            None,
        )));
        workflow_context.write().await.status = WorkflowStatus::Running;

        // Create RunTask with Runner configuration
        let run_task = workflow::RunTask {
            metadata: serde_json::Map::new(),
            timeout: None,
            run: RunTaskConfiguration::Runner(workflow::RunRunner {
                runner: RunJobRunner {
                    name: "COMMAND".to_string(),
                    settings: serde_json::Map::new(),
                    arguments: {
                        let mut args = serde_json::Map::new();
                        args.insert("command".to_string(), serde_json::json!("echo"));
                        args.insert(
                            "args".to_string(),
                            serde_json::json!(["stream", "runner", "test"]),
                        );
                        args
                    },
                    options: None,
                    using: None,
                },
            }),
            export: None,
            if_: None,
            checkpoint: false,
            input: None,
            output: None,
            then: None,
            use_streaming: true,
        };

        let executor = RunStreamTaskExecutor::new(
            workflow_context.clone(),
            Duration::from_secs(30),
            job_executors.clone(),
            run_task,
            Arc::new(HashMap::new()),
        );

        let task_context = TaskContext::new(
            None,
            Arc::new(serde_json::json!({})),
            Arc::new(Mutex::new(Default::default())),
        );

        eprintln!("\n📤 Executing RunStreamTaskExecutor with Runner config...");
        let cx = Arc::new(opentelemetry::Context::new());
        let stream =
            executor.execute_stream(cx, Arc::new("test_runner_config".to_string()), task_context);
        futures::pin_mut!(stream);

        let mut started = false;
        let mut completed = false;
        let mut final_context: Option<TaskContext> = None;

        while let Some(event_result) = stream.next().await {
            match event_result {
                Ok(WorkflowStreamEvent::StreamingJobStarted { job_id, .. }) => {
                    eprintln!("📥 StreamingJobStarted: job_id={}", job_id.value);
                    started = true;
                }
                Ok(WorkflowStreamEvent::StreamingJobCompleted {
                    job_id, context, ..
                }) => {
                    eprintln!("📥 StreamingJobCompleted: job_id={}", job_id.value);
                    eprintln!("   Output: {:?}", context.raw_output);
                    completed = true;
                    final_context = Some(context);
                }
                Ok(other) => {
                    eprintln!("📥 Other event: {:?}", other);
                }
                Err(e) => {
                    eprintln!("❌ Execution failed: {:?}", e);
                    return Err(anyhow::anyhow!("RunStreamTaskExecutor failed: {:?}", e));
                }
            }
        }

        assert!(started, "StreamingJobStarted event should be emitted");
        assert!(completed, "StreamingJobCompleted event should be emitted");
        assert!(final_context.is_some(), "Final context should be set");
        eprintln!("✅ Execution succeeded!");

        eprintln!("\n✅ test_run_stream_task_executor_runner_config passed");
        Ok(())
    })
}

/// Test: RunStreamTaskExecutor correctly collects stream using RunnerSpec::collect_stream
///
/// This test verifies that the streaming results are properly collected
/// using the collect_stream mechanism moved to RunnerSpec trait.
/// TODO cannot pass the test...
#[test]
#[ignore = "Requires Redis, running worker process, and same RDB as worker process"]
fn test_run_stream_task_executor_collect_stream() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app_module = Arc::new(create_hybrid_test_app().await?);
        let job_executors = Arc::new(JobExecutorWrapper::new(app_module.clone()));

        // Create a worker that outputs multiple lines (to test stream collection)
        let worker_name = "test-stream-collect-worker";
        let worker_id = create_command_worker(&app_module, worker_name).await?;
        eprintln!("✅ Created worker '{}': {:?}", worker_name, worker_id);

        // Wait for worker registration
        eprintln!("⏳ Waiting for worker registration (2s)...");
        tokio::time::sleep(Duration::from_secs(2)).await;

        // Load a workflow for context
        let loader = WorkflowLoader::new_local_only();
        let flow = loader
            .load_workflow(Some("test-files/ls-test.yaml"), None, false)
            .await?;

        let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
            &flow,
            Arc::new(serde_json::json!({})),
            Arc::new(serde_json::json!({})),
            None,
        )));
        workflow_context.write().await.status = WorkflowStatus::Running;

        // Create RunTask that outputs multiple lines
        let run_task = workflow::RunTask {
            metadata: serde_json::Map::new(),
            timeout: None,
            run: RunTaskConfiguration::Worker(workflow::RunWorker {
                worker: RunJobWorker {
                    name: worker_name.to_string(),
                    arguments: {
                        let mut args = serde_json::Map::new();
                        // Use sleep to give time for subscribe to connect before job completes
                        // This avoids race condition where publish happens before subscribe
                        args.insert("command".to_string(), serde_json::json!("sh"));
                        args.insert(
                            "args".to_string(),
                            serde_json::json!([
                                "-c",
                                "echo 'Line 1'; echo 'Line 2'; echo 'Line 3'"
                            ]),
                        );
                        args
                    },
                    using: None,
                },
            }),
            export: None,
            if_: None,
            checkpoint: false,
            input: None,
            output: None,
            then: None,
            use_streaming: true,
        };

        let executor = RunStreamTaskExecutor::new(
            workflow_context.clone(),
            Duration::from_secs(30),
            job_executors.clone(),
            run_task,
            Arc::new(HashMap::new()),
        );

        let task_context = TaskContext::new(
            None,
            Arc::new(serde_json::json!({})),
            Arc::new(Mutex::new(Default::default())),
        );

        eprintln!("\n📤 Executing RunStreamTaskExecutor with multi-line output...");
        let cx = Arc::new(opentelemetry::Context::new());
        let stream = executor.execute_stream(
            cx,
            Arc::new("test_collect_stream".to_string()),
            task_context,
        );
        futures::pin_mut!(stream);

        let mut started = false;
        let mut completed = false;
        let mut final_context: Option<TaskContext> = None;

        while let Some(event_result) = stream.next().await {
            match event_result {
                Ok(WorkflowStreamEvent::StreamingJobStarted { job_id, .. }) => {
                    eprintln!("📥 StreamingJobStarted: job_id={}", job_id.value);
                    started = true;
                }
                Ok(WorkflowStreamEvent::StreamingJobCompleted {
                    job_id, context, ..
                }) => {
                    eprintln!("📥 StreamingJobCompleted: job_id={}", job_id.value);
                    eprintln!("   Output: {:?}", context.raw_output);
                    completed = true;
                    final_context = Some(context);
                }
                Ok(other) => {
                    eprintln!("📥 Other event: {:?}", other);
                }
                Err(e) => {
                    eprintln!("❌ Execution failed: {:?}", e);
                    let _ = app_module.worker_app.delete(&worker_id).await;
                    return Err(anyhow::anyhow!("RunStreamTaskExecutor failed: {:?}", e));
                }
            }
        }

        // Cleanup
        let _ = app_module.worker_app.delete(&worker_id).await;

        assert!(started, "StreamingJobStarted event should be emitted");
        assert!(completed, "StreamingJobCompleted event should be emitted");

        if let Some(task_context) = final_context {
            eprintln!("✅ Execution succeeded!");
            eprintln!("   Output: {:?}", task_context.raw_output);

            // // Verify output contains all lines (collected from stream)
            // let output_str = task_context.raw_output.to_string();
            // eprintln!("   Output string: {}", output_str);
            // // The output should contain the collected results
            // assert!(
            //     output_str.contains("Line")
            //         || output_str.contains("stdout")
            //         || output_str.contains("exit_code"),
            //     "Output should contain the collected stream data"
            // );
            // Verify output contains expected data from stream collection
            let output_str = task_context.raw_output.to_string();
            eprintln!("   Output string: {}", output_str);
            // TODO: Define expected output format and use more specific assertions
            // For now, verify the output is non-empty and contains some expected data
            assert!(!output_str.is_empty(), "Output should not be empty");
            assert!(
                output_str.contains("Line") || output_str.contains("exit_code"),
                "Output should contain 'Line' (stdout) or 'exit_code' (result): got {}",
                output_str
            );
        } else {
            return Err(anyhow::anyhow!("Final context should be set"));
        }

        eprintln!("\n✅ test_run_stream_task_executor_collect_stream passed");
        Ok(())
    })
}
