use crate::app::{UseWorkerConfig, WorkerConfig};
use crate::module::AppConfigModule;

use super::super::worker::{UseWorkerApp, WorkerApp};
use super::super::JobBuilder;
use super::{JobApp, JobCacheKeys, RedisJobAppHelper};
use anyhow::Result;
use async_trait::async_trait;
use command_utils::util::datetime;
use command_utils::util::option::{Exists, FlatMap};
use futures::stream::BoxStream;
use infra::infra::job::queue::redis::RedisJobQueueRepository;
use infra::infra::job::rdb::{RdbJobRepository, UseRdbChanJobRepository};
use infra::infra::job::redis::{RedisJobRepository, UseRedisJobRepository};
use infra::infra::job::status::{JobStatusRepository, UseJobStatusRepository};
use infra::infra::job_result::pubsub::redis::{
    RedisJobResultPubSubRepositoryImpl, UseRedisJobResultPubSubRepository,
};
use infra::infra::job_result::pubsub::JobResultPublisher;
use infra::infra::module::rdb::{RdbChanRepositoryModule, UseRdbChanRepositoryModule};
use infra::infra::module::redis::{RedisRepositoryModule, UseRedisRepositoryModule};
use infra::infra::module::HybridRepositoryModule;
use infra::infra::{IdGeneratorWrapper, JobQueueConfig, UseIdGenerator, UseJobQueueConfig};
use infra_utils::infra::memory::{MemoryCacheImpl, UseMemoryCache};
use infra_utils::infra::rdb::UseRdbPool;
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{
    Job, JobData, JobId, JobResult, JobResultData, JobResultId, JobStatus, Priority, QueueType,
    ResponseType, ResultOutputItem, Worker, WorkerId,
};
use std::collections::HashSet;
use std::{sync::Arc, time::Duration};

#[derive(Clone, Debug)]
pub struct HybridJobAppImpl {
    app_config_module: Arc<AppConfigModule>,
    id_generator: Arc<IdGeneratorWrapper>,
    repositories: Arc<HybridRepositoryModule>,
    worker_app: Arc<dyn WorkerApp + 'static>,
    memory_cache: MemoryCacheImpl<Arc<String>, Vec<Job>>,
}

impl HybridJobAppImpl {
    pub fn new(
        app_config_module: Arc<AppConfigModule>,
        id_generator: Arc<IdGeneratorWrapper>,
        repositories: Arc<HybridRepositoryModule>,
        worker_app: Arc<dyn WorkerApp + 'static>,
        memory_cache: MemoryCacheImpl<Arc<String>, Vec<Job>>,
    ) -> Self {
        Self {
            app_config_module,
            id_generator,
            repositories,
            worker_app,
            memory_cache,
        }
    }
    // find not queueing  jobs from argument 'jobs' in channels
    async fn find_restore_jobs_by(
        &self,
        job_ids: &HashSet<i64>,
        channels: &[String],
    ) -> Result<Vec<Job>> {
        let all_priority = [Priority::High, Priority::Medium, Priority::Low];
        // get current jids by iterate all channels and all_priority
        let mut current_jids = HashSet::new();
        for channel in channels {
            for priority in all_priority.iter() {
                let jids = self
                    .redis_job_repository()
                    .find_multi_from_queue(
                        // decode all jobs and check id in queue (heavy runner_settings when many jobs in queue)
                        // TODO change to resolve only ids for less memory
                        Some(channel.as_str()),
                        *priority,
                        Some(job_ids),
                    )
                    .await?
                    .iter()
                    .flat_map(|c| c.id.as_ref().map(|i| i.value))
                    .collect::<HashSet<i64>>();
                current_jids.extend(jids);
            }
        }
        // db record ids that not exists in job queue
        // restore jobs to redis queue (store jobs which not include current_jids)
        let diff_ids = job_ids.difference(&current_jids).collect::<Vec<&i64>>();
        let mut ret = vec![];
        // db only jobs which not is_run_after_job_data
        for jids in diff_ids.chunks(1000) {
            ret.extend(
                self.rdb_job_repository()
                    .find_list_in(jids)
                    .await?
                    .into_iter()
                    .filter(|j| j.data.as_ref().exists(|d| !self.is_run_after_job_data(d))),
            );
        }
        Ok(ret)
    }

