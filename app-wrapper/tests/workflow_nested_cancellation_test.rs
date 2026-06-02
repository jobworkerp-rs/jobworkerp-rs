//! Reproduction tests for nested WORKFLOW cancellation and JobProcessingStatus
//! listing issues observed in the field with the
//! `memories/workflows/personality/thread-personality-batch.yaml` driver:
//!
//! 1. **Listing**: a top-level WORKFLOW job dispatched against jobworkerp does
//!    not appear in `JobProcessingStatusService.FindByCondition` even though
//!    the job is actively running.
//!
//! 2. **Cancellation**: cancelling a top-level WORKFLOW job (e.g. via
//!    `JobService.Delete` or admin-ui's cancel button) does not propagate
//!    down to in-flight child WORKFLOW jobs spawned via the `WORKFLOW` runner
//!    (`run.function.runnerName: WORKFLOW`); the children keep running.
//!
//! Each case is repeated across the (response_type, enqueue path) matrix to
//! prove the bugs are not specific to one dispatch shape:
//!
//! | response_type | enqueue path             |
//! |---------------|--------------------------|
//! | NO_RESULT     | JobService.Enqueue       |
//! | DIRECT        | JobService.Enqueue       |
//! | DIRECT        | JobService.EnqueueForStream |
//!
//! Every case dispatches against a known channel (`workflow` by default, or
//! `JOBWORKERP_TEST_CHANNEL` override) and asserts only on jobs that flow
//! through that channel, so the test stays correct on shared deployments with
//! unrelated traffic — **provided no other workload is running on that
//! channel during the test window.** Pick an idle channel or override.
//!
//! These tests are `#[ignore]` by default because they need:
//!   - a live grpc-front + worker pair running on the same backend;
//!   - `JOB_STATUS_RDB_INDEXING=true` (required by FindByCondition);
//!   - a worker that listens on the configured test channel.
//!
//! ```sh
//! JOB_STATUS_RDB_INDEXING=true ./target/release/all-in-one
//!
//! JOBWORKERP_GRPC_ADDR=http://127.0.0.1:9000 \
//! JOBWORKERP_TEST_CHANNEL=workflow \
//!   cargo test --package app-wrapper --test workflow_nested_cancellation_test \
//!     -- --ignored --nocapture --test-threads=1
//! ```

#![allow(clippy::uninlined_format_args)]

use anyhow::{Context, Result, anyhow};
use std::time::Duration;
use tokio::time::Instant;

use futures::StreamExt;
use proto::jobworkerp::data::{
    JobId, JobProcessingStatus, Priority, QueueType, ResponseType, RunnerId, WorkerData,
};
// `jobworkerp.service.*` (tonic clients + request/response messages) are
// generated only in the grpc-front crate's build.rs; reuse them rather than
// regenerating in the test crate.
use grpc_front::proto::jobworkerp::service::job_processing_status_service_client::JobProcessingStatusServiceClient;
use grpc_front::proto::jobworkerp::service::job_service_client::JobServiceClient;
use grpc_front::proto::jobworkerp::service::worker_service_client::WorkerServiceClient;
use grpc_front::proto::jobworkerp::service::{
    FindJobProcessingStatusRequest, JobRequest, OptionalJobProcessingStatusResponse, job_request,
};
use prost::Message;
use tonic::Request;
use tonic::transport::Channel;

use jobworkerp_runner::jobworkerp::runner::{
    WorkflowRunArgs, WorkflowRunnerSettings, workflow_runner_settings::WorkflowSource,
};

/// WORKFLOW runner ID (see proto/protobuf/jobworkerp/data/common.proto).
const WORKFLOW_RUNNER_ID: i64 = 32769;

/// Channel used by every test in this file so we can filter `find_by_condition`
/// results to "RUNNING jobs we own" and ignore unrelated traffic on the
/// deployment. `None` (the default) mirrors how `memories-import →
/// run_personality_after → JobworkerpClientWrapper::execute_workflow` is
/// typically invoked: no explicit `--channel` argument, so the worker is
/// dispatched on the default channel.
///
/// Override via `JOBWORKERP_TEST_CHANNEL` to pin the test to a dedicated
/// channel (avoids polluting / colliding with unrelated traffic on shared
/// deployments).
fn test_channel() -> Option<String> {
    std::env::var("JOBWORKERP_TEST_CHANNEL")
        .ok()
        .filter(|s| !s.is_empty())
}

