use crate::proto::jobworkerp::data::{QueueType, ResponseType, RunnerType};
use crate::proto::jobworkerp::data::{Worker, WorkerData, WorkerId};
use crate::proto::jobworkerp::service::worker_service_server::WorkerService;
use crate::proto::jobworkerp::service::{
    CountCondition, CountResponse, CreateWorkerRequest, CreateWorkerResponse, FindListRequest,
    OptionalWorkerResponse, SuccessResponse, WorkerNameRequest,
};
use crate::service::error_handle::handle_error;
use app::app::worker::WorkerApp;
use app::app::{StorageConfig, StorageType, UseStorageConfig};
use app::module::AppModule;
use async_stream::stream;
use command_utils::util::option::{Exists, FlatMap};
use command_utils::util::result::ToOption;
use futures::stream::BoxStream;
use infra::infra::job::rows::UseJobqueueAndCodec;
use infra::infra::UseJobQueueConfig;
use infra_utils::trace::Tracing;
use proto::jobworkerp::data::worker_operation::Operation;
use proto::jobworkerp::data::{RetryPolicy, WorkerOperation};
use std::fmt::Debug;
use std::sync::Arc;
use tonic::Response;

pub trait WorkerGrpc {
    fn app(&self) -> &Arc<dyn WorkerApp + 'static>;
}

