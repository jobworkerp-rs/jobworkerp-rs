use super::super::worker::{UseWorkerApp, WorkerApp};
use super::super::{StorageConfig, UseStorageConfig};
use super::{JobResultApp, JobResultAppHelper};
use anyhow::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
use infra::infra::job_result::pubsub::redis::{
    RedisJobResultPubSubRepositoryImpl, UseRedisJobResultPubSubRepository,
};
use infra::infra::job_result::pubsub::JobResultSubscriber;
use infra::infra::job_result::rdb::{RdbJobResultRepository, UseRdbJobResultRepository};
use infra::infra::job_result::redis::{RedisJobResultRepository, UseRedisJobResultRepository};
use infra::infra::module::rdb::{RdbChanRepositoryModule, UseRdbChanRepositoryModule};
use infra::infra::module::redis::{RedisRepositoryModule, UseRedisRepositoryModule};
use infra::infra::module::HybridRepositoryModule;
use infra::infra::{IdGeneratorWrapper, UseIdGenerator};
use infra_utils::infra::rdb::UseRdbPool;
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{
    JobId, JobResult, JobResultData, JobResultId, JobResultSortField, ResultOutputItem,
    ResultStatus, Worker, WorkerId,
};
use std::pin::Pin;
use std::sync::Arc;
use tokio_stream::Stream;

#[derive(Clone, Debug)]
pub struct HybridJobResultAppImpl {
    storage_config: Arc<StorageConfig>,
    repositories: Arc<HybridRepositoryModule>,
    worker_app: Arc<dyn WorkerApp + 'static>,
    id_generator: Arc<IdGeneratorWrapper>,
}

impl HybridJobResultAppImpl {
    pub fn new(
        storage_config: Arc<StorageConfig>,
        id_generator: Arc<IdGeneratorWrapper>,
        repositories: Arc<HybridRepositoryModule>,
        worker_app: Arc<dyn WorkerApp + 'static>,
    ) -> Self {
        Self {
            storage_config,
            repositories,
            worker_app,
            id_generator,
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
                    match self
                        .rdb_job_result_repository()
                        .find_latest_by_job_id(job_id)
                        .await
                    {
                        Ok(r) => self._fill_job_result(r).await,
                        Err(e) => Err(e),
                    }
                }
            }
            Err(e) => Err(e),
        }
    }

    async fn subscribe_result_with_check(
        &self,
        job_id: &JobId,
        timeout: Option<&u64>,
        request_streaming: bool,
    ) -> Result<(JobResult, Option<BoxStream<'static, ResultOutputItem>>)> {
        if request_streaming {
            // IMPORTANT: For streaming, subscribe to stream BEFORE waiting for result.
            // If we wait for result first (subscribe_result blocks until job completes),
            // the stream data may have already been published and missed.
            // Use tokio::join! to subscribe to both in parallel.
            let result_future = self
                .job_result_pubsub_repository()
                .subscribe_result(job_id, timeout.copied());
            let stream_future = self
                .job_result_pubsub_repository()
                .subscribe_result_stream(job_id, timeout.copied());

            let (result_res, stream_res) = tokio::join!(result_future, stream_future);
            let res = result_res?;
            let stream = stream_res.ok(); // Stream subscription may fail, but result is primary

            Ok((res, stream))
        } else {
            // Non-streaming: wait for result data (long polling with grpc)
            let res = self
                .job_result_pubsub_repository()
                .subscribe_result(job_id, timeout.copied())
                .await?;
            Ok((res, None))
        }
    }
}

#[async_trait]
impl JobResultApp for HybridJobResultAppImpl {
    async fn create_job_result_if_necessary(
        &self,
        id: &JobResultId,
        data: &JobResultData,
        broadcast_results: bool,
    ) -> Result<bool> {
        // store result to rdb
        let in_db = if Self::_should_store(data) {
            self.rdb_job_result_repository().create(id, data).await?
        } else {
            false
        };
        // always store cache for ended job result in success or failure
        //     : cache for listen_after
        if data.status != ResultStatus::ErrorAndRetry as i32 && broadcast_results {
            self.redis_job_result_repository()
                .upsert_only_by_job_id(id, data)
                .await
                .map(|r| r || in_db)
        } else {
            Ok(in_db)
        }
    }

    async fn delete_job_result(&self, id: &JobResultId) -> Result<bool> {
        self.rdb_job_result_repository().delete(id).await
    }

