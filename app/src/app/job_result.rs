pub mod hybrid;
pub mod rdb;
// pub mod redis;

use super::job::resolve_job_params;
use super::worker::UseWorkerApp;
use anyhow::Result;
use async_trait::async_trait;
use futures::{StreamExt, stream::BoxStream};
use infra::infra::job::overrides::find_overrides_batch_tx;
use infra::infra::job_result::rdb::{RdbJobResultRepository, UseRdbJobResultRepository};
use infra_utils::infra::rdb::UseRdbPool;
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{
    JobExecutionOverrides, JobId, JobResult, JobResultData, JobResultId, JobResultSortField,
    ResultOutputItem, ResultStatus, WorkerId,
};
use std::collections::HashMap;
use std::{fmt, pin::Pin, sync::Arc};
use tokio_stream::Stream;

/// Reject failed executions before a consumer interprets their output as runner data.
///
/// Failure output is diagnostic text for MCP and must not be decoded with the
/// runner's success schema by FunctionApp or another consumer.
pub fn ensure_successful_job_result(result: &JobResult) -> Result<&JobResultData> {
    let data = result
        .data
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("job completed without result data"))?;
    if data.status() != ResultStatus::Success {
        let diagnostic = data
            .output
            .as_ref()
            .filter(|output| !output.items.is_empty())
            .map(|output| String::from_utf8_lossy(&output.items).into_owned())
            .filter(|message| !message.trim().is_empty())
            .unwrap_or_else(|| format!("job completed with status {:?}", data.status()));
        return Err(anyhow::anyhow!("job execution failed: {diagnostic}"));
    }
    Ok(data)
}

#[async_trait]
pub trait JobResultAppHelper: UseWorkerApp + UseRdbJobResultRepository {
    fn _should_store(data: &JobResultData) -> bool {
        data.status == ResultStatus::Success as i32 && data.store_success
            || data.status != ResultStatus::Success as i32 && data.store_failure
    }

    /// RDB part of [`JobResultApp::replace_job_result`].
    async fn _replace_stored_result(
        &self,
        id: &JobResultId,
        recorded: &JobResultData,
        replacement: &JobResultData,
    ) -> Result<()> {
        let repository = self.rdb_job_result_repository();
        if Self::_should_store(replacement) {
            let mut tx = repository
                .db_pool()
                .begin()
                .await
                .map_err(JobWorkerError::DBError)?;
            let updated = repository.update(&mut tx, id, replacement).await?;
            tx.commit().await.map_err(JobWorkerError::DBError)?;
            // No row when the recorded result was not stored (or was deleted
            // since); the replacement must still be kept.
            if !updated {
                repository.create(id, replacement).await?;
            }
        } else if Self::_should_store(recorded) && repository.delete(id).await? {
            tracing::warn!(
                result_id = id.value,
                job_id = ?replacement.job_id.map(|j| j.value),
                recorded = ?recorded.status(),
                replacement = ?replacement.status(),
                "revoked a stored job result: its replacement is not stored by \
                 the store_success / store_failure settings"
            );
        }
        Ok(())
    }

    // Batch-fill job result data with worker data (avoids N+1 overrides queries).
    // Worker deletion is already tolerated inside `_fill_worker_data_to_data_inner`
    // (returns the stored values as-is on Ok(None)), so any error reaching us
    // here is a real lookup failure (DB / Redis outage) and must surface to the
    // caller rather than degrading silently into a partial success response.
    async fn _fill_worker_data_to_vec(&self, v: Vec<JobResult>) -> Result<Vec<JobResult>> {
        // Batch-fetch overrides for all job_ids to avoid N+1 queries
        let job_ids: Vec<i64> = v
            .iter()
            .filter_map(|r| r.data.as_ref()?.job_id.as_ref().map(|j| j.value))
            .collect();
        let overrides_map =
            find_overrides_batch_tx(self.rdb_job_result_repository().db_pool(), &job_ids).await?;

        let mut rv = Vec::with_capacity(v.len());
        for it in v {
            rv.push(
                self._fill_worker_data_with_overrides(it, &overrides_map)
                    .await?,
            );
        }
        Ok(rv)
    }

    async fn _fill_worker_data_with_overrides(
        &self,
        result: JobResult,
        overrides_map: &HashMap<i64, JobExecutionOverrides>,
    ) -> Result<JobResult> {
        if let Some(d) = result.data {
            let overrides = d
                .job_id
                .as_ref()
                .and_then(|jid| overrides_map.get(&jid.value).cloned());
            match self._fill_worker_data_to_data_inner(d, overrides).await {
                Ok(res) => Ok(JobResult {
                    id: result.id,
                    data: Some(res),
                    metadata: result.metadata,
                }),
                Err(e) => Err(e),
            }
        } else {
            tracing::warn!(
                "fill_worker_data error: no data in job_result: {}",
                proto::log_ext::JobResultSummary(&result)
            );
            Ok(result)
        }
    }

