//! Internal-worker diagnostic tests for the two production bugs observed with
//! `memories-import → execute_workflow → JobService.EnqueueForStream +
//! ResponseType=DIRECT` against `thread-personality-batch.yaml`:
//!
//! 1. **Listing**: the parent WORKFLOW job is absent from
//!    `JobProcessingStatusService.FindByCondition` even though it is RUNNING
//!    according to live SoT.
//! 2. **Cancel**: cancelling the parent does not stop the child WORKFLOW or
//!    its grandchild COMMAND `sleep` task.
//!
//! The aim of this module is to **measure**, not to assert end-to-end
//! correctness. Each test prints a timeline of:
//!   - JobApp enqueue completion time
//!   - When the row appears in the `job_processing_status` RDB index
//!     (separate queries for `status`, `channel`, `deleted_at`)
//!   - When `find_by_condition(channel=…)` first returns the parent
//!   - dispatcher RUNNING-upsert timing
//!   - cancel fan-out timeline (broadcast, child token cancel, child cleanup)
//!
//! Standalone (memory + sqlite) configuration is used because the bugs
//! reported by users are exclusively against `STORAGE_TYPE=Standalone`.
//!
//! Run with:
//! ```sh
//! cargo test --package tests-with-worker --test app_wrapper -- \
//!     nested_workflow_diagnostics_test --test-threads=1 --nocapture
//! ```
//!
//! These tests are marked `#[ignore]` for now: they take 5–10 seconds each
//! and produce verbose diagnostic output. Remove the `#[ignore]` once the
//! bugs are fixed if the suite is desired in regular runs.

#![allow(clippy::uninlined_format_args)]

use anyhow::{Context, Result};
use app::module::AppModule;
use infra::infra::job::status::JobProcessingStatusRepository;
use infra_utils::infra::test::TEST_RUNTIME;
use jobworkerp_runner::jobworkerp::runner::{
    WorkflowRunArgs, WorkflowRunnerSettings, workflow_runner_settings::WorkflowSource,
};
use prost::Message;
use proto::jobworkerp::data::{
    JobProcessingStatus, Priority, QueueType, ResponseType, RunnerId, StreamingType, WorkerData,
};
use sqlx::Row;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tests_with_worker::start_test_worker;

const WORKFLOW_RUNNER_ID: i64 = 32769;

