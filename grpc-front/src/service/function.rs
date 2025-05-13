use anyhow::Result;
use std::sync::Arc;
use std::{fmt::Debug, time::Duration};

use crate::proto::jobworkerp::data::WorkerData;
use crate::proto::jobworkerp::data::{Worker, WorkerId};
use crate::proto::jobworkerp::function::data::{FunctionSchema, FunctionSpecs};
use crate::proto::jobworkerp::function::service::function_service_server::FunctionService;
use crate::proto::jobworkerp::function::service::FindFunctionRequest;
use crate::service::error_handle::handle_error;
use app::app::runner::RunnerApp;
use app::app::worker::WorkerApp;
use app::module::AppModule;
use async_stream::stream;
use futures::stream::BoxStream;
use infra::infra::runner::rows::RunnerWithSchema;
use infra_utils::trace::Tracing;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_runner::jobworkerp::runner::ReusableWorkflowRunnerSettings;
use proto::jobworkerp::data::{RunnerType, StreamingOutputType};
use proto::jobworkerp::function::data::{function_specs, McpToolList};
use tonic::Response;

pub trait FunctionGrpc {
    fn runner_app(&self) -> &Arc<dyn RunnerApp + 'static>;
    fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static>;
}

const DEFAULT_TTL: Duration = Duration::from_secs(30);