    // use find_restore_jobs_by and enqueue them to redis queue
    async fn restore_jobs_by(&self, job_ids: &HashSet<i64>, channels: &[String]) -> Result<()> {
        let restores = self.find_restore_jobs_by(job_ids, channels).await?;
        // restore jobs to redis queue (store jobs which not include current jids)
        for job in restores.iter() {
            // unwrap
            if let Ok(Some(w)) = self
                .worker_app
                .find_data_by_opt(job.data.as_ref().flat_map(|d| d.worker_id.as_ref()))
                .await
            {
                // not restore use rdb jobs (periodic worker or run after jobs)(should not exists in 'restores')
                if w.periodic_interval > 0 {
                    tracing::debug!("not restore use rdb job to redis: {:?}", &job);
                } else if w.response_type == ResponseType::Direct as i32 {
                    // not need to store to redis (should not reach here because direct response job shouldnot stored to rdb)
                    tracing::warn!(
                        "restore jobs from db: not restore direct response job: {:?}",
                        &job
                    );
                } else {
                    // need to store to redis
                    self.enqueue_job_to_redis_with_wait_if_needed(job, &w)
                        .await?;
                }
            } else {
                tracing::warn!("restore jobs from db: worker not found for job: {:?}", &job);
            }
        }
        Ok(())
    }
}

