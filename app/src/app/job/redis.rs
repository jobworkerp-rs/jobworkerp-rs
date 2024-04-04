use super::super::job_result::UseJobResultApp;
use super::{JobApp, JobBuilder, RedisJobAppHelper};
use crate::app::job_result::pubsub::JobResultPublishApp;
use crate::app::job_result::JobResultApp;
use crate::app::worker::{UseWorkerApp, WorkerApp};
use anyhow::Result;
use async_trait::async_trait;
use command_utils::util::datetime;
use command_utils::util::option::Exists;
use infra::error::JobWorkerError;
use infra::infra::job::redis::job_status::JobStatusRepository;
use infra::infra::job::redis::queue::RedisJobQueueRepository;
use infra::infra::job::redis::schedule::RedisJobScheduleRepository;
use infra::infra::job::redis::{RedisJobRepository, UseRedisJobRepository};
use infra::infra::job::rows::UseJobqueueAndCodec;
use infra::infra::module::redis::{RedisRepositoryModule, UseRedisRepositoryModule};
use infra::infra::{IdGeneratorWrapper, JobQueueConfig, UseIdGenerator, UseJobQueueConfig};
use infra_utils::infra::memory::UseMemoryCache;
use infra_utils::infra::redis::{RedisClient, UseRedisClient};
use proto::jobworkerp::data::{
    Job, JobData, JobId, JobResult, JobResultData, JobResultId, JobStatus, QueueType, ResponseType,
    RunnerArg, WorkerId,
};
use std::{sync::Arc, time::Duration};
use stretto::AsyncCache;

pub struct RedisJobAppImpl {
    job_queue_config: Arc<JobQueueConfig>,
    id_generator: Arc<IdGeneratorWrapper>,
    repositories: Arc<RedisRepositoryModule>,
    worker_app: Arc<dyn WorkerApp + 'static>,
    job_result_app: Arc<dyn JobResultApp + 'static>,
    memory_cache: AsyncCache<Arc<String>, Vec<Job>>,
}

impl RedisJobAppImpl {
    pub fn new(
        job_queue_config: Arc<JobQueueConfig>,
        id_generator: Arc<IdGeneratorWrapper>,
        repositories: Arc<RedisRepositoryModule>,
        worker_app: Arc<dyn WorkerApp + 'static>,
        job_result_app: Arc<dyn JobResultApp + 'static>,
        memory_cache: AsyncCache<Arc<String>, Vec<Job>>,
    ) -> Self {
        Self {
            job_queue_config,
            id_generator,
            repositories,
            worker_app,
            job_result_app,
            memory_cache,
        }
    }
}

// TODO not used now
// TODO should create and update by job_app (not used now)
#[async_trait]
impl JobApp for RedisJobAppImpl {
    async fn enqueue_job(
        &self,
        worker_id: Option<&WorkerId>,
        worker_name: Option<&String>,
        arg: Option<RunnerArg>,
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
        if let Some(w) = worker_res {
            let job_data = JobData {
                worker_id: w.id,
                arg,
                uniq_key,
                enqueue_time: datetime::now_millis(),
                grabbed_until_time: None,
                run_after_time,
                retried: 0u32,
                priority,
                timeout,
            };

            // need to store to db
            //TODO handle properly for periodic or run_after_time job
            if let Some(wd) = &w.data {
                let job = Job {
                    id: Some(JobId {
                        value: self.id_generator().generate_id()?,
                    }),
                    data: Some(job_data),
                };
                if wd.queue_type == QueueType::Rdb as i32 {
                    tracing::warn!("Try to use invalid queue_type{:?}, but Redis is only available by setting, use RDB queue: worker={:?}, job={:?}", &wd.queue_type, &wd.name, &job.data)
                }

                let res = self
                    .enqueue_job_to_redis_with_wait_if_needed(&job, wd)
                    .await?;
                //
                // create job record for find
                if let Job {
                    id: Some(id),
                    data: Some(data),
                } = &job
                {
                    self.redis_job_repository().create(id, data).await?;
                }

                Ok(res)
            } else {
                Err(JobWorkerError::WorkerNotFound(format!(
                    "worker data not found: worker name: {:?}",
                    &worker_name
                ))
                .into())
            }
        } else {
            Err(JobWorkerError::WorkerNotFound(format!("name: {:?}", &worker_name)).into())
        }
    }