    // find only from rdb
    async fn find_job_result_from_db(&self, id: &JobResultId) -> Result<Option<JobResult>>
    where
        Self: Send + 'static,
    {
        match self.rdb_job_result_repository().find(id).await {
            Ok(res) => self._fill_job_result(res).await,
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

    // can listen until expired job_id cache in redis or store_success
    // XXX same as hybrid
    async fn listen_result(
        &self,
        job_id: &JobId,
        worker_id: Option<&WorkerId>,
        worker_name: Option<&String>,
        timeout: Option<u64>,
        request_streaming: bool,
    ) -> Result<(JobResult, Option<BoxStream<'static, ResultOutputItem>>)>
    where
        Self: Send + 'static,
    {
        // get worker data
        if let Worker {
            id: Some(wid),
            data: Some(wd),
        } = self
            .worker_app
            .find_by_id_or_name(worker_id, worker_name)
            .await?
        {
            if !wd.broadcast_results {
                return Err(JobWorkerError::InvalidParameter(format!(
                    "Cannot listen result not broadcast worker: {:?}",
                    &wd
                ))
                .into());
            }
            // check request streaming
            self.worker_app()
                .check_worker_streaming(&wid, request_streaming)
                .await?;
            // check job result (already finished or not)
            let res = self.find_job_result_by_job_id(job_id).await?;
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
        } else {
            Err(JobWorkerError::WorkerNotFound(
                "cannot listen job which worker is None".to_string(),
            )
            .into())
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
        // Check if result already exists
        let res = self.find_job_result_by_job_id(job_id).await?;
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
        let Worker {
            id: worker_id,
            data: wd,
        } = self
            .worker_app
            .find_by_id_or_name(worker_id, worker_name)
            .await?;
        let wid = worker_id.ok_or(JobWorkerError::NotFound("worker id not found".to_string()))?;
        let wd = wd.ok_or(JobWorkerError::NotFound(
            "worker data not found".to_string(),
        ))?;
        if !wd.broadcast_results {
            return Err(JobWorkerError::InvalidParameter(format!(
                "Cannot listen result not broadcast worker: {:?}",
                &wd
            ))
            .into());
        }
        // if wd.response_type == ResponseType::Direct as i32 {
        //     return Err(JobWorkerError::InvalidParameter(format!(
        //         "Cannot listen result for direct response: {:?}",
        //         &wd
        //     ))
        //     .into());
        // }
        self.job_result_pubsub_repository()
            .subscribe_result_stream_by_worker(wid)
            .await
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
        // Always use RDB repository for both Standalone and Scalable modes
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
        // Always use RDB repository
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
        // See docs/admin/spec-job-result-service.md Section 6.1

        tracing::info!(
            deleted_count = deleted_count,
            "DeleteBulk completed in HybridJobResultApp"
        );

        Ok(deleted_count)
    }
}

impl UseRedisRepositoryModule for HybridJobResultAppImpl {
    fn redis_repository_module(&self) -> &RedisRepositoryModule {
        &self.repositories.redis_module
    }
}

impl UseStorageConfig for HybridJobResultAppImpl {
    fn storage_config(&self) -> &StorageConfig {
        &self.storage_config
    }
}
impl UseIdGenerator for HybridJobResultAppImpl {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        &self.id_generator
    }
}
impl UseRdbChanRepositoryModule for HybridJobResultAppImpl {
    fn rdb_repository_module(&self) -> &RdbChanRepositoryModule {
        &self.repositories.rdb_chan_module
    }
}
impl UseWorkerApp for HybridJobResultAppImpl {
    fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static> {
        &self.worker_app
    }
}
impl JobResultAppHelper for HybridJobResultAppImpl {}
impl UseRedisJobResultPubSubRepository for HybridJobResultAppImpl {
    fn job_result_pubsub_repository(&self) -> &RedisJobResultPubSubRepositoryImpl {
        &self
            .repositories
            .redis_module
            .redis_job_result_pubsub_repository
    }
}

//TODO
// create test
#[cfg(any(test, feature = "test-utils"))]
pub mod tests {
    use super::HybridJobResultAppImpl;
    use crate::app::runner::hybrid::HybridRunnerAppImpl;
    use crate::app::runner::RunnerApp;
    use crate::app::worker::hybrid::HybridWorkerAppImpl;
    use crate::app::{StorageConfig, StorageType};
    use crate::module::test::TEST_PLUGIN_DIR;
    use anyhow::Result;
    use infra::infra::module::rdb::test::setup_test_rdb_module;
    use infra::infra::module::redis::test::setup_test_redis_module;
    use infra::infra::module::HybridRepositoryModule;
    use infra::infra::IdGeneratorWrapper;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn test_should_store() {
        use crate::app::job_result::JobResultAppHelper;
        use command_utils::util::datetime;
        use infra::infra::job::rows::JobqueueAndCodec;
        use infra::infra::job::rows::UseJobqueueAndCodec;

        let args = JobqueueAndCodec::serialize_message(&proto::TestArgs {
            args: vec!["test".to_string()],
        });
        let mut job_result_data = proto::jobworkerp::data::JobResultData {
            job_id: None,
            worker_id: None,
            status: proto::jobworkerp::data::ResultStatus::Success as i32,
            worker_name: "".to_string(),
            args,
            uniq_key: None,
            output: Some(proto::jobworkerp::data::ResultOutput {
                items: b"data".to_vec(),
            }),
            retried: 0,
            max_retry: 0,
            priority: proto::jobworkerp::data::Priority::High as i32,
            timeout: 0,
            streaming_type: 0,
            enqueue_time: datetime::now_millis(),
            run_after_time: 0,
            response_type: proto::jobworkerp::data::ResponseType::NoResult as i32,
            start_time: datetime::now_millis(),
            end_time: datetime::now_millis(),
            store_success: false,
            store_failure: false,
            using: None,
        };
        assert!(!HybridJobResultAppImpl::_should_store(&job_result_data));

        job_result_data.status = proto::jobworkerp::data::ResultStatus::ErrorAndRetry as i32;
        assert!(!HybridJobResultAppImpl::_should_store(&job_result_data));

        job_result_data.store_success = true;
        job_result_data.status = proto::jobworkerp::data::ResultStatus::Success as i32;
        assert!(HybridJobResultAppImpl::_should_store(&job_result_data));
        job_result_data.status = proto::jobworkerp::data::ResultStatus::ErrorAndRetry as i32;
        assert!(!HybridJobResultAppImpl::_should_store(&job_result_data));

        job_result_data.store_failure = true;
        job_result_data.status = proto::jobworkerp::data::ResultStatus::ErrorAndRetry as i32;
        assert!(HybridJobResultAppImpl::_should_store(&job_result_data));

        job_result_data.store_success = false;
        job_result_data.status = proto::jobworkerp::data::ResultStatus::Success as i32;
        assert!(!HybridJobResultAppImpl::_should_store(&job_result_data));
    }

    pub async fn create_test_app() -> Result<HybridJobResultAppImpl> {
        // dotenv::dotenv().ok();
        let rdb_module = setup_test_rdb_module(false).await;
        let redis_module = setup_test_redis_module().await;
        let repositories = Arc::new(HybridRepositoryModule {
            redis_module,
            rdb_chan_module: rdb_module,
        });
        let id_generator = Arc::new(IdGeneratorWrapper::new());
        let moka_config = memory_utils::cache::moka::MokaCacheConfig {
            num_counters: 10000,
            ttl: Some(Duration::from_secs(5 * 60)), // 5 minutes
        };
        let storage_config = Arc::new(StorageConfig {
            r#type: StorageType::Scalable,
            restore_at_startup: Some(false),
        });
        let descriptor_cache =
            Arc::new(memory_utils::cache::moka::MokaCacheImpl::new(&moka_config));
        let runner_app = Arc::new(HybridRunnerAppImpl::new(
            TEST_PLUGIN_DIR.to_string(),
            storage_config.clone(),
            &moka_config,
            repositories.clone(),
            descriptor_cache.clone(),
            id_generator.clone(),
        ));
        runner_app.load_runner().await.unwrap();
        let worker_app = Arc::new(HybridWorkerAppImpl::new(
            storage_config.clone(),
            id_generator.clone(),
            &moka_config,
            repositories.clone(),
            descriptor_cache.clone(),
            runner_app,
        ));
        Ok(HybridJobResultAppImpl::new(
            storage_config,
            id_generator.clone(),
            repositories,
            worker_app,
        ))
    }
    #[test]
    fn test_create_job_result_if_necessary() -> Result<()> {
        use super::*;
        use command_utils::util::datetime;
        use infra::infra::job::rows::JobqueueAndCodec;
        use infra::infra::job::rows::UseJobqueueAndCodec;
        use infra_utils::infra::test::TEST_RUNTIME;
        use jobworkerp_runner::jobworkerp::runner::CommandArgs;
        use proto::jobworkerp::data::{ResponseType, WorkerData};

        let runner_settings = vec![];
        let worker_data = WorkerData {
            name: "test".to_string(),
            description: "desc1".to_string(),
            runner_id: Some(proto::jobworkerp::data::RunnerId { value: 1 }),
            runner_settings,
            retry_policy: None,
            periodic_interval: 0,
            channel: Some("hoge".to_string()),
            queue_type: proto::jobworkerp::data::QueueType::DbOnly as i32,
            response_type: ResponseType::Direct as i32,
            store_success: true,
            store_failure: true,
            use_static: false,
            broadcast_results: false,
        };
        TEST_RUNTIME.block_on(async {
            let app = create_test_app().await?;
            let worker_id = app.worker_app().create(&worker_data).await?;
            let worker = Worker {
                id: Some(worker_id),
                data: Some(worker_data.clone()),
            };

            // app.worker_app.create(&worker_data).await?;
            let id = JobResultId {
                value: app.id_generator().generate_id()?,
            };
            let job_id = JobId { value: 100 };
            let args = JobqueueAndCodec::serialize_message(&CommandArgs {
                command: "echo".to_string(),
                args: vec!["arg1".to_string()],
                with_memory_monitoring: false,
            });
            let mut data = JobResultData {
                job_id: Some(job_id),
                worker_id: worker.id,
                status: ResultStatus::Success as i32,
                worker_name: worker_data.name.clone(),
                args,
                uniq_key: Some("uniq_key".to_string()),
                output: Some(proto::jobworkerp::data::ResultOutput {
                    items: b"data".to_vec(),
                }),
                retried: 0,
                max_retry: worker_data.retry_policy.map(|p| p.max_retry).unwrap_or(0),
                priority: proto::jobworkerp::data::Priority::High as i32,
                timeout: 0,
                streaming_type: 0,
                enqueue_time: datetime::now_millis(),
                run_after_time: 0,
                response_type: worker_data.response_type,
                start_time: datetime::now_millis(),
                end_time: datetime::now_millis(),
                store_success: worker_data.store_success,
                store_failure: worker_data.store_failure,
                using: None,
            };
            let result = JobResult {
                id: Some(id),
                data: Some(data.clone()),
                ..Default::default()
            };
            assert!(
                app.create_job_result_if_necessary(&id, &data, worker_data.broadcast_results)
                    .await?
            );
            assert_eq!(app.find_job_result_from_db(&id).await?.unwrap(), result);
            assert_eq!(
                app.find_job_result_by_job_id(&job_id).await?.unwrap(),
                result
            );
            // in retry
            let id = JobResultId {
                value: app.id_generator().generate_id()?,
            };
            let job_id = JobId { value: 101 };
            data.status = ResultStatus::ErrorAndRetry as i32;
            data.job_id = Some(job_id);
            let result = JobResult {
                id: Some(id),
                data: Some(data.clone()),
                ..Default::default()
            };
            assert!(
                app.create_job_result_if_necessary(&id, &data, worker_data.broadcast_results)
                    .await?
            );
            assert_eq!(app.find_job_result_from_db(&id).await?.unwrap(), result);
            // found from db
            assert_eq!(
                app.find_job_result_by_job_id(&job_id).await?.unwrap(),
                result
            );

            // no store, store in redis
            let id = JobResultId {
                value: app.id_generator().generate_id()?,
            };
            let job_id = JobId { value: 202 };
            data.status = ResultStatus::FatalError as i32;
            // no store to db
            data.store_failure = false;
            data.job_id = Some(job_id);
            data.response_type = ResponseType::NoResult as i32;
            let result = JobResult {
                id: Some(id),
                data: Some(data.clone()),
                ..Default::default()
            };
            // store only to redis for listen after (broadcast_results = true)
            assert!(app.create_job_result_if_necessary(&id, &data, true).await?);
            assert_eq!(app.find_job_result_from_db(&id).await?, None);
            // store ended result in cache for listen_after
            let mut res = app.find_job_result_by_job_id(&job_id).await?.unwrap();
            // restore store_failure by worker data, rewrite for skip in test
            let mut d = res.data.unwrap().clone();
            d.store_failure = false;
            d.response_type = ResponseType::NoResult as i32;
            res.data = Some(d);
            assert_eq!(res, result);

            // no store
            let id = JobResultId {
                value: app.id_generator().generate_id()?,
            };
            let job_id = JobId { value: 303 };
            data.status = ResultStatus::ErrorAndRetry as i32;
            data.job_id = Some(job_id);
            assert!(
                !(app
                    .create_job_result_if_necessary(&id, &data, worker_data.broadcast_results)
                    .await?),
                "no store"
            );
            assert_eq!(app.find_job_result_from_db(&id).await?, None);
            // no store retrying result in cache
            assert_eq!(app.find_job_result_by_job_id(&job_id).await?, None);

            Ok(())
        })
    }

    #[test]
    fn test_error_listen_result() -> Result<()> {
        use super::*;
        use infra_utils::infra::test::TEST_RUNTIME;
        use proto::jobworkerp::data::{ResponseType, WorkerData};

        let runner_settings = vec![];
        let worker_data = WorkerData {
            name: "test".to_string(),
            description: "desc1".to_string(),
            runner_id: Some(proto::jobworkerp::data::RunnerId { value: 1 }),
            runner_settings,
            retry_policy: None,
            periodic_interval: 0,
            channel: Some("hoge".to_string()),
            queue_type: proto::jobworkerp::data::QueueType::DbOnly as i32,
            response_type: ResponseType::Direct as i32,
            store_success: true,
            store_failure: true,
            use_static: false,
            broadcast_results: false,
        };
        TEST_RUNTIME.block_on(async {
            let app = create_test_app().await?;
            let worker_id = app.worker_app().create(&worker_data).await?;
            // let worker = Worker {
            //     id: Some(worker_id),
            //     data: Some(worker_data.clone()),
            // };

            // // app.worker_app.create(&worker_data).await?;
            // let id = JobResultId {
            //     value: app.id_generator().generate_id()?,
            // };
            let job_id = JobId { value: 100 };

            let res = app
                .listen_result(
                    &job_id,
                    Some(&worker_id),
                    Some(&worker_data.name),
                    None,
                    false,
                )
                .await;
            assert!(res.is_err());

            let res = app
                .listen_result_stream_by_worker(Some(&worker_id), Some(&worker_data.name))
                .await;
            assert!(res.is_err());
            Ok(())
        })
    }

    // #[tokio::test]
    // async fn test_wait_for_result_data_with_check_response_type() -> Result<(), anyhow::Error> {
    //     let redis_client = RedisClientWrapper::new("redis://127.0.0.1:6379/0").await?;
    //     let redis_job_repository = RedisJobQueueRepositoryImpl::new(redis_client.clone());
    //     let redis_repository_module = RedisRepositoryModule::new(redis_client.clone());
    //     let redis_job_repository = Arc::new(redis_job_repository);
    //     let redis_repository_module = Arc::new(redis_repository_module);
    //     let hybrid_job_result_app = HybridJobResultAppImpl::new(
    //         StorageConfig::default(),
    //         IdGeneratorWrapper::new(),
    //         RedisAndRdbRepositoryModule::new(
    //             redis_repository_module.clone(),
    //             RdbRepositoryModule::new(),
    //         ),
    //         Arc::new(MockJobApp {}),
    //         Arc::new(MockWorkerApp {}),
    //     );
    //     let job_id = JobId::new();
    //     let worker_id = WorkerId::new();
    //     let result = hybrid_job_result_app
    //         ._wait_for_result_data_with_check_response_type(&job_id, Some(&worker_id))
    //         .await;
    //     assert!(result.is_err());
    //     Ok(())
    // }

    // struct MockJobApp {}

    // #[async_trait]
    // impl JobApp for MockJobApp {
    //     async fn find(&self, _id: &JobId) -> Result<Option<Job>, anyhow::Error> {
    //         Ok(None)
    //     }
    // }

    // struct MockWorkerApp {}

    // #[async_trait]
    // impl WorkerApp for MockWorkerApp {
    //     async fn find(&self, _id: &WorkerId) -> Result<Option<Worker>, anyhow::Error> {
    //         Ok(None)
    //     }
    // }
}