#[async_trait]
impl JobApp for HybridJobAppImpl {
    async fn enqueue_job(
        &self,
        worker_id: Option<&WorkerId>,
        worker_name: Option<&String>,
        args: Vec<u8>,
        uniq_key: Option<String>,
        run_after_time: i64,
        priority: i32,
        timeout: u64,
        reserved_job_id: Option<JobId>,
    ) -> Result<(
        JobId,
        Option<JobResult>,
        Option<BoxStream<'static, ResultOutputItem>>,
    )> {
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
            data: Some(w),
        }) = worker_res.as_ref()
        {
            let job_data = JobData {
                worker_id: Some(*wid),
                args,
                uniq_key,
                enqueue_time: datetime::now_millis(),
                grabbed_until_time: None,
                run_after_time,
                retried: 0u32,
                priority,
                timeout,
            };
            // TODO validate argument types (using Runner)
            // self.validate_worker_and_job_arg(w, job_data.arg.as_ref())?;
            // cannot wait for direct response
            if run_after_time > 0 && w.response_type == ResponseType::Direct as i32 {
                return Err(JobWorkerError::InvalidParameter(format!(
                        "run_after_time must be 0 for worker response_type=Direct, must use ListenAfter. job: {:?}",
                        &job_data
                    ))
                    .into());
            }
            let jid = reserved_job_id.unwrap_or(JobId {
                value: self.id_generator().generate_id()?,
            });
            // job fetched by rdb (periodic job) should have positive run_after_time
            let data = if (w.periodic_interval > 0 || w.queue_type == QueueType::ForcedRdb as i32)
                && job_data.run_after_time == 0
            {
                // make job_data.run_after_time datetime::now_millis() and create job by db
                JobData {
                    run_after_time: datetime::now_millis(), // set now millis
                    ..job_data
                }
            } else {
                job_data
            };

            if w.response_type == ResponseType::Direct as i32 {
                // use redis only for direct response (not restore)
                // TODO create backup for queue_type == Hybrid ?
                let job = Job {
                    id: Some(jid),
                    data: Some(data.to_owned()),
                };
                self.enqueue_job_to_redis_with_wait_if_needed(&job, w).await
            } else if w.periodic_interval > 0 || self.is_run_after_job_data(&data) {
                let job = Job {
                    id: Some(jid),
                    data: Some(data),
                };
                // enqueue rdb only
                if self.rdb_job_repository().create(&job).await? {
                    self.job_status_repository()
                        .upsert_status(&jid, &JobStatus::Pending)
                        .await?;
                    Ok((jid, None, None))
                } else {
                    Err(
                        JobWorkerError::RuntimeError(format!("cannot create record: {:?}", &job))
                            .into(),
                    )
                }
            } else {
                // normal instant job
                let job = Job {
                    id: Some(jid),
                    data: Some(data),
                };
                if w.queue_type == QueueType::WithBackup as i32 {
                    // instant job (store rdb for failback, and enqueue to redis)
                    // TODO store async to rdb (not necessary to wait)
                    match self.rdb_job_repository().create(&job).await {
                        Ok(_id) => self.enqueue_job_to_redis_with_wait_if_needed(&job, w).await,
                        Err(e) => Err(e),
                    }
                } else if w.queue_type == QueueType::ForcedRdb as i32 {
                    // use only rdb queue (not recommended for hybrid storage)
                    let created = self.rdb_job_repository().create(&job).await?;
                    if created {
                        Ok((job.id.unwrap(), None, None))
                    } else {
                        // storage error?
                        Err(JobWorkerError::RuntimeError(format!(
                            "cannot create record: {:?}",
                            &job
                        ))
                        .into())
                    }
                } else {
                    // instant job (enqueue to redis only)
                    self.enqueue_job_to_redis_with_wait_if_needed(&job, w).await
                }
            }
        } else {
            Err(JobWorkerError::WorkerNotFound(format!("name: {:?}", &worker_name)).into())
        }
    }

    // update (re-enqueue) job with id (redis: upsert, rdb: update)
    async fn update_job(&self, job: &Job) -> Result<()> {
        if let Job {
            id: Some(jid),
            data: Some(data),
        } = job
        {
            let is_run_after_job_data = self.is_run_after_job_data(data);
            if let Ok(Some(w)) = self
                .worker_app()
                .find_data_by_opt(data.worker_id.as_ref())
                .await
            {
                // TODO validate argument types (using Runner)
                // self.validate_worker_and_job_arg(&w, data.arg.as_ref())?;

                // use db queue (run after, periodic, queue_type=DB worker)
                let res_db = if is_run_after_job_data
                    || w.periodic_interval > 0
                    || w.queue_type == QueueType::ForcedRdb as i32
                    || w.queue_type == QueueType::WithBackup as i32
                {
                    // XXX should compare grabbed_until_time and update if not changed or not (now not compared)
                    self.rdb_job_repository().update(jid, data).await
                } else {
                    Ok(false)
                };
                // update job status of redis(memory)
                self.job_status_repository()
                    .upsert_status(jid, &JobStatus::Pending)
                    .await?;
                let res_redis = if !is_run_after_job_data
                    && w.periodic_interval == 0
                    && (w.queue_type == QueueType::Normal as i32
                        || w.queue_type == QueueType::WithBackup as i32)
                {
                    // enqueue to redis for instant job
                    self.enqueue_job_to_redis_with_wait_if_needed(job, &w)
                        .await
                        .map(|_| true)
                } else {
                    Ok(false)
                };
                let res = res_redis.or(res_db);
                match res {
                    Ok(_updated) => {
                        let _ = self
                            .memory_cache
                            .delete_cache(&Arc::new(Self::find_cache_key(jid)))
                            .await;
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            } else {
                Err(JobWorkerError::WorkerNotFound(format!("in re-enqueue job: {:?}", &job)).into())
            }
        } else {
            Err(JobWorkerError::NotFound(format!("illegal re-enqueue job: {:?}", &job)).into())
        }
    }

    async fn complete_job(
        &self,
        id: &JobResultId,
        data: &JobResultData,
        stream: Option<BoxStream<'static, ResultOutputItem>>,
    ) -> Result<bool> {
        tracing::debug!("complete_job: res_id={}", &id.value);
        if let Some(jid) = data.job_id.as_ref() {
            self.job_status_repository().delete_status(jid).await?;
            match ResponseType::try_from(data.response_type) {
                Ok(ResponseType::Direct) => {
                    // send result for direct or listen after response
                    let res = self
                        .redis_job_repository()
                        .enqueue_result_direct(id, data)
                        .await;
                    // publish for listening result client
                    // (XXX can receive response by listen_after, listen_by_worker for DIRECT response)
                    let _ = self
                        .job_result_pubsub_repository()
                        .publish_result(id, data, true)
                        .await
                        .inspect_err(|e| {
                            tracing::warn!("complete_job: pubsub publish error: {:?}", e)
                        });
                    // stream data
                    if let Some(stream) = stream {
                        let pubsub_repo = self.job_result_pubsub_repository().clone();
                        tracing::debug!(
                            "complete_job(direct): publish stream data: {}",
                            &jid.value
                        );
                        pubsub_repo.publish_result_stream_data(*jid, stream).await?;
                        tracing::debug!(
                            "complete_job(direct): stream data published: {}",
                            &jid.value
                        );
                    }
                    // tokio::time::sleep(Duration::from_secs(3)).await;
                    res
                }
                Ok(rtype) => {
                    // publish for listening result client
                    let r = self
                        .job_result_pubsub_repository()
                        .publish_result(id, data, rtype == ResponseType::ListenAfter)
                        .await;
                    // stream data
                    if let Some(stream) = stream {
                        let pubsub_repo = self.job_result_pubsub_repository().clone();
                        pubsub_repo.publish_result_stream_data(*jid, stream).await?;
                        tracing::debug!("complete_job: stream data published: {}", &jid.value);
                    }
                    self.delete_job(jid).await?;
                    r
                }
                _ => {
                    tracing::warn!("complete_job: invalid response_type: {:?}", &data);
                    // abnormal response type, no publish
                    self.delete_job(jid).await?;
                    Ok(false)
                }
            }
        } else {
            // something wrong
            tracing::error!("no job found from result: {:?}", data);
            Ok(false)
        }
    }

    async fn delete_job(&self, id: &JobId) -> Result<bool> {
        // delete from db and redis
        match self.rdb_job_repository().delete(id).await {
            Ok(r) => {
                let _ = self
                    .memory_cache
                    .delete_cache(&Arc::new(Self::find_cache_key(id)))
                    .await;
                self.redis_job_repository()
                    .delete(id)
                    .await
                    .map(|r2| r || r2)
            }
            Err(e) => Err(e),
        }
    }
    // cannot get job of queue type REDIS (redis is used for queue and job cache)
    async fn find_job(&self, id: &JobId, ttl: Option<&Duration>) -> Result<Option<Job>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_cache_key(id));
        // XXX negative memory cache exists
        self.memory_cache
            .with_cache(&k, ttl, || async {
                // find from redis as cache
                let v = self.redis_job_repository().find(id).await;
                match v {
                    Ok(opt) => match opt {
                        Some(v) => Ok(vec![v]),
                        None => {
                            let rv = self
                                .rdb_job_repository()
                                .find(id)
                                .await
                                .map(|r| r.map(|o| vec![o]).unwrap_or_default())?;
                            if let Some(Job {
                                id: Some(_),
                                data: Some(r),
                            }) = rv.first()
                            {
                                self.redis_job_repository().create(id, r).await?;
                            }
                            Ok(rv)
                        }
                    },
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
        ttl: Option<&Duration>,
    ) -> Result<Vec<Job>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_list_cache_key(limit, offset.unwrap_or(&0i64)));
        self.memory_cache
            .with_cache(&k, ttl, || async {
                // from rdb with limit offset
                // XXX use redis as cache ?
                let v = self.rdb_job_repository().find_list(limit, offset).await?;
                Ok(v)
            })
            .await
    }
    async fn find_job_queue_list(
        &self,
        limit: Option<&i32>,
        channel: Option<&str>,
        ttl: Option<&Duration>,
    ) -> Result<Vec<(Job, Option<JobStatus>)>>
    where
        Self: Send + 'static,
    {
        // let k = Arc::new(Self::find_queue_cache_key(channel, limit));
        // self.memory_cache
        //     .with_cache(&k, ttl, || async {
        // from redis queue with limit channel
        let priorities = [Priority::High, Priority::Medium, Priority::Low];
        let mut ret = vec![];
        for priority in priorities.iter() {
            let v = self
                .redis_job_repository()
                .find_multi_from_queue(channel, *priority, None)
                .await?;
            ret.extend(v);
            if ret.len() >= *limit.unwrap_or(&100) as usize {
                ret.truncate(*limit.unwrap_or(&100) as usize);
                break;
            }
        }
        let mut job_and_status = vec![];
        for j in ret.iter() {
            if let Some(jid) = j.id.as_ref() {
                if let Some(j) = self.find_job(jid, ttl).await? {
                    let s = self.job_status_repository().find_status(jid).await?;
                    job_and_status.push((j, s));
                }
            }
        }
        Ok(job_and_status)
        // })
        // .await
    }

    async fn find_job_status(&self, id: &JobId) -> Result<Option<JobStatus>>
    where
        Self: Send + 'static,
    {
        self.job_status_repository().find_status(id).await
    }

    async fn find_all_job_status(&self) -> Result<Vec<(JobId, JobStatus)>>
    where
        Self: Send + 'static,
    {
        self.job_status_repository().find_status_all().await
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

    async fn pop_run_after_jobs_to_run(&self) -> Result<Vec<Job>> {
        Ok(vec![])
    }

    /// restore jobs from rdb to redis
    /// (TODO offset, limit iteration)
    async fn restore_jobs_from_rdb(
        &self,
        include_grabbed: bool,
        limit: Option<&i32>,
    ) -> Result<()> {
        let channels = self.worker_config().get_channels();
        if let Some(l) = limit {
            let job_ids = self
                .rdb_job_repository()
                .find_id_set_in_instant(include_grabbed, Some(l), None)
                .await?;
            if !job_ids.is_empty() {
                self.restore_jobs_by(&job_ids, channels.as_slice()).await?
            }
        } else {
            // fetch all with limit, offset (XXX heavy process: iterate and decode all job queue element multiple times (if larger than limit))
            let limit = 2000; // XXX depends on memory size and job size
            let mut offset = 0;
            loop {
                let job_id_set = self
                    .rdb_job_repository()
                    .find_id_set_in_instant(include_grabbed, Some(&limit), Some(&offset))
                    .await?;
                if job_id_set.is_empty() {
                    break;
                }
                self.restore_jobs_by(&job_id_set, channels.as_slice())
                    .await?;
                offset += limit as i64;
            }
        }
        Ok(())
    }
    // TODO return with streaming
    async fn find_restore_jobs_from_rdb(
        &self,
        include_grabbed: bool,
        limit: Option<&i32>,
    ) -> Result<Vec<Job>> {
        let channels = self.worker_config().get_channels();
        let job_id_set = self
            .rdb_job_repository()
            .find_id_set_in_instant(include_grabbed, limit, None)
            .await?;
        if job_id_set.is_empty() {
            return Ok(vec![]);
        } else {
            self.find_restore_jobs_by(&job_id_set, channels.as_slice())
                .await
        }
    }
}
impl UseRdbChanRepositoryModule for HybridJobAppImpl {
    fn rdb_repository_module(&self) -> &RdbChanRepositoryModule {
        &self.repositories.rdb_chan_module
    }
}
impl UseRedisRepositoryModule for HybridJobAppImpl {
    fn redis_repository_module(&self) -> &RedisRepositoryModule {
        &self.repositories.redis_module
    }
}
impl UseJobStatusRepository for HybridJobAppImpl {
    fn job_status_repository(&self) -> Arc<dyn JobStatusRepository> {
        self.repositories
            .redis_job_repository()
            .job_status_repository()
            .clone()
    }
}
impl UseIdGenerator for HybridJobAppImpl {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        &self.id_generator
    }
}
impl UseWorkerApp for HybridJobAppImpl {
    fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static> {
        &self.worker_app
    }
}
impl JobCacheKeys for HybridJobAppImpl {}

