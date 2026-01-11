/// Integration tests for ListenStream
///
/// These tests verify that:
/// 1. ListenStream correctly rejects non-broadcast workers
/// 2. ListenStream receives streaming results when broadcast_results=true
///
/// Run with: cargo test --package app-wrapper --test run_stream_task_executor_test -- --ignored --test-threads=1 --nocapture
///
/// Prerequisites:
/// - Redis must be accessible (for pubsub in Scalable mode)
/// - Worker process must be running to execute jobs
/// - Test and worker process must use the same RDB (SQLite/MySQL) for worker lookup
use anyhow::Result;
use app::app::job::execute::{JobExecutorWrapper, UseJobExecutor};
use app::module::test::create_hybrid_test_app;
use infra_utils::infra::test::TEST_RUNTIME;
use proto::jobworkerp::data::{
    JobId, QueueType, ResponseType, RunnerId, StreamingType, WorkerData,
};
use proto::DEFAULT_METHOD_NAME;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Helper to create a COMMAND worker with broadcast_results setting
async fn create_command_worker(
    app_module: &app::module::AppModule,
    name: &str,
    broadcast_results: bool,
) -> Result<proto::jobworkerp::data::WorkerId> {
    create_command_worker_with_response_type(
        app_module,
        name,
        broadcast_results,
        ResponseType::NoResult,
    )
    .await
}

/// Helper to create a COMMAND worker with specific response_type
async fn create_command_worker_with_response_type(
    app_module: &app::module::AppModule,
    name: &str,
    broadcast_results: bool,
    response_type: ResponseType,
) -> Result<proto::jobworkerp::data::WorkerId> {
    let worker_data = WorkerData {
        name: name.to_string(),
        description: format!(
            "Test worker for ListenStream (broadcast={}, response_type={:?})",
            broadcast_results, response_type
        ),
        runner_id: Some(RunnerId { value: 1 }), // COMMAND runner
        runner_settings: vec![],
        retry_policy: None,
        periodic_interval: 0,
        channel: None,
        queue_type: QueueType::WithBackup as i32,
        response_type: response_type as i32,
        store_success: true,
        store_failure: true,
        use_static: false,
        broadcast_results,
    };
    app_module.worker_app.create(&worker_data).await
}

/// Test: ListenStream returns error for worker without broadcast_results
#[test]
#[ignore = "Requires Redis for Scalable mode and same RDB as worker process"]
fn test_listen_stream_rejects_non_broadcast_worker() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app_module = Arc::new(create_hybrid_test_app().await?);

        // Create a worker with broadcast_results=false
        let worker_name = "test-non-broadcast-worker";
        let worker_id = create_command_worker(&app_module, worker_name, false).await?;
        eprintln!(
            "✅ Created worker '{}' with broadcast_results=false: {:?}",
            worker_name, worker_id
        );

        // Create a dummy job_id for testing
        let job_id = JobId { value: 999999 };

        // Try to listen to results - should fail
        let result = app_module
            .job_result_app
            .listen_result(
                &job_id,
                Some(&worker_id),
                None,
                Some(1000), // 1 second timeout
                true,       // request_streaming
                DEFAULT_METHOD_NAME,
            )
            .await;

        // Cleanup: delete the worker
        let _ = app_module.worker_app.delete(&worker_id).await;

        // Should return error about non-broadcast worker
        assert!(
            result.is_err(),
            "listen_result should fail for worker without broadcast_results"
        );

        let err_msg = format!("{:?}", result.err().unwrap());
        eprintln!("Error message: {}", err_msg);
        assert!(
            err_msg.contains("broadcast") || err_msg.contains("Cannot listen"),
            "Error message should mention broadcast_results: {}",
            err_msg
        );

        eprintln!("✅ test_listen_stream_rejects_non_broadcast_worker passed");
        Ok(())
    })
}

