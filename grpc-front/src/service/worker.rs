use crate::proto::jobworkerp::data::{Empty, QueueType, ResponseType};
use crate::proto::jobworkerp::data::{Worker, WorkerData, WorkerId};
use crate::proto::jobworkerp::service::worker_service_server::WorkerService;
use crate::proto::jobworkerp::service::{
    ChannelInfo, CountCondition, CountResponse, CountWorkerRequest, CreateWorkerResponse,
    FindChannelListResponse, FindWorkerListRequest, OptionalWorkerResponse, SuccessResponse,
    WorkerNameRequest,
};
use crate::service::error_handle::handle_error;
use app::app::worker::WorkerApp;
use app::app::{StorageConfig, UseStorageConfig, UseWorkerConfig, WorkerConfig};
use app::module::AppModule;
use async_stream::stream;
use command_utils::trace::Tracing;
use futures::stream::BoxStream;
use infra::infra::UseJobQueueConfig;
use infra::infra::job::rows::UseJobqueueAndCodec;
use infra::infra::worker_instance::UseWorkerInstanceRepository;
use infra::infra::worker_instance::WorkerInstanceRepository;
use jobworkerp_base::WORKER_INSTANCE_CONFIG;
use proto::jobworkerp::data::{RetryPolicy, StorageType};
use std::fmt::Debug;
use std::sync::Arc;
use tonic::Response;

pub trait WorkerGrpc {
    fn app(&self) -> &Arc<dyn WorkerApp + 'static>;
    fn worker_instance_repository(&self) -> Arc<dyn WorkerInstanceRepository>;
}

const DEFAULT_CHANNEL_DISPLAY_NAME: &str = "[default]";

pub trait RequestValidator: UseJobQueueConfig + UseStorageConfig + UseWorkerConfig {
    fn default_queue_type(&self) -> QueueType {
        match self.storage_config().r#type {
            StorageType::Standalone => QueueType::Normal,
            StorageType::Scalable => QueueType::Normal,
            // StorageType::Redis => QueueType::Normal,
        }
    }
    #[allow(clippy::result_large_err)]
    fn validate_create(&self, dat: WorkerData) -> Result<WorkerData, tonic::Status> {
        let data = WorkerData {
            name: dat.name,
            description: dat.description,
            runner_id: dat.runner_id,
            runner_settings: dat.runner_settings,
            retry_policy: dat.retry_policy,
            periodic_interval: dat.periodic_interval,
            channel: dat.channel,
            queue_type: self
                .validate_queue_type(
                    QueueType::try_from(dat.queue_type)
                        .ok()
                        .unwrap_or(self.default_queue_type()),
                )
                .map(|r| r as i32)?,
            response_type: dat.response_type,
            store_success: dat.store_success,
            store_failure: dat.store_failure,
            use_static: dat.use_static,
            broadcast_results: dat.broadcast_results, // no effect
        };
        self.validate_worker(&data)?;
        Ok(data)
    }
    // XXX not necessary now
    #[allow(clippy::result_large_err)]
    fn validate_queue_type(&self, qt: QueueType) -> Result<QueueType, tonic::Status> {
        Ok(qt)
    }
    #[allow(clippy::result_large_err)]
    fn validate_update(&self, dat: Option<&WorkerData>) -> Result<(), tonic::Status> {
        if let Some(d) = dat {
            self.validate_worker(d)?
        }
        Ok(())
    }
    #[allow(clippy::result_large_err)]
    fn validate_worker(&self, req: &WorkerData) -> Result<(), tonic::Status> {
        if req.periodic_interval != 0 && req.response_type == ResponseType::Direct as i32 {
            return Err(tonic::Status::invalid_argument(
                "periodic and direct_response can't be set at the same time",
            ));
        }
        // periodic interval must be greater than fetch_interval
        if req.periodic_interval != 0
            && req.periodic_interval <= self.job_queue_config().fetch_interval
        {
            return Err(tonic::Status::invalid_argument(format!(
                "periodic interval can't be set lesser than {}msec(jobqueue config fetch_interval)",
                self.job_queue_config().fetch_interval
            )));
        }
        self.validate_queue_type(req.queue_type())?;
        if req.queue_type == QueueType::DbOnly as i32
            && req.response_type == ResponseType::Direct as i32
        {
            return Err(tonic::Status::invalid_argument(
                "can't use db queue in direct_response.",
            ));
        }
        //        // runner_settings should not be empty (depends on worker, not checked here)
        //        if req.runner_settings.is_empty() {
        //            return Err(tonic::Status::invalid_argument("runner_settings should not be empty"));
        //        }
        // name should not be empty
        if req.name.is_empty() {
            return Err(tonic::Status::invalid_argument("name should not be empty"));
        }
        // retry policy should be positive or none
        if let Some(rp) = req.retry_policy.as_ref() {
            self.validate_retry_policy(rp)?
        }
        // channel should exist
        self.validate_channel(req.channel.as_ref())?;
        Ok(())
    }
    #[allow(clippy::result_large_err)]
    fn validate_channel(&self, channel: Option<&String>) -> Result<(), tonic::Status> {
        if let Some(ch) = channel {
            if ch.is_empty() {
                return Err(tonic::Status::invalid_argument(
                    "channel name cannot be empty",
                ));
            }

            let available_channels: std::collections::HashSet<String> =
                self.worker_config().get_channels().into_iter().collect();

            if !available_channels.contains(ch) {
                return Err(tonic::Status::invalid_argument(format!(
                    "specified channel '{}' does not exist. Available channels: {:?}",
                    ch,
                    available_channels.into_iter().collect::<Vec<_>>()
                )));
            }
        }
        Ok(())
    }
    #[allow(clippy::result_large_err)]
    fn validate_retry_policy(&self, rp: &RetryPolicy) -> Result<(), tonic::Status> {
        if rp.basis < 1.0 {
            return Err(tonic::Status::invalid_argument(
                "retry_basis should be greater than 1.0",
            ));
        }
        Ok(())
    }
}

