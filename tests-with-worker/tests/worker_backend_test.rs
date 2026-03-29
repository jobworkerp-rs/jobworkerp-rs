//! Tests that verify the backend worker correctly processes jobs.
//! These tests do not require an LLM server - they directly enqueue
//! COMMAND runner jobs and verify the results.
//!
//! All sub-tests share a single `start_test_worker` call to avoid
//! accumulating leaked dispatchers (Box::leak in start_test_worker),
//! which would exhaust the shared DB connection pool (especially MySQL).
//! Must run with --test-threads=1.
//! Requires Redis (uses create_hybrid_test_app / Scalable mode).
//!
//! ## Ordering constraint
//! `call_function_for_llm` (used by sub_test_worker_executes_command_via_function)
//! creates a temp worker with Direct response_type whose lifecycle affects
//! dispatcher state. It must run **last** so that streaming-based sub-tests
//! (enqueue_function_for_llm) are not affected.

use anyhow::Result;
use app::app::function::FunctionApp;
use app::app::function::function_set::FunctionSetApp;
use proto::jobworkerp::data::RunnerId;
use proto::jobworkerp::function::data::{FunctionId, FunctionSetData, FunctionUsing, function_id};
use std::collections::HashMap;
use std::sync::Arc;
use tests_with_worker::start_test_worker;
use tokio::time::{Duration, timeout};

// ---------------------------------------------------------------------------
// Sub-test functions called from the single entry-point test below.
// ---------------------------------------------------------------------------

/// Sub-test: backend worker can execute a COMMAND runner job
/// via call_function_for_llm (the same path used by LLM tool calling).
async fn sub_test_worker_executes_command_via_function(
    app_module: &Arc<app::module::AppModule>,
) -> Result<()> {
    // Create a function set targeting the COMMAND runner (runner_id=1)
    match app_module
        .function_set_app
        .create_function_set(&FunctionSetData {
            name: "worker_backend_test".to_string(),
            description: "Test set for worker backend verification".to_string(),
            category: 0,
            targets: vec![FunctionUsing {
                function_id: Some(FunctionId {
                    id: Some(function_id::Id::RunnerId(RunnerId { value: 1 })),
                }),
                using: None,
            }],
        })
        .await
    {
        Ok(_) => {}
        Err(e) if e.to_string().to_lowercase().contains("unique") => {}
        Err(e) if e.to_string().to_lowercase().contains("duplicate") => {}
        Err(e) => return Err(e),
    }

    // Call function_for_llm directly - this enqueues a job and waits for the result
    let metadata = Arc::new(HashMap::new());
    let arguments: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(serde_json::json!({
            "command": "echo",
            "args": ["hello_from_worker_test"]
        }))?;

    let result = timeout(
        Duration::from_secs(30),
        app_module
            .function_app
            .call_function_for_llm(metadata, "COMMAND", Some(arguments), 30),
    )
    .await??;

    let output = result.to_string();
    println!("Function result: {}", output);
    assert!(
        output.contains("hello_from_worker_test"),
        "Output should contain echo text, got: {}",
        output
    );

    println!("sub_test_worker_executes_command_via_function passed");
    Ok(())
}

/// Sub-test: 2-stage enqueue + await flow with streaming COMMAND runner.
/// Verifies: enqueue returns immediately with job_id (is_streaming=true, result=None),
/// then await_function_result retrieves the completed result.
async fn sub_test_enqueue_and_await_function_result_streaming(
    app_module: &Arc<app::module::AppModule>,
) -> Result<()> {
    let metadata = Arc::new(HashMap::new());
    let arguments: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(serde_json::json!({
            "command": "echo",
            "args": ["hello_2stage_test"]
        }))?;

    // Phase A: enqueue — should return immediately with job_id
    let enqueued = timeout(
        Duration::from_secs(10),
        app_module
            .function_app
            .enqueue_function_for_llm(metadata, "COMMAND", Some(arguments), 30),
    )
    .await??;

    assert!(
        enqueued.is_streaming,
        "COMMAND runner should be streaming-capable"
    );
    assert!(
        enqueued.result.is_none(),
        "Streaming enqueue should return None result"
    );
    assert!(enqueued.job_id.value > 0, "Job ID should be assigned");

    // Phase B: await result — worker processes the job
    let result_handle = enqueued
        .result_handle
        .expect("Streaming enqueue should provide result_handle");
    let result = timeout(
        Duration::from_secs(30),
        app_module.function_app.await_function_result(
            result_handle,
            &enqueued.runner_name,
            enqueued.using.as_deref(),
        ),
    )
    .await??;

    let output = result.to_string();
    println!("2-stage result: {}", output);
    // COMMAND runner returns CommandResult protobuf (exitCode, stdout, etc.)
    // When decoded via decode_job_result_output, it becomes JSON with exitCode field
    assert!(
        result.get("exitCode").is_some() || output.contains("hello_2stage_test"),
        "Output should contain exitCode or echo text, got: {}",
        output
    );

    println!("sub_test_enqueue_and_await_function_result_streaming passed");
    Ok(())
}

