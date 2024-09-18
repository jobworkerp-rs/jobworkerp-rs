use anyhow::Result;
use async_trait::async_trait;
use command_utils::util::option::ToVec;
use infra::error::JobWorkerError;
use infra::infra::job_result::pubsub::redis::{
    RedisJobResultPubSubRepositoryImpl, UseRedisJobResultPubSubRepository,
};
use infra::infra::job_result::pubsub::JobResultSubscriber;
use infra::infra::job_result::redis::{RedisJobResultRepository, UseRedisJobResultRepository};
use infra::infra::module::redis::{RedisRepositoryModule, UseRedisRepositoryModule};
use proto::jobworkerp::data::{
    JobId, JobResult, JobResultData, JobResultId, ResponseType, ResultStatus, WorkerData, WorkerId,
};
use std::sync::Arc;

use super::super::worker::{UseWorkerApp, WorkerApp};
use super::super::{StorageConfig, UseStorageConfig};
use super::{JobResultApp, JobResultAppHelper};

#[derive(Clone, Debug)]
pub struct RedisJobResultAppImpl {
    storage_config: Arc<StorageConfig>,
    repositories: Arc<RedisRepositoryModule>,
    worker_app: Arc<dyn WorkerApp + 'static>,
}

impl RedisJobResultAppImpl {
    pub fn new(
        storage_config: Arc<StorageConfig>,
        repositories: Arc<RedisRepositoryModule>,
        worker_app: Arc<dyn WorkerApp + 'static>,
    ) -> Self {
        Self {
            storage_config,
            repositories,
            worker_app,
        }
    }
    // XXX same logic as HybridRedisJobResultAppImpl
    async fn subscribe_result_with_check(
        &self,
        job_id: &JobId,
        wdata: &WorkerData,
        timeout: Option<&u64>,
    ) -> Result<JobResult> {
        if wdata.response_type != ResponseType::ListenAfter as i32 {
            Err(JobWorkerError::InvalidParameter(
                "cannot listen job which response_type is None".to_string(),
            )
            .into())
        } else {
            // wait for result data (long polling with grpc (keep connection)))
            self.job_result_pubsub_repository()
                .subscribe_result(job_id, timeout.copied())
                .await
        }
    }
    async fn find_job_result_by_job_id(&self, job_id: &JobId) -> Result<Option<JobResult>>
    where
        Self: Send + 'static,
    {
        match self
            .redis_job_result_repository()
            .find_by_job_id(job_id)
            .await
        {
            Ok(res) => {
                // got temporary redis result data
                if let Some(r) = res {
                    self._fill_job_result(Some(r)).await
                } else {
                    Ok(None)
                }
            }
            Err(e) => Err(e),
        }
    }
}

#[async_trait]
impl JobResultApp for RedisJobResultAppImpl {
    // from job_app
    async fn create_job_result_if_necessary(
        &self,
        id: &JobResultId,
        data: &JobResultData,
    ) -> Result<bool> {
        let in_db = if Self::_should_store(data) {
            self.redis_job_result_repository().create(id, data).await?;
            true
        } else {
            false
        };
        // always store cache for ended job result in success or failure
        //     : cache for listen_after
        if data.status != ResultStatus::ErrorAndRetry as i32
            && data.response_type == ResponseType::ListenAfter as i32
        {
            self.redis_job_result_repository()
                .upsert_only_by_job_id(id, data)
                .await
                .map(|r| r || in_db)
        } else {
            Ok(in_db)
        }
    }

    //XXX not implemented
    async fn delete_job_result(&self, id: &JobResultId) -> Result<bool> {
        self.redis_job_result_repository().delete(id).await
    }

    async fn find_job_result_from_db(&self, id: &JobResultId) -> Result<Option<JobResult>>
    where
        Self: Send + 'static,
    {
        // find from db first if enabled
        match self.redis_job_result_repository().find(id).await {
            Ok(o) => {
                if let Some(r) = o {
                    self._fill_worker_data(r).await.map(Some)
                } else {
                    Ok(None)
                }
            }
            Err(e) => Err(e),
        }
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
        let v = self.redis_job_result_repository().find_all().await?;
        // take limit, offset. if limit is None, take all
        let r = if let Some(l) = limit {
            v.into_iter()
                .skip(*offset.unwrap_or(&0i64) as usize)
                .take(*l as usize)
                .collect::<Vec<JobResult>>()
        } else {
            v.into_iter()
                .skip(*offset.unwrap_or(&0i64) as usize)
                .collect::<Vec<JobResult>>()
        };
        self._fill_worker_data_to_vec(r).await
    }

    /// find job result list by job_id
    /// XXX find only latest result
    async fn find_job_result_list_by_job_id(&self, job_id: &JobId) -> Result<Vec<JobResult>>
    where
        Self: Send + 'static,
    {
        // find from db first if enabled
        let v = self
            .redis_job_result_repository()
            .find_by_job_id(job_id)
            .await;
        match v {
            Ok(v) => self._fill_worker_data_to_vec(v.to_vec()).await,
            Err(e) => {
                tracing::warn!("find_job_result_list error: {:?}", e);
                Err(e)
            }
        }
    }

    // can listen until expired job_id cache in redis or store_success
    // XXX same as hybrid
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

        if wd.response_type != ResponseType::ListenAfter as i32 {
            return Err(JobWorkerError::InvalidParameter(format!(
                "Cannot listen result not stored worker: {:?}",
                &wd
            ))
            .into());
        }

        // check job result (already finished or not)
        let res = self.find_job_result_by_job_id(job_id).await?;
        match res {
            // already finished: return resolved result
            Some(v) if self.is_finished(&v) => Ok(v),
            // result in rdb (not finished by store_failure option)
            Some(_v) => {
                // found not finished result: wait for result data
                self.subscribe_result_with_check(job_id, &wd, Some(&timeout))
                    .await
            }
            None => {
                // not found result: wait for job
                tracing::debug!("job result not found: find job: {:?}", job_id);
                self.subscribe_result_with_check(job_id, &wd, Some(&timeout))
                    .await
            }
        }
    }

    async fn count(&self) -> Result<i64>
    where
        Self: Send + 'static,
    {
        self.redis_job_result_repository().count().await
    }
}
impl UseRedisRepositoryModule for RedisJobResultAppImpl {
    fn redis_repository_module(&self) -> &RedisRepositoryModule {
        &self.repositories
    }
}

impl UseStorageConfig for RedisJobResultAppImpl {
    fn storage_config(&self) -> &StorageConfig {
        &self.storage_config
    }
}

impl UseWorkerApp for RedisJobResultAppImpl {
    fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static> {
        &self.worker_app
    }
}

impl UseRedisJobResultPubSubRepository for RedisJobResultAppImpl {
    fn job_result_pubsub_repository(&self) -> &RedisJobResultPubSubRepositoryImpl {
        &self.repositories.redis_job_result_pubsub_repository
    }
}
impl JobResultAppHelper for RedisJobResultAppImpl {}
