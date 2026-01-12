/// Reproduction tests for workflow streaming in Standalone mode
///
/// Test scenarios:
/// 1. enqueue_for_stream + response_type: Direct -> realtime streaming response
/// 2. enqueue_for_stream + response_type: NoResult + broadcast_results: true + listen_stream -> realtime streaming
/// 3. regular enqueue + broadcast_results: true + listen_stream -> blocking or error behavior check
///
/// Run with: cargo test --package grpc-front --test workflow_streaming_reproduction_test -- --ignored --nocapture
///
/// Prerequisites:
/// - jobworkerp-rs server running in Standalone mode on localhost:9000
use futures::StreamExt;
use prost::Message;
use std::time::{Duration, Instant};
use tonic::transport::Channel;

use grpc_front::proto::jobworkerp::service::{
    job_result_service_client::JobResultServiceClient, job_service_client::JobServiceClient,
    runner_service_client::RunnerServiceClient, worker_service_client::WorkerServiceClient,
    CreateWorkerResponse, JobRequest, ListenRequest, OptionalRunnerResponse, RunnerNameRequest,
};

use proto::jobworkerp::data::{result_output_item, QueueType, ResponseType, WorkerData, WorkerId};

use jobworkerp_runner::jobworkerp::runner::{
    workflow_run_args::WorkflowSource, WorkflowResult, WorkflowRunArgs,
};

const GRPC_URL: &str = "http://localhost:9000";

async fn create_channel() -> Result<Channel, Box<dyn std::error::Error>> {
    let channel = Channel::from_static(GRPC_URL)
        .connect_timeout(Duration::from_secs(5))
        .connect()
        .await?;
    Ok(channel)
}

fn timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .to_string()
}

/// Test 1: enqueue_for_stream + response_type: Direct (via worker options in workflow)
/// Expectation: Results are streamed in realtime
#[test]
#[ignore = "Requires jobworkerp-rs server running on localhost:9000"]
fn test_workflow_enqueue_for_stream_direct_response() -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        eprintln!("\n=== Test 1: Workflow enqueue_for_stream + response_type: Direct ===\n");

        let channel = create_channel().await?;
        let mut runner_client = RunnerServiceClient::new(channel.clone());
        let mut worker_client = WorkerServiceClient::new(channel.clone());
        let mut job_client = JobServiceClient::new(channel.clone());

        // Find WORKFLOW runner
        eprintln!("Step 1: Finding WORKFLOW runner...");
        let runner_resp: tonic::Response<OptionalRunnerResponse> = runner_client
            .find_by_name(RunnerNameRequest {
                name: "WORKFLOW".to_string(),
            })
            .await?;
        let runner = runner_resp
            .into_inner()
            .data
            .ok_or("WORKFLOW runner not found")?;
        let runner_id = runner.id.ok_or("Runner has no ID")?;
        eprintln!("   Found runner: ID={}", runner_id.value);

        // Create worker with response_type: Direct for WORKFLOW runner
        eprintln!("Step 2: Creating WORKFLOW worker with response_type: Direct...");
        let worker_name = format!("test-wf-direct-{}", timestamp());

        let worker_data = WorkerData {
            name: worker_name.clone(),
            description: "Test workflow worker with Direct response".to_string(),
            runner_id: Some(runner_id),
            runner_settings: vec![],
            retry_policy: None,
            periodic_interval: 0,
            channel: Some("workflow".to_string()),
            queue_type: QueueType::WithBackup as i32,
            response_type: ResponseType::Direct as i32, // Direct response
            store_success: true,
            store_failure: true,
            use_static: false,
            broadcast_results: true,
        };
        let created: tonic::Response<CreateWorkerResponse> =
            worker_client.create(worker_data).await?;
        let worker_id = created.into_inner().id.ok_or("Create worker failed")?;
        eprintln!("   Created worker: {:?}", worker_id);

        // Execute workflow with enqueue_for_stream
        // The workflow uses runner with options specifying responseType: DIRECT
        eprintln!("Step 3: Executing workflow with enqueue_for_stream...");

        // Workflow with runner options to ensure DIRECT response in subtasks
        let workflow_yaml = r#"
document:
  name: streaming-direct-test
  namespace: default
  version: 0.0.1
  dsl: 0.0.1
input:
  schema:
    format: json
    document:
      type: object
