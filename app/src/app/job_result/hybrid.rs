use super::super::worker::{UseWorkerApp, WorkerApp};
use super::super::{StorageConfig, UseStorageConfig};
use super::pubsub::JobResultSubscribeApp;
use super::{JobResultApp, JobResultAppHelper};
use anyhow::Result;
use async_trait::async_trait;
use infra::error::JobWorkerError;
use infra::infra::job::rows::UseJobqueueAndCodec;
use infra::infra::job_result::rdb::{RdbJobResultRepository, UseRdbJobResultRepository};
use infra::infra::job_result::redis::{RedisJobResultRepository, UseRedisJobResultRepository};
use infra::infra::module::rdb::{RdbRepositoryModule, UseRdbRepositoryModule};
use infra::infra::module::redis::{RedisRepositoryModule, UseRedisRepositoryModule};
use infra::infra::module::HybridRepositoryModule;
use infra::infra::{IdGeneratorWrapper, UseIdGenerator};
use infra_utils::infra::rdb::UseRdbPool;
use infra_utils::infra::redis::{RedisClient, UseRedisClient};
use proto::jobworkerp::data::{
    JobId, JobResult, JobResultData, JobResultId, ResponseType, ResultStatus, WorkerData, WorkerId,
};
use std::sync::Arc;

#[derive(Clone)]
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

    pub async fn subscribe_result_with_check(
        &self,
        job_id: &JobId,
        wdata: &WorkerData,
        timeout: Option<&u64>,
    ) -> Result<JobResult> {
        if wdata.response_type != ResponseType::ListenAfter as i32 {
            Err(JobWorkerError::InvalidParameter(
                "cannot listen job which response_type isnot ListenAfter".to_string(),
            )
            .into())
        } else {
            // wait for result data (long polling with grpc (keep connection)))
            self.subscribe_result(job_id, timeout).await
        }
    }
}

#[async_trait]
impl JobResultApp for HybridJobResultAppImpl {
    async fn create_job_result_if_necessary(
        &self,
        id: &JobResultId,
        data: &JobResultData,
    ) -> Result<bool> {
        // store result to rdb
        let in_db = if Self::_should_store(data) {
            self.rdb_job_result_repository().create(id, data).await?
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
        // TODO cache
        self.rdb_job_result_repository()
            .count_list_tx(self.rdb_job_result_repository().db_pool())
            .await
    }
}

impl UseRedisRepositoryModule for HybridJobResultAppImpl {
    fn redis_repository_module(&self) -> &RedisRepositoryModule {
        &self.repositories.redis_module
    }
}
impl UseRedisClient for HybridJobResultAppImpl {
    fn redis_client(&self) -> &RedisClient {
        &self.redis_repository_module().redis_client
    }
}
impl UseJobqueueAndCodec for HybridJobResultAppImpl {}
impl JobResultSubscribeApp for HybridJobResultAppImpl {}

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
impl UseRdbRepositoryModule for HybridJobResultAppImpl {
    fn rdb_repository_module(&self) -> &RdbRepositoryModule {
        &self.repositories.rdb_module
    }
}
impl UseWorkerApp for HybridJobResultAppImpl {
    fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static> {
        &self.worker_app
    }
}
impl JobResultAppHelper for HybridJobResultAppImpl {}

//TODO
// create test
#[cfg(test)]
mod tests {
    use super::HybridJobResultAppImpl;
    use super::JobResultApp;
    use super::*;
    use crate::app::worker::hybrid::HybridWorkerAppImpl;
    use crate::app::{StorageConfig, StorageType};
    use anyhow::Result;
    use command_utils::util::datetime;
    use infra::infra::module::rdb::test::setup_test_rdb_module;
    use infra::infra::module::redis::test::setup_test_redis_module;
    use infra::infra::module::HybridRepositoryModule;
    use infra::infra::IdGeneratorWrapper;
    use infra_utils::infra::test::TEST_RUNTIME;
    use proto::jobworkerp::data::worker_operation::Operation;
    use proto::jobworkerp::data::Priority;
    use proto::jobworkerp::data::QueueType;
    use proto::jobworkerp::data::ResultOutput;
    use proto::jobworkerp::data::RunnerArg;
    use proto::jobworkerp::data::WorkerOperation;
    use proto::jobworkerp::data::{
        JobId, JobResult, ResponseType, ResultStatus, RunnerType, Worker, WorkerData,
    };
    use std::sync::Arc;

