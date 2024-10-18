use crate::app::{UseWorkerConfig, WorkerConfig};
use crate::module::AppConfigModule;

use super::super::worker::{UseWorkerApp, WorkerApp};
use super::super::JobBuilder;
use super::{JobApp, JobCacheKeys};
use anyhow::Result;
use async_trait::async_trait;
use command_utils::util::datetime;
use infra::error::JobWorkerError;
use infra::infra::job::queue::chan::{
    ChanJobQueueRepository, ChanJobQueueRepositoryImpl, UseChanJobQueueRepository,
};
use infra::infra::job::rdb::{RdbJobRepository, UseRdbChanJobRepository};
use infra::infra::job::rows::UseJobqueueAndCodec;
use infra::infra::job::status::{JobStatusRepository, UseJobStatusRepository};
use infra::infra::job_result::pubsub::chan::{
    ChanJobResultPubSubRepositoryImpl, UseChanJobResultPubSubRepository,
};
use infra::infra::job_result::pubsub::JobResultPublisher;
use infra::infra::module::rdb::{RdbChanRepositoryModule, UseRdbChanRepositoryModule};
use infra::infra::{IdGeneratorWrapper, JobQueueConfig, UseIdGenerator, UseJobQueueConfig};
use infra_utils::infra::memory::{MemoryCacheImpl, UseMemoryCache};
use infra_utils::infra::rdb::UseRdbPool;
use proto::jobworkerp::data::{
    Job, JobData, JobId, JobResult, JobResultData, JobResultId, JobStatus, QueueType, ResponseType,
    WorkerData, WorkerId,
};
use std::collections::HashSet;
use std::{sync::Arc, time::Duration};

#[derive(Clone, Debug)]
pub struct RdbChanJobAppImpl {
    app_config_module: Arc<AppConfigModule>,
    id_generator: Arc<IdGeneratorWrapper>,
    repositories: Arc<RdbChanRepositoryModule>,
    worker_app: Arc<dyn WorkerApp + 'static>,
    memory_cache: MemoryCacheImpl<Arc<String>, Vec<Job>>,
}