pub trait ResponseProcessor: UseJobqueueAndCodec {
    // replace internal values
    fn process(&self, w: Worker) -> Worker {
        if let Some(d) = w.data {
            let dat = self.process_data(d);
            Worker {
                id: w.id,
                data: Some(dat),
            }
        } else {
            w
        }
    }
    fn process_data(&self, dat: WorkerData) -> WorkerData {
        if dat
            .channel
            .as_ref()
            .is_some_and(|c| c.as_str() == Self::DEFAULT_CHANNEL_NAME)
        {
            let mut d = dat;
            d.channel = None;
            d
        } else {
            dat
        }
    }
}

#[tonic::async_trait]
impl<
    T: WorkerGrpc
        + RequestValidator
        + ResponseProcessor
        + UseWorkerConfig
        + Tracing
        + Send
        + Debug
        + Sync
        + 'static,
> WorkerService for T
{
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "create"))]
    async fn create(
        &self,
        request: tonic::Request<WorkerData>,
    ) -> Result<tonic::Response<CreateWorkerResponse>, tonic::Status> {
        let _span = Self::trace_request("worker", "create", &request);
        //validation
        let data = self.validate_create(request.into_inner())?;
        // create worker
        match self.app().create(&data).await {
            Ok(id) => Ok(Response::new(CreateWorkerResponse { id: Some(id) })),
            Err(e) => Err(handle_error(&e)),
        }
    }
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "update"))]
    async fn update(
        &self,
        request: tonic::Request<Worker>,
    ) -> Result<tonic::Response<SuccessResponse>, tonic::Status> {
        let _s = Self::trace_request("worker", "update", &request);
        let req = request.get_ref();
        //validation
        self.validate_update(req.data.as_ref())?;
        if let Some(i) = &req.id {
            match self.app().update(i, &req.data).await {
                Ok(res) => Ok(Response::new(SuccessResponse { is_success: res })),
                Err(e) => Err(handle_error(&e)),
            }
        } else {
            tracing::warn!("id not found in updating: {:?}", req);
            Err(tonic::Status::not_found("id not found".to_string()))
        }
    }
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "delete"))]
    async fn delete(
        &self,
        request: tonic::Request<WorkerId>,
    ) -> Result<tonic::Response<SuccessResponse>, tonic::Status> {
        let _s = Self::trace_request("worker", "delete", &request);
        let req = request.get_ref();
        match self.app().delete(req).await {
            Ok(r) => Ok(Response::new(SuccessResponse { is_success: r })),
            Err(e) => Err(handle_error(&e)),
        }
    }
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "find"))]
    async fn find(
        &self,
        request: tonic::Request<WorkerId>,
    ) -> Result<tonic::Response<OptionalWorkerResponse>, tonic::Status> {
        let _s = Self::trace_request("worker", "find", &request);
        let req = request.get_ref();
        match self.app().find(req).await {
            Ok(res) => Ok(Response::new(OptionalWorkerResponse {
                data: res.map(|w| self.process(w)),
            })),
            Err(e) => Err(handle_error(&e)),
        }
    }

    #[tracing::instrument(level = "info", skip(self, request), fields(method = "find_by_name"))]
    async fn find_by_name(
        &self,
        request: tonic::Request<WorkerNameRequest>,
    ) -> Result<tonic::Response<OptionalWorkerResponse>, tonic::Status> {
        let _s = Self::trace_request("worker", "find", &request);
        let req = request.get_ref();
        // XXX use worker_map ?
        match self.app().find_by_name(&req.name).await {
            Ok(res) => Ok(Response::new(OptionalWorkerResponse {
                data: res.map(|w| self.process(w)),
            })),
            Err(e) => Err(handle_error(&e)),
        }
    }

    type FindListStream = BoxStream<'static, Result<Worker, tonic::Status>>;
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "find_list"))]
    async fn find_list(
        &self,
        request: tonic::Request<FindWorkerListRequest>,
    ) -> Result<tonic::Response<Self::FindListStream>, tonic::Status> {
        let _s = Self::trace_request("worker", "find_list", &request);
        let req = request.into_inner();

        super::validation::validate_limit(req.limit)?;
        super::validation::validate_offset(req.offset)?;
        super::validation::validate_name_filter(req.name_filter.as_ref())?;
        super::validation::validate_filter_enums(&req.runner_types, "runner_types")?;

        let runner_ids: Vec<i64> = req.runner_ids.into_iter().map(|rid| rid.value).collect();
        super::validation::validate_filter_ids(&runner_ids, "runner_ids")?;
        let sort_by = req
            .sort_by
            .and_then(|val| proto::jobworkerp::data::WorkerSortField::try_from(val).ok());
        match self
            .app()
            .find_list(
                req.runner_types,
                req.channel,
                req.limit,
                req.offset,
                req.name_filter,
                req.is_periodic,
                runner_ids,
                sort_by,
                req.ascending,
            )
            .await
        {
            Ok(list) => {
                let l = list
                    .into_iter()
                    .map(|w| self.process(w))
                    .collect::<Vec<_>>();
                Ok(Response::new(Box::pin(stream! {
                    for s in l {
                        yield Ok(s)
                    }
                })))
            }
            Err(e) => Err(handle_error(&e)),
        }
    }
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "count"))]
    async fn count(
        &self,
        request: tonic::Request<CountCondition>,
    ) -> Result<tonic::Response<CountResponse>, tonic::Status> {
        let _s = Self::trace_request("worker", "count", &request);
        // XXX use worker_map ?
        match self.app().count().await {
            Ok(res) => Ok(Response::new(CountResponse { total: res })),
            Err(e) => Err(handle_error(&e)),
        }
    }
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "count_by"))]
    async fn count_by(
        &self,
        request: tonic::Request<CountWorkerRequest>,
    ) -> Result<tonic::Response<CountResponse>, tonic::Status> {
        let _s = Self::trace_request("worker", "count_by", &request);
        let req = request.into_inner();

        super::validation::validate_name_filter(req.name_filter.as_ref())?;
        super::validation::validate_channel(req.channel.as_ref())?;
        super::validation::validate_filter_enums(&req.runner_types, "runner_types")?;

        let runner_ids: Vec<i64> = req.runner_ids.into_iter().map(|rid| rid.value).collect();
        super::validation::validate_filter_ids(&runner_ids, "runner_ids")?;
        match self
            .app()
            .count_by(
                req.runner_types,
                req.channel,
                req.name_filter,
                req.is_periodic,
                runner_ids,
            )
            .await
        {
            Ok(res) => Ok(Response::new(CountResponse { total: res })),
            Err(e) => Err(handle_error(&e)),
        }
    }

    #[tracing::instrument(
        level = "info",
        skip(self, request),
        fields(method = "find_channel_list")
    )]
    async fn find_channel_list(
        &self,
        request: tonic::Request<Empty>,
    ) -> Result<tonic::Response<FindChannelListResponse>, tonic::Status> {
        let _s = Self::trace_request("worker", "find_channel_list", &request);

        let worker_counts = match self.app().count_by_channel().await {
            Ok(counts) => counts
                .into_iter()
                .collect::<std::collections::HashMap<_, _>>(),
            Err(e) => {
                tracing::warn!("Failed to get worker counts by channel: {}", e);
                std::collections::HashMap::new()
            }
        };

        // Get instance aggregation for total_concurrency and active_instances
        let timeout_millis = WORKER_INSTANCE_CONFIG.timeout_millis();
        let instance_aggregation = match self
            .worker_instance_repository()
            .get_channel_aggregation(timeout_millis)
            .await
        {
            Ok(agg) => agg,
            Err(e) => {
                tracing::warn!(
                    "Failed to get channel aggregation, using empty fallback: {}",
                    e
                );
                std::collections::HashMap::new()
            }
        };

        let channels = self
            .worker_config()
            .channel_concurrency_pair()
            .into_iter()
            .map(|(name, concurrency)| {
                let display_name = if name == T::DEFAULT_CHANNEL_NAME {
                    DEFAULT_CHANNEL_DISPLAY_NAME.to_string()
                } else {
                    name.clone()
                };
                let worker_count = *worker_counts.get(&name).unwrap_or(&0);

                // Get instance aggregation data
                let agg = instance_aggregation.get(&name);
                let total_concurrency = agg.map(|a| a.total_concurrency).unwrap_or(0);
                let active_instances = agg.map(|a| a.active_instances as i32).unwrap_or(0);

                ChannelInfo {
                    name: display_name,
                    concurrency,
                    worker_count,
                    total_concurrency: Some(total_concurrency),
                    active_instances: Some(active_instances),
                }
            })
            .collect();
        Ok(Response::new(FindChannelListResponse { channels }))
    }
}