impl JobBuilder for HybridJobAppImpl {}

impl UseRedisJobResultPubSubRepository for HybridJobAppImpl {
    fn job_result_pubsub_repository(&self) -> &RedisJobResultPubSubRepositoryImpl {
        &self
            .repositories
            .redis_module
            .redis_job_result_pubsub_repository
    }
}

impl UseJobQueueConfig for HybridJobAppImpl {
    fn job_queue_config(&self) -> &JobQueueConfig {
        &self.app_config_module.job_queue_config
    }
}
impl UseWorkerConfig for HybridJobAppImpl {
    fn worker_config(&self) -> &WorkerConfig {
        &self.app_config_module.worker_config
    }
}
impl RedisJobAppHelper for HybridJobAppImpl {}

//TODO
// create test
#[cfg(any(test, feature = "test-utils"))]
pub mod tests {
    use super::*;
    use crate::app::runner::hybrid::HybridRunnerAppImpl;
    use crate::app::runner::RunnerApp;
    use crate::app::worker::hybrid::HybridWorkerAppImpl;
    use crate::app::{StorageConfig, StorageType};
    use anyhow::Result;
    use infra::infra::job_result::pubsub::redis::RedisJobResultPubSubRepositoryImpl;
    use infra::infra::module::rdb::test::setup_test_rdb_module;
    use infra::infra::module::redis::test::setup_test_redis_module;
    use infra::infra::module::HybridRepositoryModule;
    use infra::infra::IdGeneratorWrapper;
    use jobworkerp_runner::runner::factory::RunnerSpecFactory;
    use jobworkerp_runner::runner::plugins::Plugins;
    use proto::jobworkerp::data::RunnerId;
    use std::sync::Arc;