do:
  - Step1:
      run:
        runner:
          name: COMMAND
          arguments:
            command: sh
            args:
              - "-c"
              - "echo 'Step1 output'; sleep 1"
          options:
            channel: workflow
            responseType: DIRECT
            broadcastResults: true
      useStreaming: true
  - Step2:
      run:
        runner:
          name: COMMAND
          arguments:
            command: sh
            args:
              - "-c"
              - "echo 'Step2 output'; sleep 1"
          options:
            channel: workflow
            responseType: DIRECT
            broadcastResults: true
      useStreaming: true
  - Step3:
      run:
        runner:
          name: COMMAND
          arguments:
            command: sh
            args:
              - "-c"
              - "echo 'Step3 output'"
          options:
            channel: workflow
            responseType: DIRECT
            broadcastResults: true
      useStreaming: true
"#;

        let workflow_args = WorkflowRunArgs {
            workflow_source: Some(WorkflowSource::WorkflowData(workflow_yaml.to_string())),
            input: "{}".to_string(),
            workflow_context: None,
            execution_id: None,
            from_checkpoint: None,
        };

        let request = JobRequest {
            worker: Some(
                grpc_front::proto::jobworkerp::service::job_request::Worker::WorkerId(worker_id),
            ),
            args: workflow_args.encode_to_vec(),
            uniq_key: None,
            run_after_time: None,
            priority: None,
            timeout: Some(60000),
            using: Some("run".to_string()),
        };

        let start_time = Instant::now();
        eprintln!("   Start time: {:?}", start_time);

        let response = job_client.enqueue_for_stream(request).await?;
        let enqueue_time = start_time.elapsed();
        eprintln!("   Enqueue response received in: {:?}", enqueue_time);

        let mut stream = response.into_inner();

        eprintln!("\n Streaming Results (Direct Response):");
        let mut event_times: Vec<(Duration, String)> = vec![];

        while let Some(result) = stream.next().await {
            let elapsed = start_time.elapsed();
            match result {
                Ok(item) => match item.item {
                    Some(result_output_item::Item::Data(data)) => {
                        match WorkflowResult::decode(data.as_slice()) {
                            Ok(workflow_result) => {
                                eprintln!(
                                    "   [{:>6.3}s] WorkflowResult: id={}, position={}, status={:?}",
                                    elapsed.as_secs_f64(),
                                    workflow_result.id,
                                    workflow_result.position,
                                    workflow_result.status,
                                );
                                event_times.push((
                                    elapsed,
                                    format!("WorkflowResult({})", workflow_result.position),
                                ));
                            }
                            Err(e) => {
                                eprintln!(
                                    "   [{:>6.3}s] Data decode error: {:?}, raw={} bytes",
                                    elapsed.as_secs_f64(),
                                    e,
                                    data.len()
                                );
                                event_times.push((elapsed, format!("DecodeError({})", data.len())));
                            }
                        }
                    }
                    Some(result_output_item::Item::End(trailer)) => {
                        eprintln!(
                            "   [{:>6.3}s] End: metadata_count={}",
                            elapsed.as_secs_f64(),
                            trailer.metadata.len()
                        );
                        event_times.push((elapsed, "End".to_string()));
                    }
                    Some(result_output_item::Item::FinalCollected(data)) => {
                        eprintln!(
                            "   [{:>6.3}s] FinalCollected: {} bytes",
                            elapsed.as_secs_f64(),
                            data.len()
                        );
                        event_times.push((elapsed, "FinalCollected".to_string()));
                    }
                    None => {
                        eprintln!("   [{:>6.3}s] Empty item", elapsed.as_secs_f64());
                    }
                },
                Err(e) => {
                    eprintln!("   [{:>6.3}s] Error: {:?}", elapsed.as_secs_f64(), e);
                    break;
                }
            }
        }

        let total_time = start_time.elapsed();

        eprintln!("\n Summary (Test 1 - Direct Response):");
        eprintln!("   Total events: {}", event_times.len());
        eprintln!("   Total time: {:?}", total_time);

        analyze_streaming_behavior(&event_times);

        eprintln!("\n=== Test 1 Complete ===\n");
        Ok(())
    })
}