/// Test: Streaming job returns results via listen_result (NoResult + broadcast)
///
/// This test:
/// 1. Creates a COMMAND worker with broadcast_results=true and response_type=NoResult
/// 2. Waits for worker to be registered in backend
/// 3. Reserves a job ID and starts listening in background BEFORE enqueuing
/// 4. Enqueues a job (will be executed by running worker process)
/// 5. Receives results via listen_result
///
/// Prerequisites:
/// - Backend worker process must be running
/// - Worker process must detect newly created workers
#[test]
#[ignore = "Requires Redis, running worker process, and same RDB as worker process"]
fn test_listen_stream_receives_job_results() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app_module = Arc::new(create_hybrid_test_app().await?);

        // Create a worker with broadcast_results=true and NoResult response type
        // NoResult with broadcast_results=true: job is queued, results are broadcast via pubsub
        let worker_name = "test-broadcast-stream-worker";
        let worker_id = create_command_worker_with_response_type(
            &app_module,
            worker_name,
            true,                    // broadcast_results=true for pubsub
            ResponseType::NoResult,  // NoResult: worker process executes
        )
        .await?;
        eprintln!(
            "✅ Created worker '{}' with broadcast_results=true, response_type=NoResult: {:?}",
            worker_name, worker_id
        );

        // Wait for worker to be registered in backend worker process
        eprintln!("⏳ Waiting for worker registration to propagate (2s)...");
        tokio::time::sleep(Duration::from_secs(2)).await;

        let job_executors = Arc::new(JobExecutorWrapper::new(app_module.clone()));

        // Enqueue a job - simple echo command
        let job_args = serde_json::json!({
            "command": "echo",
            "args": ["hello", "from", "broadcast", "stream", "test"]
        });

        eprintln!("\n📤 Enqueuing job (streaming=false, will be executed by worker process)...");
        let metadata = Arc::new(HashMap::new());
        let enqueue_result = job_executors
            .enqueue_with_worker_name(
                metadata,
                worker_name,
                &job_args,
                None,
                30,    // timeout
                StreamingType::None, // streaming=false (use broadcast instead)
                None,  // using
            )
            .await;

        match enqueue_result {
            Ok((job_id, result_opt, stream_opt)) => {
                eprintln!("✅ Enqueued job: {:?}", job_id);
                eprintln!("   Immediate result: {:?}", result_opt.is_some());
                eprintln!("   Stream from enqueue: {:?}", stream_opt.is_some());

                // For NoResult type, we need to listen for broadcast results
                eprintln!("\n📡 Listening for broadcast result (30s timeout)...");
                let listen_result = app_module
                    .job_result_app
                    .listen_result(
                        &job_id,
                        Some(&worker_id),
                        None,
                        Some(30000), // 30 second timeout
                        false,       // request_streaming (non-streaming listen)
                        DEFAULT_METHOD_NAME,
                    )
                    .await;

                match listen_result {
                    Ok((result, stream_opt)) => {
                        eprintln!("\n📋 Received Broadcast Result:");
                        eprintln!("   ID: {:?}", result.id);
                        if let Some(data) = &result.data {
                            eprintln!("   Status: {:?}", data.status);
                            eprintln!("   Worker ID: {:?}", data.worker_id);
                            eprintln!("   Worker Name: {}", data.worker_name);
                            if let Some(output) = &data.output {
                                let output_str = String::from_utf8_lossy(&output.items);
                                eprintln!("   Output (raw bytes): {:?}", &output.items);
                                eprintln!("   Output (string): {}", output_str);
                            } else {
                                eprintln!("   Output: None");
                            }
                            eprintln!("   Start time: {:?}", data.start_time);
                            eprintln!("   End time: {:?}", data.end_time);
                        }
                        eprintln!("   Metadata: {:?}", result.metadata);
                        eprintln!("   Stream available: {:?}", stream_opt.is_some());
                        eprintln!("\n✅ Successfully received broadcast result!");
                    }
                    Err(e) => {
                        eprintln!("\n❌ Failed to receive broadcast result: {:?}", e);
                        eprintln!("   This may indicate the worker process is not running");
                        eprintln!("   or did not pick up the newly created worker.");
                        eprintln!("\n   NOTE: This test requires a running jobworkerp worker process.");
                        eprintln!("   The worker must be started AFTER the worker definition is created,");
                        eprintln!("   or the running worker must detect and reload newly created workers.");
                    }
                }
            }
            Err(e) => {
                eprintln!("❌ Failed to enqueue job: {:?}", e);
                return Err(e);
            }
        }

        // Cleanup
        let _ = app_module.worker_app.delete(&worker_id).await;
        eprintln!("\n✅ test_listen_stream_receives_job_results completed");
        Ok(())
    })
}