    pub async fn create_test_app(
        use_mock_id: bool,
    ) -> Result<(HybridJobAppImpl, RedisJobResultPubSubRepositoryImpl)> {
        let rdb_module = setup_test_rdb_module().await;
        let redis_module = setup_test_redis_module().await;
        let repositories = Arc::new(HybridRepositoryModule {
            redis_module: redis_module.clone(),
            rdb_chan_module: rdb_module,
        });
        // mock id generator (generate 1 until called set method)
        let id_generator = if use_mock_id {
            Arc::new(IdGeneratorWrapper::new_mock())
        } else {
            Arc::new(IdGeneratorWrapper::new())
        };
        let mc_config = infra_utils::infra::memory::MemoryCacheConfig {
            num_counters: 1000000,
            max_cost: 1000000,
            use_metrics: false,
        };
        let job_memory_cache = infra_utils::infra::memory::MemoryCacheImpl::new(
            &mc_config,
            Some(Duration::from_secs(60)),
        );
        let storage_config = Arc::new(StorageConfig {
            r#type: StorageType::Scalable,
            restore_at_startup: Some(false),
        });
        let job_queue_config = Arc::new(JobQueueConfig {
            expire_job_result_seconds: 10,
            fetch_interval: 1000,
        });
        let worker_config = Arc::new(WorkerConfig {
            default_concurrency: 4,
            channels: vec!["test".to_string()],
            channel_concurrencies: vec![2],
        });
        let descriptor_cache = Arc::new(infra_utils::infra::memory::MemoryCacheImpl::new(
            &mc_config,
            Some(Duration::from_secs(60 * 60)),
        ));
        let runner_app = Arc::new(HybridRunnerAppImpl::new(
            storage_config.clone(),
            &mc_config,
            repositories.clone(),
            descriptor_cache.clone(),
        ));
        runner_app.load_runner().await?;
        let _ = runner_app
            .create_test_runner(&RunnerId { value: 10000 }, "Test")
            .await
            .unwrap();
        let worker_app = HybridWorkerAppImpl::new(
            storage_config.clone(),
            id_generator.clone(),
            &mc_config,
            repositories.clone(),
            descriptor_cache,
            runner_app,
        );
        let subscrber = RedisJobResultPubSubRepositoryImpl::new(
            redis_module.redis_client,
            job_queue_config.clone(),
        );
        let runner_factory = RunnerSpecFactory::new(Arc::new(Plugins::new()));
        runner_factory.load_plugins().await;
        let config_module = Arc::new(AppConfigModule {
            storage_config,
            worker_config,
            job_queue_config,
            runner_factory: Arc::new(runner_factory),
        });
        Ok((
            HybridJobAppImpl::new(
                config_module,
                id_generator,
                repositories,
                Arc::new(worker_app),
                job_memory_cache,
            ),
            subscrber,
        ))
    }