#[derive(DebugStub)]
pub(crate) struct WorkerGrpcImpl {
    #[debug_stub = "WorkerAppImpl"]
    app_module: Arc<AppModule>,
}

impl WorkerGrpcImpl {
    pub fn new(app_module: Arc<AppModule>) -> Self {
        WorkerGrpcImpl { app_module }
    }
}
impl WorkerGrpc for WorkerGrpcImpl {
    fn app(&self) -> &Arc<dyn WorkerApp + 'static> {
        &self.app_module.worker_app
    }
    fn worker_instance_repository(&self) -> Arc<dyn WorkerInstanceRepository> {
        self.app_module.repositories.worker_instance_repository()
    }
}

// use tracing
impl Tracing for WorkerGrpcImpl {}
impl jobworkerp_base::codec::UseProstCodec for WorkerGrpcImpl {}
impl UseJobqueueAndCodec for WorkerGrpcImpl {}
impl UseJobQueueConfig for WorkerGrpcImpl {
    fn job_queue_config(&self) -> &infra::infra::JobQueueConfig {
        &self.app_module.config_module.job_queue_config
    }
}
impl UseStorageConfig for WorkerGrpcImpl {
    fn storage_config(&self) -> &StorageConfig {
        &self.app_module.config_module.storage_config
    }
}
impl UseWorkerConfig for WorkerGrpcImpl {
    fn worker_config(&self) -> &WorkerConfig {
        &self.app_module.config_module.worker_config
    }
}
impl RequestValidator for WorkerGrpcImpl {}
impl ResponseProcessor for WorkerGrpcImpl {}