#[tonic::async_trait]
impl<T: FunctionGrpc + Tracing + Send + Debug + Sync + 'static> FunctionService for T {
    type FindListStream = BoxStream<'static, Result<FunctionSpecs, tonic::Status>>;

    #[tracing::instrument(level = "info", skip(self, request), fields(method = "find_list"))]
    async fn find_list(
        &self,
        request: tonic::Request<FindFunctionRequest>,
    ) -> Result<tonic::Response<<T as FunctionService>::FindListStream>, tonic::Status> {
        let _s = Self::trace_request("function", "find_list", &request);
        let req = request.into_inner();

        let mut functions = Vec::new();

        // Get runners if not excluded
        if !req.exclude_runner {
            match self
                .runner_app()
                .find_runner_list(None, None, Some(&DEFAULT_TTL))
                .await
            {
                Ok(runners) => {
                    for runner in runners {
                        functions.push(convert_runner_to_function_specs(runner));
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
                                            // warn only
                                            let _ =  convert_worker_to_function_specs(
                                                wid, data, runner,
                                            ).map(|r| {
                                                functions.push(r);
                                            }).inspect_err(|e|
                                                tracing::warn!(
                                                    "Failed to convert worker to function specs: {:?}",
                                                    e
                                                )
                                            );
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

        // Return stream of functions
        Ok(Response::new(Box::pin(stream! {
            for function in functions {
                yield Ok(function);
            }
        })))
    }
}

// Helper function to convert Runner to FunctionSpecs
fn convert_runner_to_function_specs(runner: RunnerWithSchema) -> FunctionSpecs {
    if runner
        .data
        .as_ref()
        .is_some_and(|r| r.runner_type == RunnerType::McpServer as i32)
    {
        FunctionSpecs {
            runner_type: RunnerType::McpServer as i32,
            runner_id: runner.id,
            worker_id: None,
            name: runner
                .data
                .as_ref()
                .map_or(String::new(), |data| data.name.clone()),
            description: runner
                .data
                .as_ref()
                .map_or(String::new(), |data| data.description.clone()),
            // TODO divide and extract settings for each tool
            schema: Some(function_specs::Schema::McpTools(McpToolList {
                list: runner.tools,
            })),
            output_type: runner
                .data
                .as_ref()
                .map(|data| data.output_type)
                .unwrap_or(StreamingOutputType::NonStreaming as i32),
        }
    } else {
        FunctionSpecs {
            runner_type: runner
                .data
                .as_ref()
                .map(|data| data.runner_type)
                .unwrap_or(RunnerType::Plugin as i32),
            runner_id: runner.id,
            worker_id: None,
            name: runner
                .data
                .as_ref()
                .map_or(String::new(), |data| data.name.clone()),
            description: runner
                .data
                .as_ref()
                .map_or(String::new(), |data| data.description.clone()),
            schema: Some(function_specs::Schema::SingleSchema(FunctionSchema {
                settings: Some(runner.settings_schema),
                arguments: runner.arguments_schema,
                result_output_schema: runner.output_schema,
            })),
            output_type: runner
                .data
                .as_ref()
                .map(|data| data.output_type)
                .unwrap_or(StreamingOutputType::NonStreaming as i32),
        }
    }
}

// Helper function to convert Worker to FunctionSpecs
fn convert_worker_to_function_specs(
    id: WorkerId,
    data: WorkerData,
    runner: RunnerWithSchema,
) -> Result<FunctionSpecs> {
    // change input schema to the input of saved workflow
    if runner
        .data
        .as_ref()
        .is_some_and(|d| d.runner_type == RunnerType::ReusableWorkflow as i32)
    {
        let settings = ProstMessageCodec::deserialize_message::<ReusableWorkflowRunnerSettings>(
            data.runner_settings.as_slice(),
        )?;
        let input_schema = settings
            .input_schema()
            .map(|s| parse_as_json_with_key_or_noop("schema", s));
        let input_schema = input_schema.map(|s| parse_as_json_with_key_or_noop("document", s));
        Ok(FunctionSpecs {
            runner_type: RunnerType::ReusableWorkflow as i32,
            runner_id: runner.id,
            worker_id: Some(id),
            name: data.name,
            description: data.description,
            schema: Some(function_specs::Schema::SingleSchema(FunctionSchema {
                settings: None, // Workers don't have config (already set)
                arguments: input_schema.map(|s| s.to_string()).unwrap_or_default(),
                result_output_schema: runner.output_schema,
            })),
            output_type: runner
                .data
                .map(|data| data.output_type)
                .unwrap_or(StreamingOutputType::NonStreaming as i32),
        })
    } else if runner
        .data
        .as_ref()
        .is_some_and(|r| r.runner_type == RunnerType::McpServer as i32)
    {
        Ok(FunctionSpecs {
            runner_type: RunnerType::McpServer as i32,
            runner_id: runner.id,
            worker_id: Some(id),
            name: data.name,
            description: data.description,
            // TODO divide and extract settings for each tool
            schema: Some(function_specs::Schema::McpTools(McpToolList {
                list: runner.tools,
            })),
            output_type: runner
                .data
                .as_ref()
                .map(|data| data.output_type)
                .unwrap_or(StreamingOutputType::NonStreaming as i32),
        })
    } else {
        // TODO mcp server schema
        Ok(FunctionSpecs {
            runner_type: runner
                .data
                .as_ref()
                .map(|data| data.runner_type)
                .unwrap_or(RunnerType::Plugin as i32),
            runner_id: runner.id,
            worker_id: Some(id),
            name: data.name,
            description: data.description,
            schema: Some(function_specs::Schema::SingleSchema(FunctionSchema {
                settings: None, // Workers don't have config (already set)
                arguments: runner.arguments_schema,
                result_output_schema: runner.output_schema,
            })),
            output_type: runner
                .data
                .as_ref()
                .map(|data| data.output_type)
                .unwrap_or(StreamingOutputType::NonStreaming as i32),
        })
    }
}

// try value -> map -> map.remove(key) -> (json or string) or original
// (for json schema extraction)
#[allow(clippy::if_same_then_else)]
pub fn parse_as_json_with_key_or_noop(key: &str, value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(mut value_map) => {
            if let Some(candidate_value) = value_map.remove(key) {
                // try to remove key or noop
                // not empty object
                if candidate_value.is_object()
                    && candidate_value.as_object().is_some_and(|o| !o.is_empty())
                {
                    candidate_value
                } else if candidate_value.is_string()
                    && candidate_value.as_str().is_some_and(|s| !s.is_empty())
                {
                    candidate_value
                } else {
                    tracing::warn!(
                        "data key:{} is not a valid json or string: {:#?}",
                        key,
                        &candidate_value
                    );
                    // original value
                    value_map.insert(key.to_string(), candidate_value.clone());
                    serde_json::Value::Object(value_map)
                }
            } else {
                serde_json::Value::Object(value_map)
            }
        }
        _ => {
            tracing::warn!("value is not json object: {:#?}", &value);
            value
        }
    }
}
#[derive(DebugStub)]
pub(crate) struct FunctionGrpcImpl {
    #[debug_stub = "AppModule"]
    app_module: Arc<AppModule>,
}

impl FunctionGrpcImpl {
    pub fn new(app_module: Arc<AppModule>) -> Self {
        FunctionGrpcImpl { app_module }
    }
}

impl FunctionGrpc for FunctionGrpcImpl {
    fn runner_app(&self) -> &Arc<dyn RunnerApp + 'static> {
        &self.app_module.runner_app
    }

    fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static> {
        &self.app_module.worker_app
    }
}

// Implement tracing for FunctionGrpcImpl
impl Tracing for FunctionGrpcImpl {}