/// Test 2: enqueue_for_stream + response_type: NoResult + broadcast_results: true + listen_stream
/// Expectation: Results are streamed in realtime via listen_stream
#[test]
#[ignore = "Requires jobworkerp-rs server running on localhost:9000"]
fn test_workflow_enqueue_for_stream_no_result_with_listen_stream(
) -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        eprintln!("\n=== Test 2: Workflow enqueue_for_stream + NoResult + listen_stream ===\n");

        let channel = create_channel().await?;
        let mut runner_client = RunnerServiceClient::new(channel.clone());
        let mut worker_client = WorkerServiceClient::new(channel.clone());
        let mut job_client = JobServiceClient::new(channel.clone());
        let mut job_result_client = JobResultServiceClient::new(channel.clone());

        // Find WORKFLOW runner
        eprintln!("Step 1: Finding WORKFLOW runner...");
        let runner_resp: tonic::Response<OptionalRunnerResponse> = runner_client
            .find_by_name(RunnerNameRequest {
                name: "WORKFLOW".to_string(),
            })
            .await?;
        let runner = runner_resp
            .into_inner()
            .data
            .ok_or("WORKFLOW runner not found")?;
        let runner_id = runner.id.ok_or("Runner has no ID")?;
        eprintln!("   Found runner: ID={}", runner_id.value);

        // Create worker with response_type: NoResult and broadcast_results: true
        eprintln!(
            "Step 2: Creating worker with response_type: NoResult, broadcast_results: true..."
        );
        let worker_name = format!("test-wf-noresult-{}", timestamp());

        let worker_data = WorkerData {
            name: worker_name.clone(),
            description: "Test workflow worker with NoResult + broadcast".to_string(),
            runner_id: Some(runner_id),
            runner_settings: vec![],
            retry_policy: None,
            periodic_interval: 0,
            channel: Some("workflow".to_string()),
            queue_type: QueueType::WithBackup as i32,
            response_type: ResponseType::NoResult as i32, // NoResult
            store_success: true,
            store_failure: true,
            use_static: false,
            broadcast_results: true, // Required for listen_stream
        };
        let created: tonic::Response<CreateWorkerResponse> =
            worker_client.create(worker_data).await?;
        let worker_id: WorkerId = created.into_inner().id.ok_or("Create worker failed")?;
        eprintln!("   Created worker: {:?}", worker_id);

        // Execute workflow with enqueue_for_stream
        eprintln!("Step 3: Executing workflow with enqueue_for_stream...");

        // Use initial delay to allow listen_stream subscription time
        // Subtasks use responseType: DIRECT with broadcastResults: true
        let workflow_yaml = r#"
document:
  name: streaming-noresult-test
  namespace: default
  version: 0.0.1
  dsl: 0.0.1
input:
  schema:
    format: json
    document:
      type: object
do:
  - InitialDelay:
      run:
        runner:
          name: COMMAND
          arguments:
            command: sh
            args:
              - "-c"
              - "sleep 2; echo 'InitialDelay done'"
          options:
            channel: workflow
            responseType: DIRECT
            broadcastResults: true
      useStreaming: true
  - Step1:
      run:
        runner:
          name: COMMAND
          arguments:
            command: sh
            args:
              - "-c"
              - "echo 'Step1 output'; sleep 1"
          options:
            channel: workflow
            responseType: DIRECT
            broadcastResults: true
      useStreaming: true
  - Step2:
      run:
        runner:
          name: COMMAND
          arguments:
            command: sh
            args:
              - "-c"
              - "echo 'Step2 output'"
          options:
            channel: workflow
            responseType: DIRECT
            broadcastResults: true
      useStreaming: true