pub trait RequestValidator: UseJobQueueConfig + UseStorageConfig {
    fn default_queue_type(&self) -> QueueType {
        match self.storage_config().r#type {
            StorageType::RDB => QueueType::Rdb,
            StorageType::Redis => QueueType::Redis,
            StorageType::Hybrid => QueueType::Redis,
        }
    }
    fn validate_type_from_operation(
        &self,
        op: Option<&WorkerOperation>,
    ) -> Result<RunnerType, tonic::Status> {
        match op {
            Some(WorkerOperation {
                operation: Some(op),
            }) => match op {
                Operation::Command(_) => Ok(RunnerType::Command),
                Operation::Docker(_) => Ok(RunnerType::Docker),
                Operation::GrpcUnary(_) => Ok(RunnerType::GrpcUnary),
                Operation::Plugin(_) => Ok(RunnerType::Plugin),
                Operation::SlackInternal(_) => Ok(RunnerType::SlackInternal),
                Operation::HttpRequest(_) => Ok(RunnerType::HttpRequest),
                _ => Err(tonic::Status::invalid_argument("not implemented operation")),
            },
            _ => Err(tonic::Status::invalid_argument("operation not found")),
        }
    }
    fn validate_create(&self, dat: CreateWorkerRequest) -> Result<WorkerData, tonic::Status> {
        let data = WorkerData {
            name: dat.name,
            r#type: self
                .validate_type_from_operation(dat.operation.as_ref())?
                .into(),
            operation: dat.operation,
            retry_policy: dat.retry_policy,
            periodic_interval: dat.periodic_interval.unwrap_or(0),
            channel: dat.channel,
            queue_type: self
                .validate_queue_type(
                    dat.queue_type
                        .flat_map(|qt| QueueType::try_from(qt).to_option())
                        .unwrap_or(self.default_queue_type()),
                )
                .map(|r| r as i32)?,
            response_type: dat.response_type.unwrap_or(ResponseType::NoResult as i32),
            store_success: dat.store_success.unwrap_or(false),
            store_failure: dat.store_failure.unwrap_or(false),
            next_workers: dat.next_workers,
            use_static: dat.use_static.unwrap_or(false),
        };
        self.validate_worker(&data)?;
        Ok(data)
    }
    fn validate_queue_type(&self, qt: QueueType) -> Result<QueueType, tonic::Status> {
        match self.storage_config().r#type {
            StorageType::RDB => {
                if qt == QueueType::Redis {
                    Err(tonic::Status::invalid_argument(
                        "queue_type Redis can't be used with RDB storage type",
                    ))
                } else if qt == QueueType::Hybrid {
                    tracing::warn!("queue_type: hybrid is not available by StorageType::RDB. Failback to QueueType:Rdb");
                    Ok(QueueType::Rdb)
                } else {
                    Ok(qt)
                }
            }
            StorageType::Redis => {
                if qt == QueueType::Rdb {
                    Err(tonic::Status::invalid_argument(
                        "queue_type Rdb can't be used with Redis storage type",
                    ))
                } else if qt == QueueType::Hybrid {
                    tracing::warn!("queue_type: hybrid is not available by StorageType::Redis. Failback to QueueType:Redis");
                    Ok(QueueType::Redis)
                } else {
                    Ok(qt)
                }
            }
            StorageType::Hybrid => Ok(qt),
        }
    }
    fn validate_update(&self, dat: Option<&WorkerData>) -> Result<(), tonic::Status> {
        if let Some(d) = dat {
            self.validate_worker(d)?
        }
        Ok(())
    }
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
        if req.queue_type == QueueType::Rdb as i32
            && req.response_type == ResponseType::Direct as i32
        {
            return Err(tonic::Status::invalid_argument(
                "can't use db queue in direct_response.",
            ));
        }
        // rdb listen_after response type need to store result to rdb
        if self.storage_config().r#type == StorageType::RDB
            && req.response_type == ResponseType::ListenAfter as i32
            && (!req.store_success || !req.store_failure)
        {
            return Err(tonic::Status::invalid_argument(
                "must specify store_success and store_failure TRUE when use db queue in ListenAfter response_type.",
            ));
        }
        //        // operation should not be empty (depends on worker, not checked here)
        //        if req.operation.is_empty() {
        //            return Err(tonic::Status::invalid_argument("operation should not be empty"));
        //        }
        // name should not be empty
        if req.name.is_empty() {
            return Err(tonic::Status::invalid_argument("name should not be empty"));
        }
        // retry policy should be positive or none
        if let Some(rp) = req.retry_policy.as_ref() {
            self.validate_retry_policy(rp)?
        }
        // check operation and type
        if self.validate_type_from_operation(req.operation.as_ref())? != req.r#type() {
            Err(tonic::Status::invalid_argument("operation type mismatch"))
        } else {
            Ok(())
        }?;
        Ok(())
    }
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
                id: w.id.clone(),
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
            .exists(|c| c.as_str() == Self::DEFAULT_CHANNEL_NAME)
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
        T: WorkerGrpc + RequestValidator + ResponseProcessor + Tracing + Send + Debug + Sync + 'static,
    > WorkerService for T
{
    #[tracing::instrument]
    async fn create(
        &self,
        request: tonic::Request<CreateWorkerRequest>,
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
    #[tracing::instrument]
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
    #[tracing::instrument]
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
    #[tracing::instrument]
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

    #[tracing::instrument]
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
    #[tracing::instrument]
    async fn find_list(
        &self,
        request: tonic::Request<FindListRequest>,
    ) -> Result<tonic::Response<Self::FindListStream>, tonic::Status> {
        let _s = Self::trace_request("worker", "find_list", &request);
        let req = request.get_ref();
        // TODO streaming?
        match self.app().find_list(req.limit, req.offset).await {
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
    #[tracing::instrument]
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
}

// use tracing
impl Tracing for WorkerGrpcImpl {}
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
impl RequestValidator for WorkerGrpcImpl {}
impl ResponseProcessor for WorkerGrpcImpl {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::jobworkerp::data::{RunnerType, WorkerOperation};
    use infra::infra::JobQueueConfig;
    use proto::jobworkerp::data::{worker_operation::Operation, RetryType};

    static JOB_QUEUE_CONFIG: JobQueueConfig = infra::infra::JobQueueConfig {
        fetch_interval: 1000,
        expire_job_result_seconds: 1000,
    };
    static mut STORAGE_CONFIG: StorageConfig = StorageConfig {
        r#type: StorageType::RDB,
        restore_at_startup: Some(false),
    };
    struct Validator {
        pub storage_type: StorageType,
    }
    impl RequestValidator for Validator {}
    impl UseJobQueueConfig for Validator {
        fn job_queue_config(&self) -> &infra::infra::JobQueueConfig {
            &JOB_QUEUE_CONFIG
        }
    }
    impl UseStorageConfig for Validator {
        fn storage_config(&self) -> &StorageConfig {
            unsafe { STORAGE_CONFIG.r#type = self.storage_type };
            unsafe { &STORAGE_CONFIG }
        }
    }

    #[test]
    fn test_validate_queue_type() {
        let v = Validator {
            storage_type: StorageType::RDB,
        };
        assert_eq!(
            v.validate_queue_type(QueueType::Rdb).unwrap(),
            QueueType::Rdb
        );
        assert!(v.validate_queue_type(QueueType::Redis).is_err());
        assert_eq!(
            v.validate_queue_type(QueueType::Hybrid).unwrap(),
            QueueType::Rdb
        );

        let v = Validator {
            storage_type: StorageType::Redis,
        };
        assert!(v.validate_queue_type(QueueType::Rdb).is_err());
        assert_eq!(
            v.validate_queue_type(QueueType::Redis).unwrap(),
            QueueType::Redis
        );
        assert_eq!(
            v.validate_queue_type(QueueType::Hybrid).unwrap(),
            QueueType::Redis
        );
        let v = Validator {
            storage_type: StorageType::Hybrid,
        };
        assert_eq!(
            v.validate_queue_type(QueueType::Rdb).unwrap(),
            QueueType::Rdb
        );
        assert_eq!(
            v.validate_queue_type(QueueType::Redis).unwrap(),
            QueueType::Redis
        );
        assert_eq!(
            v.validate_queue_type(QueueType::Hybrid).unwrap(),
            QueueType::Hybrid
        );
    }

    #[test]
    fn test_validate_retry_policy() {
        let v = Validator {
            storage_type: StorageType::RDB,
        };
        let rp = RetryPolicy {
            r#type: RetryType::Exponential as i32,
            basis: 1.2,
            interval: 0,
            max_retry: 0,
            max_interval: 0,
        };
        assert!(v.validate_retry_policy(&rp).is_ok());
        let mut r = rp.clone();
        r.basis = 0.3;
        assert!(v.validate_retry_policy(&r).is_err());
    }
    #[test]
    fn test_validate_worker_by_response_type_for_rdb_storage() {
        let v = Validator {
            storage_type: StorageType::RDB,
        };
        let operation = WorkerOperation {
            operation: Some(Operation::Command(
                proto::jobworkerp::data::CommandOperation {
                    name: "ls".to_string(),
                },
            )),
        };
        let mut w = WorkerData {
            name: "ListCommand".to_string(),
            r#type: RunnerType::Command as i32,
            operation: Some(operation),
            queue_type: QueueType::Rdb as i32,
            response_type: ResponseType::Direct as i32,
            store_failure: true,
            store_success: true,
            ..Default::default()
        };
        // in ResponseType::Direct cannot be used by storage_type: RDB
        assert!(v.validate_worker(&w).is_err());

        // in ResponseType::ListenAfter, store_success and store_failure must be set to true for storage_type: RDB
        w.response_type = ResponseType::ListenAfter as i32;
        assert!(v.validate_worker(&w).is_ok());
        w.store_success = false;
        assert!(v.validate_worker(&w).is_err());
        w.store_success = true;
        w.store_failure = false;
        assert!(v.validate_worker(&w).is_err());
        w.store_failure = true;
        assert!(v.validate_worker(&w).is_ok());

        let v = Validator {
            storage_type: StorageType::Hybrid,
        };

        w.response_type = ResponseType::Direct as i32;
        assert!(v.validate_worker(&w).is_err());
        w.queue_type = QueueType::Rdb as i32;
        assert!(v.validate_worker(&w).is_err());
        w.queue_type = QueueType::Redis as i32;
        assert!(v.validate_worker(&w).is_ok());
        w.queue_type = QueueType::Hybrid as i32;
        assert!(v.validate_worker(&w).is_ok());

        w.response_type = ResponseType::NoResult as i32;
        assert!(v.validate_worker(&w).is_ok());
        w.queue_type = QueueType::Rdb as i32;
        assert!(v.validate_worker(&w).is_ok());
        w.queue_type = QueueType::Hybrid as i32;
        assert!(v.validate_worker(&w).is_ok());
        w.queue_type = QueueType::Redis as i32;
        assert!(v.validate_worker(&w).is_ok());
    }

    #[test]
    fn test_validate_worker_by_periodic_interval() {
        let v = Validator {
            storage_type: StorageType::Hybrid,
        };
        let operation = WorkerOperation {
            operation: Some(Operation::Command(
                proto::jobworkerp::data::CommandOperation {
                    name: "ls".to_string(),
                },
            )),
        };
        let mut w = WorkerData {
            name: "ListCommand".to_string(),
            r#type: RunnerType::Command as i32,
            operation: Some(operation),
            queue_type: QueueType::Rdb as i32,
            response_type: ResponseType::NoResult as i32,
            store_failure: true,
            store_success: true,
            periodic_interval: 0,
            ..Default::default()
        };
        assert!(v.validate_worker(&w).is_ok());
        w.periodic_interval = 1000;
        assert!(v.validate_worker(&w).is_err());
        w.periodic_interval = 1001;
        w.response_type = ResponseType::ListenAfter as i32;
        assert!(v.validate_worker(&w).is_ok());
        w.periodic_interval = 0;
        assert!(v.validate_worker(&w).is_ok());
        w.periodic_interval = 1001;
        assert!(v.validate_worker(&w).is_ok());
        w.periodic_interval = 500;
        assert!(v.validate_worker(&w).is_err());
    }
}
