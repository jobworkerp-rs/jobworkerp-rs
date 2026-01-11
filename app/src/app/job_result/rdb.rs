use super::super::worker::{UseWorkerApp, WorkerApp};
use super::super::{StorageConfig, UseStorageConfig};
use super::{JobResultApp, JobResultAppHelper};
use anyhow::Result;
use async_trait::async_trait;
use command_utils::util::datetime;
use futures::stream::BoxStream;
use futures::Stream;
use infra::infra::job::rows::UseJobqueueAndCodec;
use infra::infra::job_result::pubsub::chan::{
    ChanJobResultPubSubRepositoryImpl, UseChanJobResultPubSubRepository,
};
use infra::infra::job_result::pubsub::JobResultSubscriber;
use infra::infra::job_result::rdb::{RdbJobResultRepository, UseRdbJobResultRepository};
use infra::infra::module::rdb::{RdbChanRepositoryModule, UseRdbChanRepositoryModule};
use infra::infra::{IdGeneratorWrapper, UseIdGenerator};
use infra_utils::infra::rdb::UseRdbPool;
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{
    JobId, JobResult, JobResultData, JobResultId, JobResultSortField, ResultOutputItem,
    ResultStatus, Worker, WorkerId,
};
use std::pin::Pin;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct RdbJobResultAppImpl {
    storage_config: Arc<StorageConfig>,
    id_generator: Arc<IdGeneratorWrapper>,
    rdb_chan_repositories: Arc<RdbChanRepositoryModule>,
    worker_app: Arc<dyn WorkerApp + 'static>,
}

impl RdbJobResultAppImpl {
    pub fn new(
        storage_config: Arc<StorageConfig>,
        id_generator: Arc<IdGeneratorWrapper>,
        rdb_repositories: Arc<RdbChanRepositoryModule>,
        worker_app: Arc<dyn WorkerApp + 'static>,
    ) -> Self {
        Self {
            storage_config,
            id_generator,
            rdb_chan_repositories: rdb_repositories,
            worker_app,
        }
    }
    async fn listen_result_stream(
        &self,
        job_id: &JobId,
        worker_id: Option<&WorkerId>,
        worker_name: Option<&String>,
        timeout: Option<u64>,
    ) -> Result<(JobResult, Option<BoxStream<'static, ResultOutputItem>>)>
    where
        Self: Send + 'static,
    {
        // get worker data
        let Worker { id: _wid, data: wd } = self
            .worker_app
            .find_by_id_or_name(worker_id, worker_name)
            .await?;
        let wd = wd.ok_or(JobWorkerError::WorkerNotFound(format!(
            "cannot listen job which worker is None: id={worker_id:?} or name={worker_name:?}"
        )))?;
        if !wd.broadcast_results {
            return Err(JobWorkerError::InvalidParameter(format!(
                "Cannot listen result not broadcast worker: {:?}",
                &wd
            ))
            .into());
        }

        // check job result (already finished or not)
        let res = self
            .rdb_job_result_repository()
            .find_latest_by_job_id(job_id)
            .await?;
        match res {
            // already finished: return resolved result only (no stream)
            Some(v) if self.is_finished(&v) => Ok((v, None)),
            // result in rdb (not finished by store_failure option)
            Some(_v) => {
                // found not finished result: wait for result data
                self.subscribe_result_with_check(job_id, timeout.as_ref(), true)
                    .await
            }
            None => {
                // not found result: wait for job
                tracing::debug!("job result not found: find job: {:?}", job_id);
                self.subscribe_result_with_check(job_id, timeout.as_ref(), true)
                    .await
            }
        }
    }
    // same as hybrid implementation
    async fn subscribe_result_with_check(
        &self,
        job_id: &JobId,
        timeout: Option<&u64>,
        request_streaming: bool,
    ) -> Result<(JobResult, Option<BoxStream<'static, ResultOutputItem>>)> {
        let res = self
            .job_result_pubsub_repository()
            .subscribe_result(job_id, timeout.copied())
            .await?;
        if request_streaming {
            let stream = self
                .job_result_pubsub_repository()
                .subscribe_result_stream(job_id, timeout.copied())
                .await?;
            Ok((res, Some(stream)))
        } else {
            // wait for result data (long polling with grpc (keep connection)))
            Ok((res, None))
        }
    }
}

#[async_trait]
impl JobResultApp for RdbJobResultAppImpl {
    // from job_app
    async fn create_job_result_if_necessary(
        &self,
        id: &JobResultId,
        data: &JobResultData,
        _broadcast_results: bool,
    ) -> Result<bool> {
        // need to record
        if Self::_should_store(data) {
            self.rdb_job_result_repository().create(id, data).await
        } else {
            Ok(false)
        }
    }