    #[inline]
    async fn _fill_worker_data_to_data(&self, data: JobResultData) -> Result<JobResultData> {
        // Single-item path: fetch overrides individually
        let overrides = if let Some(job_id) = data.job_id.as_ref() {
            self.rdb_job_result_repository()
                .find_overrides_by_job_id(job_id)
                .await?
        } else {
            None
        };
        self._fill_worker_data_to_data_inner(data, overrides).await
    }

    // Tolerate worker deletion: when the referenced worker no longer exists we
    // keep the raw stored values (worker_name etc. as recorded at execution
    // time) so Find/Listen APIs stay readable instead of failing the whole
    // request. Real lookup failures (DB / Redis outages) still propagate.
    #[inline]
    async fn _fill_worker_data_to_data_inner(
        &self,
        data: JobResultData,
        overrides: Option<JobExecutionOverrides>,
    ) -> Result<JobResultData> {
        let mut d = data;
        // Distinguish "worker deleted" (Ok(None)) from a real lookup failure
        // (Err): only the former is tolerated with a warn + raw payload, so
        // DB / Redis outages still surface to the caller.
        let worker = self
            .worker_app()
            .find_data_by_opt(d.worker_id.as_ref())
            .await?;
        if let Some(w) = worker {
            d.worker_name.clone_from(&w.name);

            // Restore resolved values from job_execution_overrides if available;
            // these survive after job deletion (FK removed) and accurately reflect
            // the per-job overrides that were active at enqueue time.
            // NOTE: For Redis-only jobs (QueueType::Normal in Scalable mode), the
            // job_execution_overrides RDB table is not populated. The fallback here
            // uses worker defaults, which is correct because Redis-only job results
            // are served directly from Redis without calling this method.
            // NOTE: response_type, store_success, store_failure, broadcast_results are
            // resolved from the *current* worker config + saved overrides. If the worker
            // config changed after execution, these may differ from execution-time values.
            // This is pre-existing behavior, not introduced by the overrides feature.
            let resolved = resolve_job_params(&w, overrides.as_ref());
            d.response_type = resolved.response_type;
            d.store_success = resolved.store_success;
            d.store_failure = resolved.store_failure;
            d.broadcast_results = resolved.broadcast_results;

            // Prefer resolved_retry_policy (set at execution time with overrides applied);
            // fall back to resolved retry_policy for legacy results without it.
            if d.resolved_retry_policy.is_some() {
                d.max_retry = d
                    .resolved_retry_policy
                    .as_ref()
                    .map(|p| p.max_retry)
                    .unwrap_or(0);
            } else {
                d.max_retry = resolved
                    .retry_policy
                    .as_ref()
                    .map(|p| p.max_retry)
                    .unwrap_or(0);
            }
            tracing::debug!(
                "filled_worker_data: {}",
                proto::log_ext::JobResultDataSummary(&d)
            );
            Ok(d)
        } else {
            tracing::warn!(
                "Worker not found while filling job_result (worker_id={:?}); returning stored data as-is",
                &d.worker_id.as_ref().map(|i| i.value)
            );
            Ok(d)
        }
    }
    async fn _fill_worker_data(&self, result: JobResult) -> Result<JobResult> {
        if let Some(d) = result.data {
            match self._fill_worker_data_to_data(d).await {
                Ok(res) => Ok(JobResult {
                    id: result.id,
                    data: Some(res),
                    metadata: result.metadata,
                }),
                Err(e) => Err(e), // remove on error
            }
        } else {
            tracing::warn!(
                "fill_worker_data error: no data in job_result: {}",
                proto::log_ext::JobResultSummary(&result)
            );
            Ok(result)
        }
    }
    async fn _fill_job_result(&self, res_opt: Option<JobResult>) -> Result<Option<JobResult>>
    where
        Self: Send + 'static,
    {
        tracing::debug!(
            "fill job result: {}",
            proto::log_ext::OptionJobResultSummary(&res_opt)
        );
        // fill found.worker_name and found.max_retry from worker
        if let Some(JobResult {
            id: Some(id),
            data: Some(dat),
            metadata,
        }) = res_opt
        {
            self._fill_worker_data_to_data(dat).await.map(|d| {
                Some(JobResult {
                    id: Some(id),
                    data: Some(d),
                    metadata,
                })
            })
        } else {
            // not found (shouldnot occur)
            Ok(res_opt)
        }
    }
}

/// Await `future` until `deadline`, failing with a `TimeoutError` labelled by
/// `stage`. An already expired deadline fails without starting the work.
pub(crate) async fn with_deadline<T>(
    deadline: Option<tokio::time::Instant>,
    stage: &str,
    future: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    match deadline {
        Some(deadline) if tokio::time::Instant::now() >= deadline => {
            Err(JobWorkerError::TimeoutError(format!("{stage} timeout")).into())
        }
        Some(deadline) => tokio::time::timeout_at(deadline, future)
            .await
            .map_err(|_| JobWorkerError::TimeoutError(format!("{stage} timeout")))?,
        None => future.await,
    }
}