/// Sub-test: 2-stage enqueue with non-streaming FUNCTION_SET_SELECTOR runner.
/// Verifies: enqueue waits for completion and returns result immediately
/// (is_streaming=false, result=Some).
async fn sub_test_enqueue_function_non_streaming_with_runner(
    app_module: &Arc<app::module::AppModule>,
) -> Result<()> {
    let metadata = Arc::new(HashMap::new());

    let arguments: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(serde_json::json!({
            "arguments": {
                "function_set_name": "nonexistent-set"
            }
        }))?;

    let enqueued = timeout(
        Duration::from_secs(30),
        app_module.function_app.enqueue_function_for_llm(
            metadata,
            "FUNCTION_SET_SELECTOR",
            Some(arguments),
            30,
        ),
    )
    .await??;

    assert!(
        !enqueued.is_streaming,
        "FUNCTION_SET_SELECTOR should be non-streaming"
    );
    assert!(
        enqueued.result.is_some(),
        "Non-streaming runner should return immediate result"
    );
    assert!(enqueued.job_id.value > 0, "Job ID should be assigned");

    let result = enqueued.result.unwrap();
    println!("Non-streaming enqueue result: {}", result);
    // FUNCTION_SET_SELECTOR with nonexistent set should return an error message
    let output = result.to_string();
    assert!(
        !output.is_empty(),
        "FUNCTION_SET_SELECTOR result should not be empty"
    );
    println!("sub_test_enqueue_function_non_streaming_with_runner passed");
    Ok(())
}

/// Sub-test: nested workflow progress (position changes) can be received
/// in real-time via subscribe_result_stream while the workflow is running.
/// Uses WORKFLOW___run with a 3-step inline workflow where each step sleeps briefly.
async fn sub_test_workflow_progress_realtime(
    app_module: &Arc<app::module::AppModule>,
) -> Result<()> {
    // 3-step workflow: each step runs a short command with sleep
    let workflow_def = serde_json::json!({
        "document": {
            "dsl": "0.0.1",
            "namespace": "progress-test",
            "name": "progress-realtime-test",
            "version": "1.0.0"
        },
        "input": {},
        "do": [
            { "step_alpha": { "run": { "runner": { "name": "COMMAND", "arguments": { "command": "sleep", "args": ["0.3"] } } } } },
            { "step_beta":  { "run": { "runner": { "name": "COMMAND", "arguments": { "command": "sleep", "args": ["0.3"] } } } } },
            { "step_gamma": { "run": { "runner": { "name": "COMMAND", "arguments": { "command": "echo", "args": ["done"] } } } } }
        ]
    });

    let metadata = Arc::new(HashMap::new());
    let arguments: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(serde_json::json!({
            "workflow_data": workflow_def.to_string(),
            "input": "{}"
        }))?;

    // Enqueue WORKFLOW___run (streaming-capable)
    let enqueued = timeout(
        Duration::from_secs(10),
        app_module.function_app.enqueue_function_for_llm(
            metadata,
            "WORKFLOW___run",
            Some(arguments),
            60,
        ),
    )
    .await??;

    assert!(
        enqueued.is_streaming,
        "WORKFLOW.run should be streaming-capable"
    );
    assert!(enqueued.result.is_none(), "Should return immediately");
    println!("Workflow enqueued: job_id={}", enqueued.job_id.value);

    // Use the pre-started result_handle to get the stream (avoids race condition
    // where listen_result_by_job_id called after enqueue misses early events)
    let result_handle = enqueued.result_handle.expect("Should have result_handle");
    let (job_result, stream_opt) = timeout(Duration::from_secs(30), result_handle)
        .await?
        .map_err(|e| anyhow::anyhow!("Result listener task failed: {}", e))??;

    let mut positions: Vec<String> = Vec::new();

    if let Some(mut stream) = stream_opt {
        use futures::StreamExt;
        use prost::Message;
        use proto::jobworkerp::data::result_output_item;

        while let Ok(Some(item)) =
            tokio::time::timeout(Duration::from_secs(30), stream.next()).await
        {
            match item.item {
                Some(result_output_item::Item::Data(data)) => {
                    // Try to decode as WorkflowResult
                    if let Ok(wf_result) =
                        jobworkerp_runner::jobworkerp::runner::WorkflowResult::decode(&data[..])
                        && !wf_result.position.is_empty()
                    {
                        println!(
                            "  Progress: position={}, status={:?}",
                            wf_result.position, wf_result.status
                        );
                        positions.push(wf_result.position);
                    }
                }
                Some(result_output_item::Item::FinalCollected(_)) => {
                    println!("  FinalCollected received");
                    break;
                }
                Some(result_output_item::Item::End(_)) => {
                    println!("  End received");
                    break;
                }
                None => {}
            }
        }
    } else {
        println!("No stream available from result_handle");
    }

    // Verify we received multiple position changes (at least 2 steps)
    positions.dedup();
    println!("Positions received (deduped): {:?}", positions);
    assert!(
        positions.len() >= 2,
        "Should receive at least 2 position changes for 3-step workflow, got {}",
        positions.len()
    );

    // Verify positions contain step names
    let has_step_names = positions.iter().any(|p| p.contains("step_"));
    assert!(
        has_step_names,
        "Positions should contain step names: {:?}",
        positions
    );

    // Verify final result is available
    assert!(job_result.data.is_some(), "Job result should have data");
    println!(
        "Final job result received: job_id={:?}",
        job_result.data.as_ref().and_then(|d| d.job_id)
    );

    println!("sub_test_workflow_progress_realtime passed");
    Ok(())
}