    async fn delete_job_result(&self, id: &JobResultId) -> Result<bool> {
        self.rdb_job_result_repository().delete(id).await
    }

    async fn find_job_result_from_db(&self, id: &JobResultId) -> Result<Option<JobResult>>
    where
        Self: Send + 'static,
    {
        // find from db first if enabled
        let found = self.rdb_job_result_repository().find(id).await;
        // fill found.worker_name and found.max_retry from worker
        let v = if let Ok(Some(ref fnd)) = found.as_ref() {
            if let Some(dat) = fnd.data.as_ref() {
                self._fill_worker_data_to_data(dat.clone()).await.map(|d| {
                    Some(JobResult {
                        id: Some(*id),
                        data: Some(d),
                        ..Default::default()
                    })
                })
            } else {
                // unknown (id only?)
                Ok(Some(JobResult {
                    id: Some(*id),
                    data: None,
                    ..Default::default()
                }))
            }
        } else {
            // error or not found
            found
        };
        match v {
            Ok(opt) => Ok(match opt {
                Some(v) => vec![v],
                None => Vec::new(),
            }),
            Err(e) => Err(e),
        }
        .map(|r| r.first().map(|o| (*o).clone()))
    }

    async fn find_job_result_list(
        &self,
        limit: Option<&i32>,
        offset: Option<&i64>,
    ) -> Result<Vec<JobResult>>
    where
        Self: Send + 'static,
    {
        // find from db first if enabled
        let v = self
            .rdb_job_result_repository()
            .find_list(limit, offset)
            .await;
        match v {
            Ok(v) => self._fill_worker_data_to_vec(v).await,
            Err(e) => {
                tracing::warn!("find_job_result_list error: {:?}", e);
                Err(e)
            }
        }
    }

    async fn find_job_result_list_by_job_id(&self, job_id: &JobId) -> Result<Vec<JobResult>>
    where
        Self: Send + 'static,
    {
        // find from db first if enabled
        let v = self
            .rdb_job_result_repository()
            .find_list_by_job_id(job_id)
            .await;
        match v {
            Ok(v) => self._fill_worker_data_to_vec(v).await,
            Err(e) => {
                tracing::warn!("find_job_result_list error: {:?}", e);
                Err(e)
            }
        }
    }