/// Spawn a streaming listener for a job that is about to be enqueued and
/// return once it is subscribed (see `listen_new_job_result_when_subscribed`).
/// Spawning alone does not order the subscription before the enqueue.
pub async fn spawn_subscribed_listener(
    job_result_app: Arc<dyn JobResultApp + 'static>,
    job_id: JobId,
    deadline: Option<tokio::time::Instant>,
) -> Result<ResultListenerHandle> {
    let (subscribed_tx, subscribed_rx) = tokio::sync::oneshot::channel();
    let timeout = deadline.map(|deadline| {
        deadline
            .saturating_duration_since(tokio::time::Instant::now())
            .as_millis()
            .min(u128::from(u64::MAX)) as u64
    });
    // Dropping the handle (readiness failure or caller cancellation) aborts the listener.
    let handle = ResultListenerHandle {
        task: tokio::spawn(async move {
            with_deadline(
                deadline,
                "result listener",
                job_result_app.listen_new_job_result_when_subscribed(
                    &job_id,
                    timeout,
                    subscribed_tx,
                ),
            )
            .await
        }),
        deadline,
    };
    // The listener's own deadline drops the sender, so this wait is bounded too.
    wait_for_listener_readiness(subscribed_rx, deadline).await?;
    Ok(handle)
}

fn listener_readiness_error(deadline: Option<tokio::time::Instant>) -> anyhow::Error {
    if deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
        JobWorkerError::TimeoutError("subscription preparation timeout".into()).into()
    } else {
        anyhow::anyhow!("result listener stopped before subscriptions were ready")
    }
}

async fn wait_for_listener_readiness(
    subscribed: tokio::sync::oneshot::Receiver<()>,
    deadline: Option<tokio::time::Instant>,
) -> Result<()> {
    subscribed
        .await
        .map_err(|_| listener_readiness_error(deadline))
}

async fn signal_ready_after_subscriptions<F, T>(
    result_ready: tokio::sync::oneshot::Receiver<()>,
    stream_subscription: F,
    subscribed: tokio::sync::oneshot::Sender<()>,
) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    let wait_for_result_subscription = async {
        result_ready
            .await
            .map_err(|_| anyhow::anyhow!("result subscription ended before becoming ready"))
    };
    let ((), stream) = tokio::try_join!(wait_for_result_subscription, stream_subscription)?;
    subscribed
        .send(())
        .map_err(|_| anyhow::anyhow!("subscription receiver was dropped"))?;
    Ok(stream)
}

/// Run the result and stream subscriptions together, report `subscribed` once
/// both are active, then keep the stream only for a successful execution.
/// `subscribe_result` must send on the given sender once its subscription is active.
async fn listen_for_new_job<FResult, FStream>(
    subscribe_result: impl FnOnce(tokio::sync::oneshot::Sender<()>) -> FResult,
    stream_subscription: FStream,
    subscribed: tokio::sync::oneshot::Sender<()>,
) -> Result<(JobResult, Option<BoxStream<'static, ResultOutputItem>>)>
where
    FResult: std::future::Future<Output = Result<JobResult>>,
    FStream: std::future::Future<Output = Result<BoxStream<'static, ResultOutputItem>>>,
{
    let (result_ready_tx, result_ready_rx) = tokio::sync::oneshot::channel();
    let (result, stream) = tokio::try_join!(
        subscribe_result(result_ready_tx),
        signal_ready_after_subscriptions(result_ready_rx, stream_subscription, subscribed),
    )?;
    let stream = stream_for_successful_result(&result, Some(stream));
    Ok((result, stream))
}

/// Keep the output stream only when the result confirms successful execution:
/// a failed job may never produce the stream, so waiting on it could hang.
/// A result without data cannot confirm success and is treated the same way.
pub(crate) fn stream_for_successful_result(
    result: &JobResult,
    stream: Option<BoxStream<'static, ResultOutputItem>>,
) -> Option<BoxStream<'static, ResultOutputItem>> {
    stream.filter(|_| is_successful_result(result))
}

fn is_successful_result(result: &JobResult) -> bool {
    result
        .data
        .as_ref()
        .is_some_and(|data| data.status == ResultStatus::Success as i32)
}

/// Owns the streaming result data returned by a listener and applies its deadline.
pub struct PreparedResultStream {
    stream: Option<BoxStream<'static, ResultOutputItem>>,
    deadline: Option<tokio::time::Instant>,
}