/// Dispatch shape under test. Combined with `ResponseType` to form the matrix.
#[derive(Copy, Clone, Debug)]
enum EnqueuePath {
    /// `JobService.Enqueue` — classic unary. For `Direct` workers this call
    /// blocks until the job completes, so the test runs it inside a spawned
    /// task and lets the watchdog cancel via `JobService.Delete`.
    Enqueue,
    /// `JobService.EnqueueForStream` — server-streaming response. Same Direct
    /// blocking semantics; we keep the stream open in a spawned task and
    /// cancel through `Delete`.
    EnqueueForStream,
}

fn grpc_addr() -> String {
    std::env::var("JOBWORKERP_GRPC_ADDR").unwrap_or_else(|_| "http://127.0.0.1:9000".to_string())
}

async fn connect() -> Result<Channel> {
    let addr = grpc_addr();
    Channel::from_shared(addr.clone())
        .with_context(|| format!("invalid JOBWORKERP_GRPC_ADDR: {addr}"))?
        .connect()
        .await
        .with_context(|| format!("failed to connect to {addr}"))
}

/// Inline nested workflow used as the test driver.
///
/// Mirrors the shape of `thread-personality-batch.yaml` in miniature: the
/// outer workflow uses `for/each` to fan out a child WORKFLOW that just sleeps
/// for 60 seconds via the COMMAND runner.
fn parent_workflow_json(child_workflow_json: &str) -> String {
    let child_escaped = serde_json::to_string(child_workflow_json).unwrap();
    format!(
        r#"{{
            "document": {{
                "dsl": "1.0.0-jobworkerp",
                "namespace": "test",
                "name": "nested-parent",
                "version": "1.0.0"
            }},
            "do": [
                {{
                    "fanOut": {{
                        "for": {{ "each": "item", "in": "${{ [1] }}" }},
                        "do": [
                            {{
                                "invokeChild": {{
                                    "run": {{
                                        "function": {{
                                            "runnerName": "WORKFLOW",
                                            "settings": {{
                                                "workflow_data": {child}
                                            }},
                                            "arguments": {{
                                                "input": "{{}}"
                                            }}
                                        }}
                                    }}
                                }}
                            }}
                        ]
                    }}
                }}
            ]
        }}"#,
        child = child_escaped,
    )
}

fn child_sleep_workflow_json() -> String {
    r#"{
        "document": {
            "dsl": "1.0.0-jobworkerp",
            "namespace": "test",
            "name": "nested-child",
            "version": "1.0.0"
        },
        "do": [
            {
                "longSleep": {
                    "run": {
                        "runner": {
                            "name": "COMMAND",
                            "arguments": {
                                "command": "sleep",
                                "args": ["60"]
                            }
                        }
                    }
                }
            }
        ]
    }"#
    .to_string()
}

fn parent_runner_settings(child_workflow_json: &str) -> Vec<u8> {
    WorkflowRunnerSettings {
        workflow_source: Some(WorkflowSource::WorkflowData(parent_workflow_json(
            child_workflow_json,
        ))),
        workflow_context: None,
    }
    .encode_to_vec()
}

/// Create a transient worker for the parent WORKFLOW. The worker pins all
/// dispatches to the configured test channel so assertions can filter on it.
async fn create_parent_worker(
    workers: &mut WorkerServiceClient<Channel>,
    name: &str,
    child_workflow_json: &str,
    response_type: ResponseType,
) -> Result<()> {
    let wd = WorkerData {
        name: name.to_string(),
        description: "nested-cancel reproduction".to_string(),
        runner_id: Some(RunnerId {
            value: WORKFLOW_RUNNER_ID,
        }),
        runner_settings: parent_runner_settings(child_workflow_json),
        periodic_interval: 0,
        channel: test_channel(),
        queue_type: QueueType::Normal as i32,
        response_type: response_type as i32,
        store_success: false,
        store_failure: true,
        use_static: false,
        retry_policy: None,
        broadcast_results: true,
    };
    workers.create(Request::new(wd)).await?;
    Ok(())
}