    // update job with id (redis: upsert, rdb: update)
    async fn update_job(&self, job: &Job) -> Result<()> {
        if let Some(data) = &job.data {
            if let Ok(Some(d)) = self
                .worker_app()
                .find_data_by_opt(data.worker_id.as_ref())
                .await
            {
                // need to store to db
                // use same id
                //TODO handle properly for periodic or run_after_time job
                self.enqueue_job_to_redis_with_wait_if_needed(job, &d)
                    .await?;
                // update job record for find
                if let Job {
                    id: Some(id),
                    data: Some(data),
                } = job
                {
                    self.redis_job_repository()
                        .upsert_status(
                            id,
                            if data.grabbed_until_time.as_ref().exists(|g| *g > 0) {
                                &JobStatus::Running
                            } else {
                                &JobStatus::Pending
                            },
                        )
                        .await?;
                    self.redis_job_repository().upsert(id, data).await?;
                }
                Ok(())
            } else {
                tracing::error!("worker data not found: worker id: {:?}", &data.worker_id);
                Err(JobWorkerError::WorkerNotFound(format!("in re-enqueue job: {:?}", &job)).into())
            }
        } else {
            Err(JobWorkerError::WorkerNotFound(format!("in re-enqueue job: {:?}", &job)).into())
        }
    }

    async fn complete_job(&self, id: &JobResultId, data: &JobResultData) -> Result<bool> {
        if let Some(jid) = data.job_id.as_ref() {
            self.redis_job_repository().delete_status(jid).await?;
            match ResponseType::try_from(data.response_type) {
                Ok(ResponseType::Direct) => {
                    // send result for direct or listen after response
                    self.redis_job_repository()
                        .enqueue_result_direct(id, data)
                        .await
                }
                Ok(ResponseType::ListenAfter) => {
                    // publish for listening result client
                    self.publish_result(id, data).await?;
                    self.delete_job(jid).await?;
                    Ok(true)
                }
                _ => {
                    self.delete_job(jid).await?;
                    Ok(true)
                }
            }
        } else {
            // something wrong
            tracing::error!("no job found from result: {:?}", data);
            Ok(false)
        }
    }

    async fn delete_job(&self, id: &JobId) -> Result<bool> {
        self.redis_job_repository().delete(id).await
    }

    async fn find_job(&self, id: &JobId, _ttl: Option<Duration>) -> Result<Option<Job>>
    where
        Self: Send + 'static,
    {
        self.redis_job_repository().find(id).await
    }

    async fn find_job_list(
        &self,
        limit: Option<&i32>,
        offset: Option<&i64>,
        _ttl: Option<Duration>,
    ) -> Result<Vec<Job>>
    where
        Self: Send + 'static,
    {
        let all = self.redis_job_repository().find_all().await?;
        // sort by id asc
        // all.sort_by(|a, b| a.id.cmp(&b.id));
        // take from offset by limit if limit is set
        let v = if let Some(l) = limit {
            all.into_iter()
                .skip(*offset.unwrap_or(&0i64) as usize)
                .take(*l as usize)
                .collect()
        } else {
            all.into_iter()
                .skip(*offset.unwrap_or(&0i64) as usize)
                .collect()
        };
        Ok(v)
    }

    async fn count(&self) -> Result<i64>
    where
        Self: Send + 'static,
    {
        // TODO cache
        self.redis_job_repository().count().await
    }

    async fn find_job_status(&self, id: &JobId) -> Result<Option<JobStatus>>
    where
        Self: Send + 'static,
    {
        self.redis_job_repository().find_status(id).await
    }

    async fn find_all_job_status(&self) -> Result<Vec<(JobId, JobStatus)>>
    where
        Self: Send + 'static,
    {
        self.redis_job_repository().find_status_all().await
    }

    // delegate
    async fn pop_run_after_jobs_to_run(&self) -> Result<Vec<Job>> {
        self.redis_job_repository()
            .pop_run_after_jobs_to_run()
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
    async fn find_restore_jobs_from_rdb(
        &self,
        _include_grabbed: bool,
        _limit: Option<&i32>,
    ) -> Result<Vec<Job>> {
        Ok(vec![])
    }
}

impl UseJobQueueConfig for RedisJobAppImpl {
    fn job_queue_config(&self) -> &JobQueueConfig {
        &self.job_queue_config
    }
}

impl RedisJobAppHelper for RedisJobAppImpl {}

impl UseRedisRepositoryModule for RedisJobAppImpl {
    fn redis_repository_module(&self) -> &RedisRepositoryModule {
        &self.repositories
    }
}
impl UseIdGenerator for RedisJobAppImpl {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        &self.id_generator
    }
}
impl UseWorkerApp for RedisJobAppImpl {
    fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static> {
        &self.worker_app
    }
}
impl UseJobResultApp for RedisJobAppImpl {
    fn job_result_app(&self) -> &Arc<dyn JobResultApp + 'static> {
        &self.job_result_app
    }
}
impl JobBuilder for RedisJobAppImpl {}

impl UseMemoryCache<Arc<String>, Vec<Job>> for RedisJobAppImpl {
    const CACHE_TTL: Option<Duration> = Some(Duration::from_secs(60)); // 1 min
    fn cache(&self) -> &AsyncCache<Arc<String>, Vec<Job>> {
        &self.memory_cache
    }
}

impl UseJobqueueAndCodec for RedisJobAppImpl {}
impl UseRedisClient for RedisJobAppImpl {
    fn redis_client(&self) -> &RedisClient {
        &self.repositories.redis_client
    }
}

impl JobResultPublishApp for RedisJobAppImpl {}