impl PreparedResultStream {
    pub async fn next_stream_item(&mut self) -> Result<Option<ResultOutputItem>> {
        let Some(stream) = self.stream.as_mut() else {
            return Ok(None);
        };
        let next = if let Some(deadline) = self.deadline {
            match tokio::time::timeout_at(deadline, stream.next()).await {
                Ok(item) => item,
                Err(_) => {
                    self.stream.take();
                    return Err(JobWorkerError::TimeoutError("stream result timeout".into()).into());
                }
            }
        } else {
            stream.next().await
        };
        Ok(next)
    }
}

pub struct PreparedJobResult {
    pub result: JobResult,
    pub stream: Option<PreparedResultStream>,
}

impl PreparedJobResult {
    pub async fn next_stream_item(&mut self) -> Result<Option<ResultOutputItem>> {
        match self.stream.as_mut() {
            Some(stream) => stream.next_stream_item().await,
            None => Ok(None),
        }
    }
}

pub struct ResultListenerHandle {
    task:
        tokio::task::JoinHandle<Result<(JobResult, Option<BoxStream<'static, ResultOutputItem>>)>>,
    deadline: Option<tokio::time::Instant>,
}

impl ResultListenerHandle {
    #[cfg(test)]
    pub(crate) fn from_result_and_stream_for_test(
        result: JobResult,
        stream: Option<BoxStream<'static, ResultOutputItem>>,
    ) -> Self {
        Self {
            task: tokio::spawn(async move { Ok::<_, anyhow::Error>((result, stream)) }),
            deadline: None,
        }
    }
}

impl std::future::Future for ResultListenerHandle {
    type Output = Result<PreparedJobResult>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        match std::pin::Pin::new(&mut self.task).poll(cx) {
            std::task::Poll::Pending => std::task::Poll::Pending,
            std::task::Poll::Ready(Err(error)) => std::task::Poll::Ready(Err(error.into())),
            std::task::Poll::Ready(Ok(Err(error))) => std::task::Poll::Ready(Err(error)),
            std::task::Poll::Ready(Ok(Ok((result, stream)))) => {
                let stream = stream.map(|stream| PreparedResultStream {
                    stream: Some(stream),
                    deadline: self.deadline,
                });
                std::task::Poll::Ready(Ok(PreparedJobResult { result, stream }))
            }
        }
    }
}