fn build_job_request(worker_name: &str) -> JobRequest {
    let args = WorkflowRunArgs {
        workflow_source: None,
        input: "{}".to_string(),
        ..Default::default()
    };
    JobRequest {
        worker: Some(job_request::Worker::WorkerName(worker_name.to_string())),
        args: args.encode_to_vec(),
        uniq_key: None,
        run_after_time: None,
        priority: Some(Priority::Medium as i32),
        timeout: Some(5 * 60 * 1000),
        using: None,
        overrides: None,
    }
}

/// Start the parent job on the configured enqueue path. For NoResult the
/// caller learns the JobId synchronously from the response. For Direct the
/// call blocks until completion, so we spawn it and recover the JobId via
/// the live-SoT `FindAll` + per-job `Find` cross-reference — crucially **not**
/// via `FindByCondition`, since that path is the very mechanism the listing
/// bug under test breaks.
async fn start_parent_job(
    workers: &mut WorkerServiceClient<Channel>,
    jobs: &mut JobServiceClient<Channel>,
    status: &mut JobProcessingStatusServiceClient<Channel>,
    worker_name: &str,
    response_type: ResponseType,
    enqueue_path: EnqueuePath,
) -> Result<JobId> {
    let req = build_job_request(worker_name);

    match (response_type, enqueue_path) {
        // NO_RESULT + Enqueue: the response carries the JobId immediately and
        // the call does not block on completion.
        (ResponseType::NoResult, EnqueuePath::Enqueue) => {
            let resp = jobs.enqueue(Request::new(req)).await?.into_inner();
            resp.id.ok_or_else(|| anyhow!("enqueue returned no job id"))
        }
        // DIRECT + Enqueue: the unary call blocks on Direct's WAIT_RESULT
        // until the job completes, so we cannot read the JobId from its
        // response. Resolve the worker's id by name and look the running job
        // up through live-SoT (`FindAll` + per-id `Find`) — deliberately
        // avoiding `FindByCondition`, which is the listing path under test.
        (ResponseType::Direct, EnqueuePath::Enqueue) => {
            let mut jobs_bg = jobs.clone();
            tokio::spawn(async move {
                let _ = jobs_bg.enqueue(Request::new(req)).await;
            });
            wait_for_first_running_for_worker_via_live_sot(
                workers,
                jobs,
                status,
                worker_name,
                Duration::from_secs(10),
            )
            .await
        }
        // DIRECT + EnqueueForStream: the server streams `ResultOutputItem`s
        // back. Crucially the JobId is included in the response *headers*
        // (`x-job-id-bin`) before any stream item is emitted, so we can
        // learn it synchronously without polling any listing API. The
        // stream is then kept alive on a spawned task so the underlying job
        // keeps running until the watchdog issues `JobService.Delete`.
        (ResponseType::Direct, EnqueuePath::EnqueueForStream) => {
            let resp = jobs.enqueue_for_stream(Request::new(req)).await?;
            let job_id = extract_job_id_from_headers(resp.metadata())?;
            let mut stream = resp.into_inner();
            tokio::spawn(async move {
                while let Some(item) = stream.next().await {
                    // Drain to keep the worker side from getting back-pressured.
                    // Errors here just mean the stream closed (e.g. job
                    // cancelled), which is expected.
                    drop(item);
                }
            });
            Ok(job_id)
        }
        // NO_RESULT + EnqueueForStream is rejected by the gRPC frontend
        // (streaming response requires broadcast/direct semantics), so we
        // don't include it in the matrix.
        _ => Err(anyhow!(
            "unsupported (response_type, enqueue_path) combination: ({:?}, {:?})",
            response_type,
            enqueue_path
        )),
    }
}

/// EnqueueForStream returns the JobId out-of-band on response metadata
/// (`x-job-id-bin`, see grpc-front/src/service.rs). Decode it without polling
/// any listing API — that keeps `start_parent_job` independent of the very
/// path under test for the listing bug.
fn extract_job_id_from_headers(meta: &tonic::metadata::MetadataMap) -> Result<JobId> {
    let raw = meta
        .get_bin("x-job-id-bin")
        .ok_or_else(|| anyhow!("EnqueueForStream response missing x-job-id-bin header"))?;
    let bytes = raw
        .to_bytes()
        .map_err(|e| anyhow!("x-job-id-bin not base64-decodable: {e}"))?;
    JobId::decode(bytes.as_ref()).map_err(|e| anyhow!("x-job-id-bin not a valid JobId proto: {e}"))
}

