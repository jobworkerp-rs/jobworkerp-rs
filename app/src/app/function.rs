use super::function::function_set::FunctionSetAppImpl;
use super::job::{JobApp, UseJobApp};
use super::runner::RunnerApp;
use super::worker::WorkerApp;
use super::{
    function::function_set::UseFunctionSetApp, runner::UseRunnerApp, worker::UseWorkerApp,
};
use crate::app::function::function_set::FunctionSetApp as BaseFunctionSetApp;
use crate::app::job::execute::UseJobExecutor;
use crate::app::job_result::UseJobResultApp;
use crate::app::runner::{RunnerDataWithDescriptor, UseRunnerParserWithCache};
use anyhow::Result;
use async_stream::stream;
use async_trait::async_trait;
use core::fmt;
use helper::{workflow::ReusableWorkflowHelper, FunctionCallHelper, McpNameConverter};
use infra::infra::runner::rows::RunnerWithSchema;
use infra_utils::infra::cache::{MokaCacheImpl, UseMokaCache};
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::jobworkerp::runner::ReusableWorkflowRunnerSettings;
use proto::jobworkerp::data::{RunnerType, StreamingOutputType, WorkerData, WorkerId};
use proto::jobworkerp::function::data::{
    function_specs, FunctionResult, FunctionSchema, FunctionSpecs, McpToolList, WorkerOptions,
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
    #[allow(clippy::too_many_arguments)]
    fn handle_runner_for_front<'a>(
        &'a self,
        metadata: Arc<HashMap<String, String>>,
        runner_name: &'a str,
        runner_settings: Option<serde_json::Value>,
        worker_options: Option<WorkerOptions>,
        arg_json: serde_json::Value,
        uniq_key: Option<String>,
        timeout_sec: u32,
        streaming: bool,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = Result<FunctionResult>> + Send + 'a>> {
        use futures::{stream, StreamExt};

        let future = async move {
            tracing::debug!(
                "handle_runner_for_front: runner_name: {:?}, uniq_key: {:?}, streaming: {}",
                runner_name,
                uniq_key,
                streaming
            );

            let runner = self
                .runner_app()
                .find_runner_by_name(runner_name)
                .await?
                .ok_or(JobWorkerError::NotFound(format!(
                    "Runner with name '{runner_name}' not found"
                )))?;
            // XXX serialize worker options to JSON... (transform function for WorkerOptions)
            let worker_params = worker_options
                .map(serde_json::to_value)
                .unwrap_or_else(|| Ok(serde_json::json!({})))?;
            let worker_data = self
                .create_worker_data(&runner, runner_settings, Some(worker_params))
                .await?;
            if let RunnerWithSchema {
                id: Some(_id),
                data: Some(runner_data),
                ..
            } = &runner
            {
                match self
                    .setup_worker_and_enqueue_with_json_full_output(
                        metadata,
                        runner_data.name.as_str(),
                        worker_data,
                        arg_json,
                        uniq_key,
                        timeout_sec,
                        streaming,
                    )
                    .await
                {
                    Ok((jid, jres, stream_opt)) => {
                        let job_id = jid.value.to_string();
                        let started_at = chrono::Utc::now().timestamp_millis();

                        // Use runner name directly since we have it
                        let runner_name = Some(runner_data.name.clone());

                        // Use the common stream processing method
                        Ok(self.process_job_result_to_stream(
                            job_id,
                            started_at,
                            jres,
                            stream_opt,
                            runner_name,
                        ))
                    }
                    Err(e) => {
                        tracing::error!("Error setting up worker and enqueueing job: {:?}", e);
                        Err(e)
                    }
                }
            } else {
                tracing::error!("Runner not found");
                Err(JobWorkerError::NotFound("Runner not found".to_string()).into())
            }
        };

        Box::pin(
            stream::once(future)
                .then(|result| async move {
                    match result {
                        Ok(stream) => stream,
                        Err(e) => Box::pin(stream::once(async move { Err(e) })),
                    }
                })
                .flatten(),
        )
    }

    // for LLM_CHAT, mcp proxy
    fn handle_worker_call_for_front<'a>(
        &'a self,
        meta: Arc<HashMap<String, String>>,
        name: &'a str,
        arguments: serde_json::Value,
        unique_key: Option<String>,
        job_timeout_sec: u32,
        streaming: bool, // TODO if true, use streaming job
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = Result<FunctionResult>> + Send + 'a>> {
        use futures::StreamExt;

        Box::pin(stream! {
            tracing::info!("runner not found, run as worker: {:?}", &name);

            let result = self
                .enqueue_with_worker_name(
                    meta.clone(),
                    name,
                    &arguments,
                    unique_key,
                    job_timeout_sec,
                    streaming,
                )
                .await;

            match result {
                Ok((jid, jres, stream_opt)) => {
                    let job_id = jid.value.to_string();
                    let started_at = chrono::Utc::now().timestamp_millis();

                    // Find worker to get runner information for decoding
                    let worker_opt = self.worker_app().find_by_name(name).await.ok().flatten();
                    let runner_name = if let Some(worker) = &worker_opt {
                        if let Some(worker_data) = &worker.data {
                            if let Some(runner_id) = &worker_data.runner_id {
                                self.runner_app().find_runner(runner_id).await.ok().flatten()
                                    .and_then(|r| r.data.map(|rd| rd.name))
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    // Use the common stream processing method
                    let stream = self.process_job_result_to_stream(
                        job_id,
                        started_at,
                        jres,
                        stream_opt,
                        runner_name,
                    );

                    // Yield all results from the stream
                    let mut stream = std::pin::pin!(stream);
                    while let Some(result) = stream.next().await {
                        yield result;
                    }
                }
                Err(e) => {
                    yield Err(e);
                }
            }
        })
    }

    // Common method to process job execution results into FunctionResult stream
    fn process_job_result_to_stream<'a>(
        &'a self,
        job_id: String,
        started_at: i64,
        jres: Option<proto::jobworkerp::data::JobResult>,
        stream_opt: Option<
            futures::stream::BoxStream<'static, proto::jobworkerp::data::ResultOutputItem>,
        >,
        runner_name: Option<String>,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = Result<FunctionResult>> + Send + 'a>> {
        use futures::StreamExt;
        use proto::jobworkerp::data::{result_output_item, ResultStatus};
        use proto::jobworkerp::function::data::FunctionExecutionInfo;

        Box::pin(stream! {
                // Handle streaming case
            if let Some(mut result_stream) = stream_opt {
                while let Some(item) = result_stream.next().await {
                    match item.item {
                        Some(result_output_item::Item::Data(data)) => {
                            // Decode the data using runner information
                            let decoded_output = if let Some(ref rname) = runner_name {
                                self.decode_job_result_output(None, Some(rname), &data).await?
                            } else {
                                Err(JobWorkerError::NotFound(
                                    "Runner name not found for decoding".to_string(),
                                ))?
                            };

                            // Yield intermediate result without metadata
                            yield Ok(FunctionResult {
                                output: decoded_output.to_string(),
                                status: Some(ResultStatus::Success as i32),
                                error_message: None,
                                error_code: None,
                                last_info: None,
                            });
                        }
                        Some(result_output_item::Item::End(trailer)) => {
                            // Yield final result with metadata
                            let completed_at = chrono::Utc::now().timestamp_millis();

                            yield Ok(FunctionResult {
                                output: "".to_string(),
                                status: Some(ResultStatus::Success as i32),
                                error_message: None,
                                error_code: None,
                                last_info: Some(FunctionExecutionInfo {
                                    job_id: job_id.clone(),
                                    started_at,
                                    completed_at: Some(completed_at),
                                    execution_time_ms: Some(completed_at - started_at),
                                    metadata: trailer.metadata,
                                }),
                            });
                            break;
                        }
                        None => {
                            // Skip empty items
                        }
                    }
                }
            } else if let Some(job_result) = jres {
                // Handle non-streaming case with direct result
                let completed_at = chrono::Utc::now().timestamp_millis();

                if let Some(result_data) = job_result.data {
                    let status = result_data.status();
                    let raw_output = if let Some(output_data) = result_data.output {
                        output_data.items
                    } else {
                        Vec::new()
                    };

                    // Decode the output using runner information
                    let decoded_output = if let Some(ref rname) = runner_name {
                        match self.decode_job_result_output(None, Some(rname), &raw_output).await {
                            Ok(decoded) => decoded.to_string(),
                            Err(_) => String::from_utf8_lossy(&raw_output).to_string(),
                        }
                    } else {
                        String::from_utf8_lossy(&raw_output).to_string()
                    };

                    // error result
                    let (error_message, error_code) = if status != ResultStatus::Success {
                        (
                            Some("Job execution failed".to_string()),
                            Some("EXECUTION_ERROR".to_string()),
                        )
                    } else {
                        (None, None)
                    };

                    yield Ok(FunctionResult {
                        output: decoded_output,
                        status: Some(status as i32),
                        error_message,
                        error_code,
                        last_info: Some(FunctionExecutionInfo {
                            job_id,
                            started_at,
                            completed_at: Some(completed_at),
                            execution_time_ms: Some(completed_at - started_at),
                            metadata: HashMap::new(),
                        }),
                    });
                } else {
                    yield Err(JobWorkerError::RuntimeError("Job result data is empty".to_string()).into());
                }
            } else {
                yield Err(JobWorkerError::RuntimeError("No result or stream available".to_string()).into());
            }
        })
    }

    // for LLM function calling (LLM_CHAT runner)
    async fn call_function_for_llm(
        &self,
        meta: Arc<HashMap<String, String>>,
        name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
        timeout_sec: u32,
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
            Ok(Some((runner, tool_name_opt))) => {
                self.handle_runner_call_from_llm(
                    meta,
                    arguments,
                    runner,
                    tool_name_opt,
                    None,
                    None,
                    timeout_sec,
                    false,
                ) // XXX default worker params, non streaming
                .await
            }
            Ok(None) => {
                self.handle_worker_call_for_llm(meta, name, arguments, None, false)
                    .await
            }
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
    job_result_app: Arc<dyn crate::app::job_result::JobResultApp>,
    function_cache: infra_utils::infra::cache::MokaCacheImpl<Arc<String>, Vec<FunctionSpecs>>,
    descriptor_cache: Arc<MokaCacheImpl<Arc<String>, RunnerDataWithDescriptor>>,
}

impl FunctionAppImpl {
    pub fn new(
        function_set_app: Arc<FunctionSetAppImpl>,
        runner_app: Arc<dyn crate::app::runner::RunnerApp>,
        worker_app: Arc<dyn crate::app::worker::WorkerApp>,
        job_app: Arc<dyn crate::app::job::JobApp>,
        job_result_app: Arc<dyn crate::app::job_result::JobResultApp>,
        descriptor_cache: Arc<MokaCacheImpl<Arc<String>, RunnerDataWithDescriptor>>,
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
            job_result_app,
            function_cache,
            descriptor_cache,
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
impl UseJobResultApp for FunctionAppImpl {
    fn job_result_app(&self) -> &Arc<dyn crate::app::job_result::JobResultApp + 'static> {
        &self.job_result_app
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
impl UseRunnerParserWithCache for FunctionAppImpl {
    fn descriptor_cache(&self) -> &MokaCacheImpl<Arc<String>, RunnerDataWithDescriptor> {
        &self.descriptor_cache
    }
}
impl UseJobExecutor for FunctionAppImpl {}
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