    #[test]
    fn test_create_direct_job_complete() -> Result<()> {
        use infra::infra::job::rows::{JobqueueAndCodec, UseJobqueueAndCodec};
        // enqueue, find, complete, find, delete, find
        // tracing_subscriber::fmt()
        //     .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        //     .with_max_level(tracing::Level::TRACE)
        //     .compact()
        //     .init();

        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let (app, _) = create_test_app(true).await?;
            let runner_settings = JobqueueAndCodec::serialize_message(&proto::TestRunnerSettings {
                name: "ls".to_string(),
            });
            let wd = proto::jobworkerp::data::WorkerData {
                name: "testworker".to_string(),
                runner_id: Some(RunnerId { value: 10000 }),
                runner_settings,
                channel: None,
                response_type: ResponseType::Direct as i32,
                periodic_interval: 0,
                retry_policy: None,
                queue_type: QueueType::Normal as i32,
                store_failure: false,
                store_success: false,
                use_static: false,
                output_as_stream: false,
            };
            let worker_id = app.worker_app().create(&wd).await?;
            let jarg = JobqueueAndCodec::serialize_message(&proto::TestArgs {
                args: vec!["/".to_string()],
            });

            // move
            let worker_id1 = worker_id;
            let jarg1 = jarg.clone();
            let app1 = app.clone();
            // need waiting for direct response
            let jh = tokio::spawn(async move {
                let res = app1
                    .enqueue_job(Some(&worker_id1), None, jarg1, None, 0, 0, 0, None)
                    .await;
                let (jid, job_res, _) = res.unwrap();
                assert!(jid.value > 0);
                assert!(job_res.is_some());
                // cannot find job in direct response type
                let job = app1
                    .find_job(
                        &job_res
                            .clone()
                            .flat_map(|j| j.data.flat_map(|d| d.job_id))
                            .unwrap(),
                        Some(&Duration::from_millis(100)),
                    )
                    .await
                    .unwrap();
                assert!(job.is_none());
                assert_eq!(
                    app1.job_status_repository()
                        .find_status(&jid)
                        .await
                        .unwrap(),
                    None
                );

                (jid, job_res)
            });
            tokio::time::sleep(Duration::from_millis(200)).await;
            let rid = JobResultId {
                value: app.id_generator().generate_id().unwrap(),
            };
            let result = JobResult {
                id: Some(rid),
                data: Some(JobResultData {
                    job_id: Some(JobId { value: 1 }), // generated by mock generator
                    worker_id: Some(worker_id),
                    worker_name: wd.name.clone(),
                    args: jarg,
                    uniq_key: None,
                    status: proto::jobworkerp::data::ResultStatus::Success as i32,
                    output: Some(proto::jobworkerp::data::ResultOutput {
                        items: { vec!["test".as_bytes().to_vec()] },
                    }),
                    retried: 0,
                    max_retry: 0,
                    priority: 0,
                    timeout: 0,
                    enqueue_time: datetime::now_millis(),
                    run_after_time: 0,
                    start_time: datetime::now_millis(),
                    end_time: datetime::now_millis(),
                    response_type: ResponseType::Direct as i32,
                    store_success: false,
                    store_failure: false,
                }),
            };
            assert!(
                app.complete_job(&rid, result.data.as_ref().unwrap(), None)
                    .await?
            );
            let (jid, job_res) = jh.await?;
            tokio::time::sleep(Duration::from_millis(100)).await;

            assert_eq!(&job_res.unwrap(), &result);
            let job0 = app.find_job(&jid, None).await?;
            assert!(job0.is_none());
            assert_eq!(
                app.job_status_repository().find_status(&jid).await.unwrap(),
                None
            );
            Ok(())
        })
    }

    #[test]
    fn test_create_listen_after_job_complete() -> Result<()> {
        use infra::infra::job::rows::{JobqueueAndCodec, UseJobqueueAndCodec};
        use infra::infra::job_result::pubsub::JobResultSubscriber;

        // enqueue, find, complete, find, delete, find
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let (app, subscriber) = create_test_app(true).await?;
            let runner_settings = JobqueueAndCodec::serialize_message(&proto::TestRunnerSettings {
                name: "ls".to_string(),
            });
            let wd = proto::jobworkerp::data::WorkerData {
                name: "testworker".to_string(),
                runner_id: Some(RunnerId { value: 10000 }),
                runner_settings,
                channel: None,
                response_type: ResponseType::ListenAfter as i32,
                periodic_interval: 0,
                retry_policy: None,
                queue_type: QueueType::Normal as i32, // store to rdb for failback (can find job from rdb but not from redis)
                store_failure: false,
                store_success: false,
                use_static: false,
                output_as_stream: false,
            };
            let worker_id = app.worker_app().create(&wd).await?;
            let jarg = JobqueueAndCodec::serialize_message(&proto::TestArgs {
                args: vec!["/".to_string()],
            });

            // wait for direct response
            let job_id = app
                .enqueue_job(Some(&worker_id), None, jarg.clone(), None, 0, 0, 0, None)
                .await?
                .0;
            let job = Job {
                id: Some(job_id),
                data: Some(JobData {
                    worker_id: Some(worker_id),
                    args: jarg.clone(),
                    uniq_key: None,
                    enqueue_time: datetime::now_millis(),
                    grabbed_until_time: None,
                    run_after_time: 0,
                    retried: 0,
                    priority: 0,
                    timeout: 0,
                }),
            };
            assert_eq!(
                app.job_status_repository()
                    .find_status(&job_id)
                    .await
                    .unwrap(),
                Some(JobStatus::Pending)
            );

            let result = JobResult {
                id: Some(JobResultId { value: 15555 }),
                data: Some(JobResultData {
                    job_id: Some(job_id),
                    worker_id: Some(worker_id),
                    worker_name: wd.name.clone(),
                    args: jarg,
                    uniq_key: None,
                    status: proto::jobworkerp::data::ResultStatus::Success as i32,
                    output: Some(proto::jobworkerp::data::ResultOutput {
                        items: { vec!["test".as_bytes().to_vec()] },
                    }),
                    retried: 0,
                    max_retry: 0,
                    priority: 0,
                    timeout: 0,
                    enqueue_time: job.data.as_ref().unwrap().enqueue_time,
                    run_after_time: job.data.as_ref().unwrap().run_after_time,
                    start_time: datetime::now_millis(),
                    end_time: datetime::now_millis(),
                    response_type: ResponseType::ListenAfter as i32,
                    store_success: false,
                    store_failure: false,
                }),
            };
            let jid = job_id;
            let res = result.clone();
            let jh = tokio::task::spawn(async move {
                // assert_eq!(job_result.data.as_ref().unwrap().retried, 0);
                let job_result = subscriber.subscribe_result(&jid, None).await.unwrap();
                assert!(job_result.id.unwrap().value > 0);
                assert_eq!(&job_result.data, &res.data);
            });
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert!(
                // send mock result as completed job result
                app.complete_job(
                    result.id.as_ref().unwrap(),
                    result.data.as_ref().unwrap(),
                    None
                )
                .await?
            );
            jh.await?;

            let job0 = app
                .find_job(&job_id, Some(&Duration::from_millis(1)))
                .await
                .unwrap();
            assert!(job0.is_none());
            assert_eq!(
                app.job_status_repository()
                    .find_status(&job_id)
                    .await
                    .unwrap(),
                None
            );
            Ok(())
        })
    }
    #[test]
    fn test_create_normal_job_complete() -> Result<()> {
        use infra::infra::job::rows::{JobqueueAndCodec, UseJobqueueAndCodec};
        // enqueue, find, complete, find, delete, find
        let runner_settings = JobqueueAndCodec::serialize_message(&proto::TestRunnerSettings {
            name: "ls".to_string(),
        });
        let wd = proto::jobworkerp::data::WorkerData {
            name: "testworker".to_string(),
            runner_id: Some(RunnerId { value: 10000 }),
            runner_settings,
            channel: None,
            response_type: ResponseType::NoResult as i32,
            periodic_interval: 0,
            retry_policy: None,
            queue_type: QueueType::WithBackup as i32,
            store_success: true,
            store_failure: false,
            use_static: false,
            output_as_stream: false,
        };
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let (app, _) = create_test_app(true).await?;
            let worker_id = app.worker_app().create(&wd).await?;
            let jarg = JobqueueAndCodec::serialize_message(&proto::TestArgs {
                args: vec!["/".to_string()],
            });

            // wait for direct response
            let (job_id, res, _) = app
                .enqueue_job(Some(&worker_id), None, jarg.clone(), None, 0, 0, 0, None)
                .await?;
            assert!(job_id.value > 0);
            assert!(res.is_none());
            let job = app
                .find_job(&job_id, Some(&Duration::from_millis(100)))
                .await?
                .unwrap();
            assert_eq!(job.id.as_ref().unwrap(), &job_id);
            assert_eq!(
                job.data.as_ref().unwrap().worker_id.as_ref(),
                Some(&worker_id)
            );
            assert_eq!(job.data.as_ref().unwrap().retried, 0);
            assert_eq!(
                app.job_status_repository()
                    .find_status(&job_id)
                    .await
                    .unwrap(),
                Some(JobStatus::Pending)
            );

            let result = JobResult {
                id: Some(JobResultId {
                    value: app.id_generator().generate_id().unwrap(),
                }),
                data: Some(JobResultData {
                    job_id: Some(job_id),
                    worker_id: Some(worker_id),
                    worker_name: wd.name.clone(),
                    args: jarg,
                    uniq_key: None,
                    status: proto::jobworkerp::data::ResultStatus::Success as i32,
                    output: Some(proto::jobworkerp::data::ResultOutput {
                        items: { vec!["test".as_bytes().to_vec()] },
                    }),
                    retried: 0,
                    max_retry: 0,
                    priority: 0,
                    timeout: 0,
                    enqueue_time: job.data.as_ref().unwrap().enqueue_time,
                    run_after_time: job.data.as_ref().unwrap().run_after_time,
                    start_time: datetime::now_millis(),
                    end_time: datetime::now_millis(),
                    response_type: ResponseType::NoResult as i32,
                    store_success: true,
                    store_failure: false,
                }),
            };
            assert!(
                !app.complete_job(
                    result.id.as_ref().unwrap(),
                    result.data.as_ref().unwrap(),
                    None
                )
                .await?
            );
            // not fetched job (because of not use job_dispatcher)
            assert!(app.find_job(&job_id, None).await?.is_none());
            assert_eq!(
                app.job_status_repository()
                    .find_status(&job_id)
                    .await
                    .unwrap(),
                None
            );
            Ok(())
        })
    }

    #[test]
    fn test_restore_jobs_from_rdb() -> Result<()> {
        use infra::infra::job::rows::{JobqueueAndCodec, UseJobqueueAndCodec};

        let priority = Priority::Medium;
        let channel: Option<&String> = None;

        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let (app, _) = create_test_app(false).await?;
            // create command worker with hybrid queue
            let runner_settings = JobqueueAndCodec::serialize_message(&proto::TestRunnerSettings {
                name: "ls".to_string(),
            });
            let wd = proto::jobworkerp::data::WorkerData {
                name: "testworker".to_string(),
                runner_id: Some(RunnerId { value: 10000 }),
                runner_settings,
                channel: channel.cloned(),
                response_type: ResponseType::NoResult as i32,
                periodic_interval: 0,
                retry_policy: None,
                queue_type: QueueType::WithBackup as i32,
                store_success: true,
                store_failure: false,
                use_static: false,
                output_as_stream: false,
            };
            let worker_id = app.worker_app().create(&wd).await?;

            // enqueue job
            let jarg = JobqueueAndCodec::serialize_message(&proto::TestArgs {
                args: vec!["/".to_string()],
            });
            assert_eq!(
                app.redis_job_repository()
                    .count_queue(channel, priority)
                    .await?,
                0
            );
            let (job_id, res, _) = app
                .enqueue_job(
                    Some(&worker_id),
                    None,
                    jarg.clone(),
                    None,
                    0,
                    priority as i32,
                    0,
                    None,
                )
                .await?;
            assert!(job_id.value > 0);
            assert!(res.is_none());
            // job2
            let (job_id2, res2, _) = app
                .enqueue_job(
                    Some(&worker_id),
                    None,
                    jarg.clone(),
                    None,
                    0,
                    priority as i32,
                    0,
                    None,
                )
                .await?;
            assert!(job_id2.value > 0);
            assert!(res2.is_none());

            // get job from redis
            let job = app
                .find_job(&job_id, Some(&Duration::from_millis(100)))
                .await?
                .unwrap();
            assert_eq!(job.id.as_ref().unwrap(), &job_id);
            assert_eq!(
                job.data.as_ref().unwrap().worker_id.as_ref(),
                Some(&worker_id)
            );

            // check job status
            assert_eq!(
                app.job_status_repository()
                    .find_status(&job_id)
                    .await
                    .unwrap(),
                Some(JobStatus::Pending)
            );
            assert_eq!(
                app.job_status_repository()
                    .find_status(&job_id2)
                    .await
                    .unwrap(),
                Some(JobStatus::Pending)
            );

            // no jobs to restore  (exists in both redis and rdb)
            assert_eq!(
                app.redis_job_repository()
                    .count_queue(channel, priority)
                    .await?,
                2
            );
            app.restore_jobs_from_rdb(false, None).await?;
            assert_eq!(
                app.redis_job_repository()
                    .count_queue(channel, priority)
                    .await?,
                2
            );

            // find job for delete (grabbed_until_time is SOme(0) in queue)
            let job_d = app
                .redis_job_repository()
                .find_from_queue(channel, priority, &job_id)
                .await?
                .unwrap();
            // lost only from redis (delete)
            assert!(
                app.redis_job_repository()
                    .delete_from_queue(channel, priority, &job_d)
                    .await?
                    > 0
            );
            assert_eq!(
                app.redis_job_repository()
                    .count_queue(channel, priority)
                    .await?,
                1
            );

            // restore 1 lost jobs
            app.restore_jobs_from_rdb(false, None).await?;
            assert!(app
                .redis_job_repository()
                .find_from_queue(channel, priority, &job_id)
                .await?
                .is_some());
            assert!(app
                .redis_job_repository()
                .find_from_queue(channel, priority, &job_id2)
                .await?
                .is_some());
            assert_eq!(
                app.redis_job_repository()
                    .count_queue(channel, priority)
                    .await?,
                2
            );
            Ok(())
        })
    }
}
