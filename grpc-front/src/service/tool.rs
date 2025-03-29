use std::sync::Arc;
use std::{fmt::Debug, time::Duration};

use crate::proto::jobworkerp::data::ToolId;
use crate::proto::jobworkerp::data::WorkerData;
use crate::proto::jobworkerp::data::{RunnerId, ToolInputSchema, ToolSpecs, Worker, WorkerId};
use crate::proto::jobworkerp::service::tool_service_server::ToolService;
use crate::proto::jobworkerp::service::FindToolRequest;
use crate::service::error_handle::handle_error;
use app::app::runner::RunnerApp;
use app::app::worker::WorkerApp;
use app::module::AppModule;
use async_stream::stream;
use futures::stream::BoxStream;
use infra::infra::runner::rows::RunnerWithSchema;
use infra_utils::trace::Tracing;
use proto::jobworkerp::data::StreamingOutputType;
use tonic::Response;

pub trait ToolGrpc {
    fn runner_app(&self) -> &Arc<dyn RunnerApp + 'static>;
    fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static>;
}

const DEFAULT_TTL: Duration = Duration::from_secs(30);

#[tonic::async_trait]
impl<T: ToolGrpc + Tracing + Send + Debug + Sync + 'static> ToolService for T {
    type FindListStream = BoxStream<'static, Result<ToolSpecs, tonic::Status>>;

    #[tracing::instrument(level = "info", skip(self, request), fields(method = "find_list"))]
    async fn find_list(
        &self,
        request: tonic::Request<FindToolRequest>,
    ) -> Result<tonic::Response<Self::FindListStream>, tonic::Status> {
        let _s = Self::trace_request("tool", "find_list", &request);
        let req = request.into_inner();

        let mut tools = Vec::new();

        // Get runners if not excluded
        if !req.exclude_runner {
            match self
                .runner_app()
                .find_runner_list(None, None, Some(&DEFAULT_TTL))
                .await
            {
                Ok(runners) => {
                    for runner in runners {
                        tools.push(convert_runner_to_tool_specs(runner));
                    }
                }
                Err(e) => return Err(handle_error(&e)),
            }
        }

        // Get workers if not excluded
        if !req.exclude_worker {
            match self.worker_app().find_list(None, None).await {
                Ok(workers) => {
                    for worker in workers {
                        if let Worker {
                            id: Some(wid),
                            data: Some(data),
                        } = worker
                        {
                            if let Some(rid) = data.runner_id {
                                match self.runner_app().find_runner(&rid, None).await {
                                    Ok(Some(runner)) => {
                                        // Check if the worker is associated with the runner
                                        if runner.id == Some(rid) {
                                            tools.push(convert_worker_to_tool_specs(
                                                wid, data, runner,
                                            ));
                                        }
                                    }
                                    Ok(None) => {
                                        // No associated runner found, ignore this worker
                                        tracing::error!(
                                            "Worker {} has no associated runner",
                                            wid.value
                                        );
                                    }
                                    Err(e) => return Err(handle_error(&e)),
                                }
                            }
                        }
                    }
                }
                Err(e) => return Err(handle_error(&e)),
            }
        }

        // Return stream of tools
        Ok(Response::new(Box::pin(stream! {
            for tool in tools {
                yield Ok(tool);
            }
        })))
    }
}

// Helper function to convert Runner to ToolSpecs
fn convert_runner_to_tool_specs(runner: RunnerWithSchema) -> ToolSpecs {
    ToolSpecs {
        tool_id: Some(ToolId::RunnerId(RunnerId {
            value: runner.id.as_ref().map_or(0, |id| id.value),
        })),
        name: runner
            .data
            .as_ref()
            .map_or(String::new(), |data| data.name.clone()),
        description: runner
            .data
            .as_ref()
            .map_or(String::new(), |data| data.description.clone()),
        input_schema: Some(ToolInputSchema {
            settings: Some(runner.settings_schema),
            arguments: runner.arguments_schema,
        }),
        result_output_schema: runner
            .data
            .as_ref()
            .and_then(|data| data.result_output_proto.clone()),
        output_type: runner
            .data
            .as_ref()
            .map(|data| data.output_type)
            .unwrap_or(StreamingOutputType::NonStreaming as i32),
    }
}

// Helper function to convert Worker to ToolSpecs
fn convert_worker_to_tool_specs(
    id: WorkerId,
    data: WorkerData,
    runner: RunnerWithSchema,
) -> ToolSpecs {
    ToolSpecs {
        tool_id: Some(ToolId::WorkerId(id)),
        name: data.name,
        description: data.description,
        input_schema: Some(ToolInputSchema {
            settings: None, // Workers don't have config (already set)
            arguments: runner.arguments_schema,
        }),
        result_output_schema: None, // Workers don't have result schema in the proto definition
        output_type: runner
            .data
            .map(|data| data.output_type)
            .unwrap_or(StreamingOutputType::NonStreaming as i32),
    }
}

#[derive(DebugStub)]
pub(crate) struct ToolGrpcImpl {
    #[debug_stub = "AppModule"]
    app_module: Arc<AppModule>,
}

impl ToolGrpcImpl {
    pub fn new(app_module: Arc<AppModule>) -> Self {
        ToolGrpcImpl { app_module }
    }
}

impl ToolGrpc for ToolGrpcImpl {
    fn runner_app(&self) -> &Arc<dyn RunnerApp + 'static> {
        &self.app_module.runner_app
    }

    fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static> {
        &self.app_module.worker_app
    }
}

// Implement tracing for ToolGrpcImpl
impl Tracing for ToolGrpcImpl {}