"#;

        let workflow_args = WorkflowRunArgs {
            workflow_source: Some(WorkflowSource::WorkflowData(workflow_yaml.to_string())),
            input: "{}".to_string(),
            workflow_context: None,
            execution_id: None,
            from_checkpoint: None,
        };

        let request = JobRequest {
            worker: Some(
                grpc_front::proto::jobworkerp::service::job_request::Worker::WorkerId(worker_id),
            ),
            args: workflow_args.encode_to_vec(),
            uniq_key: None,
            run_after_time: None,
            priority: None,
            timeout: Some(60000),
            using: Some("run".to_string()),
        };

        let start_time = Instant::now();
        eprintln!("   Start time: {:?}", start_time);

        let enqueue_resp = job_client.enqueue_for_stream(request).await?;
        let enqueue_time = start_time.elapsed();
        eprintln!("   Enqueue response received in: {:?}", enqueue_time);

        // Get job_id from metadata
        let metadata = enqueue_resp.metadata();
        let job_id_bytes = metadata
            .get_bin("x-job-id-bin")
            .ok_or("No job ID in metadata")?
            .to_bytes()
            .map_err(|e| format!("Failed to get job ID bytes: {:?}", e))?;

        let job_id: proto::jobworkerp::data::JobId = prost::Message::decode(&job_id_bytes[..])?;
        eprintln!("   Job ID: {:?}", job_id);

        // Call listen_stream to subscribe to results
        eprintln!("\nStep 4: Calling listen_stream to subscribe...");

        let listen_request = ListenRequest {
            job_id: Some(job_id),
            worker: Some(
                grpc_front::proto::jobworkerp::service::listen_request::Worker::WorkerId(worker_id),
            ),
            timeout: Some(60000),
            using: Some("run".to_string()),
        };
        let listen_resp = job_result_client.listen_stream(listen_request).await?;
        let listen_start = start_time.elapsed();
        eprintln!("   listen_stream started at: {:?}", listen_start);

        let mut stream = listen_resp.into_inner();

        eprintln!("\n ListenStream Results:");
        let mut event_times: Vec<(Duration, String)> = vec![];

        while let Some(result) = stream.next().await {
            let elapsed = start_time.elapsed();
            match result {
                Ok(item) => match item.item {
                    Some(result_output_item::Item::Data(data)) => {
                        match WorkflowResult::decode(data.as_slice()) {
                            Ok(workflow_result) => {
                                eprintln!(
                                    "   [{:>6.3}s] WorkflowResult: id={}, position={}, status={:?}",
                                    elapsed.as_secs_f64(),
                                    workflow_result.id,
                                    workflow_result.position,
                                    workflow_result.status,
                                );
                                event_times.push((
                                    elapsed,
                                    format!("WorkflowResult({})", workflow_result.position),
                                ));
                            }
                            Err(e) => {
                                eprintln!(
                                    "   [{:>6.3}s] Data decode error: {:?}, raw={} bytes",
                                    elapsed.as_secs_f64(),
                                    e,
                                    data.len()
                                );
                                event_times.push((elapsed, format!("DecodeError({})", data.len())));
                            }
                        }
                    }
                    Some(result_output_item::Item::End(trailer)) => {
                        eprintln!(
                            "   [{:>6.3}s] End: metadata_count={}",
                            elapsed.as_secs_f64(),
                            trailer.metadata.len()
                        );
                        event_times.push((elapsed, "End".to_string()));
                    }
                    Some(result_output_item::Item::FinalCollected(data)) => {
                        eprintln!(
                            "   [{:>6.3}s] FinalCollected: {} bytes",
                            elapsed.as_secs_f64(),
                            data.len()
                        );
                        event_times.push((elapsed, "FinalCollected".to_string()));
                    }
                    None => {
                        eprintln!("   [{:>6.3}s] Empty item", elapsed.as_secs_f64());
                    }
                },
                Err(e) => {
                    eprintln!("   [{:>6.3}s] Error: {:?}", elapsed.as_secs_f64(), e);
                    break;
                }
            }
        }

        let total_time = start_time.elapsed();

        eprintln!("\n Summary (Test 2 - NoResult + listen_stream):");
        eprintln!("   Total events: {}", event_times.len());
        eprintln!("   Total time: {:?}", total_time);

        analyze_streaming_behavior(&event_times);

        eprintln!("\n=== Test 2 Complete ===\n");
        Ok(())
    })
}

