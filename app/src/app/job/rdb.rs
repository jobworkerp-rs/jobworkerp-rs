use crate::app::job_result::JobResultApp;
use crate::app::worker::{UseWorkerApp, WorkerApp};

use super::{JobApp, JobBuilder, JobCacheKeys, StorageConfig, UseStorageConfig};
use anyhow::Result;
use async_trait::async_trait;
use command_utils::util::datetime;
use infra::error::JobWorkerError;
use infra::infra::job::rdb::{RdbJobRepository, UseRdbJobRepository};
use infra::infra::module::rdb::{RdbRepositoryModule, UseRdbRepositoryModule};
use infra::infra::{IdGeneratorWrapper, JobQueueConfig, UseIdGenerator, UseJobQueueConfig};
use infra_utils::infra::memory::UseMemoryCache;
use infra_utils::infra::rdb::UseRdbPool;
use proto::jobworkerp::data::{
    Job, JobData, JobId, JobResult, JobResultData, JobResultId, JobStatus, QueueType, ResponseType,
    Worker, WorkerId,
};
use std::{sync::Arc, time::Duration};
use stretto::AsyncCache;

pub struct RdbJobAppImpl {
    job_queue_config: Arc<JobQueueConfig>,
    storage_config: Arc<StorageConfig>,
    id_generator: Arc<IdGeneratorWrapper>,
    repositories: Arc<RdbRepositoryModule>,
    worker_app: Arc<dyn WorkerApp + 'static>,
    job_result_app: Arc<dyn JobResultApp + 'static>,
    memory_cache: AsyncCache<Arc<String>, Vec<Job>>,
}

impl RdbJobAppImpl {
    pub fn new(
        job_queue_config: Arc<JobQueueConfig>,
        storage_config: Arc<StorageConfig>,
        id_generator: Arc<IdGeneratorWrapper>,
        repositories: Arc<RdbRepositoryModule>,
        worker_app: Arc<dyn WorkerApp + 'static>,
        job_result_app: Arc<dyn JobResultApp + 'static>,
        memory_cache: AsyncCache<Arc<String>, Vec<Job>>,
    ) -> Self {
        Self {
            job_queue_config,
            storage_config,
            id_generator,
            repositories,
            worker_app,
            job_result_app,
            memory_cache,
        }
    }
}

// TODO not used
#[async_trait]
impl JobApp for RdbJobAppImpl {
    async fn enqueue_job(
        &self,
        worker_id: Option<&WorkerId>,
        worker_name: Option<&String>,
        arg: Vec<u8>,
        uniq_key: Option<String>,
        run_after_time: i64,
        priority: i32,
        timeout: u64,
    ) -> Result<(JobId, Option<JobResult>)> {
        let worker_res = if let Some(id) = worker_id {
            self.worker_app().find(id).await?
        } else if let Some(name) = worker_name {
            self.worker_app().find_by_name(name).await?
        } else {
            return Err(JobWorkerError::WorkerNotFound(
                "worker_id or worker_name is required".to_string(),
            )
            .into());
        };

        if let Some(Worker {
            id: Some(wid),
            data: Some(wd),
        }) = worker_res
        {
            let id = self.id_generator().generate_id()?;
            let jid = JobId { value: id };

            let job = Job {
                id: Some(jid.clone()),
                data: Some(JobData {
                    worker_id: Some(wid.clone()),
                    arg,
                    uniq_key,
                    enqueue_time: datetime::now_millis(),
                    grabbed_until_time: None,
                    run_after_time: if run_after_time == 0 {
                        datetime::now_millis() // set now millis
                    } else {
                        run_after_time
                    },
                    retried: 0u32,
                    priority,
                    timeout,
                }),
            };
            if wd.queue_type != QueueType::Rdb as i32 {
                tracing::warn!("Try to use invalid queue_type, but RDB is only available by setting, use RDB queue: worker={:?}, job={:?}", &wd, &job)
            }

            if self.rdb_job_repository().create(&job).await? {
                if wd.response_type == ResponseType::Direct as i32 {
                    let res = self
                        .job_result_app
                        .listen_result(&jid, Some(&wid), Some(&wd.name), timeout)
                        .await?;
                    Ok((jid, Some(res)))
                } else {
                    Ok((jid, None))
                }
            } else {
                Err(JobWorkerError::RuntimeError(format!("cannot create job: {:?}", &job)).into())
            }
        } else {
            Err(JobWorkerError::WorkerNotFound(format!("name: {:?}", &worker_name)).into())
        }
    }