impl Drop for ResultListenerHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[async_trait]
pub trait JobResultApp: fmt::Debug + Send + Sync + 'static {
    // from job_app
    async fn create_job_result_if_necessary(
        &self,
        id: &JobResultId,
        data: &JobResultData,
        broadcast_result: bool,
    ) -> Result<bool>; // record result and create retry job if needed
    // return

    /// Replaces `recorded`, a result already passed to
    /// `create_job_result_if_necessary`, with `replacement`. The RDB row and
    /// the listen_after cache end up as if `replacement` had been recorded
    /// instead: written or deleted by the store_success / store_failure
    /// settings of both results.
    async fn replace_job_result(
        &self,
        id: &JobResultId,
        recorded: &JobResultData,
        replacement: &JobResultData,
    ) -> Result<()>;

    async fn delete_job_result(&self, id: &JobResultId) -> Result<bool>;
    // fill in worker data in vector using _fill_worker_data() iteratively
    async fn find_job_result_from_db(&self, id: &JobResultId) -> Result<Option<JobResult>>
    where
        Self: Send + 'static;

    async fn find_job_result_list(
        &self,
        limit: Option<&i32>,
        offset: Option<&i64>,
    ) -> Result<Vec<JobResult>>
    where
        Self: Send + 'static;

    async fn find_job_result_list_by_job_id(&self, job_id: &JobId) -> Result<Vec<JobResult>>
    where
        Self: Send + 'static;

    async fn listen_result(
        &self,
        job_id: &JobId,
        worker_id: Option<&WorkerId>,
        worker_name: Option<&String>,
        timeout: Option<u64>,
        request_streaming: bool,
        using: &str,
    ) -> Result<(JobResult, Option<BoxStream<'static, ResultOutputItem>>)>
    where
        Self: Send + 'static;

    /// Listen for job result by job_id only (without worker validation).
    ///
    /// This method listens for results by job_id without validating the worker.
    /// It is intended for temporary/ephemeral streaming workers that are not persisted.
    ///
    /// When `request_streaming` is true, the implementation may return a stream alongside
    /// the final `JobResult`, allowing callers to receive intermediate streaming data.
    ///
    /// Callers are responsible for authorization and lifecycle management, including
    /// honoring the optional timeout. The server does not perform worker validation or
    /// persistence for such jobs.
    async fn listen_result_by_job_id(
        &self,
        job_id: &JobId,
        timeout: Option<u64>,
        request_streaming: bool,
    ) -> Result<(JobResult, Option<BoxStream<'static, ResultOutputItem>>)>
    where
        Self: Send + 'static;

    /// Listen for a newly allocated streaming job and send on `subscribed` once its
    /// result and stream subscriptions are active, so the caller can enqueue the job
    /// without losing a fast result. Existing-result lookup is skipped because the
    /// job does not exist yet.
    async fn listen_new_job_result_when_subscribed(
        &self,
        job_id: &JobId,
        timeout: Option<u64>,
        subscribed: tokio::sync::oneshot::Sender<()>,
    ) -> Result<(JobResult, Option<BoxStream<'static, ResultOutputItem>>)>
    where
        Self: Send + 'static;

    /// Subscribe to the streaming data for a job without waiting for final result.
    ///
    /// Unlike `listen_result_by_job_id`, this returns the stream immediately
    /// so callers can process intermediate data in real-time (e.g., nested
    /// workflow progress tracking).
    async fn subscribe_stream_by_job_id(
        &self,
        job_id: &JobId,
        timeout: Option<u64>,
    ) -> Result<Option<BoxStream<'static, ResultOutputItem>>>
    where
        Self: Send + 'static;

    async fn listen_result_stream_by_worker(
        &self,
        worker_id: Option<&WorkerId>,
        worker_name: Option<&String>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<JobResult>> + Send>>>
    where
        Self: Send + 'static;

    async fn count(&self) -> Result<i64>
    where
        Self: Send + 'static;

    fn is_finished(&self, result: &JobResult) -> bool {
        result
            .data
            .as_ref()
            .map(|d| d.status != ResultStatus::ErrorAndRetry as i32)
            .unwrap_or(false)
    }

    // Sprint 4: Advanced filtering and bulk operations

    /// Find job results by multiple filter conditions with pagination and sorting
    ///
    /// # Arguments
    /// - `worker_ids`: Filter by worker IDs
    /// - `statuses`: Filter by result statuses
    /// - `start_time_from`: Filter by start time (from timestamp in milliseconds)
    /// - `start_time_to`: Filter by start time (to timestamp in milliseconds)
    /// - `end_time_from`: Filter by end time (from timestamp in milliseconds)
    /// - `end_time_to`: Filter by end time (to timestamp in milliseconds)
    /// - `priorities`: Filter by priorities
    /// - `uniq_key`: Filter by unique key (exact match)
    /// - `limit`: Maximum number of results (default: 100, max: 1000)
    /// - `offset`: Number of results to skip (max: 10000)
    /// - `sort_by`: Sort field (default: END_TIME)
    /// - `ascending`: Sort order (default: false for DESC)
    #[allow(clippy::too_many_arguments)]
    async fn find_list_by(
        &self,
        worker_ids: Vec<i64>,
        statuses: Vec<i32>,
        start_time_from: Option<i64>,
        start_time_to: Option<i64>,
        end_time_from: Option<i64>,
        end_time_to: Option<i64>,
        priorities: Vec<i32>,
        uniq_key: Option<String>,
        limit: Option<i32>,
        offset: Option<i64>,
        sort_by: Option<JobResultSortField>,
        ascending: Option<bool>,
    ) -> Result<Vec<JobResult>>
    where
        Self: Send + 'static;

    /// Count job results by filter conditions
    ///
    /// # Arguments
    /// Same filters as `find_list_by` (without pagination and sorting)
    #[allow(clippy::too_many_arguments)]
    async fn count_by(
        &self,
        worker_ids: Vec<i64>,
        statuses: Vec<i32>,
        start_time_from: Option<i64>,
        start_time_to: Option<i64>,
        end_time_from: Option<i64>,
        end_time_to: Option<i64>,
        priorities: Vec<i32>,
        uniq_key: Option<String>,
    ) -> Result<i64>
    where
        Self: Send + 'static;

    /// Bulk delete job results with safety features
    ///
    /// # Safety Features (Defense in Depth)
    /// 1. **Required filter condition**: At least one of end_time_before, statuses, or worker_ids must be specified
    /// 2. **Recent data protection**: Cannot delete results within last 24 hours
    /// 3. **Transaction timeout**: 30-second limit to prevent long-running locks
    ///
    /// # Arguments
    /// - `end_time_before`: Delete results older than this timestamp (Unix time in milliseconds)
    /// - `statuses`: Delete results with these statuses
    /// - `worker_ids`: Delete results from these workers
    ///
    /// # Returns
    /// Number of deleted records
    async fn delete_bulk(
        &self,
        end_time_before: Option<i64>,
        statuses: Vec<i32>,
        worker_ids: Vec<i64>,
    ) -> Result<i64>
    where
        Self: Send + 'static;
}

pub trait UseJobResultApp {
    fn job_result_app(&self) -> &Arc<dyn JobResultApp + 'static>;
}

#[cfg(test)]
mod subscribed_listener_tests {
    use super::{
        PreparedResultStream, ensure_successful_job_result, signal_ready_after_subscriptions,
        spawn_subscribed_listener,
    };
    use crate::module::AppModule;
    use crate::module::test::{create_hybrid_test_app, create_rdb_chan_test_app};
    use anyhow::Result;
    use futures::StreamExt;
    use infra_utils::infra::test::TEST_RUNTIME;
    use proto::jobworkerp::data::{
        JobId, JobResult, JobResultData, JobResultId, ResponseType, ResultOutputItem, ResultStatus,
        Trailer, WorkerId, result_output_item,
    };
    use std::time::Duration;

    /// Signals when the owning future or task is dropped.
    struct NotifyOnDrop(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for NotifyOnDrop {
        fn drop(&mut self) {
            if let Some(tx) = self.0.take() {
                let _ = tx.send(());
            }
        }
    }

    fn unique_id() -> i64 {
        rand::random::<i64>().abs()
    }

    #[test]
    fn failed_result_returns_runner_diagnostic_without_decoding_it() {
        let result = JobResult {
            data: Some(JobResultData {
                status: ResultStatus::FatalError as i32,
                output: Some(proto::jobworkerp::data::ResultOutput {
                    items: b"original failure".to_vec(),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };

        let error = ensure_successful_job_result(&result).unwrap_err();
        assert!(error.to_string().contains("job execution failed"));
        assert!(error.to_string().contains("original failure"));
    }

    #[test]
    fn failed_result_diagnostics_preserve_mcp_fallback_and_lossy_utf8_rules() {
        let cases = [
            (None, "job completed with status FatalError"),
            (Some(Vec::new()), "job completed with status FatalError"),
            (
                Some(b" \t\n".to_vec()),
                "job completed with status FatalError",
            ),
            (Some(vec![0xff, 0xfe]), "��"),
        ];

        for (output_items, expected_diagnostic) in cases {
            let result = JobResult {
                data: Some(JobResultData {
                    status: ResultStatus::FatalError as i32,
                    output: output_items
                        .map(|items| proto::jobworkerp::data::ResultOutput { items }),
                    ..Default::default()
                }),
                ..Default::default()
            };

            let error = ensure_successful_job_result(&result).unwrap_err();
            assert!(
                error.to_string().contains(expected_diagnostic),
                "unexpected diagnostic: {error}"
            );
        }
    }

    #[test]
    fn missing_result_data_returns_mcp_compatible_diagnostic() {
        let error = ensure_successful_job_result(&JobResult::default()).unwrap_err();
        assert_eq!(error.to_string(), "job completed without result data");
    }

    #[test]
    fn successful_result_data_passes_validation() {
        let result = JobResult {
            data: Some(JobResultData {
                status: ResultStatus::Success as i32,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(ensure_successful_job_result(&result).is_ok());
    }

    // A job enqueued only after the listener reports `subscribed` must be
    // observed (result and stream) even if it completes immediately.
    async fn receives_job_completed_right_after_subscribed(app: &AppModule) -> Result<()> {
        let job_id = JobId { value: unique_id() };
        let listener = spawn_subscribed_listener(
            app.job_result_app.clone(),
            job_id,
            Some(tokio::time::Instant::now() + Duration::from_secs(5)),
        )
        .await?;

        let result_id = JobResultId { value: unique_id() };
        let data = JobResultData {
            job_id: Some(job_id),
            worker_id: Some(WorkerId { value: 1 }),
            status: ResultStatus::Success as i32,
            response_type: ResponseType::NoResult as i32,
            broadcast_results: true,
            ..Default::default()
        };
        let items = vec![
            ResultOutputItem {
                item: Some(result_output_item::Item::Data(b"chunk".to_vec())),
            },
            ResultOutputItem {
                item: Some(result_output_item::Item::End(Trailer::default())),
            },
        ];
        app.job_app
            .complete_job(
                &result_id,
                &data,
                Some(futures::stream::iter(items).boxed()),
            )
            .await?;

        let mut result = tokio::time::timeout(Duration::from_secs(5), listener).await??;
        assert_eq!(result.result.id, Some(result_id));
        let mut received = Vec::new();
        while let Some(item) = result.next_stream_item().await? {
            received.push(item);
        }
        assert!(matches!(
            received.first().and_then(|i| i.item.as_ref()),
            Some(result_output_item::Item::Data(d)) if d == b"chunk"
        ));
        Ok(())
    }

    async fn failed_job_does_not_wait_for_stream(app: &AppModule) -> Result<()> {
        let job_id = JobId { value: unique_id() };
        let listener = spawn_subscribed_listener(app.job_result_app.clone(), job_id, None).await?;
        let result_id = JobResultId { value: unique_id() };
        let data = JobResultData {
            job_id: Some(job_id),
            worker_id: Some(WorkerId { value: 1 }),
            status: ResultStatus::FatalError as i32,
            response_type: ResponseType::NoResult as i32,
            broadcast_results: true,
            ..Default::default()
        };
        app.job_app.complete_job(&result_id, &data, None).await?;

        let result = tokio::time::timeout(Duration::from_secs(1), listener).await??;
        assert_eq!(result.result.id, Some(result_id));
        assert!(result.stream.is_none());
        Ok(())
    }

    #[test]
    fn hybrid_failed_result_does_not_wait_for_stream() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            failed_job_does_not_wait_for_stream(&create_hybrid_test_app().await?).await
        })
    }

    #[test]
    fn standalone_failed_result_does_not_wait_for_stream() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            failed_job_does_not_wait_for_stream(&create_rdb_chan_test_app(true, false).await?).await
        })
    }

    #[test]
    fn hybrid_listener_receives_job_completed_right_after_subscribed() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            receives_job_completed_right_after_subscribed(&create_hybrid_test_app().await?).await
        })
    }

    #[test]
    fn standalone_listener_receives_job_completed_right_after_subscribed() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            receives_job_completed_right_after_subscribed(
                &create_rdb_chan_test_app(true, false).await?,
            )
            .await
        })
    }

    #[test]
    fn prepared_stream_expires_at_shared_deadline_and_releases_stream() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let mut stream = PreparedResultStream {
                stream: Some(futures::stream::pending().boxed()),
                deadline: Some(tokio::time::Instant::now()),
            };

            assert!(stream.next_stream_item().await.is_err());
            assert!(stream.stream.is_none());
            assert!(stream.next_stream_item().await?.is_none());
            Ok(())
        })
    }

    #[test]
    fn dropping_result_listener_handle_aborts_listener_task() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(async move {
                let _notify = NotifyOnDrop(Some(dropped_tx));
                let _ = started_tx.send(());
                std::future::pending::<()>().await;
                Ok::<_, anyhow::Error>((proto::jobworkerp::data::JobResult::default(), None))
            });
            started_rx.await?;
            drop(super::ResultListenerHandle {
                task,
                deadline: None,
            });
            tokio::time::timeout(Duration::from_secs(1), dropped_rx).await??;
            Ok(())
        })
    }

    #[test]
    fn dropped_readiness_receiver_fails_prepared_listener() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let app = create_hybrid_test_app().await?;
            let job_id = JobId { value: unique_id() };
            let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
            drop(ready_rx);

            let result = tokio::time::timeout(
                Duration::from_secs(2),
                app.job_result_app.listen_new_job_result_when_subscribed(
                    &job_id,
                    Some(2_000),
                    ready_tx,
                ),
            )
            .await?;
            assert!(result.is_err());
            Ok(())
        })
    }

    #[test]
    fn listener_deadline_returns_the_standard_timeout_error() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let app = create_hybrid_test_app().await?;
            let handle = spawn_subscribed_listener(
                app.job_result_app.clone(),
                JobId { value: unique_id() },
                Some(tokio::time::Instant::now() + Duration::from_millis(50)),
            )
            .await?;
            let error = match tokio::time::timeout(Duration::from_secs(2), handle).await? {
                Err(error) => error,
                Ok(_) => anyhow::bail!("listener unexpectedly received a result"),
            };

            assert!(matches!(
                error.downcast_ref::<jobworkerp_base::error::JobWorkerError>(),
                Some(jobworkerp_base::error::JobWorkerError::TimeoutError(_))
            ));
            Ok(())
        })
    }

    #[test]
    fn dropped_listener_readiness_uses_timeout_only_after_deadline() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
            drop(ready_tx);
            let expired = super::wait_for_listener_readiness(
                ready_rx,
                Some(tokio::time::Instant::now() - Duration::from_secs(1)),
            )
            .await
            .unwrap_err();
            assert!(matches!(
                expired.downcast_ref::<jobworkerp_base::error::JobWorkerError>(),
                Some(jobworkerp_base::error::JobWorkerError::TimeoutError(_))
            ));

            let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
            drop(ready_tx);
            let early = super::wait_for_listener_readiness(
                ready_rx,
                Some(tokio::time::Instant::now() + Duration::from_secs(1)),
            )
            .await
            .unwrap_err();
            assert!(
                early
                    .downcast_ref::<jobworkerp_base::error::JobWorkerError>()
                    .is_none()
            );
            Ok(())
        })
    }

    #[test]
    fn failed_stream_subscription_does_not_report_ready() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let (result_ready_tx, result_ready_rx) = tokio::sync::oneshot::channel();
            let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
            result_ready_tx.send(()).unwrap();

            let result = signal_ready_after_subscriptions(
                result_ready_rx,
                async { Err::<(), _>(anyhow::anyhow!("simulated stream subscribe failure")) },
                ready_tx,
            )
            .await;
            assert!(result.is_err());
            assert!(ready_rx.await.is_err());
            Ok(())
        })
    }

    #[test]
    fn failed_result_subscription_does_not_report_ready_or_keep_stream_task() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let (result_ready_tx, result_ready_rx) = tokio::sync::oneshot::channel();
            drop(result_ready_tx);
            let (stream_dropped_tx, stream_dropped_rx) = tokio::sync::oneshot::channel();
            let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
            let notify_on_drop = NotifyOnDrop(Some(stream_dropped_tx));
            let stream_subscription = async move {
                let _notify = notify_on_drop;
                std::future::pending::<Result<()>>().await
            };

            let result =
                signal_ready_after_subscriptions(result_ready_rx, stream_subscription, ready_tx)
                    .await;
            assert!(result.is_err());
            assert!(ready_rx.await.is_err());
            tokio::time::timeout(Duration::from_secs(1), stream_dropped_rx).await??;
            Ok(())
        })
    }
}