/// Test 3: Regular enqueue (non-streaming) + broadcast_results: true + listen_stream
/// Expectation: May error or block until completion
#[test]
#[ignore = "Requires jobworkerp-rs server running on localhost:9000"]
fn test_workflow_regular_enqueue_with_listen_stream() -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        eprintln!("\n=== Test 3: Workflow regular enqueue + listen_stream ===\n");

        let channel = create_channel().await?;
        let mut runner_client = RunnerServiceClient::new(channel.clone());
        let mut worker_client = WorkerServiceClient::new(channel.clone());
        let mut job_client = JobServiceClient::new(channel.clone());
        let mut job_result_client = JobResultServiceClient::new(channel.clone());

        // Find WORKFLOW runner
        eprintln!("Step 1: Finding WORKFLOW runner...");
        let runner_resp: tonic::Response<OptionalRunnerResponse> = runner_client
            .find_by_name(RunnerNameRequest {
                name: "WORKFLOW".to_string(),
            })
            .await?;
        let runner = runner_resp
            .into_inner()
            .data
            .ok_or("WORKFLOW runner not found")?;
        let runner_id = runner.id.ok_or("Runner has no ID")?;
        eprintln!("   Found runner: ID={}", runner_id.value);

        // Create worker with response_type: NoResult and broadcast_results: true
        eprintln!(
            "Step 2: Creating worker with response_type: NoResult, broadcast_results: true..."
        );
        let worker_name = format!("test-wf-regular-{}", timestamp());

        let worker_data = WorkerData {
            name: worker_name.clone(),
            description: "Test workflow worker for regular enqueue".to_string(),
            runner_id: Some(runner_id),
            runner_settings: vec![],
            retry_policy: None,
            periodic_interval: 0,
            channel: Some("workflow".to_string()),
            queue_type: QueueType::WithBackup as i32,
            response_type: ResponseType::NoResult as i32,
            store_success: true,
            store_failure: true,
            use_static: false,
            broadcast_results: true,
        };
        let created: tonic::Response<CreateWorkerResponse> =
            worker_client.create(worker_data).await?;
        let worker_id: WorkerId = created.into_inner().id.ok_or("Create worker failed")?;
        eprintln!("   Created worker: {:?}", worker_id);

        // Execute workflow with regular enqueue (NOT enqueue_for_stream)
        // Subtasks do NOT use useStreaming (regular non-streaming execution)
        eprintln!("Step 3: Executing workflow with regular enqueue (non-streaming)...");

        let workflow_yaml = r#"
document:
  name: streaming-regular-test
  namespace: default
  version: 0.0.1
  dsl: 0.0.1
input:
  schema:
    format: json
    document:
      type: object
do:
  - InitialDelay:
      run:
        runner:
          name: COMMAND
          arguments:
            command: sh
            args:
              - "-c"
              - "sleep 2; echo 'InitialDelay done'"
          options:
            channel: workflow
            responseType: DIRECT
  - Step1:
      run:
        runner:
          name: COMMAND
          arguments:
            command: sh
            args:
              - "-c"
              - "echo 'Step1 output'; sleep 1"
          options:
            channel: workflow
            responseType: DIRECT
  - Step2:
      run:
        runner:
          name: COMMAND
          arguments:
            command: sh
            args:
              - "-c"
              - "echo 'Step2 output'"
          options:
            channel: workflow
            responseType: DIRECT
