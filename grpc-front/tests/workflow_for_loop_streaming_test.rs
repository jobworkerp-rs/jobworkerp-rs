/// Reproduction test: for(inParallel=false) sequential loop streaming
///
/// Goal: verify that each iteration of a sequential `for` loop streams its
/// TaskCompleted event in real time (incrementally) rather than all at once
/// at the end of the loop.
///
/// Run with:
///   cargo test --package grpc-front --test workflow_for_loop_streaming_test -- --ignored --nocapture
///
/// Prerequisites:
/// - jobworkerp-rs server running on localhost:9000
use futures::StreamExt;
use prost::Message;
use std::time::{Duration, Instant};
use tonic::transport::Channel;

use grpc_front::proto::jobworkerp::service::{
    CreateWorkerResponse, JobRequest, OptionalRunnerResponse, RunnerNameRequest,
    job_service_client::JobServiceClient, runner_service_client::RunnerServiceClient,
    worker_service_client::WorkerServiceClient,
};

use proto::jobworkerp::data::{QueueType, ResponseType, WorkerData, result_output_item};

use jobworkerp_runner::jobworkerp::runner::{
    WorkflowResult, WorkflowRunArgs, workflow_run_args::WorkflowSource,
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

/// Outcome of one streaming run: the per-iteration completed-event arrival
/// times, so callers can assert on real-time delivery.
struct StreamOutcome {
    /// Arrival time (seconds since enqueue) of each per-iteration `EchoItem`
    /// TaskCompleted (the events the bug delayed/bunched).
    iteration_times: Vec<f64>,
}

async fn run_for_loop_test(in_parallel: bool) -> Result<StreamOutcome, Box<dyn std::error::Error>> {
    eprintln!(
        "\n=== for-loop streaming test (inParallel={}) ===\n",
        in_parallel
    );

    let channel = create_channel().await?;
    let mut runner_client = RunnerServiceClient::new(channel.clone());
    let mut worker_client = WorkerServiceClient::new(channel.clone());
    let mut job_client = JobServiceClient::new(channel.clone());

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

    let worker_name = format!("test-wf-for-{}-{}", in_parallel, timestamp());
    let worker_data = WorkerData {
        name: worker_name.clone(),
        description: "Test workflow worker for for-loop streaming".to_string(),
        runner_id: Some(runner_id),
        runner_settings: vec![],
        retry_policy: None,
        periodic_interval: 0,
        channel: Some("workflow".to_string()),
        queue_type: QueueType::WithBackup as i32,
        response_type: ResponseType::Direct as i32,
        store_success: true,
        store_failure: true,
        use_static: false,
        broadcast_results: true,
    };
    let created: tonic::Response<CreateWorkerResponse> = worker_client.create(worker_data).await?;
    let worker_id = created.into_inner().id.ok_or("Create worker failed")?;

    // for loop over 4 items; each iteration sleeps 1s then echoes.
    // useStreaming: false (plain run) so each iteration emits a single TaskCompleted.
    let workflow_yaml = format!(
        r#"
document:
  name: for-loop-streaming-test
  namespace: default
  version: 0.0.1
  dsl: 0.0.1
input:
  schema:
    format: json
    document:
      type: object
do:
  - LoopStep:
      for:
        each: item
        in: "${{ [1, 2, 3, 4] }}"
      inParallel: {in_parallel}
      do:
        - EchoItem:
            run:
              runner:
                name: COMMAND
                arguments:
                  command: sh
                  args:
                    - "-c"
                    - "sleep 1; echo done"
                options:
                  channel: workflow
                  responseType: DIRECT
"#
    );

    let workflow_args = WorkflowRunArgs {
        workflow_source: Some(WorkflowSource::WorkflowData(workflow_yaml)),
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
        overrides: None,
    };

    let start_time = Instant::now();
    let response = job_client.enqueue_for_stream(request).await?;
    eprintln!(
        "   Enqueue response received in: {:?}",
        start_time.elapsed()
    );

    let mut stream = response.into_inner();
    let mut event_times: Vec<(Duration, String)> = vec![];
    // Arrival times of the per-iteration EchoItem completed events specifically.
    let mut iteration_times: Vec<f64> = vec![];

    while let Some(result) = stream.next().await {
        let elapsed = start_time.elapsed();
        match result {
            Ok(item) => match item.item {
                Some(result_output_item::Item::Data(data)) => {
                    match WorkflowResult::decode(data.as_slice()) {
                        Ok(wr) => {
                            eprintln!(
                                "   [{:>6.3}s] WorkflowResult: pos={}, status={:?}, out={}",
                                elapsed.as_secs_f64(),
                                wr.position,
                                wr.status,
                                wr.output.chars().take(60).collect::<String>(),
                            );
                            // The EchoItem completed event is the per-iteration
                            // result; its position ends with the inner task name.
                            if wr.position.ends_with("/EchoItem") {
                                iteration_times.push(elapsed.as_secs_f64());
                            }
                            event_times.push((elapsed, format!("Result({})", wr.position)));
                        }
                        Err(e) => {
                            eprintln!(
                                "   [{:>6.3}s] decode error: {:?} ({} bytes)",
                                elapsed.as_secs_f64(),
                                e,
                                data.len()
                            );
                        }
                    }
                }
                Some(result_output_item::Item::End(_)) => {
                    eprintln!("   [{:>6.3}s] End", elapsed.as_secs_f64());
                    event_times.push((elapsed, "End".to_string()));
                }
                Some(result_output_item::Item::FinalCollected(d)) => {
                    eprintln!(
                        "   [{:>6.3}s] FinalCollected: {} bytes",
                        elapsed.as_secs_f64(),
                        d.len()
                    );
                }
                None => {}
            },
            Err(e) => {
                eprintln!("   [{:>6.3}s] Error: {:?}", elapsed.as_secs_f64(), e);
                break;
            }
        }
    }

    // Clean up the worker we created so repeated runs don't leak workers on the
    // server. Best-effort: a failure here must not mask the test result.
    if let Err(e) = worker_client.delete(worker_id).await {
        eprintln!("   warning: failed to delete test worker: {:?}", e);
    }

    eprintln!("\n   Timeline (inParallel={}):", in_parallel);
    let gaps: Vec<f64> = event_times
        .windows(2)
        .map(|w| (w[1].0 - w[0].0).as_secs_f64())
        .collect();
    for (t, e) in &event_times {
        eprintln!("      {:>6.3}s - {}", t.as_secs_f64(), e);
    }
    eprintln!("   gaps: {:?}", gaps);
    eprintln!("   iteration_times: {:?}", iteration_times);

    Ok(StreamOutcome { iteration_times })
}

#[test]
#[ignore = "Requires jobworkerp-rs server running on localhost:9000"]
fn test_for_loop_sequential_streaming() -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Runtime::new()?;
    let outcome = rt.block_on(run_for_loop_test(false))?;

    // The workflow loops over 4 items, each iteration sleeping 1s. With correct
    // real-time streaming each EchoItem TaskCompleted must arrive as its
    // iteration finishes, i.e. one roughly every second.
    assert_eq!(
        outcome.iteration_times.len(),
        4,
        "expected 4 per-iteration EchoItem events, got {:?}",
        outcome.iteration_times
    );

    // Regression guard for the previous_item buffering bug: the last iteration's
    // event used to be withheld and flushed together with the for-completion
    // event at the end, so consecutive iterations bunched up. Assert every
    // consecutive iteration is separated by a real gap (each iteration sleeps
    // 1s; use 0.5s to tolerate scheduling jitter). The buggy version produced a
    // ~0s gap between the 3rd and 4th iteration.
    let iter_gaps: Vec<f64> = outcome
        .iteration_times
        .windows(2)
        .map(|w| w[1] - w[0])
        .collect();
    for (i, gap) in iter_gaps.iter().enumerate() {
        assert!(
            *gap > 0.5,
            "iteration {} arrived only {:.3}s after iteration {} (events bunched, \
             not streamed in real time); iteration_times={:?}",
            i + 1,
            gap,
            i,
            outcome.iteration_times
        );
    }
    Ok(())
}

#[test]
#[ignore = "Requires jobworkerp-rs server running on localhost:9000"]
fn test_for_loop_parallel_streaming() -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Runtime::new()?;
    let outcome = rt.block_on(run_for_loop_test(true))?;

    // Parallel execution still streams 4 iteration events; batching depends on
    // the runtime worker count, so we only assert all iterations are delivered
    // (not their timing).
    assert_eq!(
        outcome.iteration_times.len(),
        4,
        "expected 4 per-iteration EchoItem events, got {:?}",
        outcome.iteration_times
    );
    Ok(())
}