#[cfg(test)]
mod tests {
    use super::*;
    use infra::infra::{JobQueueConfig, job::rows::JobqueueAndCodec};
    use jobworkerp_base::codec::UseProstCodec;
    use proto::jobworkerp::data::RetryType;

    static JOB_QUEUE_CONFIG: JobQueueConfig = infra::infra::JobQueueConfig {
        fetch_interval: 1000,
        expire_job_result_seconds: 1000,
        channel_capacity: 10_000,
    };
    static mut STORAGE_CONFIG: StorageConfig = StorageConfig {
        r#type: StorageType::Standalone,
        restore_at_startup: Some(false),
    };
    struct Validator {
        pub storage_type: StorageType,
        pub worker_config: WorkerConfig,
    }
    impl RequestValidator for Validator {}
    impl UseJobQueueConfig for Validator {
        fn job_queue_config(&self) -> &infra::infra::JobQueueConfig {
            &JOB_QUEUE_CONFIG
        }
    }
    impl UseStorageConfig for Validator {
        #[allow(static_mut_refs)] // use for only test
        fn storage_config(&self) -> &StorageConfig {
            unsafe { STORAGE_CONFIG.r#type = self.storage_type };
            unsafe { &STORAGE_CONFIG }
        }
    }
    impl UseWorkerConfig for Validator {
        fn worker_config(&self) -> &WorkerConfig {
            &self.worker_config
        }
    }
    impl UseProstCodec for Validator {}
    impl UseJobqueueAndCodec for Validator {}