#[cfg(test)]
mod replace_job_result_tests {
    use crate::module::AppModule;
    use crate::module::test::{create_hybrid_test_app, create_rdb_chan_test_app};
    use anyhow::Result;
    use infra::infra::job_result::redis::RedisJobResultRepository;
    use infra_utils::infra::test::TEST_RUNTIME;
    use proto::jobworkerp::data::{
        JobId, JobResultData, JobResultId, ResponseType, ResultOutput, ResultStatus,
        StreamErrorCode, StreamingType, WorkerId,
    };

    fn recorded(store_success: bool, store_failure: bool) -> JobResultData {
        JobResultData {
            job_id: Some(JobId {
                value: rand::random::<i64>().abs(),
            }),
            worker_id: Some(WorkerId { value: 1 }),
            status: ResultStatus::Success as i32,
            output: Some(ResultOutput { items: vec![] }),
            streaming_type: StreamingType::Response as i32,
            response_type: ResponseType::Direct as i32,
            store_success,
            store_failure,
            ..Default::default()
        }
    }

    fn failure_of(recorded: &JobResultData) -> JobResultData {
        let error = proto::stream_error::classified_error(
            StreamErrorCode::ExecutionFailed,
            "exit 3",
            "COMMAND",
        );
        JobResultData {
            status: ResultStatus::MaxRetry as i32,
            output: Some(ResultOutput {
                items: proto::stream_error::stream_failure_output(&error).into_bytes(),
            }),
            error: Some(error),
            ..recorded.clone()
        }
    }