    // update job (re enqueue etc)
    async fn update_job(&self, job: &Job) -> Result<()> {
        if let Some(data) = &job.data {
            if let Some(wid) = data.worker_id.as_ref() {
                if let Ok(Some(_w)) = self.worker_app().find(wid).await {
                    self.rdb_job_repository()
                        .update(job.id.as_ref().unwrap(), data)
                        .await?;
                    Ok(())
                } else {
                    Err(
                        JobWorkerError::WorkerNotFound(format!("in re-enqueue job: {:?}", &job))
                            .into(),
                    )
                }
            } else {
                Err(JobWorkerError::NotFound(format!(
                    "in re-enqueue job: data not found:{:?}",
                    &job
                ))
                .into())
            }
        } else {
            Err(
                JobWorkerError::NotFound(format!("in re-enqueue job: data not found:{:?}", &job))
                    .into(),
            )
        }
    }

    // need to delete only from rdb
    #[inline]
    async fn complete_job(&self, _id: &JobResultId, result: &JobResultData) -> Result<bool> {
        if let Some(jid) = result.job_id.as_ref() {
            // for rdb queue (perhaps not necessary)
            self.delete_job(jid).await
        } else {
            // something wrong
            tracing::error!("no job found from result: {:?}", result);
            Ok(false)
        }
    }

    #[inline]
    async fn delete_job(&self, id: &JobId) -> Result<bool> {
        self.rdb_job_repository().delete(id).await
    }
    async fn find_job(&self, id: &JobId, ttl: Option<Duration>) -> Result<Option<Job>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_cache_key(id));
        self.with_cache(&k, ttl, || async {
            let v = self.rdb_job_repository().find(id).await;
            match v {
                Ok(opt) => Ok(match opt {
                    Some(v) => vec![v],
                    None => Vec::new(),
                }),
                Err(e) => Err(e),
            }
        })
        .await
        .map(|r| r.first().map(|o| (*o).clone()))
    }

    async fn find_job_list(
        &self,
        limit: Option<&i32>,
        offset: Option<&i64>,
        ttl: Option<Duration>,
    ) -> Result<Vec<Job>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_list_cache_key(limit, offset.unwrap_or(&0i64)));
        self.with_cache(&k, ttl, || async {
            self.rdb_job_repository().find_list(limit, offset).await
        })
        .await
    }

    async fn find_job_status(&self, id: &JobId) -> Result<Option<JobStatus>>
    where
        Self: Send + 'static,
    {
        self.rdb_job_repository().find_status(id).await
    }

    async fn find_all_job_status(&self) -> Result<Vec<(JobId, JobStatus)>>
    where
        Self: Send + 'static,
    {
        self.rdb_job_repository().find_all_status().await
    }

    async fn count(&self) -> Result<i64>
    where
        Self: Send + 'static,
    {
        // TODO cache
        self.rdb_job_repository()
            .count_list_tx(self.rdb_job_repository().db_pool())
            .await
    }

    /// noop (only for hybrid)
    async fn restore_jobs_from_rdb(
        &self,
        _include_grabbed: bool,
        _limit: Option<&i32>,
    ) -> Result<()> {
        Ok(())
    }

    /// noop (only for hybrid)
    async fn find_restore_jobs_from_rdb(
        &self,
        _include_grabbed: bool,
        _limit: Option<&i32>,
    ) -> Result<Vec<Job>> {
        Ok(vec![])
    }

    // run_after job, necessary only for redis
    async fn pop_run_after_jobs_to_run(&self) -> Result<Vec<Job>> {
        Ok(vec![])
    }
}
impl UseJobQueueConfig for RdbJobAppImpl {
    fn job_queue_config(&self) -> &JobQueueConfig {
        &self.job_queue_config
    }
}
impl UseRdbRepositoryModule for RdbJobAppImpl {
    fn rdb_repository_module(&self) -> &RdbRepositoryModule {
        &self.repositories
    }
}
impl UseIdGenerator for RdbJobAppImpl {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        &self.id_generator
    }
}
impl UseWorkerApp for RdbJobAppImpl {
    fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static> {
        &self.worker_app
    }
}
impl UseStorageConfig for RdbJobAppImpl {
    fn storage_config(&self) -> &StorageConfig {
        &self.storage_config
    }
}
impl JobCacheKeys for RdbJobAppImpl {}

impl JobBuilder for RdbJobAppImpl {}

impl UseMemoryCache<Arc<String>, Vec<Job>> for RdbJobAppImpl {
    const CACHE_TTL: Option<Duration> = Some(Duration::from_secs(5));
    fn cache(&self) -> &AsyncCache<Arc<String>, Vec<Job>> {
        &self.memory_cache
    }
}