    #[test]
    fn test_should_store() {
        let arg = RunnerArg {
            data: Some(proto::jobworkerp::data::runner_arg::Data::Plugin(
                proto::jobworkerp::data::PluginArg {
                    arg: b"test".to_vec(),
                },
            )),
        };
        let mut job_result_data = JobResultData {
            job_id: None,
            worker_id: None,
            status: ResultStatus::Success as i32,
            worker_name: "".to_string(),
            arg: Some(arg),
            uniq_key: None,
            output: Some(ResultOutput {
                items: vec![b"data".to_vec()],
            }),
            retried: 0,
            max_retry: 0,
            priority: Priority::High as i32,
            timeout: 0,
            enqueue_time: datetime::now_millis(),
            run_after_time: 0,
            response_type: ResponseType::NoResult as i32,
            start_time: datetime::now_millis(),
            end_time: datetime::now_millis(),
            store_success: false,
            store_failure: false,
        };
        assert!(!HybridJobResultAppImpl::_should_store(&job_result_data));

        job_result_data.status = ResultStatus::ErrorAndRetry as i32;
        assert!(!HybridJobResultAppImpl::_should_store(&job_result_data));

        job_result_data.store_success = true;
        job_result_data.status = ResultStatus::Success as i32;
        assert!(HybridJobResultAppImpl::_should_store(&job_result_data));
        job_result_data.status = ResultStatus::ErrorAndRetry as i32;
        assert!(!HybridJobResultAppImpl::_should_store(&job_result_data));

        job_result_data.store_failure = true;
        job_result_data.status = ResultStatus::ErrorAndRetry as i32;
        assert!(HybridJobResultAppImpl::_should_store(&job_result_data));

        job_result_data.store_success = false;
        job_result_data.status = ResultStatus::Success as i32;
        assert!(!HybridJobResultAppImpl::_should_store(&job_result_data));
    }