"#;

        let workflow_args = WorkflowRunArgs {
            workflow_source: Some(WorkflowSource::WorkflowData(workflow_yaml.to_string())),
            input: "{}".to_string(),
            workflow_context: None,
            execution_id: None,
            from_checkpoint: None,
        };

        let request = JobRequest {
            worker: Some(
                grpc_front::proto::jobworkerp::service::job_request::Worker::WorkerId(
                    worker_id,
                ),
            ),
            args: workflow_args.encode_to_vec(),
            uniq_key: None,
            run_after_time: None,
            priority: None,
            timeout: Some(60000),
            using: Some("run".to_string()),
        };

        let start_time = Instant::now();
        eprintln!("   Start time: {:?}", start_time);

        // Use regular enqueue (NOT enqueue_for_stream)
        let enqueue_resp = job_client.enqueue(request).await?;
        let job_id = enqueue_resp.into_inner().id.ok_or("No job ID")?;
        eprintln!("   Job ID: {:?}", job_id);
        eprintln!("   Enqueue time: {:?}", start_time.elapsed());

        // Call listen_stream immediately after enqueue
        eprintln!("\nStep 4: Calling listen_stream to subscribe...");

        let listen_request = ListenRequest {
            job_id: Some(job_id),
            worker: Some(
                grpc_front::proto::jobworkerp::service::listen_request::Worker::WorkerId(
                    worker_id,
                ),
            ),
            timeout: Some(60000),
            using: Some("run".to_string()),
        };

        let listen_resp = job_result_client.listen_stream(listen_request).await;

        match listen_resp {
            Ok(resp) => {
                let listen_start = start_time.elapsed();
                eprintln!("   listen_stream started at: {:?}", listen_start);

                let mut stream = resp.into_inner();

                eprintln!("\n ListenStream Results (regular enqueue):");
                let mut event_times: Vec<(Duration, String)> = vec![];

                while let Some(result) = stream.next().await {
                    let elapsed = start_time.elapsed();
                    match result {
                        Ok(item) => match item.item {
                            Some(result_output_item::Item::Data(data)) => {
                                match WorkflowResult::decode(data.as_slice()) {
                                    Ok(workflow_result) => {
                                        eprintln!(
                                            "   [{:>6.3}s] WorkflowResult: id={}, position={}, status={:?}",
                                            elapsed.as_secs_f64(),
                                            workflow_result.id,
                                            workflow_result.position,
                                            workflow_result.status,
                                        );
                                        event_times.push((
                                            elapsed,
                                            format!("WorkflowResult({})", workflow_result.position),
                                        ));
                                    }
                                    Err(e) => {
                                        eprintln!(
                                            "   [{:>6.3}s] Data decode error: {:?}, raw={} bytes",
                                            elapsed.as_secs_f64(),
                                            e,
                                            data.len()
                                        );
                                        event_times
                                            .push((elapsed, format!("DecodeError({})", data.len())));
                                    }
                                }
                            }
                            Some(result_output_item::Item::End(trailer)) => {
                                eprintln!(
                                    "   [{:>6.3}s] End: metadata_count={}",
                                    elapsed.as_secs_f64(),
                                    trailer.metadata.len()
                                );
                                event_times.push((elapsed, "End".to_string()));
                            }
                            Some(result_output_item::Item::FinalCollected(data)) => {
                                eprintln!(
                                    "   [{:>6.3}s] FinalCollected: {} bytes",
                                    elapsed.as_secs_f64(),
                                    data.len()
                                );
                                event_times.push((elapsed, "FinalCollected".to_string()));
                            }
                            None => {
                                eprintln!("   [{:>6.3}s] Empty item", elapsed.as_secs_f64());
                                event_times.push((elapsed, "Empty".to_string()));
                            }
                        },
                        Err(e) => {
                            eprintln!("   [{:>6.3}s] Error: {:?}", elapsed.as_secs_f64(), e);
                            break;
                        }
                    }
                }

                let total_time = start_time.elapsed();

                eprintln!("\n Summary (Test 3 - Regular enqueue + listen_stream):");
                eprintln!("   Total events: {}", event_times.len());
                eprintln!("   Total time: {:?}", total_time);

                analyze_streaming_behavior(&event_times);
            }
            Err(e) => {
                eprintln!("   listen_stream returned error (expected): {:?}", e);
                eprintln!("\n Behavior: listen_stream rejected non-streaming job");
            }
        }

        eprintln!("\n=== Test 3 Complete ===\n");
        Ok(())
    })
}

fn analyze_streaming_behavior(event_times: &[(Duration, String)]) {
    eprintln!("\n Streaming Analysis:");

    if event_times.is_empty() {
        eprintln!("    No events received");
        return;
    }

    if event_times.len() < 2 {
        eprintln!(
            "    Only {} event(s) - insufficient for analysis",
            event_times.len()
        );
        return;
    }

    // Calculate time gaps between events
    let time_diffs: Vec<f64> = event_times
        .windows(2)
        .map(|w| (w[1].0 - w[0].0).as_secs_f64())
        .collect();

    eprintln!("   Event timeline:");
    for (time, event) in event_times {
        eprintln!("      {:>6.3}s - {}", time.as_secs_f64(), event);
    }

    eprintln!("\n   Time gaps between events: {:?}", time_diffs);

    // Count significant gaps (> 0.1s indicates realtime streaming)
    let significant_gaps = time_diffs.iter().filter(|&&d| d > 0.1).count();
    let avg_gap = time_diffs.iter().sum::<f64>() / time_diffs.len() as f64;

    eprintln!("   Significant gaps (>0.1s): {}", significant_gaps);
    eprintln!("   Average gap: {:.3}s", avg_gap);

    if significant_gaps > 0 {
        eprintln!("\n    REALTIME: Events arrived with delays (streaming working correctly)");
    } else {
        eprintln!("\n    NOT REALTIME: All events arrived at once (streaming may NOT be working)");
    }
}