/// Minimal child workflow: COMMAND `sleep 30`. Long enough for the parent's
/// cancel to be observable, short enough that an orphaned test job doesn't
/// linger forever.
fn child_sleep_workflow_json() -> String {
    r#"{
        "document": {
            "dsl": "1.0.0-jobworkerp",
            "namespace": "diag",
            "name": "diag-child",
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

/// Parent that fans out a single child WORKFLOW. Mirrors the shape of
/// `thread-personality-batch.yaml` with `for/each` + `run.function: WORKFLOW`,
/// but trimmed to one iteration to keep the test focused.
fn parent_workflow_json(child_workflow_json: &str) -> String {
    let child_escaped = serde_json::to_string(child_workflow_json).unwrap();
    format!(
        r#"{{
            "document": {{
                "dsl": "1.0.0-jobworkerp",
                "namespace": "diag",
                "name": "diag-parent",
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

fn parent_runner_settings(child_workflow_json: &str) -> Vec<u8> {
    WorkflowRunnerSettings {
        workflow_source: Some(WorkflowSource::WorkflowData(parent_workflow_json(
            child_workflow_json,
        ))),
        workflow_context: None,
    }
    .encode_to_vec()
}

/// Dump the entire row for the given job_id from the indexed status table.
/// Returns `Ok(None)` if the row does not (yet) exist.
async fn dump_status_row(
    app_module: &AppModule,
    job_id_value: i64,
) -> Result<Option<StatusRowSnapshot>> {
    let rdb = app_module
        .repositories
        .rdb_module
        .as_ref()
        .context("rdb_module must be present in Standalone test app")?;
    let index_repo = rdb
        .rdb_job_processing_status_index_repository
        .as_ref()
        .context("RDB indexing must be enabled (enable_rdb_indexing=true)")?;
    let pool_arc = index_repo.rdb_pool();
    let row = sqlx::query(
        "SELECT job_id, status, worker_id, channel, deleted_at, pending_time, \
                start_time, version, updated_at, is_streamable, broadcast_results \
         FROM job_processing_status WHERE job_id = ?",
    )
    .bind(job_id_value)
    .fetch_optional(pool_arc.as_ref())
    .await?;

    Ok(row.map(|r| StatusRowSnapshot {
        job_id: r.get::<i64, _>("job_id"),
        status: r.get::<i32, _>("status"),
        worker_id: r.get::<i64, _>("worker_id"),
        channel: r.get::<String, _>("channel"),
        deleted_at: r.get::<Option<i64>, _>("deleted_at"),
        pending_time: r.get::<Option<i64>, _>("pending_time"),
        start_time: r.get::<Option<i64>, _>("start_time"),
        version: r.get::<i64, _>("version"),
        updated_at: r.get::<i64, _>("updated_at"),
        is_streamable: r.get::<bool, _>("is_streamable"),
        broadcast_results: r.get::<bool, _>("broadcast_results"),
    }))
}

#[derive(Debug, Clone)]
struct StatusRowSnapshot {
    job_id: i64,
    status: i32,
    worker_id: i64,
    channel: String,
    deleted_at: Option<i64>,
    pending_time: Option<i64>,
    start_time: Option<i64>,
    version: i64,
    updated_at: i64,
    is_streamable: bool,
    broadcast_results: bool,
}

impl StatusRowSnapshot {
    fn pretty(&self) -> String {
        format!(
            "row(jid={}, status={} ({:?}), worker_id={}, channel={:?}, deleted_at={:?}, \
             pending_time={:?}, start_time={:?}, version={}, updated_at={}, is_streamable={}, broadcast_results={})",
            self.job_id,
            self.status,
            JobProcessingStatus::try_from(self.status).ok(),
            self.worker_id,
            self.channel,
            self.deleted_at,
            self.pending_time,
            self.start_time,
            self.version,
            self.updated_at,
            self.is_streamable,
            self.broadcast_results,
        )
    }
}

/// Count rows matched by the same predicate `JobProcessingStatusService.FindByCondition`
/// uses (status + channel + deleted_at IS NULL).
async fn find_by_condition_hits(
    app_module: &AppModule,
    status: JobProcessingStatus,
    channel: Option<&str>,
    job_id_value: i64,
) -> Result<(bool, Vec<i64>)> {
    let rows = app_module
        .job_app
        .find_by_condition(
            Some(status),
            None,
            channel.map(|s| s.to_string()),
            None,
            200,
            0,
            true,
        )
        .await?;
    let ids: Vec<i64> = rows.iter().map(|r| r.job_id.value).collect();
    let hit = ids.contains(&job_id_value);
    Ok((hit, ids))
}

/// Build a Standalone (sqlite + memory) test app with RDB indexing enabled,
/// then start an internal worker dispatcher against it.
async fn build_standalone_app_with_worker()
-> Result<(Arc<AppModule>, tests_with_worker::TestWorkerHandle)> {
    let app_module = Arc::new(app::module::test::create_rdb_chan_test_app(false, true).await?);
    let worker_handle = start_test_worker(app_module.clone()).await?;
    // Give dispatchers a moment to actually start consuming.
    tokio::time::sleep(Duration::from_millis(500)).await;
    Ok((app_module, worker_handle))
}

/// Create a WORKFLOW worker for the parent (channel=Some("workflow"),
/// response_type=Direct, use_static=false — matching how `memories-import →
/// execute_workflow` shapes the worker in production).
async fn create_parent_workflow_worker(
    app_module: &AppModule,
    name: &str,
    child_workflow_json: &str,
    channel: Option<String>,
) -> Result<proto::jobworkerp::data::WorkerId> {
    let wd = WorkerData {
        name: name.to_string(),
        description: "nested workflow diagnostics".to_string(),
        runner_id: Some(RunnerId {
            value: WORKFLOW_RUNNER_ID,
        }),
        runner_settings: parent_runner_settings(child_workflow_json),
        periodic_interval: 0,
        channel,
        queue_type: QueueType::Normal as i32,
        response_type: ResponseType::Direct as i32,
        store_success: false,
        store_failure: true,
        use_static: false,
        retry_policy: None,
        broadcast_results: true,
    };
    app_module.worker_app.create(&wd).await
}

/// Diagnostic 1 — listing visibility.
///
/// Goal: capture **exactly** when the parent WORKFLOW row becomes visible to
/// `find_by_condition`, when it disappears, and what its row state looks
/// like at each transition. No assertions on success/failure — this prints
/// a timeline so we can pick the right fix.
#[test]
#[ignore = "diagnostic: prints a 10s timeline; run only when investigating the listing bug"]
fn diag_listing_visibility() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    TEST_RUNTIME.block_on(async {
        let (app_module, worker_handle) = build_standalone_app_with_worker().await?;
        // Production-mirroring shape: `memories-import → execute_workflow`
        // dispatches against the worker's default channel (the env-configured
        // worker side normally pins this to "workflow", but the rdb_chan test
        // app only spawns dispatchers for `channels: ["test"]` plus the default
        // channel — so leave channel=None to land on the default queue the
        // test dispatcher actually consumes).
        let channel: Option<String> = None;
        let worker_name = format!("diag-parent-{}", chrono::Utc::now().timestamp_millis());
        let parent_worker_id = create_parent_workflow_worker(
            &app_module,
            &worker_name,
            &child_sleep_workflow_json(),
            channel.clone(),
        )
        .await?;
        eprintln!(
            "==> Created parent worker id={}, channel={:?}",
            parent_worker_id.value, channel
        );

        // Build a JobRequest equivalent to JobService.Enqueue with NoResult
        // overrides (simulating both NoResult and Direct shapes — we run with
        // the worker's default Direct here, but the bug is about the index
        // row, which both shapes go through).
        let job_args = WorkflowRunArgs {
            workflow_source: None,
            input: "{}".to_string(),
            ..Default::default()
        }
        .encode_to_vec();

        // `enqueue_job` on a Direct-response worker blocks on WAIT_RESULT
        // until the workflow completes. The child WORKFLOW does a 30 s
        // `sleep`, so the caller's await would idle for ~30 s. Hand it to
        // a background task and resolve the JobId via live SoT instead — we
        // need to observe the row state, not the result payload.
        let t_enqueue_start = Instant::now();
        let app_for_enqueue = app_module.clone();
        let worker_for_enqueue = parent_worker_id;
        let job_args_for_enqueue = job_args.clone();
        let enqueue_task = tokio::spawn(async move {
            let _ = app_for_enqueue
                .job_app
                .enqueue_job(
                    Arc::new(HashMap::new()),
                    Some(&worker_for_enqueue),
                    None,
                    job_args_for_enqueue,
                    None,
                    0,
                    Priority::Medium as i32,
                    300_000,
                    None,
                    StreamingType::None,
                    None,
                    None,
                )
                .await;
        });

        // Resolve parent_id by watching the live SoT for a new RUNNING job
        // owned by `parent_worker_id`. We deliberately do not touch the RDB
        // index here — that's the system under test.
        let mut parent_id_opt: Option<proto::jobworkerp::data::JobId> = None;
        let status_repo_for_resolve = app_module
            .repositories
            .rdb_module
            .as_ref()
            .unwrap()
            .memory_job_processing_status_repository
            .as_ref();
        let resolve_deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < resolve_deadline {
            let all = status_repo_for_resolve.find_status_all().await?;
            if let Some((jid, _)) = all.iter().find(|(_jid, s)| {
                matches!(
                    s,
                    JobProcessingStatus::Pending | JobProcessingStatus::Running
                )
            }) {
                parent_id_opt = Some(*jid);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let parent_id = parent_id_opt
            .ok_or_else(|| anyhow::anyhow!("parent job never reached live SoT — dispatcher absent?"))?;
        let t_enqueue_done = t_enqueue_start.elapsed();
        eprintln!(
            "==> live SoT resolved parent_id={} after {:?}",
            parent_id.value, t_enqueue_done
        );

        // Probe live SoT (memory) for Running, and the RDB index, every 100ms.
        let probe_deadline = Instant::now() + Duration::from_secs(10);
        let status_repo = app_module
            .repositories
            .rdb_module
            .as_ref()
            .unwrap()
            .memory_job_processing_status_repository
            .as_ref();
        let mut first_running_at: Option<Duration> = None;
        let mut first_index_row_at: Option<Duration> = None;
        let mut first_find_by_condition_hit_at: Option<Duration> = None;
        let started = Instant::now();
        while Instant::now() < probe_deadline {
            let dt = started.elapsed();
            let live = status_repo.find_status(&parent_id).await?;
            let row = dump_status_row(&app_module, parent_id.value).await?;
            // Two slices: hit-with-channel-filter (using whatever the worker
            // was created with) and hit-without-filter (any RUNNING). Lets us
            // tell apart a channel-mismatch bug from a row-missing bug.
            let channel_filter: Option<&str> = channel.as_deref();
            let (hit_filtered, ids_filtered) = find_by_condition_hits(
                &app_module,
                JobProcessingStatus::Running,
                channel_filter,
                parent_id.value,
            )
            .await?;
            let (hit_unfiltered, ids_unfiltered) = find_by_condition_hits(
                &app_module,
                JobProcessingStatus::Running,
                None,
                parent_id.value,
            )
            .await?;
            eprintln!(
                "[{:>6}ms] live={:?}, row={}, find_by_condition(filtered_channel={:?})=hit={} ids={:?}, find_by_condition(any_channel)=hit={} ids={:?}",
                dt.as_millis(),
                live,
                row.as_ref().map(|r| r.pretty()).unwrap_or_else(|| "<absent>".to_string()),
                channel,
                hit_filtered,
                ids_filtered,
                hit_unfiltered,
                ids_unfiltered,
            );
            let hit = hit_filtered || hit_unfiltered;

            if first_running_at.is_none() && matches!(live, Some(JobProcessingStatus::Running)) {
                first_running_at = Some(dt);
            }
            if first_index_row_at.is_none() && row.is_some() {
                first_index_row_at = Some(dt);
            }
            if first_find_by_condition_hit_at.is_none() && hit {
                first_find_by_condition_hit_at = Some(dt);
            }
            // Stop early once both have been observed AND a few seconds have passed
            // — we want to see whether the parent later disappears from the index.
            if let (Some(_), Some(_), Some(_)) = (
                first_running_at,
                first_index_row_at,
                first_find_by_condition_hit_at,
            ) && dt >= Duration::from_secs(5)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        eprintln!("==> timeline summary:");
        eprintln!("   first_live_running_at         = {:?}", first_running_at);
        eprintln!("   first_indexed_row_at          = {:?}", first_index_row_at);
        eprintln!("   first_find_by_condition_hit   = {:?}", first_find_by_condition_hit_at);

        // Clean up so the 30s child sleep doesn't linger past the test.
        let _ = app_module.job_app.delete_job(&parent_id).await;
        // Drop the background enqueue task; its result is irrelevant now.
        enqueue_task.abort();
        worker_handle.shutdown().await;
        Ok(())
    })
}

/// Diagnostic 2 — cancel propagation.
///
/// Goal: print a timeline of what happens after `JobService.Delete(parent_id)`
/// — when the parent row flips to Cancelling, when the child row flips, when
/// the grandchild COMMAND `sleep` row disappears. No success/failure
/// assertions: we want to see whether the broadcast is lost, whether the
/// child's runner observes its cancel, or whether `delete_job(child_jid)`
/// is even called by the parent's `WorkflowExecutor::cancel`.
#[test]
#[ignore = "diagnostic: prints a 25s timeline; run only when investigating cancel propagation"]
fn diag_cancel_propagation() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    TEST_RUNTIME.block_on(async {
        let (app_module, worker_handle) = build_standalone_app_with_worker().await?;
        // Default channel (None) so the rdb_chan test dispatcher actually
        // picks up the parent — matches production `memories-import →
        // execute_workflow` shape.
        let channel: Option<String> = None;
        let worker_name = format!("diag-parent-{}", chrono::Utc::now().timestamp_millis());
        let parent_worker_id = create_parent_workflow_worker(
            &app_module,
            &worker_name,
            &child_sleep_workflow_json(),
            channel.clone(),
        )
        .await?;

        let job_args = WorkflowRunArgs {
            workflow_source: None,
            input: "{}".to_string(),
            ..Default::default()
        }
        .encode_to_vec();

        // Spawn the Direct-response enqueue (blocks ~30s waiting on the child
        // sleep) and resolve parent_id via live SoT.
        let app_for_enqueue = app_module.clone();
        let worker_for_enqueue = parent_worker_id;
        let job_args_for_enqueue = job_args.clone();
        let enqueue_task = tokio::spawn(async move {
            let _ = app_for_enqueue
                .job_app
                .enqueue_job(
                    Arc::new(HashMap::new()),
                    Some(&worker_for_enqueue),
                    None,
                    job_args_for_enqueue,
                    None,
                    0,
                    Priority::Medium as i32,
                    300_000,
                    None,
                    StreamingType::None,
                    None,
                    None,
                )
                .await;
        });

        let status_repo_for_resolve = app_module
            .repositories
            .rdb_module
            .as_ref()
            .unwrap()
            .memory_job_processing_status_repository
            .as_ref();
        let mut parent_id_opt: Option<proto::jobworkerp::data::JobId> = None;
        let resolve_deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < resolve_deadline {
            let all = status_repo_for_resolve.find_status_all().await?;
            if let Some((jid, _)) = all.iter().find(|(_jid, s)| {
                matches!(
                    s,
                    JobProcessingStatus::Pending | JobProcessingStatus::Running
                )
            }) {
                parent_id_opt = Some(*jid);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let parent_id = parent_id_opt.ok_or_else(|| {
            anyhow::anyhow!("parent job never reached live SoT — dispatcher absent?")
        })?;
        eprintln!("==> live SoT resolved parent_id={}", parent_id.value);

        // Wait for parent + child + grandchild to all become RUNNING.
        let prep_deadline = Instant::now() + Duration::from_secs(15);
        let mut parent_was_running = false;
        let mut descendants_before_cancel: Vec<i64> = Vec::new();
        let status_repo = app_module
            .repositories
            .rdb_module
            .as_ref()
            .unwrap()
            .memory_job_processing_status_repository
            .as_ref();
        while Instant::now() < prep_deadline {
            let live = status_repo.find_status(&parent_id).await?;
            if matches!(live, Some(JobProcessingStatus::Running)) {
                parent_was_running = true;
            }
            // Get every live RUNNING JobId; treat anything other than parent
            // as a candidate descendant. (On Standalone the test app is the
            // only producer, so there's no noise.)
            let all_running = status_repo.find_status_all().await?;
            descendants_before_cancel = all_running
                .iter()
                .filter(|(jid, s)| {
                    matches!(s, JobProcessingStatus::Running) && jid.value != parent_id.value
                })
                .map(|(jid, _)| jid.value)
                .collect();
            if parent_was_running && !descendants_before_cancel.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        eprintln!(
            "==> pre-cancel snapshot: parent_running={}, descendants={:?}",
            parent_was_running, descendants_before_cancel
        );

        // Let the workflow run for a few extra seconds — the user-reported
        // failure mode kicks in tens of seconds in, so verify the steady
        // state is captured.
        eprintln!("==> dwelling 5s pre-cancel to mimic user-reported timing");
        tokio::time::sleep(Duration::from_secs(5)).await;

        eprintln!("==> calling delete_job(parent_id={})", parent_id.value);
        let t_delete = Instant::now();
        let _ = app_module.job_app.delete_job(&parent_id).await?;

        // Post-cancel: probe every 250ms for 20s, recording:
        //  - parent live_status & indexed row
        //  - each descendant's live_status & indexed row (was the broadcast
        //    received? did its row transition to Cancelling?)
        let post_deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < post_deadline {
            let dt = t_delete.elapsed();
            let parent_live = status_repo.find_status(&parent_id).await?;
            let parent_row = dump_status_row(&app_module, parent_id.value).await?;
            eprintln!(
                "[+{:>6}ms] parent live={:?}, indexed={}",
                dt.as_millis(),
                parent_live,
                parent_row
                    .as_ref()
                    .map(|r| r.pretty())
                    .unwrap_or_else(|| "<absent>".to_string()),
            );
            for jid_val in &descendants_before_cancel {
                let live = status_repo
                    .find_status(&proto::jobworkerp::data::JobId { value: *jid_val })
                    .await?;
                let row = dump_status_row(&app_module, *jid_val).await?;
                eprintln!(
                    "         descendant jid={}: live={:?}, indexed={}",
                    jid_val,
                    live,
                    row.as_ref()
                        .map(|r| r.pretty())
                        .unwrap_or_else(|| "<absent>".to_string()),
                );
            }
            // We always run the full 20s window so the log captures the
            // steady state — useful for distinguishing "broadcast lost" from
            // "broadcast received but child runner ignored it".
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        eprintln!("==> finished cancel diagnostic; cleaning up");
        // Final cleanup: best-effort delete the descendants we found so the
        // 30s sleeps don't outlive the test.
        for jid_val in &descendants_before_cancel {
            let _ = app_module
                .job_app
                .delete_job(&proto::jobworkerp::data::JobId { value: *jid_val })
                .await;
        }
        enqueue_task.abort();
        worker_handle.shutdown().await;
        Ok::<_, anyhow::Error>(())
    })?;
    Ok(())
}
