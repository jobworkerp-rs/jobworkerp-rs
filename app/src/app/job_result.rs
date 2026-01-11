pub mod hybrid;
pub mod rdb;
// pub mod redis;

use super::worker::UseWorkerApp;
use anyhow::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{
    JobId, JobResult, JobResultData, JobResultId, JobResultSortField, ResultOutputItem,
    ResultStatus, WorkerId,
};
use std::{fmt, pin::Pin, sync::Arc};
use tokio_stream::Stream;

#[async_trait]
pub trait JobResultAppHelper: UseWorkerApp {
    fn _should_store(data: &JobResultData) -> bool {
        data.status == ResultStatus::Success as i32 && data.store_success
            || data.status != ResultStatus::Success as i32 && data.store_failure
    }

    // fill job result data with worker data
    async fn _fill_worker_data_to_vec(&self, v: Vec<JobResult>) -> Result<Vec<JobResult>> {
        let mut rv = Vec::new();
        for it in v {
            // ignore error
            let _ = self._fill_worker_data(it).await.map(|it| rv.push(it));
        }
        Ok(rv)
    }
    #[inline]
    async fn _fill_worker_data_to_data(&self, data: JobResultData) -> Result<JobResultData> {
        let mut d = data;
        if let Ok(Some(w)) = self
            .worker_app()
            .find_data_by_opt(d.worker_id.as_ref())
            .await
        {
            d.worker_name.clone_from(&w.name);
            d.max_retry = w.retry_policy.as_ref().map(|p| p.max_retry).unwrap_or(0);
            d.response_type = w.response_type;
            d.store_success = w.store_success;
            d.store_failure = w.store_failure;
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
                Err(e) => {
                    tracing::warn!("fill_worker_data error: {:?}", e);
                    Err(e)
                } // remove on error
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