    // #[test]
    // fn test_validate_queue_type() {
    //     let v = Validator {
    //         storage_type: StorageType::Standalone,
    //     };
    //     assert_eq!(
    //         v.validate_queue_type(QueueType::DbOnly).unwrap(),
    //         QueueType::DbOnly
    //     );
    //     /// not valid
    //     assert!(v.validate_queue_type(QueueType::Normal).is_ok());
    //     assert_eq!(
    //         v.validate_queue_type(QueueType::WithBackup).unwrap(),
    //         QueueType::WithBackup
    //     );
    //     // let v = Validator {
    //     //     storage_type: StorageType::OnlyRedis,
    //     // };
    //     // assert!(v.validate_queue_type(QueueType::DbOnly).is_err());
    //     // assert_eq!(
    //     //     v.validate_queue_type(QueueType::Normal).unwrap(),
    //     //     QueueType::Normal
    //     // );
    //     // assert_eq!(
    //     //     v.validate_queue_type(QueueType::WithBackup).unwrap(),
    //     //     QueueType::Normal
    //     // );
    //     let v = Validator {
    //         storage_type: StorageType::Scalable,
    //     };
    //     assert_eq!(
    //         v.validate_queue_type(QueueType::DbOnly).unwrap(),
    //         QueueType::DbOnly
    //     );
    //     assert_eq!(
    //         v.validate_queue_type(QueueType::Normal).unwrap(),
    //         QueueType::Normal
    //     );
    //     assert_eq!(
    //         v.validate_queue_type(QueueType::WithBackup).unwrap(),
    //         QueueType::WithBackup
    //     );
    // }

    #[test]
    fn test_validate_retry_policy() {
        let v = Validator {
            storage_type: StorageType::Standalone,
            worker_config: WorkerConfig {
                default_concurrency: 4,
                channels: vec![],
                channel_concurrencies: vec![],
            },
        };
        let rp = RetryPolicy {
            r#type: RetryType::Exponential as i32,
            basis: 1.2,
            interval: 0,
            max_retry: 0,
            max_interval: 0,
        };
        assert!(v.validate_retry_policy(&rp).is_ok());
        let mut r = rp;
        r.basis = 0.3;
        assert!(v.validate_retry_policy(&r).is_err());
    }
    #[test]
    fn test_validate_worker_by_response_type_for_rdb_storage() {
        let v = Validator {
            storage_type: StorageType::Standalone,
            worker_config: WorkerConfig {
                default_concurrency: 4,
                channels: vec![],
                channel_concurrencies: vec![],
            },
        };
        let runner_settings =
            <JobqueueAndCodec as jobworkerp_base::codec::UseProstCodec>::serialize_message(
                &proto::TestRunnerSettings {
                    name: "ls".to_string(),
                },
            )
            .unwrap();
        let mut w = WorkerData {
            name: "ListCommand".to_string(),
            runner_settings,
            queue_type: QueueType::DbOnly as i32,
            response_type: ResponseType::Direct as i32,
            store_failure: true,
            store_success: true,
            ..Default::default()
        };
        // in ResponseType::Direct cannot be used by storage_type: RDB
        assert!(v.validate_worker(&w).is_err());

        let v = Validator {
            storage_type: StorageType::Scalable,
            worker_config: WorkerConfig {
                default_concurrency: 4,
                channels: vec![],
                channel_concurrencies: vec![],
            },
        };

        w.response_type = ResponseType::Direct as i32;
        assert!(v.validate_worker(&w).is_err());
        w.queue_type = QueueType::DbOnly as i32;
        assert!(v.validate_worker(&w).is_err());
        w.queue_type = QueueType::Normal as i32;
        assert!(v.validate_worker(&w).is_ok());
        w.queue_type = QueueType::WithBackup as i32;
        assert!(v.validate_worker(&w).is_ok());

        w.response_type = ResponseType::NoResult as i32;
        assert!(v.validate_worker(&w).is_ok());
        w.queue_type = QueueType::DbOnly as i32;
        assert!(v.validate_worker(&w).is_ok());
        w.queue_type = QueueType::WithBackup as i32;
        assert!(v.validate_worker(&w).is_ok());
        w.queue_type = QueueType::Normal as i32;
        assert!(v.validate_worker(&w).is_ok());
    }

