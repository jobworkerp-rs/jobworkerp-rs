pub mod hybrid;
pub mod rdb;
// pub mod redis;

use super::job::resolve_job_params;
use super::worker::UseWorkerApp;
use anyhow::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
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

#[async_trait]
pub trait JobResultAppHelper: UseWorkerApp + UseRdbJobResultRepository {
    fn _should_store(data: &JobResultData) -> bool {
        data.status == ResultStatus::Success as i32 && data.store_success
            || data.status != ResultStatus::Success as i32 && data.store_failure
    }

    // Batch-fill job result data with worker data (avoids N+1 overrides queries)
    async fn _fill_worker_data_to_vec(&self, v: Vec<JobResult>) -> Result<Vec<JobResult>> {
        // Batch-fetch overrides for all job_ids to avoid N+1 queries
        let job_ids: Vec<i64> = v
            .iter()
            .filter_map(|r| r.data.as_ref()?.job_id.as_ref().map(|j| j.value))
            .collect();
        let overrides_map =
            find_overrides_batch_tx(self.rdb_job_result_repository().db_pool(), &job_ids)
                .await
                .unwrap_or_default();

        let mut rv = Vec::new();
        for it in v {
            match self
                ._fill_worker_data_with_overrides(it, &overrides_map)
                .await
            {
                Ok(filled) => rv.push(filled),
                Err(e) => {
                    tracing::warn!("Skipping job_result in batch fill: {:?}", e);
                }
            }
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
                "fill_worker_data error: no data in job_result: {:?}",
                result
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
                .await
                .unwrap_or(None)
        } else {
            None
        };
        self._fill_worker_data_to_data_inner(data, overrides).await
    }

    #[inline]
    async fn _fill_worker_data_to_data_inner(
        &self,
        data: JobResultData,
        overrides: Option<JobExecutionOverrides>,
    ) -> Result<JobResultData> {
        let mut d = data;
        if let Ok(Some(w)) = self
            .worker_app()
            .find_data_by_opt(d.worker_id.as_ref())
            .await
        {
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
            tracing::debug!("filled_worker_data: {:?}", &d);
            Ok(d)
        } else {
            Err(JobWorkerError::WorkerNotFound(format!(
                "worker not found for worker_id: {:?}",
                &d.worker_id.as_ref().map(|i| i.value)
            ))
            .into())
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
                "fill_worker_data error: no data in job_result: {:?}",
                result
            );
            Ok(result)
        }
    }
    async fn _fill_job_result(&self, res_opt: Option<JobResult>) -> Result<Option<JobResult>>
    where
        Self: Send + 'static,
    {
        tracing::debug!("fill job result: {:?}", res_opt);
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

    /// Listen for job result by job_id only (without worker validation)
    ///
    /// This method listens for results by job_id without validating the worker.
    /// It is intended for temporary/ephemeral streaming workers that are not persisted.
    ///
    /// When `request_streaming` is true, the implementation may return a stream alongside
    /// the final `JobResult`, allowing callers to receive intermediate streaming data.
    ///
    /// NOTE: Callers are responsible for authorization and lifecycle management,
    /// including honoring the optional timeout. The server does not perform worker
    /// validation or persistence for such jobs.
    async fn listen_result_by_job_id(
        &self,
        job_id: &JobId,
        timeout: Option<u64>,
        request_streaming: bool,
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
