use crate::proto::jobworkerp::data::{Empty, WorkerInstance, WorkerInstanceId};
use crate::proto::jobworkerp::service::worker_instance_service_server::WorkerInstanceService;
use crate::proto::jobworkerp::service::{
    CountInstanceResponse, FindInstanceChannelListResponse, FindInstanceListRequest,
    InstanceChannelInfo,
};
use app::module::AppModule;
use async_stream::stream;
use command_utils::trace::Tracing;
use debug_stub_derive::DebugStub;
use futures::stream::BoxStream;
use infra::infra::worker_instance::{UseWorkerInstanceRepository, WorkerInstanceRepository};
use jobworkerp_base::WORKER_INSTANCE_CONFIG;
use std::collections::HashMap;
use std::sync::Arc;
use tonic::Response;

/// Implementation of WorkerInstanceService
#[derive(DebugStub)]
pub struct WorkerInstanceGrpcImpl {
    #[debug_stub = "AppModule"]
    app_module: Arc<AppModule>,
}

impl WorkerInstanceGrpcImpl {
    pub fn new(app_module: Arc<AppModule>) -> Self {
        Self { app_module }
    }

    fn worker_instance_repository(&self) -> Arc<dyn WorkerInstanceRepository> {
        self.app_module.repositories.worker_instance_repository()
    }

    async fn get_worker_counts_by_channel(&self) -> HashMap<String, i64> {
        match self.app_module.worker_app.count_by_channel().await {
            Ok(counts) => counts.into_iter().collect(),
            Err(e) => {
                tracing::warn!("Failed to get worker counts by channel: {}", e);
                HashMap::new()
            }
        }
    }
}

impl Tracing for WorkerInstanceGrpcImpl {}

#[tonic::async_trait]
impl WorkerInstanceService for WorkerInstanceGrpcImpl {
    type FindListStream = BoxStream<'static, Result<WorkerInstance, tonic::Status>>;

    #[tracing::instrument(level = "info", skip(self, request), fields(method = "find_list"))]
    async fn find_list(
        &self,
        request: tonic::Request<FindInstanceListRequest>,
    ) -> Result<tonic::Response<Self::FindListStream>, tonic::Status> {
        let _s = Self::trace_request("worker_instance", "find_list", &request);
        let req = request.into_inner();

        let active_only = req.active_only.unwrap_or(true);
        let timeout_millis = WORKER_INSTANCE_CONFIG.timeout_millis();

        let instances = if active_only {
            self.worker_instance_repository()
                .find_all_active(timeout_millis)
                .await
        } else {
            self.worker_instance_repository().find_all().await
        }
        .map_err(|e| {
            tracing::error!("Failed to get worker instances: {}", e);
            tonic::Status::internal("Failed to get worker instances")
        })?;

        // Filter by channel if specified
        let filtered: Vec<WorkerInstance> = if let Some(channel) = req.channel {
            instances
                .into_iter()
                .filter(|inst| {
                    inst.data
                        .as_ref()
                        .map(|d| d.channels.iter().any(|c| c.name == channel))
                        .unwrap_or(false)
                })
                .collect()
        } else {
            instances
        };

        Ok(Response::new(Box::pin(stream! {
            for inst in filtered {
                yield Ok(inst);
            }
        })))
    }

    #[tracing::instrument(level = "info", skip(self, request), fields(method = "count"))]
    async fn count(
        &self,
        request: tonic::Request<Empty>,
    ) -> Result<tonic::Response<CountInstanceResponse>, tonic::Status> {
        let _s = Self::trace_request("worker_instance", "count", &request);

        let timeout_millis = WORKER_INSTANCE_CONFIG.timeout_millis();

        let all = self
            .worker_instance_repository()
            .find_all()
            .await
            .map_err(|e| {
                tracing::error!("Failed to get worker instances: {}", e);
                tonic::Status::internal("Failed to get worker instances")
            })?;

        let active = self
            .worker_instance_repository()
            .find_all_active(timeout_millis)
            .await
            .map_err(|e| {
                tracing::error!("Failed to get active worker instances: {}", e);
                tonic::Status::internal("Failed to get active worker instances")
            })?;

        Ok(Response::new(CountInstanceResponse {
            total: all.len() as i64,
            active: active.len() as i64,
        }))
    }

    #[tracing::instrument(level = "info", skip(self, request), fields(method = "find"))]
    async fn find(
        &self,
        request: tonic::Request<WorkerInstanceId>,
    ) -> Result<tonic::Response<WorkerInstance>, tonic::Status> {
        let _s = Self::trace_request("worker_instance", "find", &request);
        let id = request.into_inner();

        match self.worker_instance_repository().find(&id).await {
            Ok(Some(inst)) => Ok(Response::new(inst)),
            Ok(None) => Err(tonic::Status::not_found("Worker instance not found")),
            Err(e) => {
                tracing::error!("Failed to find worker instance: {}", e);
                Err(tonic::Status::internal("Failed to find worker instance"))
            }
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
    ) -> Result<tonic::Response<FindInstanceChannelListResponse>, tonic::Status> {
        let _s = Self::trace_request("worker_instance", "find_channel_list", &request);

        let timeout_millis = WORKER_INSTANCE_CONFIG.timeout_millis();

        // Get channel aggregation from repository
        let channel_aggregation = self
            .worker_instance_repository()
            .get_channel_aggregation(timeout_millis)
            .await
            .map_err(|e| {
                tracing::error!("Failed to get channel aggregation: {}", e);
                tonic::Status::internal("Failed to get channel aggregation")
            })?;

        // Get worker definition counts
        let worker_counts = self.get_worker_counts_by_channel().await;

        // Build response
        let channels = channel_aggregation
            .into_iter()
            .map(|(name, agg)| {
                let display_name = if name.is_empty() {
                    "[default]".to_string()
                } else {
                    name.clone()
                };
                let worker_count = *worker_counts.get(&name).unwrap_or(&0);

                InstanceChannelInfo {
                    name: display_name,
                    total_concurrency: agg.total_concurrency,
                    active_instances: agg.active_instances as i32,
                    worker_count,
                }
            })
            .collect();

        Ok(Response::new(FindInstanceChannelListResponse { channels }))
    }
}