    #[test]
    fn test_validate_worker_by_periodic_interval() {
        let v = Validator {
            storage_type: StorageType::Scalable,
            worker_config: WorkerConfig {
                default_concurrency: 4,
                channels: vec![],
                channel_concurrencies: vec![],
            },
        };
        let runner_settings =
            <JobqueueAndCodec as jobworkerp_base::codec::UseProstCodec>::serialize_message(
                &proto::TestRunnerSettings {
                    name: "ls".to_string(),
                },
            )
            .unwrap();
        let mut w = WorkerData {
            name: "ListCommand".to_string(),
            runner_settings,
            queue_type: QueueType::DbOnly as i32,
            response_type: ResponseType::NoResult as i32,
            store_failure: true,
            store_success: true,
            periodic_interval: 0,
            ..Default::default()
        };
        assert!(v.validate_worker(&w).is_ok());
        w.periodic_interval = 1000;
        assert!(v.validate_worker(&w).is_err());

        w.periodic_interval = 0;

        // in ResponseType::Direct cannot be used by storage_type: RDB
        w.response_type = ResponseType::Direct as i32;
        assert!(v.validate_worker(&w).is_err());
    }

    #[test]
    fn test_validate_channel_success() {
        // Test with default channel (None)
        let v = Validator {
            storage_type: StorageType::Standalone,
            worker_config: WorkerConfig {
                default_concurrency: 4,
                channels: vec!["custom-channel".to_string()],
                channel_concurrencies: vec![2],
            },
        };

        // Test with None (should use default channel)
        assert!(v.validate_channel(None).is_ok());

        // Test with existing custom channel
        assert!(
            v.validate_channel(Some(&"custom-channel".to_string()))
                .is_ok()
        );

        // Test with default channel name explicitly
        assert!(
            v.validate_channel(Some(
                &<Validator as UseJobqueueAndCodec>::DEFAULT_CHANNEL_NAME.to_string()
            ))
            .is_ok()
        );
    }

    #[test]
    fn test_validate_channel_not_exist() {
        let v = Validator {
            storage_type: StorageType::Standalone,
            worker_config: WorkerConfig {
                default_concurrency: 4,
                channels: vec!["valid-channel".to_string()],
                channel_concurrencies: vec![2],
            },
        };

        // Test with non-existing channel
        let result = v.validate_channel(Some(&"non-existing-channel".to_string()));
        assert!(result.is_err());

        if let Err(status) = result {
            assert_eq!(status.code(), tonic::Code::InvalidArgument);
            let message = status.message();
            assert!(message.contains("non-existing-channel"));
            assert!(message.contains("does not exist"));
        }
    }

    #[test]
    fn test_validate_channel_empty_string() {
        let v = Validator {
            storage_type: StorageType::Standalone,
            worker_config: WorkerConfig {
                default_concurrency: 4,
                channels: vec![],
                channel_concurrencies: vec![],
            },
        };

        // Test with empty string channel
        let result = v.validate_channel(Some(&"".to_string()));
        assert!(result.is_err());

        if let Err(status) = result {
            assert_eq!(status.code(), tonic::Code::InvalidArgument);
            assert!(status.message().contains("channel name cannot be empty"));
        }
    }

    #[test]
    fn test_validate_worker_with_channel_validation() {
        let v = Validator {
            storage_type: StorageType::Standalone,
            worker_config: WorkerConfig {
                default_concurrency: 4,
                channels: vec!["valid-channel".to_string()],
                channel_concurrencies: vec![2],
            },
        };

        let runner_settings =
            <JobqueueAndCodec as jobworkerp_base::codec::UseProstCodec>::serialize_message(
                &proto::TestRunnerSettings {
                    name: "ls".to_string(),
                },
            )
            .unwrap();

        // Test with valid channel
        let w_valid = WorkerData {
            name: "TestWorker".to_string(),
            runner_settings: runner_settings.clone(),
            channel: Some("valid-channel".to_string()),
            ..Default::default()
        };
        assert!(v.validate_worker(&w_valid).is_ok());

        // Test with invalid channel
        let w_invalid = WorkerData {
            name: "TestWorker".to_string(),
            runner_settings,
            channel: Some("invalid-channel".to_string()),
            ..Default::default()
        };
        assert!(v.validate_worker(&w_invalid).is_err());
    }
}
