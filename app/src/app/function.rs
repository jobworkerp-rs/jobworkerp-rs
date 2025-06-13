use super::function::function_set::FunctionSetAppImpl;
use super::job::{JobApp, UseJobApp};
use super::runner::RunnerApp;
use super::worker::WorkerApp;
use super::{
    function::function_set::UseFunctionSetApp, runner::UseRunnerApp, worker::UseWorkerApp,
};
use crate::app::function::function_set::FunctionSetApp as BaseFunctionSetApp;
use anyhow::Result;
use async_trait::async_trait;
use core::fmt;
use helper::{workflow::ReusableWorkflowHelper, FunctionCallHelper, McpNameConverter};
use infra::infra::runner::rows::RunnerWithSchema;
use infra_utils::infra::cache::UseMokaCache;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_runner::jobworkerp::runner::ReusableWorkflowRunnerSettings;
use proto::jobworkerp::data::{RunnerType, StreamingOutputType, WorkerData, WorkerId};
use proto::jobworkerp::function::data::{
    function_specs, FunctionSchema, FunctionSpecs, McpToolList,
};
use proto::ProtobufHelper;
use std::collections::HashMap;
use std::sync::Arc;

pub mod function_set;
pub mod helper;

#[async_trait]
pub trait FunctionApp:
    UseFunctionSetApp
    + UseWorkerApp
    + UseRunnerApp
    + UseMokaCache<Arc<String>, Vec<FunctionSpecs>>
    + FunctionSpecConverter
    + ReusableWorkflowHelper
    + FunctionCallHelper
    + fmt::Debug
    + Send
    + Sync
    + 'static
{
    async fn find_functions(
        &self,
        exclude_runner: bool,
        exclude_worker: bool,
    ) -> Result<Vec<FunctionSpecs>> {
        let mut functions = Vec::new();

        // Get runners if not excluded
        if !exclude_runner {
            let runners = self.runner_app().find_runner_list(None, None).await?;
            for runner in runners {
                functions.push(Self::convert_runner_to_function_specs(runner));
            }
        }

        // Get workers if not excluded
        if !exclude_worker {
            let workers = self
                .worker_app()
                .find_list(vec![], None, None, None)
                .await?;
            for worker in workers {
                if let Some(wid) = worker.id {
                    if let Some(data) = worker.data {
                        if let Some(runner_id) = data.runner_id {
                            if let Some(runner) = self.runner_app().find_runner(&runner_id).await? {
                                // Check if the worker is associated with the runner
                                if runner.id == Some(runner_id) {
                                    // warn only
                                    match Self::convert_worker_to_function_specs(wid, data, runner)
                                    {
                                        Ok(specs) => {
                                            functions.push(specs);
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                "Failed to convert worker to function specs: {:?}",
                                                e
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(functions)
    }
    async fn find_functions_by_set(&self, set_name: &str) -> Result<Vec<FunctionSpecs>> {
        // Get function set by name
        let function_set = self
            .function_set_app()
            .find_function_set_by_name(set_name)
            .await?;

        if let Some(set) = function_set {
            if let Some(data) = set.data {
                let mut functions = Vec::new();

                // Process each target in the function set
                for target in &data.targets {
                    match target.r#type {
                        // Runner type target (FunctionType::Runner = 0)
                        0 => {
                            // Create runner ID and find runner
                            let runner_id = proto::jobworkerp::data::RunnerId { value: target.id };
                            if let Some(runner) = self.runner_app().find_runner(&runner_id).await? {
                                functions.push(Self::convert_runner_to_function_specs(runner));
                            }
                        }
                        // Worker type target (FunctionType::Worker = 1)
                        1 => {
                            // Create worker ID and find worker
                            let worker_id = proto::jobworkerp::data::WorkerId { value: target.id };
                            if let Some(worker) = self.worker_app().find(&worker_id).await? {
                                if let Some(wid) = worker.id {
                                    if let Some(worker_data) = worker.data {
                                        if let Some(rid) = worker_data.runner_id {
                                            if let Some(runner) =
                                                self.runner_app().find_runner(&rid).await?
                                            {
                                                if let Ok(specs) =
                                                    Self::convert_worker_to_function_specs(
                                                        wid,
                                                        worker_data,
                                                        runner,
                                                    )
                                                {
                                                    functions.push(specs);
                                                } else {
                                                    tracing::warn!(
                                                        "Failed to convert worker to function specs"
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        // Unknown type, log warning and skip
                        unknown_type => {
                            tracing::warn!("Unknown function target type: {}", unknown_type);
                        }
                    }
                }

                Ok(functions)
            } else {
                Ok(Vec::new())
            }
        } else {
            Ok(Vec::new())
        }
    }
    async fn call_function(
        &self,
        meta: Arc<HashMap<String, String>>,
        name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<serde_json::Value> {
        tracing::debug!("call_tool: {}: {:?}", name, &arguments);

        match self.find_runner_by_name_with_mcp(name).await {
            Ok(Some((
                RunnerWithSchema {
                    id: Some(rid),
                    data: Some(rdata),
                    ..
                },
                _,
            ))) if rdata.runner_type == RunnerType::ReusableWorkflow as i32 => {
                self.handle_reusable_workflow(name, arguments, rid, rdata)
                    .await
            }
            Ok(Some((runner, tool_name_opt))) => self
                .handle_runner_call(meta, arguments, runner, tool_name_opt)
                .await
                .map(|r| r.unwrap_or_default()),
            Ok(None) => self.handle_worker_call(meta, name, arguments).await,
            Err(e) => {
                tracing::error!("error: {:#?}", &e);
                Err(e)
            }
        }
    }
}

#[derive(Debug)]
pub struct FunctionAppImpl {
    function_set_app: Arc<FunctionSetAppImpl>,
    runner_app: Arc<dyn crate::app::runner::RunnerApp>,
    worker_app: Arc<dyn crate::app::worker::WorkerApp>,
    job_app: Arc<dyn crate::app::job::JobApp>,
    function_cache: infra_utils::infra::cache::MokaCacheImpl<Arc<String>, Vec<FunctionSpecs>>,
}

impl FunctionAppImpl {
    pub fn new(
        function_set_app: Arc<FunctionSetAppImpl>,
        runner_app: Arc<dyn crate::app::runner::RunnerApp>,
        worker_app: Arc<dyn crate::app::worker::WorkerApp>,
        job_app: Arc<dyn crate::app::job::JobApp>,
        mc_config: &infra_utils::infra::memory::MemoryCacheConfig,
    ) -> Self {
        let function_cache = infra_utils::infra::cache::MokaCacheImpl::new(
            &infra_utils::infra::cache::MokaCacheConfig {
                num_counters: mc_config.num_counters,
                ttl: Some(std::time::Duration::from_secs(60)), // 60 seconds TTL
            },
        );
        Self {
            function_set_app,
            runner_app,
            worker_app,
            job_app,
            function_cache,
        }
    }
}

impl UseFunctionSetApp for FunctionAppImpl {
    fn function_set_app(&self) -> &FunctionSetAppImpl {
        &self.function_set_app
    }
}

impl UseRunnerApp for FunctionAppImpl {
    fn runner_app(&self) -> Arc<dyn RunnerApp + 'static> {
        self.runner_app.clone()
    }
}

impl UseWorkerApp for FunctionAppImpl {
    fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static> {
        &self.worker_app
    }
}
impl UseJobApp for FunctionAppImpl {
    fn job_app(&self) -> &Arc<dyn JobApp + 'static> {
        &self.job_app
    }
}

impl infra_utils::infra::cache::UseMokaCache<Arc<String>, Vec<FunctionSpecs>> for FunctionAppImpl {
    fn cache(&self) -> &infra_utils::infra::cache::MokaCache<Arc<String>, Vec<FunctionSpecs>> {
        self.function_cache.cache()
    }
}
impl FunctionSpecConverter for FunctionAppImpl {}
impl ProtobufHelper for FunctionAppImpl {}
impl ReusableWorkflowHelper for FunctionAppImpl {}
impl McpNameConverter for FunctionAppImpl {}
impl FunctionCallHelper for FunctionAppImpl {
    fn timeout_sec(&self) -> u32 {
        30 * 60 // 30 minutes
    }
}
impl FunctionApp for FunctionAppImpl {}

pub trait UseFunctionApp {
    fn function_app(&self) -> &FunctionAppImpl;
}

pub trait FunctionSpecConverter {
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
                .map(|s| Self::parse_as_json_with_key_or_noop("schema", s));
            let input_schema =
                input_schema.map(|s| Self::parse_as_json_with_key_or_noop("document", s));
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
                    .as_ref()
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

    // Parse JSON value with key extraction
    #[allow(clippy::if_same_then_else)]
    fn parse_as_json_with_key_or_noop(key: &str, value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(mut value_map) => {
                if let Some(candidate_value) = value_map.remove(key) {
                    // Try to remove key or noop
                    // Check if not empty object
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
                        // Original value
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
}
