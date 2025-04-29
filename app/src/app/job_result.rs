pub mod hybrid;
pub mod rdb;
// pub mod redis;

use super::worker::UseWorkerApp;
use anyhow::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{
    JobId, JobResult, JobResultData, JobResultId, ResultOutputItem, ResultStatus, WorkerId,
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
        }) = res_opt
        {
            self._fill_worker_data_to_data(dat).await.map(|d| {
                Some(JobResult {
                    id: Some(id),
                    data: Some(d),
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
}

pub trait UseJobResultApp {
    fn job_result_app(&self) -> &Arc<dyn JobResultApp + 'static>;
}