    async fn listen_result(
        &self,
        job_id: &JobId,
        worker_id: Option<&WorkerId>,
        worker_name: Option<&String>,
        timeout: Option<u64>,
        request_streaming: bool,
        _using: &str,
    ) -> Result<(JobResult, Option<BoxStream<'static, ResultOutputItem>>)>
    where
        Self: Send + 'static,
    {
        // get worker data,
        let Worker { id: _, data: wd } = self
            .worker_app
            .find_by_id_or_name(worker_id, worker_name)
            .await?;
        let wd = wd.ok_or(JobWorkerError::WorkerNotFound(format!(
            "cannot listen job which worker is None: id={worker_id:?}"
        )))?;
        if request_streaming {
            tracing::debug!("listen_result_stream: worker_id={:?}", &worker_id);
            // stream
            return self
                .listen_result_stream(job_id, worker_id, worker_name, timeout)
                .await;
        }
        if !wd.broadcast_results {
            return Err(JobWorkerError::InvalidParameter(format!(
                "Cannot listen result not broadcast worker: {:?}",
                &wd
            ))
            .into());
        }
        if !(wd.store_failure && wd.store_success) {
            return Err(JobWorkerError::InvalidParameter(format!(
                "Cannot listen result not stored worker: {:?}",
                &wd
            ))
            .into());
        }
        let start = datetime::now();
        // loop until found (sleep 1sec per loop)
        loop {
            match self
                .rdb_job_result_repository()
                .find_latest_by_job_id(job_id)
                .await
            {
                // retry result: timeout or sleep 3sec
                Ok(Some(v))
                    if v.data
                        .as_ref()
                        .is_some_and(|d| d.status == ResultStatus::ErrorAndRetry as i32) =>
                {
                    // XXX setting?
                    if timeout.is_some_and(|t| {
                        datetime::now()
                            .signed_duration_since(start)
                            .num_milliseconds() as u64
                            > t
                    }) {
                        tracing::info!("listen_result: timeout");
                        return Err(JobWorkerError::TimeoutError(format!(
                            "listen timeout: job_id:{}",
                            &job_id.value
                        ))
                        .into());
                    } else {
                        // XXX fixed adhoc value
                        tracing::debug!("listen_result: In retrying. sleep 1sec");
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                }
                Ok(Some(v)) => {
                    // fill worker data
                    return self._fill_worker_data(v).await.map(|r| (r, None));
                }
                Ok(None) => {
                    if timeout.is_some_and(|t| {
                        datetime::now()
                            .signed_duration_since(start)
                            .num_milliseconds() as u64
                            > t
                    }) {
                        tracing::warn!(
                            "listen_result: timeout: job={}, worker={}, {:?}",
                            &job_id.value,
                            &worker_name.unwrap_or(&"".to_string()),
                            &worker_id
                        );
                        return Err(JobWorkerError::TimeoutError(format!(
                            "listen timeout: job_id:{}",
                            &job_id.value
                        ))
                        .into());
                    } else {
                        tracing::debug!("listen_result: not found. sleep 3sec");
                        // XXX setting?
                        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
                    }
                }
                Err(e) => {
                    tracing::warn!("listen_result error: {:?}", e);
                    return Err(e);
                }
            }
        }
    }

    async fn listen_result_by_job_id(
        &self,
        job_id: &JobId,
        timeout: Option<u64>,
        request_streaming: bool,
    ) -> Result<(JobResult, Option<BoxStream<'static, ResultOutputItem>>)>
    where
        Self: Send + 'static,
    {
        // Check if result already exists (get latest result for this job)
        let res = self
            .find_job_result_list_by_job_id(job_id)
            .await?
            .into_iter()
            .next();
        match res {
            // already finished: return resolved result
            Some(v) if self.is_finished(&v) => Ok((v, None)),
            // result in rdb (not finished by store_failure option)
            Some(v) => {
                // found not finished result: wait for result data
                self.subscribe_result_with_check(
                    job_id,
                    timeout.as_ref(),
                    v.data
                        .map(|r| r.streaming_type != 0)
                        .unwrap_or(request_streaming),
                )
                .await
            }
            None => {
                // not found result: wait for job
                tracing::debug!("job result not found: find job: {:?}", job_id);
                self.subscribe_result_with_check(job_id, timeout.as_ref(), request_streaming)
                    .await
            }
        }
    }

    async fn listen_result_stream_by_worker(
        &self,
        worker_id: Option<&WorkerId>,
        worker_name: Option<&String>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<JobResult>> + Send>>>
    where
        Self: Send + 'static,
    {
        // get worker data
        let Worker { id: wid, data: _wd } = self
            .worker_app
            .find_by_id_or_name(worker_id, worker_name)
            .await?;
        let wid = wid.ok_or(JobWorkerError::WorkerNotFound(format!(
            "cannot listen job which worker is None: id={worker_id:?} or name={worker_name:?}"
        )))?;
        // let wd = wd.ok_or(JobWorkerError::WorkerNotFound(format!(
        //     "cannot listen job which worker is None: id={:?} or name={:?}",
        //     worker_id, worker_name
        // )))?;
        // XXX can listen by worker for direct response
        // if wd.response_type == ResponseType::Direct as i32 {
        //     return Err(JobWorkerError::InvalidParameter(format!(
        //         "Cannot listen by worker result for direct response: {:?}",
        //         &wd
        //     ))
        //     .into());
        // }

        let cn = Self::job_result_by_worker_pubsub_channel_name(&wid);
        tracing::debug!(
            "listen_result_stream_by_worker: worker_id={}, ch={}",
            &wid.value,
            &cn
        );
        self.job_result_pubsub_repository()
            .subscribe_result_stream_by_worker(wid)
            .await
            .map_err(|e| {
                tracing::error!("subscribe chan_err:{:?}", e);
                e
            })
    }

    async fn count(&self) -> Result<i64>
    where
        Self: Send + 'static,
    {
        // TODO cache
        self.rdb_job_result_repository()
            .count_list_tx(self.rdb_job_result_repository().db_pool())
            .await
    }

    // Sprint 4: Advanced filtering and bulk operations

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
        Self: Send + 'static,
    {
        // Call RDB repository (no caching for real-time data)
        let results = self
            .rdb_job_result_repository()
            .find_list_by(
                worker_ids,
                statuses,
                start_time_from,
                start_time_to,
                end_time_from,
                end_time_to,
                priorities,
                uniq_key,
                limit,
                offset,
                sort_by,
                ascending,
            )
            .await?;

        // Fill worker data
        self._fill_worker_data_to_vec(results).await
    }

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
        Self: Send + 'static,
    {
        // Call RDB repository (no caching)
        self.rdb_job_result_repository()
            .count_by(
                worker_ids,
                statuses,
                start_time_from,
                start_time_to,
                end_time_from,
                end_time_to,
                priorities,
                uniq_key,
            )
            .await
    }

    async fn delete_bulk(
        &self,
        end_time_before: Option<i64>,
        statuses: Vec<i32>,
        worker_ids: Vec<i64>,
    ) -> Result<i64>
    where
        Self: Send + 'static,
    {
        // Call RDB repository with safety features
        let deleted_count = self
            .rdb_job_result_repository()
            .delete_bulk(end_time_before, statuses, worker_ids)
            .await?;

        // Note: FindListBy results are not cached, so no cache clearing needed
        // Only individual record cache (FindById) would need clearing
        // See docs/admin/spec-job-result-service.md Section 6.1

        tracing::info!(
            deleted_count = deleted_count,
            "DeleteBulk completed in RdbJobResultApp"
        );

        Ok(deleted_count)
    }
}

impl UseStorageConfig for RdbJobResultAppImpl {
    fn storage_config(&self) -> &StorageConfig {
        &self.storage_config
    }
}
impl UseIdGenerator for RdbJobResultAppImpl {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        &self.id_generator
    }
}

impl UseRdbChanRepositoryModule for RdbJobResultAppImpl {
    fn rdb_repository_module(&self) -> &RdbChanRepositoryModule {
        &self.rdb_chan_repositories
    }
}
impl UseWorkerApp for RdbJobResultAppImpl {
    fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static> {
        &self.worker_app
    }
}
impl UseChanJobResultPubSubRepository for RdbJobResultAppImpl {
    fn job_result_pubsub_repository(&self) -> &ChanJobResultPubSubRepositoryImpl {
        &self.rdb_chan_repositories.chan_job_result_pubsub_repository
    }
}

impl jobworkerp_base::codec::UseProstCodec for RdbJobResultAppImpl {}
impl UseJobqueueAndCodec for RdbJobResultAppImpl {}
impl JobResultAppHelper for RdbJobResultAppImpl {}