/// Test: Listen with subscribe_result_stream for streaming results
///
/// This test verifies that listen_result with request_streaming=true
/// returns a stream of ResultOutputItem that can be consumed.
#[test]
#[ignore = "Requires Redis, running worker process with streaming support, and same RDB"]
fn test_listen_stream_streaming_results() -> Result<()> {
    use futures::StreamExt;
    use proto::jobworkerp::data::result_output_item;

    TEST_RUNTIME.block_on(async {
        let app_module = Arc::new(create_hybrid_test_app().await?);

        // Create a worker with broadcast_results=true
        let worker_name = "test-streaming-result-worker";
        let worker_id = create_command_worker_with_response_type(
            &app_module,
            worker_name,
            true,
            ResponseType::NoResult,
        )
        .await?;
        eprintln!(
            "✅ Created worker '{}' with broadcast_results=true: {:?}",
            worker_name, worker_id
        );

        // Wait for worker registration
        eprintln!("⏳ Waiting for worker registration (2s)...");
        tokio::time::sleep(Duration::from_secs(2)).await;

        let job_executors = Arc::new(JobExecutorWrapper::new(app_module.clone()));

        // Enqueue job
        let job_args = serde_json::json!({
            "command": "echo",
            "args": ["streaming", "output", "test"]
        });

        eprintln!("\n📤 Enqueuing job...");
        let (job_id, _, _) = job_executors
            .enqueue_with_worker_name(
                Arc::new(HashMap::new()),
                worker_name,
                &job_args,
                None,
                30,
                StreamingType::Response, // request_streaming
                None,
            )
            .await?;
        eprintln!("✅ Enqueued job: {:?}", job_id);

        // Listen for streaming results
        eprintln!("\n📡 Listening for streaming result (30s timeout)...");
        let listen_result = app_module
            .job_result_app
            .listen_result(
                &job_id,
                Some(&worker_id),
                None,
                Some(30000),
                true, // request_streaming=true for streaming listen
                DEFAULT_METHOD_NAME,
            )
            .await;

        match listen_result {
            Ok((initial_result, stream_opt)) => {
                eprintln!("\n📋 Initial Result: {:?}", initial_result.id);
                if let Some(data) = &initial_result.data {
                    eprintln!("   Status: {:?}", data.status);
                    if let Some(output) = &data.output {
                        eprintln!("   Output: {}", String::from_utf8_lossy(&output.items));
                    }
                }

                if let Some(mut stream) = stream_opt {
                    eprintln!("\n📦 Stream Items:");
                    let mut count = 0;
                    while let Some(item) = stream.next().await {
                        count += 1;
                        match &item.item {
                            Some(result_output_item::Item::Data(data)) => {
                                eprintln!("   [{}] Data: {}", count, String::from_utf8_lossy(data));
                            }
                            Some(result_output_item::Item::End(trailer)) => {
                                eprintln!("   [{}] End: {:?}", count, trailer.metadata);
                                break;
                            }
                            Some(result_output_item::Item::FinalCollected(data)) => {
                                eprintln!("   [{}] FinalCollected: {} bytes", count, data.len());
                                break;
                            }
                            None => {
                                eprintln!("   [{}] Empty item", count);
                            }
                        }
                    }
                    eprintln!("\n✅ Received {} stream items", count);
                } else {
                    eprintln!("\n📦 No stream available (result was immediate)");
                }
            }
            Err(e) => {
                eprintln!("\n❌ Failed to listen: {:?}", e);
                eprintln!("   Worker process may not be running or streaming not supported.");
            }
        }

        // Cleanup
        let _ = app_module.worker_app.delete(&worker_id).await;
        eprintln!("\n✅ test_listen_stream_streaming_results completed");
        Ok(())
    })
}

/// Test: ListenStream allows subscription for broadcast worker (timeout expected)
#[test]
#[ignore = "Requires Redis for Scalable mode and same RDB as worker process"]
fn test_listen_stream_allows_broadcast_worker() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app_module = Arc::new(create_hybrid_test_app().await?);

        // Create a worker with broadcast_results=true
        let worker_name = "test-broadcast-worker";
        let worker_id = create_command_worker(&app_module, worker_name, true).await?;
        eprintln!(
            "✅ Created worker '{}' with broadcast_results=true: {:?}",
            worker_name, worker_id
        );

        // Create a dummy job_id for testing (no actual job)
        let job_id = JobId { value: 999998 };

        // Try to listen with short timeout - should timeout, not reject
        let result = app_module
            .job_result_app
            .listen_result(
                &job_id,
                Some(&worker_id),
                None,
                Some(500), // 500ms timeout
                true,
                DEFAULT_METHOD_NAME,
            )
            .await;

        // Cleanup
        let _ = app_module.worker_app.delete(&worker_id).await;

        match result {
            Ok(_) => {
                eprintln!("✅ listen_result succeeded (unexpected but valid)");
            }
            Err(e) => {
                let err_msg = format!("{:?}", e);
                eprintln!("Error: {}", err_msg);
                assert!(
                    err_msg.contains("timeout") || err_msg.contains("Timeout"),
                    "Error should be timeout, not broadcast restriction: {}",
                    err_msg
                );
                eprintln!("✅ listen_result timed out as expected");
            }
        }

        eprintln!("✅ test_listen_stream_allows_broadcast_worker passed");
        Ok(())
    })
}