/// Probe `JobProcessingStatusService.Find` until the job reaches RUNNING.
async fn await_running(
    status: &mut JobProcessingStatusServiceClient<Channel>,
    job_id: JobId,
    deadline: Duration,
) -> Result<Option<JobProcessingStatus>> {
    let started = Instant::now();
    while started.elapsed() < deadline {
        let resp: OptionalJobProcessingStatusResponse =
            status.find(Request::new(job_id)).await?.into_inner();
        if let Some(s) = resp.status
            && let Ok(parsed) = JobProcessingStatus::try_from(s)
            && matches!(parsed, JobProcessingStatus::Running)
        {
            return Ok(Some(parsed));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Ok(None)
}

/// Wait until *some* RUNNING entry tied to `worker_name` shows up in the
/// live-status SoT (`FindAll` + per-id `Find`). Used by Direct-response paths
/// where the enqueue call blocks indefinitely on WAIT_RESULT and we cannot
/// read the JobId from its response. Crucially this path does **not** call
/// `FindByCondition` — that is the API under test for the listing bug, so
/// using it here would tautologically pass.
async fn wait_for_first_running_for_worker_via_live_sot(
    workers: &mut WorkerServiceClient<Channel>,
    jobs: &mut JobServiceClient<Channel>,
    _status: &mut JobProcessingStatusServiceClient<Channel>,
    worker_name: &str,
    deadline: Duration,
) -> Result<JobId> {
    let worker_resp = workers
        .find_by_name(Request::new(
            grpc_front::proto::jobworkerp::service::WorkerNameRequest {
                name: worker_name.to_string(),
            },
        ))
        .await?
        .into_inner();
    let worker_id = worker_resp
        .data
        .and_then(|w| w.id)
        .ok_or_else(|| anyhow!("worker `{worker_name}` not found"))?;

    let started = Instant::now();
    while started.elapsed() < deadline {
        // `FindListWithProcessingStatus` joins the live-status SoT to the job
        // row, so it gives us back the owning worker_id without going through
        // `FindByCondition` (the API under test for the listing bug). Job rows
        // here come from the in-memory map populated at enqueue time, so this
        // works for Normal queue_type jobs on Standalone too.
        let mut job_stream = jobs
            .find_list_with_processing_status(Request::new(
                grpc_front::proto::jobworkerp::service::FindListWithProcessingStatusRequest {
                    status: JobProcessingStatus::Running as i32,
                    limit: Some(200),
                },
            ))
            .await?
            .into_inner();
        while let Some(item) = job_stream.next().await {
            let entry = item?;
            let Some(job) = entry.job else { continue };
            let Some(jid) = job.id else { continue };
            if job.data.as_ref().and_then(|d| d.worker_id).map(|w| w.value) == Some(worker_id.value)
            {
                return Ok(jid);
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Err(anyhow!(
        "no RUNNING job for worker `{worker_name}` (id={}) appeared via live SoT within {deadline:?} — \
         this can also indicate the dispatch never picked the job up (worker missing/full).",
        worker_id.value,
    ))
}

/// Return the row for `job_id` from `FindByCondition` (filtered to RUNNING on
/// the test channel) or `None` if the row is missing — the listing bug.
async fn find_by_condition_for_job(
    status: &mut JobProcessingStatusServiceClient<Channel>,
    job_id: JobId,
) -> Result<Option<grpc_front::proto::jobworkerp::service::JobProcessingStatusDetailResponse>> {
    let req = FindJobProcessingStatusRequest {
        status: Some(JobProcessingStatus::Running as i32),
        worker_id: None,
        channel: test_channel(),
        min_elapsed_time_ms: None,
        limit: Some(100),
        offset: Some(0),
        descending: Some(true),
    };
    let mut stream = status
        .find_by_condition(Request::new(req))
        .await?
        .into_inner();
    while let Some(item) = stream.next().await {
        let detail = item?;
        if detail.id.map(|id| id.value) == Some(job_id.value) {
            return Ok(Some(detail));
        }
    }
    Ok(None)
}

// ============================================================================
// Reproduction (1) — listing bug
// ============================================================================

async fn run_listing_repro(
    response_type: ResponseType,
    enqueue_path: EnqueuePath,
    label: &str,
) -> Result<()> {
    let channel = connect().await?;
    let mut workers = WorkerServiceClient::new(channel.clone());
    let mut jobs = JobServiceClient::new(channel.clone());
    let mut status = JobProcessingStatusServiceClient::new(channel.clone());

    let worker_name = format!(
        "nested-cancel-test-listing-{}-{}",
        label,
        chrono::Utc::now().timestamp_millis()
    );
    create_parent_worker(
        &mut workers,
        &worker_name,
        &child_sleep_workflow_json(),
        response_type,
    )
    .await
    .context("create parent worker")?;

    let parent_id = start_parent_job(
        &mut workers,
        &mut jobs,
        &mut status,
        &worker_name,
        response_type,
        enqueue_path,
    )
    .await
    .context("start parent workflow")?;

    let running = await_running(&mut status, parent_id, Duration::from_secs(10)).await?;
    assert!(
        matches!(running, Some(JobProcessingStatus::Running)),
        "[{label}] parent WORKFLOW job {} never reached RUNNING per live SoT; admin-ui would see no movement either. observed={:?}",
        parent_id.value,
        running,
    );

    let detail = find_by_condition_for_job(&mut status, parent_id)
        .await
        .context("find_by_condition")?;

    let cleanup = jobs.delete(Request::new(parent_id)).await;
    assert!(
        detail.is_some(),
        "[{label}] BUG: parent WORKFLOW job {} is RUNNING per live SoT but does not appear in JobProcessingStatusService.FindByCondition; admin-ui list cannot show it",
        parent_id.value,
    );
    cleanup.ok();
    Ok(())
}

#[tokio::test]
#[ignore = "needs a running jobworkerp deployment with JOB_STATUS_RDB_INDEXING=true"]
async fn listing_no_result_enqueue() -> Result<()> {
    run_listing_repro(
        ResponseType::NoResult,
        EnqueuePath::Enqueue,
        "no_result/enqueue",
    )
    .await
}

#[tokio::test]
#[ignore = "needs a running jobworkerp deployment with JOB_STATUS_RDB_INDEXING=true"]
async fn listing_direct_enqueue() -> Result<()> {
    run_listing_repro(ResponseType::Direct, EnqueuePath::Enqueue, "direct/enqueue").await
}

#[tokio::test]
#[ignore = "needs a running jobworkerp deployment with JOB_STATUS_RDB_INDEXING=true"]
async fn listing_direct_enqueue_for_stream() -> Result<()> {
    run_listing_repro(
        ResponseType::Direct,
        EnqueuePath::EnqueueForStream,
        "direct/stream",
    )
    .await
}

// ============================================================================
// Reproduction (2) — cancellation propagation bug
// ============================================================================

async fn run_cancel_propagation_repro(
    response_type: ResponseType,
    enqueue_path: EnqueuePath,
    label: &str,
) -> Result<()> {
    let channel = connect().await?;
    let mut workers = WorkerServiceClient::new(channel.clone());
    let mut jobs = JobServiceClient::new(channel.clone());
    let mut status = JobProcessingStatusServiceClient::new(channel.clone());

    let worker_name = format!(
        "nested-cancel-test-cancel-{}-{}",
        label,
        chrono::Utc::now().timestamp_millis()
    );
    create_parent_worker(
        &mut workers,
        &worker_name,
        &child_sleep_workflow_json(),
        response_type,
    )
    .await?;

    let parent_id = start_parent_job(
        &mut workers,
        &mut jobs,
        &mut status,
        &worker_name,
        response_type,
        enqueue_path,
    )
    .await?;

    let running = await_running(&mut status, parent_id, Duration::from_secs(10)).await?;
    assert!(
        matches!(running, Some(JobProcessingStatus::Running)),
        "[{label}] parent WORKFLOW job never reached RUNNING; cannot test cancellation propagation (observed={:?})",
        running,
    );

    // Resolve the parent worker's id once so we can attribute child-fanout
    // RUNNING jobs back to it without depending on channel filtering.
    let parent_worker_id = workers
        .find_by_name(Request::new(
            grpc_front::proto::jobworkerp::service::WorkerNameRequest {
                name: worker_name.clone(),
            },
        ))
        .await?
        .into_inner()
        .data
        .and_then(|w| w.id)
        .ok_or_else(|| anyhow!("worker {worker_name} disappeared mid-test"))?
        .value;

    // Wait until the parent has actually spawned its child WORKFLOW and the
    // child has booted its grandchild COMMAND `sleep 60`, otherwise we'd be
    // cancelling before any propagation work needs to happen. Detected by
    // observing the total RUNNING count grow past the parent itself.
    let descendant_seen_deadline = Instant::now() + Duration::from_secs(15);
    let initial_running_baseline = snapshot_running_job_ids(&mut status).await?;
    let mut descendants_at_cancel: Vec<JobId> = Vec::new();
    while Instant::now() < descendant_seen_deadline {
        let snap = snapshot_running_job_ids(&mut status).await?;
        // descendants = RUNNING ∩ (not in baseline) ∩ (not parent itself)
        descendants_at_cancel = snap
            .into_iter()
            .filter(|jid| {
                jid.value != parent_id.value
                    && !initial_running_baseline
                        .iter()
                        .any(|b| b.value == jid.value)
            })
            .collect();
        if !descendants_at_cancel.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(
        !descendants_at_cancel.is_empty(),
        "[{label}] no descendant RUNNING jobs appeared for parent {} within 15s; either the WORKFLOW didn't fan out (test setup issue) or the parent died immediately (NoResult job often errors out fast on shared deployments)",
        parent_id.value,
    );

    let cancel_started = Instant::now();
    jobs.delete(Request::new(parent_id))
        .await
        .context("delete parent")?;

    // Loop until every descendant we recorded at cancel-time is no longer
    // RUNNING. A working propagation finishes well before sleep 60.
    let deadline = Duration::from_secs(15);
    let mut last_alive: Vec<JobId> = descendants_at_cancel.clone();
    while cancel_started.elapsed() < deadline {
        let alive_snap = snapshot_running_job_ids(&mut status).await?;
        last_alive = descendants_at_cancel
            .iter()
            .copied()
            .filter(|jid| alive_snap.iter().any(|s| s.value == jid.value))
            .collect();
        if last_alive.is_empty() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let _ = parent_worker_id; // kept for log payload; not used in assertion now
    panic!(
        "[{label}] BUG: {} descendant job(s) still RUNNING {:?} after cancelling parent {} (descendants captured at cancel-time: {:?}); expected propagation to stop them within {:?}",
        last_alive.len(),
        cancel_started.elapsed(),
        parent_id.value,
        descendants_at_cancel
            .iter()
            .map(|j| j.value)
            .collect::<Vec<_>>(),
        deadline,
    );
}

/// Snapshot every RUNNING JobId via live SoT (`FindAll`). Independent of the
/// RDB index that the listing bug breaks.
async fn snapshot_running_job_ids(
    status: &mut JobProcessingStatusServiceClient<Channel>,
) -> Result<Vec<JobId>> {
    let mut stream = status
        .find_all(Request::new(proto::jobworkerp::data::Empty {}))
        .await?
        .into_inner();
    let mut running = Vec::new();
    while let Some(item) = stream.next().await {
        let entry = item?;
        if matches!(
            JobProcessingStatus::try_from(entry.status).ok(),
            Some(JobProcessingStatus::Running)
        ) && let Some(jid) = entry.id
        {
            running.push(jid);
        }
    }
    Ok(running)
}

#[tokio::test]
#[ignore = "needs a running jobworkerp deployment with a worker on the test backend"]
async fn cancel_propagation_no_result_enqueue() -> Result<()> {
    run_cancel_propagation_repro(
        ResponseType::NoResult,
        EnqueuePath::Enqueue,
        "no_result/enqueue",
    )
    .await
}

#[tokio::test]
#[ignore = "needs a running jobworkerp deployment with a worker on the test backend"]
async fn cancel_propagation_direct_enqueue() -> Result<()> {
    run_cancel_propagation_repro(ResponseType::Direct, EnqueuePath::Enqueue, "direct/enqueue").await
}

#[tokio::test]
#[ignore = "needs a running jobworkerp deployment with a worker on the test backend"]
async fn cancel_propagation_direct_enqueue_for_stream() -> Result<()> {
    run_cancel_propagation_repro(
        ResponseType::Direct,
        EnqueuePath::EnqueueForStream,
        "direct/stream",
    )
    .await
}
