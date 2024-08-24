use anyhow::Result;
use async_trait::async_trait;
use command_utils::util::datetime;
use command_utils::util::option::Exists;
use infra::error::JobWorkerError;
use infra::infra::job_result::rdb::{RdbJobResultRepository, UseRdbJobResultRepository};
use infra::infra::module::rdb::{RdbChanRepositoryModule, UseRdbChanRepositoryModule};
use infra::infra::{IdGeneratorWrapper, UseIdGenerator};
use infra_utils::infra::rdb::UseRdbPool;
use proto::jobworkerp::data::{
    JobId, JobResult, JobResultData, JobResultId, ResultStatus, WorkerId,
};
use std::sync::Arc;

use super::super::worker::{UseWorkerApp, WorkerApp};
use super::super::{StorageConfig, UseStorageConfig};
use super::{JobResultApp, JobResultAppHelper};

#[derive(Clone)]
pub struct RdbJobResultAppImpl {
    storage_config: Arc<StorageConfig>,
    id_generator: Arc<IdGeneratorWrapper>,
    rdb_repositories: Arc<RdbChanRepositoryModule>,
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
            rdb_repositories,
            worker_app,
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
                    })
                })
            } else {
                // unknown (id only?)
                Ok(Some(JobResult {
                    id: Some(*id),
                    data: None,
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
        timeout: u64,
    ) -> Result<JobResult>
    where
        Self: Send + 'static,
    {
        // get worker data
        let wd = self
            .worker_app
            .find_data_by_id_or_name(worker_id, worker_name)
            .await?;
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
                        .exists(|d| d.status == ResultStatus::ErrorAndRetry as i32) =>
                {
                    // XXX setting?
                    if datetime::now()
                        .signed_duration_since(start)
                        .num_milliseconds() as u64
                        > timeout
                    {
                        tracing::info!("listen_result: timeout");
                        return Err(JobWorkerError::TimeoutError(format!(
                            "listen timeout: job_id:{}",
                            &job_id.value
                        ))
                        .into());
                    } else {
                        // XXX fixed adhoc value
                        tracing::debug!("listen_result: In retrying. sleep 3sec");
                        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
                    }
                }
                Ok(Some(v)) => {
                    // fill worker data
                    return self._fill_worker_data(v).await;
                }
                Ok(None) => {
                    if datetime::now()
                        .signed_duration_since(start)
                        .num_milliseconds() as u64
                        > timeout
                    {
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

    async fn count(&self) -> Result<i64>
    where
        Self: Send + 'static,
    {
        // TODO cache
        self.rdb_job_result_repository()
            .count_list_tx(self.rdb_job_result_repository().db_pool())
            .await
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
        &self.rdb_repositories
    }
}
impl UseWorkerApp for RdbJobResultAppImpl {
    fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static> {
        &self.worker_app
    }
}
impl JobResultAppHelper for RdbJobResultAppImpl {}