impl RdbChanJobAppImpl {
    pub fn new(
        app_config_module: Arc<AppConfigModule>,
        id_generator: Arc<IdGeneratorWrapper>,
        repositories: Arc<RdbChanRepositoryModule>,
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
    // TODO
    async fn find_restore_jobs_by(
        &self,
        _job_ids: &HashSet<i64>,
        _channels: &[String],
    ) -> Result<Vec<Job>> {
        Ok(vec![])
    }

    // use find_restore_jobs_by and enqueue them to redis queue
    // TODO
    async fn restore_jobs_by(&self, _job_ids: &HashSet<i64>, _channels: &[String]) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl JobApp for RdbChanJobAppImpl {
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
        if let Some(worker) = worker_res {
            let job_data = JobData {
                worker_id: worker.id,
                arg,
                uniq_key,
                enqueue_time: datetime::now_millis(),
                grabbed_until_time: None,
                run_after_time,
                retried: 0u32,
                priority,
                timeout,
            };
            if let Some(w) = worker.data.as_ref() {
                // TODO validate argument types
                // self.validate_worker_and_job_arg(w, job_data.arg.as_ref())?;
                // cannot wait for direct response
                if run_after_time > 0 && w.response_type == ResponseType::Direct as i32 {
                    return Err(JobWorkerError::InvalidParameter(format!(
                        "run_after_time must be 0 for worker response_type=Direct, must use ListenAfter: {:?}",
                        &job_data
                    ))
                    .into());
                }
                // use rdb queue type for periodic or run after job with hybrid storage type (hybrid queue type is fallback to rdb)
                if w.queue_type == QueueType::Redis as i32
                    && (w.periodic_interval > 0 || self.is_run_after_job_data(&job_data))
                {
                    return Err(JobWorkerError::InvalidParameter(format!(
                        "use queue_type=Rdb or RdbChan for periodic or run_after_time job with RdbChan storage type setting: {:?}",
                        &job_data
                    ))
                    .into());
                };
                let jid = JobId {
                    value: self.id_generator().generate_id()?,
                };
                // job fetched by rdb (periodic job) should be positive run_after_time
                let data = if (w.periodic_interval > 0 || w.queue_type == QueueType::Rdb as i32)
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
                    // use chan only for direct response (not restore)
                    // TODO create backup for queue_type == RdbChan ?
                    let job = Job {
                        id: Some(jid),
                        data: Some(data.to_owned()),
                    };
                    self.enqueue_job_immediately(&job, w).await
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
                        Ok((jid, None))
                    } else {
                        Err(JobWorkerError::RuntimeError(format!(
                            "cannot create record: {:?}",
                            &job
                        ))
                        .into())
                    }
                } else {
                    // normal instant job
                    let job = Job {
                        id: Some(jid),
                        data: Some(data),
                    };
                    if w.queue_type == QueueType::Hybrid as i32 {
                        // instant job (store rdb for failback, and enqueue to chan)
                        // TODO store async to rdb (not necessary to wait)
                        match self.rdb_job_repository().create(&job).await {
                            Ok(_id) => self.enqueue_job_immediately(&job, w).await,
                            Err(e) => Err(e),
                        }
                    } else if w.queue_type == QueueType::Rdb as i32 {
                        // use only rdb queue (not recommended for hybrid storage)
                        let created = self.rdb_job_repository().create(&job).await?;
                        if created {
                            Ok((job.id.unwrap(), None))
                        } else {
                            // storage error?
                            Err(JobWorkerError::RuntimeError(format!(
                                "cannot create record: {:?}",
                                &job
                            ))
                            .into())
                        }
                    } else {
                        // instant job (enqueue to chan only)
                        self.enqueue_job_immediately(&job, w).await
                    }
                }
            } else {
                Err(JobWorkerError::WorkerNotFound(format!(
                    "worker data not found: id = {:?}, name = {:?}",
                    &worker_id, &worker_name
                ))
                .into())
            }
        } else {
            Err(JobWorkerError::WorkerNotFound(format!("name: {:?}", &worker_name)).into())
        }
    }

    // update (re-enqueue) job with id (chan: retry, rdb: update)
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
                // TODO validate argument types
                // self.validate_worker_and_job_arg(&w, data.arg.as_ref())?;

                // use db queue (run after, periodic, queue_type=DB worker)
                let res_db = if is_run_after_job_data
                    || w.periodic_interval > 0
                    || w.queue_type == QueueType::Rdb as i32
                    || w.queue_type == QueueType::Hybrid as i32
                {
                    // XXX should compare grabbed_until_time and update if not changed or not (now not compared)
                    self.rdb_job_repository().update(jid, data).await
                } else {
                    Ok(false)
                };
                // update job status of memory in hybrid
                self.job_status_repository()
                    .upsert_status(jid, &JobStatus::Pending)
                    .await?;
                let res_chan = if !is_run_after_job_data
                    && w.periodic_interval == 0
                    && (w.queue_type == QueueType::Redis as i32 // TODO rename
                        || w.queue_type == QueueType::Hybrid as i32)
                {
                    // enqueue to chan for instant job
                    self.enqueue_job_immediately(job, &w).await.map(|_| true)
                } else {
                    Ok(false)
                };
                let res = res_chan.or(res_db);
                match res {
                    Ok(_updated) => {
                        let _ = self
                            .memory_cache
                            .delete_cache(&Arc::new(Self::find_cache_key(jid)))
                            .await; // ignore error
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

    async fn complete_job(&self, id: &JobResultId, data: &JobResultData) -> Result<bool> {
        tracing::debug!("complete_job: res_id={}", &id.value);
        if let Some(jid) = data.job_id.as_ref() {
            self.job_status_repository().delete_status(jid).await?;
            match ResponseType::try_from(data.response_type) {
                Ok(ResponseType::Direct) => {
                    // send result for direct or listen after response
                    self.chan_job_queue_repository()
                        .enqueue_result_direct(id, data)
                        .await
                }
                Ok(ResponseType::ListenAfter) => {
                    // publish for listening result client
                    self.job_result_pubsub_repository()
                        .publish_result(id, data)
                        .await?;
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
        // delete from db and redis
        match self.rdb_job_repository().delete(id).await {
            Ok(r) => {
                let _ = self
                    .memory_cache
                    .delete_cache(&Arc::new(Self::find_cache_key(id)))
                    .await; // ignore error
                            // TODO delete from chan queue if possible
                Ok(r)
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
        self.memory_cache
            .with_cache(&k, ttl, || async {
                let rv = self
                    .rdb_job_repository()
                    .find(id)
                    .await
                    .map(|r| r.map(|o| vec![o]).unwrap_or_default())?;
                Ok(rv)
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
impl UseRdbChanRepositoryModule for RdbChanJobAppImpl {
    fn rdb_repository_module(&self) -> &RdbChanRepositoryModule {
        &self.repositories
    }
}
impl UseIdGenerator for RdbChanJobAppImpl {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        &self.id_generator
    }
}
impl UseWorkerApp for RdbChanJobAppImpl {
    fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static> {
        &self.worker_app
    }
}
impl JobCacheKeys for RdbChanJobAppImpl {}

impl JobBuilder for RdbChanJobAppImpl {}

impl UseJobQueueConfig for RdbChanJobAppImpl {
    fn job_queue_config(&self) -> &JobQueueConfig {
        &self.app_config_module.job_queue_config
    }
}
impl UseWorkerConfig for RdbChanJobAppImpl {
    fn worker_config(&self) -> &WorkerConfig {
        &self.app_config_module.worker_config
    }
}
impl UseChanJobQueueRepository for RdbChanJobAppImpl {
    fn chan_job_queue_repository(&self) -> &ChanJobQueueRepositoryImpl {
        &self.repositories.chan_job_queue_repository
    }
}
impl RdbChanJobAppHelper for RdbChanJobAppImpl {}
impl UseJobqueueAndCodec for RdbChanJobAppImpl {}
impl UseJobStatusRepository for RdbChanJobAppImpl {
    fn job_status_repository(&self) -> Arc<dyn JobStatusRepository> {
        self.repositories.memory_job_status_repository.clone()
    }
}
impl UseChanJobResultPubSubRepository for RdbChanJobAppImpl {
    fn job_result_pubsub_repository(&self) -> &ChanJobResultPubSubRepositoryImpl {
        &self.repositories.chan_job_result_pubsub_repository
    }
}

// for rdb chan
#[async_trait]
pub trait RdbChanJobAppHelper:
    UseRdbChanJobRepository
    + UseChanJobQueueRepository
    + UseJobStatusRepository
    + JobBuilder
    + UseJobQueueConfig
where
    Self: Sized + 'static,
{
    // for chanbuffer
    async fn enqueue_job_immediately(
        &self,
        job: &Job,
        worker: &WorkerData,
    ) -> Result<(JobId, Option<JobResult>)> {
        let job_id = job.id.unwrap();
        if self.is_run_after_job(job) {
            return Err(JobWorkerError::InvalidParameter(
                "run_after_time is not supported in rdb_chan mode".to_string(),
            )
            .into());
        }
        // use channel to enqueue job immediately
        let res = match self
            .chan_job_queue_repository()
            .enqueue_job(worker.channel.as_ref(), job)
            .await
        {
            Ok(_) => {
                // update status (not use direct response)
                self.job_status_repository()
                    .upsert_status(&job_id, &JobStatus::Pending)
                    .await?;
                // wait for result if direct response type
                if worker.response_type == ResponseType::Direct as i32 {
                    // XXX keep redis connection until response
                    self._wait_job_for_direct_response(
                        &job_id,
                        job.data.as_ref().map(|d| d.timeout),
                    )
                    .await
                    .map(|r| (job_id, Some(r)))
                } else {
                    Ok((job_id, None))
                }
            }
            Err(e) => Err(e),
        }?;
        Ok(res)
    }

    #[inline]
    async fn _wait_job_for_direct_response(
        &self,
        job_id: &JobId,
        timeout: Option<u64>,
    ) -> Result<JobResult> {
        // wait for and return result (with channel)
        self.chan_job_queue_repository()
            .wait_for_result_queue_for_response(job_id, timeout.as_ref()) // no timeout
            .await
    }
}

//TODO
// create test
#[cfg(test)]
mod tests {
    use super::RdbChanJobAppImpl;
    use super::*;
    use crate::app::worker::rdb::RdbWorkerAppImpl;
    use crate::app::{StorageConfig, StorageType};
    use anyhow::Result;
    use command_utils::util::datetime;
    use command_utils::util::option::FlatMap;
    use infra::infra::job::rows::JobqueueAndCodec;
    // use command_utils::util::tracing::tracing_init_test;
    use infra::infra::job_result::pubsub::chan::ChanJobResultPubSubRepositoryImpl;
    use infra::infra::job_result::pubsub::JobResultSubscriber;
    use infra::infra::module::rdb::test::setup_test_rdb_module;
    use infra::infra::plugins::Plugins;
    use infra::infra::IdGeneratorWrapper;
    use infra_utils::infra::test::TEST_RUNTIME;
    use proto::jobworkerp::data::{
        JobResult, JobResultId, Priority, QueueType, ResponseType, ResultOutput, ResultStatus,
        WorkerData, WorkerSchemaId,
    };
    use std::sync::Arc;

    fn create_test_app(
        use_mock_id: bool,
    ) -> Result<(RdbChanJobAppImpl, ChanJobResultPubSubRepositoryImpl)> {
        let rdb_module = setup_test_rdb_module();
        TEST_RUNTIME.block_on(async {
            let repositories = Arc::new(rdb_module);
            // mock id generator (generate 1 until called set method)
            let id_generator = if use_mock_id {
                Arc::new(IdGeneratorWrapper::new_mock())
            } else {
                Arc::new(IdGeneratorWrapper::new())
            };
            let mc_config = infra_utils::infra::memory::MemoryCacheConfig {
                num_counters: 10000,
                max_cost: 10000,
                use_metrics: false,
            };
            let worker_memory_cache = infra_utils::infra::memory::MemoryCacheImpl::new(
                &mc_config,
                Some(Duration::from_secs(5 * 60)),
            );
            let job_memory_cache = infra_utils::infra::memory::MemoryCacheImpl::new(
                &mc_config,
                Some(Duration::from_secs(60)),
            );
            let storage_config = Arc::new(StorageConfig {
                r#type: StorageType::RDB,
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
            let worker_app = RdbWorkerAppImpl::new(
                storage_config.clone(),
                id_generator.clone(),
                worker_memory_cache,
                repositories.clone(),
            );

            let mut plugins = Plugins::new();
            plugins.load_plugin_files_from_env().await?;
            let config_module = Arc::new(AppConfigModule {
                storage_config,
                worker_config,
                job_queue_config: job_queue_config.clone(),
                plugins: Arc::new(plugins),
            });
            let subscrber = repositories.chan_job_result_pubsub_repository.clone();
            Ok((
                RdbChanJobAppImpl::new(
                    config_module,
                    id_generator,
                    repositories,
                    Arc::new(worker_app),
                    job_memory_cache,
                ),
                subscrber,
            ))
        })
    }

    #[test]
    fn test_create_direct_job_complete() -> Result<()> {
        // tracing_init_test(tracing::Level::DEBUG);
        // enqueue, find, complete, find, delete, find
        let (app, _) = create_test_app(true)?;
        TEST_RUNTIME.block_on(async {
            let operation =
                JobqueueAndCodec::serialize_message(&proto::jobworkerp::data::CommandOperation {
                    name: "ls".to_string(),
                });
            let wd = WorkerData {
                name: "testworker".to_string(),
                schema_id: Some(WorkerSchemaId { value: 123 }),
                operation,
                channel: None,
                response_type: ResponseType::Direct as i32,
                periodic_interval: 0,
                retry_policy: None,
                queue_type: QueueType::Redis as i32,
                store_failure: false,
                store_success: false,
                next_workers: vec![],
                use_static: false,
            };
            let worker_id = app.worker_app().create(&wd).await?;
            let jarg = JobqueueAndCodec::serialize_message(&proto::jobworkerp::data::CommandArg {
                args: vec!["/".to_string()],
            });
            // move
            let worker_id1 = worker_id;
            let jarg1 = jarg.clone();
            let app1 = app.clone();
            // need waiting for direct response
            let jh = tokio::spawn(async move {
                let res = app1
                    .enqueue_job(Some(&worker_id1), None, jarg1, None, 0, 0, 0)
                    .await;
                tracing::info!("!!!res: {:?}", res);
                let (jid, job_res) = res.unwrap();
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
                    arg: jarg,
                    uniq_key: None,
                    status: ResultStatus::Success as i32,
                    output: Some(ResultOutput {
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
                app.complete_job(&rid, result.data.as_ref().unwrap())
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
        // tracing_init_test(tracing::Level::DEBUG);
        // enqueue, find, complete, find, delete, find
        let (app, subscriber) = create_test_app(true)?;
        TEST_RUNTIME.block_on(async {
            let operation =
                JobqueueAndCodec::serialize_message(&proto::jobworkerp::data::CommandOperation {
                    name: "ls".to_string(),
                });
            let wd = WorkerData {
                name: "testworker".to_string(),
                schema_id: Some(WorkerSchemaId { value: 123 }),
                operation,
                channel: None,
                response_type: ResponseType::ListenAfter as i32,
                periodic_interval: 0,
                retry_policy: None,
                queue_type: QueueType::Hybrid as i32,
                store_failure: true,
                store_success: true,
                next_workers: vec![],
                use_static: false,
            };
            let worker_id = app.worker_app().create(&wd).await?;
            let jarg = JobqueueAndCodec::serialize_message(&proto::jobworkerp::data::CommandArg {
                args: vec!["/".to_string()],
            });

            // wait for direct response
            let job_id = app
                .enqueue_job(Some(&worker_id), None, jarg.clone(), None, 0, 0, 0)
                .await?
                .0;
            let job = Job {
                id: Some(job_id),
                data: Some(JobData {
                    worker_id: Some(worker_id),
                    arg: jarg.clone(),
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
                    arg: jarg,
                    uniq_key: None,
                    status: ResultStatus::Success as i32,
                    output: Some(ResultOutput {
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
                    store_success: true,
                    store_failure: true,
                }),
            };
            let jid = job_id;
            let res = result.clone();
            // waiting for receiving job result at first (as same as job result listening process)
            let jh = tokio::task::spawn(async move {
                let job_result = subscriber.subscribe_result(&jid, Some(6000)).await.unwrap();
                assert!(job_result.id.unwrap().value > 0);
                assert_eq!(&job_result.data, &res.data);
            });
            // pseudo process time
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert!(
                // send mock result as completed job result
                app.complete_job(result.id.as_ref().unwrap(), result.data.as_ref().unwrap())
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
        // enqueue, find, complete, find, delete, find
        let (app, _) = create_test_app(true)?;
        let operation =
            JobqueueAndCodec::serialize_message(&proto::jobworkerp::data::CommandOperation {
                name: "ls".to_string(),
            });
        let wd = WorkerData {
            name: "testworker".to_string(),
            schema_id: Some(WorkerSchemaId { value: 123 }),
            operation,
            channel: None,
            response_type: ResponseType::NoResult as i32,
            periodic_interval: 0,
            retry_policy: None,
            queue_type: QueueType::Hybrid as i32,
            store_success: true,
            store_failure: false,
            next_workers: vec![],
            use_static: false,
        };
        TEST_RUNTIME.block_on(async {
            let worker_id = app.worker_app().create(&wd).await?;
            let jarg = JobqueueAndCodec::serialize_message(&proto::jobworkerp::data::CommandArg {
                args: vec!["/".to_string()],
            });

            // wait for direct response
            let (job_id, res) = app
                .enqueue_job(Some(&worker_id), None, jarg.clone(), None, 0, 0, 0)
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
                    arg: jarg,
                    uniq_key: None,
                    status: ResultStatus::Success as i32,
                    output: Some(ResultOutput {
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
                app.complete_job(result.id.as_ref().unwrap(), result.data.as_ref().unwrap())
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

    // TODO not implemented for rdb chan
    #[test]
    fn test_restore_jobs_from_rdb() -> Result<()> {
        // tracing_init_test(tracing::Level::DEBUG);
        let priority = Priority::Medium;
        let channel: Option<&String> = None;

        let (app, _) = create_test_app(false)?;
        TEST_RUNTIME.block_on(async {
            // create command worker with hybrid queue
            let operation =
                JobqueueAndCodec::serialize_message(&proto::jobworkerp::data::CommandOperation {
                    name: "ls".to_string(),
                });
            let wd = WorkerData {
                name: "testworker".to_string(),
                schema_id: Some(WorkerSchemaId { value: 123 }),
                operation,
                channel: channel.cloned(),
                response_type: ResponseType::NoResult as i32,
                periodic_interval: 0,
                retry_policy: None,
                queue_type: QueueType::Hybrid as i32, // with chan queue
                store_success: true,
                store_failure: false,
                next_workers: vec![],
                use_static: false,
            };
            let worker_id = app.worker_app().create(&wd).await?;

            // enqueue job
            let jarg = JobqueueAndCodec::serialize_message(&proto::jobworkerp::data::CommandArg {
                args: vec!["/".to_string()],
            });
            assert_eq!(
                app.chan_job_queue_repository()
                    .count_queue(channel, priority)
                    .await?,
                0
            );
            let (job_id, res) = app
                .enqueue_job(
                    Some(&worker_id),
                    None,
                    jarg.clone(),
                    None,
                    0,
                    priority as i32,
                    0,
                )
                .await?;
            assert!(job_id.value > 0);
            assert!(res.is_none());
            // job2
            let (job_id2, res2) = app
                .enqueue_job(
                    Some(&worker_id),
                    None,
                    jarg.clone(),
                    None,
                    0,
                    priority as i32,
                    0,
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
            assert_eq!(
                app.chan_job_queue_repository()
                    .count_queue(channel, priority)
                    .await?,
                2
            );
            println!(
                "==== statuses: {:?}",
                app.job_status_repository().find_status_all().await.unwrap()
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

            // // no jobs to restore  (exists in both redis and rdb)
            // assert_eq!(
            //     app.rdb_job_repository()
            //         .count_queue(channel, priority)
            //         .await?,
            //     2
            // );
            // app.restore_jobs_from_rdb(false, None).await?;
            // assert_eq!(
            //     app.rdb_job_repository()
            //         .count_queue(channel, priority)
            //         .await?,
            //     2
            // );

            // // find job for delete (grabbed_until_time is SOme(0) in queue)
            // let job_d = app
            //     .rdb_job_repository()
            //     .find_from_queue(channel, priority, &job_id)
            //     .await?
            //     .unwrap();
            // // lost only from redis (delete)
            // assert!(
            //     app.rdb_job_repository()
            //         .delete_from_queue(channel, priority, &job_d)
            //         .await?
            //         > 0
            // );
            // assert_eq!(
            //     app.rdb_job_repository()
            //         .count_queue(channel, priority)
            //         .await?,
            //     1
            // );
            //
            // // restore 1 lost jobs
            // app.restore_jobs_from_rdb(false, None).await?;
            // assert!(app
            //     .rdb_job_repository()
            //     .find_from_queue(channel, priority, &job_id)
            //     .await?
            //     .is_some());
            // assert!(app
            //     .rdb_job_repository()
            //     .find_from_queue(channel, priority, &job_id2)
            //     .await?
            //     .is_some());
            // assert_eq!(
            //     app.rdb_job_repository()
            //         .count_queue(channel, priority)
            //         .await?,
            //     2
            // );
            Ok(())
        })
    }
}