/// Sub-test: short-lived tool execution (milliseconds) correctly returns results
/// even when progress subscription misses all intermediate events.
async fn sub_test_short_tool_execution_skips_progress(
    app_module: &Arc<app::module::AppModule>,
) -> Result<()> {
    let metadata = Arc::new(HashMap::new());
    let arguments: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(serde_json::json!({
            "command": "echo",
            "args": ["fast_execution"]
        }))?;

    // Enqueue fast COMMAND
    let enqueued = timeout(
        Duration::from_secs(10),
        app_module
            .function_app
            .enqueue_function_for_llm(metadata, "COMMAND", Some(arguments), 30),
    )
    .await??;

    assert!(enqueued.is_streaming);
    assert!(enqueued.result.is_none());

    // Try to subscribe to progress — expect to get nothing or very little
    // because the command completes in milliseconds
    let progress_result = tokio::time::timeout(
        Duration::from_millis(200),
        app_module
            .job_result_app
            .listen_result_by_job_id(&enqueued.job_id, Some(200), true),
    )
    .await;

    match progress_result {
        Ok(Ok((_result, stream_opt))) => {
            // Stream may or may not be available for fast execution
            println!(
                "Short execution: stream available = {}",
                stream_opt.is_some()
            );
        }
        Ok(Err(e)) => {
            println!(
                "Short execution: listen error (expected for fast jobs): {}",
                e
            );
        }
        Err(_) => {
            println!("Short execution: listen timed out (expected for fast jobs)");
        }
    }

    // The key assertion: final result is always available regardless of progress
    let result_handle = enqueued.result_handle.expect("Should have result_handle");
    let result = timeout(
        Duration::from_secs(30),
        app_module.function_app.await_function_result(
            result_handle,
            &enqueued.runner_name,
            enqueued.using.as_deref(),
        ),
    )
    .await??;

    let output = result.to_string();
    println!("Short execution result: {}", output);
    // COMMAND runner returns CommandResult protobuf with exitCode
    assert!(
        result.get("exitCode").is_some() || output.contains("fast_execution"),
        "Should get valid result regardless of progress availability, got: {}",
        output
    );

    println!("sub_test_short_tool_execution_skips_progress passed");
    Ok(())
}

// ---------------------------------------------------------------------------
// Single entry-point test
// ---------------------------------------------------------------------------

/// Unified test that shares a single start_test_worker invocation
/// across all worker backend sub-tests.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_worker_backend_e2e() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    let app_module = Arc::new(app::module::test::create_hybrid_test_app().await?);
    let worker_handle = start_test_worker(app_module.clone()).await?;

    // Wait briefly for dispatcher to be ready
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Run all sub-tests sequentially, sharing the same worker.
    // Ordering matters: each sub-test creates a temp worker that may leave
    // dispatcher state affecting subsequent tests. Long-running tests
    // (workflow_progress) go first, then fast streaming tests, then
    // call_function_for_llm (Direct response) last since it most disrupts
    // dispatcher state for subsequent streaming enqueue tests.
    //
    // Wrap in async block so shutdown() always runs even if a sub-test fails.
    let test_result = async {
        sub_test_workflow_progress_realtime(&app_module).await?;
        sub_test_enqueue_and_await_function_result_streaming(&app_module).await?;
        sub_test_enqueue_function_non_streaming_with_runner(&app_module).await?;
        sub_test_short_tool_execution_skips_progress(&app_module).await?;
        sub_test_worker_executes_command_via_function(&app_module).await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;

    worker_handle.shutdown().await;
    test_result
}