    /// Records `before` as the worker does, replaces it with its failure and
    /// returns the replacement with what the RDB holds afterwards.
    async fn replace_with_failure(
        app: &AppModule,
        before: JobResultData,
    ) -> Result<(JobResultData, Option<JobResultData>)> {
        let id = JobResultId {
            value: rand::random::<i64>().abs(),
        };
        app.job_result_app
            .create_job_result_if_necessary(&id, &before, before.broadcast_results)
            .await?;
        let after = failure_of(&before);
        app.job_result_app
            .replace_job_result(&id, &before, &after)
            .await?;
        let stored = app
            .job_result_app
            .find_job_result_from_db(&id)
            .await?
            .and_then(|result| result.data);
        Ok((after, stored))
    }

    async fn rdb_row_follows_store_settings(app: &AppModule) -> Result<()> {
        // stored success -> updated to the failure
        let (after, stored) = replace_with_failure(app, recorded(true, true)).await?;
        let stored = stored.expect("the failure must stay stored");
        assert_eq!(stored.status, ResultStatus::MaxRetry as i32);
        assert_eq!(stored.output, after.output);

        // success was not stored -> the failure is inserted
        let (_, stored) = replace_with_failure(app, recorded(false, true)).await?;
        assert_eq!(
            stored.map(|data| data.status),
            Some(ResultStatus::MaxRetry as i32)
        );

        // stored success, failures are not stored -> the success is revoked
        let (_, stored) = replace_with_failure(app, recorded(true, false)).await?;
        assert_eq!(stored, None);

        // nothing stored before or after
        let (_, stored) = replace_with_failure(app, recorded(false, false)).await?;
        assert_eq!(stored, None);
        Ok(())
    }

    #[test]
    fn rdb_chan_replaces_the_rdb_row_by_store_settings() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let app = create_rdb_chan_test_app(true, false).await?;
            rdb_row_follows_store_settings(&app).await
        })
    }

    #[test]
    fn hybrid_replaces_the_rdb_row_by_store_settings() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let app = create_hybrid_test_app().await?;
            rdb_row_follows_store_settings(&app).await
        })
    }

    #[test]
    fn hybrid_replaces_the_listen_after_cache() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let app = create_hybrid_test_app().await?;
            let before = JobResultData {
                broadcast_results: true,
                ..recorded(false, false)
            };
            let job_id = before.job_id.unwrap();
            let (after, _) = replace_with_failure(&app, before).await?;

            let cached = app
                .repositories
                .redis_module
                .as_ref()
                .expect("hybrid app has redis")
                .redis_job_result_repository
                .find_by_job_id(&job_id)
                .await?
                .and_then(|result| result.data)
                .expect("ended results are cached for listen_after");
            assert_eq!(cached.status, ResultStatus::MaxRetry as i32);
            assert_eq!(cached.error, after.error);
            assert_eq!(cached.output, after.output);
            Ok(())
        })
    }
}