    fn setup() -> Result<HybridJobResultAppImpl> {
        // dotenv::dotenv().ok();
        let rdb_module = setup_test_rdb_module();
        TEST_RUNTIME.block_on(async {
            let redis_module = setup_test_redis_module().await;
            let repositories = Arc::new(HybridRepositoryModule {
                redis_module,
                rdb_module,
            });
            let id_generator = Arc::new(IdGeneratorWrapper::new());
            let mc_config = infra_utils::infra::memory::MemoryCacheConfig {
                num_counters: 10000,
                max_cost: 10000,
                use_metrics: false,
            };
            let worker_memory_cache = infra_utils::infra::memory::new_memory_cache::<
                Arc<String>,
                Vec<Worker>,
            >(&mc_config);
            // let job_memory_cache =
            //     common::infra::memory::new_memory_cache::<Arc<String>, Vec<Job>>(&mc_config);

            let storage_config = Arc::new(StorageConfig {
                r#type: StorageType::Hybrid,
                restore_at_startup: Some(false),
            });
            // let job_queue_config = Arc::new(JobQueueConfig {
            //     expire_job_result_seconds: 60,
            //     fetch_interval: 1000,
            // });

            let worker_app = Arc::new(HybridWorkerAppImpl::new(
                storage_config.clone(),
                id_generator.clone(),
                worker_memory_cache,
                repositories.clone(),
            ));
            Ok(HybridJobResultAppImpl::new(
                storage_config,
                id_generator.clone(),
                repositories,
                worker_app,
            ))
        })
    }
    #[test]
    fn test_create_job_result_if_necessary() -> Result<()> {
        let app = setup()?;
        let operation = WorkerOperation {
            operation: Some(Operation::Command(
                proto::jobworkerp::data::CommandOperation {
                    name: "ls".to_string(),
                },
            )),
        };
        let worker_data = WorkerData {
            name: "test".to_string(),
            r#type: RunnerType::Command as i32,
            operation: Some(operation),
            retry_policy: None,
            periodic_interval: 0,
            channel: Some("hoge".to_string()),
            queue_type: QueueType::Rdb as i32,
            response_type: ResponseType::Direct as i32,
            store_success: true,
            store_failure: true,
            next_workers: vec![],
            use_static: false,
        };
        TEST_RUNTIME.block_on(async {
            let worker_id = app.worker_app().create(&worker_data).await?;
            let worker = Worker {
                id: Some(worker_id.clone()),
                data: Some(worker_data.clone()),
            };

            // app.worker_app.create(&worker_data).await?;
            let id = JobResultId {
                value: app.id_generator().generate_id()?,
            };
            let job_id = JobId { value: 100 };
            let arg = RunnerArg {
                data: Some(proto::jobworkerp::data::runner_arg::Data::Command(
                    proto::jobworkerp::data::CommandArg {
                        args: vec!["arg1".to_string()],
                    },
                )),
            };
            let mut data = JobResultData {
                job_id: Some(job_id.clone()),
                worker_id: worker.id.clone(),
                status: ResultStatus::Success as i32,
                worker_name: worker_data.name.clone(),
                arg: Some(arg),
                uniq_key: Some("uniq_key".to_string()),
                output: Some(ResultOutput {
                    items: vec![b"data".to_vec()],
                }),
                retried: 0,
                max_retry: worker_data
                    .retry_policy
                    .clone()
                    .map(|p| p.max_retry)
                    .unwrap_or(0),
                priority: Priority::High as i32,
                timeout: 0,
                enqueue_time: datetime::now_millis(),
                run_after_time: 0,
                response_type: worker_data.response_type,
                start_time: datetime::now_millis(),
                end_time: datetime::now_millis(),
                store_success: worker_data.store_success,
                store_failure: worker_data.store_failure,
            };
            let result = JobResult {
                id: Some(id.clone()),
                data: Some(data.clone()),
            };
            assert!(app.create_job_result_if_necessary(&id, &data).await?);
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
            data.job_id = Some(job_id.clone());
            let result = JobResult {
                id: Some(id.clone()),
                data: Some(data.clone()),
            };
            assert!(app.create_job_result_if_necessary(&id, &data).await?);
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
            data.job_id = Some(job_id.clone());
            data.response_type = ResponseType::ListenAfter as i32;
            let result = JobResult {
                id: Some(id.clone()),
                data: Some(data.clone()),
            };
            // store only to redis for listen after
            assert!(app.create_job_result_if_necessary(&id, &data).await?);
            assert_eq!(app.find_job_result_from_db(&id).await?, None);
            // store ended result in cache for listen_after
            let mut res = app.find_job_result_by_job_id(&job_id).await?.unwrap();
            // restore store_failure by worker data, rewrite for skip in test
            let mut d = res.data.unwrap().clone();
            d.store_failure = false;
            d.response_type = ResponseType::ListenAfter as i32;
            res.data = Some(d);
            assert_eq!(res, result);

            // no store
            let id = JobResultId {
                value: app.id_generator().generate_id()?,
            };
            let job_id = JobId { value: 303 };
            data.status = ResultStatus::ErrorAndRetry as i32;
            data.job_id = Some(job_id.clone());
            assert!(
                !(app.create_job_result_if_necessary(&id, &data).await?),
                "no store"
            );
            assert_eq!(app.find_job_result_from_db(&id).await?, None);
            // no store retrying result in cache
            assert_eq!(app.find_job_result_by_job_id(&job_id).await?, None);

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
