use super::job::{JobApp, UseJobApp};
use super::runner::RunnerApp;
use super::worker::WorkerApp;
use super::{runner::UseRunnerApp, worker::UseWorkerApp};
use crate::app::job::execute::UseJobExecutor;
use crate::app::job_result::UseJobResultApp;
use crate::app::runner::{RunnerDataWithDescriptor, UseRunnerParserWithCache};
use anyhow::Result;
use async_stream::stream;
use async_trait::async_trait;
use core::fmt;
use helper::{FunctionCallHelper, McpNameConverter};
use infra::infra::runner::rows::RunnerWithSchema;
use infra::infra::{IdGeneratorWrapper, UseIdGenerator};
use jobworkerp_base::error::JobWorkerError;
use memory_utils::cache::moka::{MokaCacheImpl, UseMokaCache};
use proto::ProtobufHelper;
use proto::jobworkerp::data::RunnerData;
use proto::jobworkerp::data::{RunnerType, StreamingType, WorkerId};
use proto::jobworkerp::function::data::{FunctionResult, FunctionSpecs, WorkerOptions};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

pub mod converter;
pub mod function_set;
pub mod helper;

/// Result type from listen_result_by_job_id: (JobResult, Option<stream>).
pub type JobListenResult = Result<(
    proto::jobworkerp::data::JobResult,
    Option<futures::stream::BoxStream<'static, proto::jobworkerp::data::ResultOutputItem>>,
)>;

/// Result of enqueuing a function for LLM tool execution (Phase A of 2-stage split).
/// When `is_streaming` is true, the job was enqueued with StreamingType::Internal
/// and `result` is None — caller must use `await_function_result()` to get the result.
/// When `is_streaming` is false, `result` contains the already-completed value.
pub struct EnqueuedFunction {
    pub job_id: proto::jobworkerp::data::JobId,
    pub runner_name: String,
    pub result: Option<serde_json::Value>,
    pub is_streaming: bool,
    /// Pre-started result listener for streaming jobs (started immediately after enqueue
    /// to avoid missing pubsub events). None for non-streaming jobs.
    pub result_handle: Option<tokio::task::JoinHandle<JobListenResult>>,
    /// Method name for multi-method runners (e.g. "streaming" from "jwp-list-workers___streaming").
    /// Used to decode job results with the correct proto schema.
    pub using: Option<String>,
}

impl std::fmt::Debug for EnqueuedFunction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnqueuedFunction")
            .field("job_id", &self.job_id)
            .field("runner_name", &self.runner_name)
            .field("result", &self.result)
            .field("is_streaming", &self.is_streaming)
            .field("result_handle", &self.result_handle.as_ref().map(|_| "..."))
            .field("using", &self.using)
            .finish()
    }
}

#[async_trait]
pub trait FunctionApp:
    UseWorkerApp
    + UseRunnerApp
    + UseIdGenerator
    + UseMokaCache<Arc<String>, Vec<FunctionSpecs>>
    + converter::FunctionSpecConverter
    + FunctionCallHelper
    + infra::workflow::UseWorkflowLoader
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
        self.find_functions_all(exclude_runner, exclude_worker, false)
            .await
    }

    async fn find_functions_all(
        &self,
        exclude_runner: bool,
        exclude_worker: bool,
        include_full: bool, // Include full function specs with schema (large data)
    ) -> Result<Vec<FunctionSpecs>> {
        let mut functions = Vec::new();

        if !exclude_runner {
            let runners = self
                .runner_app()
                .find_runner_list(include_full, None, None)
                .await?;
            for runner in runners {
                functions.push(Self::convert_runner_to_function_specs(runner));
            }
        }

        if !exclude_worker {
            let workers = self
                .worker_app()
                .find_list(vec![], None, None, None, None, None, vec![], None, None)
                .await?;
            for worker in workers {
                if let Some(wid) = worker.id
                    && let Some(data) = worker.data
                    && let Some(runner_id) = data.runner_id
                    && let Some(runner) = self.runner_app().find_runner(&runner_id).await?
                    && runner.id == Some(runner_id)
                {
                    // warn only
                    match Self::convert_worker_to_function_specs(wid, data, runner) {
                        Ok(specs) => {
                            functions.push(specs);
                        }
                        Err(e) => {
                            tracing::warn!("Failed to convert worker to function specs: {:?}", e);
                        }
                    }
                }
            }
        }

        Ok(functions)
    }
    #[allow(clippy::too_many_arguments)]
    fn handle_runner_for_front<'a>(
        &'a self,
        metadata: Arc<HashMap<String, String>>,
        name: &'a str,
        runner_settings: Option<serde_json::Value>,
        worker_options: Option<WorkerOptions>,
        arg_json: serde_json::Value,
        uniq_key: Option<String>,
        timeout_sec: u32,
        streaming: bool,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = Result<FunctionResult>> + Send + 'a>> {
        use futures::{StreamExt, stream};

        let future = async move {
            tracing::debug!(
                "handle_runner_for_front: runner_name: {:?}, uniq_key: {:?}, streaming: {}",
                name,
                uniq_key,
                streaming
            );
            let (runner_name, tool_name_opt) =
                if let Some((server_name, tool_name)) = Self::divide_names(name) {
                    (server_name, Some(tool_name))
                } else {
                    (name.to_string(), None)
                };
            let runner = self
                .runner_app()
                .find_runner_by_name(&runner_name)
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
                let streaming_type = if streaming {
                    StreamingType::Response
                } else {
                    StreamingType::None
                };
                match self
                    .setup_worker_and_enqueue_with_json_full_output(
                        metadata,
                        runner_data.name.as_str(),
                        worker_data,
                        arg_json,
                        uniq_key,
                        timeout_sec,
                        streaming_type,
                        tool_name_opt.clone(),
                    )
                    .await
                {
                    Ok((jid, jres, stream_opt)) => {
                        let job_id = jid.value.to_string();
                        let started_at = chrono::Utc::now().timestamp_millis();

                        let runner_name = Some(runner_data.name.clone());

                        Ok(self.process_job_result_to_stream(
                            job_id,
                            started_at,
                            jres,
                            stream_opt,
                            runner_name,
                            tool_name_opt,
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

            let (worker_name, tool_name_opt) = if let Some((server_name, tool_name)) = Self::divide_names(name) {
                (server_name, Some(tool_name))
            } else {
                (name.to_string(), None)
            };

            let streaming_type = if streaming {
                StreamingType::Response
            } else {
                StreamingType::None
            };
            let result = self
                .enqueue_with_worker_name(
                    meta.clone(),
                    &worker_name,
                    &arguments,
                    unique_key,
                    job_timeout_sec,
                    streaming_type,
                    tool_name_opt.clone(), // Pass using parameter for worker tools
                )
                .await;

            match result {
                Ok((jid, jres, stream_opt)) => {
                    let job_id = jid.value.to_string();
                    let started_at = chrono::Utc::now().timestamp_millis();

                    // Find worker to get runner information for decoding
                    let worker_opt = self.worker_app().find_by_name(&worker_name).await.ok().flatten();
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

                    let stream = self.process_job_result_to_stream(
                        job_id,
                        started_at,
                        jres,
                        stream_opt,
                        runner_name,
                        tool_name_opt,
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
        using: Option<String>, // Method name for multi-method runners (MCP/Plugin)
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = Result<FunctionResult>> + Send + 'a>> {
        use futures::StreamExt;
        use proto::jobworkerp::data::{ResultStatus, result_output_item};
        use proto::jobworkerp::function::data::FunctionExecutionInfo;

        Box::pin(stream! {
                // Handle streaming case
            if let Some(mut result_stream) = stream_opt {
                while let Some(item) = result_stream.next().await {
                    match item.item {
                        Some(result_output_item::Item::Data(data)) => {
                            // Decode the data using runner information
                            let decoded_output = if let Some(ref rname) = runner_name {
                                self.decode_job_result_output(None, Some(rname), &data, using.as_deref()).await?
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
                        Some(result_output_item::Item::FinalCollected(_)) => {
                            // FinalCollected is for workflow internal use, treat as end
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
                                    metadata: std::collections::HashMap::new(),
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
                        match self.decode_job_result_output(None, Some(rname), &raw_output, using.as_deref()).await {
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

    fn transform_function_arguments(
        &self,
        rt: RunnerType,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Option<serde_json::Map<String, serde_json::Value>> {
        transform_function_arguments_impl(rt, arguments)
    }
    /// Find a single function by runner ID
    async fn find_function_by_runner_id(
        &self,
        runner_id: &proto::jobworkerp::data::RunnerId,
    ) -> Result<Option<FunctionSpecs>>
    where
        Self: Send + 'static,
    {
        match self.runner_app().find_runner(runner_id).await? {
            Some(runner) => Ok(Some(Self::convert_runner_to_function_specs(runner))),
            None => Ok(None),
        }
    }

    /// Find a single function by worker ID
    async fn find_function_by_worker_id(
        &self,
        worker_id: &proto::jobworkerp::data::WorkerId,
        using: Option<&str>,
    ) -> Result<Option<FunctionSpecs>>
    where
        Self: Send + 'static,
    {
        match self.worker_app().find(worker_id).await? {
            Some(proto::jobworkerp::data::Worker {
                id: Some(wid),
                data: Some(data),
            }) if data.runner_id.is_some() => {
                let runner_id = data.runner_id.unwrap();
                if let Some(runner) = self.runner_app().find_runner(&runner_id).await? {
                    // If using is specified, use convert_worker_using_to_function_specs
                    if let Some(using_str) = using {
                        let specs = Self::convert_worker_using_to_function_specs(
                            wid, data, runner, using_str,
                        )?;
                        Ok(Some(specs))
                    } else {
                        let specs = Self::convert_worker_to_function_specs(wid, data, runner)?;
                        Ok(Some(specs))
                    }
                } else {
                    Ok(None)
                }
            }
            _ => Ok(None),
        }
    }

    /// Find a single function by runner name
    async fn find_function_by_runner_name(&self, runner_name: &str) -> Result<Option<FunctionSpecs>>
    where
        Self: Send + 'static,
    {
        match self.runner_app().find_runner_by_name(runner_name).await? {
            Some(runner) => Ok(Some(Self::convert_runner_to_function_specs(runner))),
            None => Ok(None),
        }
    }

    /// Find a single function by worker name
    async fn find_function_by_worker_name(&self, worker_name: &str) -> Result<Option<FunctionSpecs>>
    where
        Self: Send + 'static,
    {
        match self.worker_app().find_by_name(worker_name).await? {
            Some(proto::jobworkerp::data::Worker {
                id: Some(wid),
                data: Some(data),
            }) if data.runner_id.is_some() => {
                let runner_id = data.runner_id.unwrap();
                if let Some(runner) = self.runner_app().find_runner(&runner_id).await? {
                    let specs = Self::convert_worker_to_function_specs(wid, data, runner)?;
                    Ok(Some(specs))
                } else {
                    Ok(None)
                }
            }
            _ => Ok(None),
        }
    }

    /// Find a single function by FunctionUsing
    ///
    /// When FunctionUsing contains using specified,
    /// returns FunctionSpecs for that specific using only.
    /// When using is None, returns the full Runner's FunctionSpecs.
    async fn find_function_by_using(
        &self,
        function_using: &proto::jobworkerp::function::data::FunctionUsing,
    ) -> Result<Option<FunctionSpecs>>
    where
        Self: Send + 'static,
    {
        use proto::jobworkerp::function::data::function_id;

        let function_id = match function_using.function_id.as_ref() {
            Some(id) => id,
            None => return Ok(None),
        };

        match &function_id.id {
            Some(function_id::Id::RunnerId(runner_id)) => {
                if let Some(using) = &function_using.using {
                    // Specific using requested - return single tool FunctionSpecs
                    self.find_function_by_runner_using(runner_id, using).await
                } else {
                    // No using - return full Runner FunctionSpecs
                    self.find_function_by_runner_id(runner_id).await
                }
            }
            Some(function_id::Id::WorkerId(worker_id)) => {
                // Pass using to find_function_by_worker_id
                if let Some(using) = &function_using.using {
                    self.find_function_by_worker_id(worker_id, Some(using.as_str()))
                        .await
                } else {
                    self.find_function_by_worker_id(worker_id, None).await
                }
            }
            None => {
                tracing::warn!("FunctionId has no id set. Returning None.");
                Ok(None)
            }
        }
    }

    /// Find a single function for a specific Runner using
    ///
    /// Returns FunctionSpecs with single tool schema for MCP/Plugin runners,
    /// or error if the runner doesn't support usings.
    async fn find_function_by_runner_using(
        &self,
        runner_id: &proto::jobworkerp::data::RunnerId,
        using: &str,
    ) -> Result<Option<FunctionSpecs>>
    where
        Self: Send + 'static,
    {
        match self.runner_app().find_runner(runner_id).await? {
            Some(runner) => {
                let specs = Self::convert_runner_using_to_function_specs(runner, using)?;
                Ok(Some(specs))
            }
            None => Ok(None),
        }
    }

    /// Convert multiple FunctionUsings to FunctionSpecs
    async fn convert_function_usings_to_specs(
        &self,
        function_usings: &[proto::jobworkerp::function::data::FunctionUsing],
        context_name: &str,
    ) -> Result<Vec<FunctionSpecs>>
    where
        Self: Send + 'static,
    {
        let mut functions = Vec::new();
        let mut skipped_count = 0u64;

        for function_using in function_usings {
            match self.find_function_by_using(function_using).await? {
                Some(specs) => functions.push(specs),
                None => {
                    skipped_count += 1;
                    if let Some(function_id) = &function_using.function_id {
                        if let Some(id) = &function_id.id {
                            use proto::jobworkerp::function::data::function_id;
                            match id {
                                function_id::Id::RunnerId(runner_id) => {
                                    tracing::warn!(
                                        "Runner not found for id: {} in context: {}. Skipping.",
                                        runner_id.value,
                                        context_name
                                    );
                                }
                                function_id::Id::WorkerId(worker_id) => {
                                    tracing::warn!(
                                        "Worker not found for id: {} in context: {}. Skipping.",
                                        worker_id.value,
                                        context_name
                                    );
                                }
                            }
                        } else {
                            tracing::warn!(
                                "FunctionId has no id set in context: {}. Skipping this target.",
                                context_name
                            );
                        }
                    } else {
                        tracing::warn!(
                            "FunctionUsing has no function_id in context: {}. Skipping this target.",
                            context_name
                        );
                    }
                }
            }
        }

        // Log metrics for skipped targets
        if skipped_count > 0 {
            tracing::info!(
                target: "metrics",
                skipped_targets = skipped_count,
                context = context_name,
                total_targets = function_usings.len(),
                "Skipped targets during FunctionUsing to FunctionSpecs conversion"
            );
        }

        Ok(functions)
    }

    /// Create a Worker from any Runner with detailed configuration
    async fn create_worker_from_runner(
        &self,
        runner_name: Option<String>,
        runner_id: Option<proto::jobworkerp::data::RunnerId>,
        name: String,
        description: Option<String>,
        settings_json: Option<String>,
        worker_options: Option<WorkerOptions>,
    ) -> Result<(WorkerId, String)> {
        // Find runner (name or id)
        let runner = self.find_runner(runner_name, runner_id).await?;

        let runner_settings_value = settings_json
            .map(|json| {
                serde_json::from_str::<serde_json::Value>(&json).map_err(|e| {
                    JobWorkerError::InvalidParameter(format!(
                        "Invalid JSON in settings_json: {}",
                        e
                    ))
                })
            })
            .transpose()?;

        let runner_settings_bytes = self
            .setup_runner_and_settings(&runner, runner_settings_value)
            .await?;

        // Build WorkerData from WorkerOptions
        let worker_data = self.build_worker_data_from_options(
            name.clone(),
            description.unwrap_or_default(),
            runner.id.unwrap(),
            runner_settings_bytes,
            worker_options,
        )?;

        self.validate_worker_options(&worker_data)?;

        let worker_id = self.worker_app().create(&worker_data).await?;

        Ok((worker_id, name))
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
        // self.handle_create_workflow(name, arguments, rid, rdata)

        match self.find_runner_by_name_with_mcp(name).await {
            Ok(Some((
                RunnerWithSchema {
                    id: Some(rid),
                    data: Some(rdata),
                    ..
                },
                tool_name_opt,
            ))) => {
                // match rdata.runner_type() {
                //    rt if rt == RunnerType::CreateWorkflow as i32 => {
                //        // CREATE_WORKFLOW-specific processing
                //        // (Arguments are raw workflow definitions, but REUSABLE_WORKFLOW arguments must be specified with json_data)
                //            .await
                //    }
                // }
                // Standard runner processing
                let arguments = self.transform_function_arguments(rdata.runner_type(), arguments);
                tracing::debug!("call_function_for_llm: {}: {arguments:#?}", rid.value);
                // Re-create runner object with schema for standard handling
                let runner = RunnerWithSchema {
                    id: Some(rid),
                    data: Some(rdata),
                    settings_schema: String::new(),
                    method_json_schema_map: Some(proto::jobworkerp::data::MethodJsonSchemaMap {
                        schemas: std::collections::HashMap::new(),
                    }),
                };
                self.handle_runner_call_from_llm(
                    meta,
                    arguments,
                    runner,
                    tool_name_opt,
                    None,
                    None,
                    timeout_sec,
                    false,
                )
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

    /// Common logic for enqueuing a runner-based function for LLM tool execution.
    #[allow(clippy::too_many_arguments)]
    async fn handle_enqueue_for_llm(
        &self,
        meta: Arc<HashMap<String, String>>,
        runner: RunnerWithSchema,
        tool_name_opt: Option<String>,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
        timeout_sec: u32,
    ) -> Result<EnqueuedFunction> {
        let rdata = runner
            .data
            .as_ref()
            .ok_or_else(|| JobWorkerError::NotFound("Runner data not found".to_string()))?;
        let supports_streaming = check_method_supports_streaming(rdata, tool_name_opt.as_deref());
        // Internal: results streamed via server-side channel, not exposed to client.
        // None: non-streaming runners return results in a single response.
        let streaming_type = if supports_streaming {
            StreamingType::Internal
        } else {
            StreamingType::None
        };
        let runner_name = rdata.name.clone();
        let arguments = self.transform_function_arguments(rdata.runner_type(), arguments);
        let (settings, args) =
            Self::prepare_runner_call_arguments(arguments.unwrap_or_default()).await?;
        let worker_data = self.create_worker_data(&runner, settings, None).await?;

        if supports_streaming {
            let rid_ref = runner
                .id
                .as_ref()
                .ok_or_else(|| JobWorkerError::NotFound("Runner id not found".to_string()))?;
            let rdata_ref = runner
                .data
                .as_ref()
                .ok_or_else(|| JobWorkerError::NotFound("Runner data not found".to_string()))?;
            let job_args = self
                .transform_job_args(rid_ref, rdata_ref, &args, tool_name_opt.as_deref())
                .await?;

            // Pre-generate job_id and start listening BEFORE enqueue to avoid
            // race condition where the job completes and publishes results via
            // pubsub before the listener subscribes.
            let job_id = proto::jobworkerp::data::JobId {
                value: self.id_generator().generate_id()?,
            };
            let job_result_app = self.job_result_app().clone();
            let listen_job_id = job_id;
            let listen_timeout = (timeout_sec as u64) * 1000;
            let result_handle = tokio::spawn(async move {
                job_result_app
                    .listen_result_by_job_id(&listen_job_id, Some(listen_timeout), true)
                    .await
            });

            // Use overrides to set NoResult response_type so enqueue returns immediately
            let streaming_overrides = streaming_no_wait_overrides();

            let using_for_result = tool_name_opt.clone();
            let enqueue_result = self
                .job_app()
                .enqueue_job_with_temp_worker(
                    meta,
                    worker_data,
                    job_args,
                    None,
                    0,
                    proto::jobworkerp::data::Priority::Medium as i32,
                    (timeout_sec as u64) * 1000,
                    Some(job_id),
                    streaming_type,
                    true,
                    tool_name_opt,
                    streaming_overrides,
                )
                .await;
            if let Err(e) = enqueue_result {
                // Abort the pre-started listener to avoid orphaned tasks
                result_handle.abort();
                return Err(e);
            }

            Ok(EnqueuedFunction {
                job_id,
                runner_name,
                result: None,
                is_streaming: true,
                result_handle: Some(result_handle),
                using: using_for_result,
            })
        } else {
            // Non-streaming: enqueue and wait for result (Direct response)
            let using_for_result = tool_name_opt.clone();
            let (job_id, job_result, _stream) = self
                .setup_worker_and_enqueue_with_json_full_output(
                    meta,
                    &runner_name,
                    worker_data,
                    args,
                    None,
                    timeout_sec,
                    streaming_type,
                    tool_name_opt,
                )
                .await?;

            let result = if let Some(jr) = job_result {
                let rid = runner
                    .id
                    .ok_or_else(|| JobWorkerError::NotFound("Runner id not found".to_string()))?;
                let rdata = runner
                    .data
                    .as_ref()
                    .ok_or_else(|| JobWorkerError::NotFound("Runner data not found".to_string()))?;
                match self.extract_job_result_output(jr) {
                    Ok(bytes) => Some(
                        self.transform_raw_output(&rid, rdata, &bytes, using_for_result.as_deref())
                            .await?,
                    ),
                    Err(e) => {
                        return Err(e);
                    }
                }
            } else {
                None
            };

            Ok(EnqueuedFunction {
                job_id,
                runner_name,
                result,
                is_streaming: false,
                result_handle: None,
                using: using_for_result,
            })
        }
    }

    /// Phase A: Enqueue a function for LLM tool execution and return immediately.
    /// For streaming-capable runners, enqueues with StreamingType::Internal (returns immediately).
    /// For non-streaming runners, enqueues with StreamingType::None (waits for completion).
    async fn enqueue_function_for_llm(
        &self,
        meta: Arc<HashMap<String, String>>,
        name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
        timeout_sec: u32,
    ) -> Result<EnqueuedFunction> {
        tracing::debug!("enqueue_function_for_llm: {}: {:?}", name, &arguments);

        match self.find_runner_by_name_with_mcp(name).await {
            Ok(Some((runner, tool_name_opt))) => {
                self.handle_enqueue_for_llm(meta, runner, tool_name_opt, arguments, timeout_sec)
                    .await
            }
            Ok(None) => {
                // Worker path: find worker and enqueue
                let (worker_data, tool_name_opt) = self
                    .find_worker_by_name_with_mcp(name)
                    .await?
                    .ok_or_else(|| {
                    JobWorkerError::WorkerNotFound(format!("worker or method not found: {name}"))
                })?;

                let (runner_name, runner_type_opt, supports_streaming, runner_for_args) =
                    if let Some(runner_id) = worker_data.runner_id.as_ref() {
                        if let Some(RunnerWithSchema {
                            id: Some(rid),
                            data: Some(rdata),
                            ..
                        }) = self.runner_app().find_runner(runner_id).await?
                        {
                            let streaming =
                                check_method_supports_streaming(&rdata, tool_name_opt.as_deref());
                            (
                                rdata.name.clone(),
                                Some(rdata.runner_type()),
                                streaming,
                                Some((rid, rdata)),
                            )
                        } else {
                            (name.to_string(), None, false, None)
                        }
                    } else {
                        (name.to_string(), None, false, None)
                    };

                // Transform arguments (e.g. wrap into 'input' for Workflow runners)
                let arguments = if let Some(rt) = runner_type_opt {
                    transform_function_arguments_impl(rt, arguments)
                } else {
                    arguments
                };
                let request_args = serde_json::Value::Object(arguments.unwrap_or_default());
                let (worker_name, tool_name_for_worker) =
                    if let Some((server_name, tool_name)) = Self::divide_names(name) {
                        (server_name, Some(tool_name))
                    } else {
                        (name.to_string(), tool_name_opt)
                    };
                let using_for_result = tool_name_for_worker.clone();

                if supports_streaming {
                    let (rid, rdata) = runner_for_args.ok_or_else(|| {
                        JobWorkerError::NotFound(
                            "Runner data required for streaming args serialization".to_string(),
                        )
                    })?;

                    // Streaming path: use overrides + pre-started listener
                    let streaming_overrides = streaming_no_wait_overrides();

                    let job_args = self
                        .transform_job_args(
                            &rid,
                            &rdata,
                            &request_args,
                            tool_name_for_worker.as_deref(),
                        )
                        .await?;

                    let job_id = proto::jobworkerp::data::JobId {
                        value: self.id_generator().generate_id()?,
                    };
                    let job_result_app = self.job_result_app().clone();
                    let listen_job_id = job_id;
                    let listen_timeout = (timeout_sec as u64) * 1000;
                    let result_handle = tokio::spawn(async move {
                        job_result_app
                            .listen_result_by_job_id(&listen_job_id, Some(listen_timeout), true)
                            .await
                    });

                    let enqueue_result = self
                        .job_app()
                        .enqueue_job(
                            meta,
                            None,
                            Some(&worker_name),
                            job_args,
                            None,
                            0,
                            proto::jobworkerp::data::Priority::Medium as i32,
                            (timeout_sec as u64) * 1000,
                            Some(job_id),
                            StreamingType::Internal,
                            tool_name_for_worker,
                            streaming_overrides,
                        )
                        .await;

                    if let Err(e) = enqueue_result {
                        result_handle.abort();
                        return Err(e);
                    }

                    Ok(EnqueuedFunction {
                        job_id,
                        runner_name,
                        result: None,
                        is_streaming: true,
                        result_handle: Some(result_handle),
                        using: using_for_result,
                    })
                } else {
                    // Non-streaming: synchronous wait via enqueue_with_worker_name
                    let (job_id, job_result, _stream) = self
                        .enqueue_with_worker_name(
                            meta,
                            &worker_name,
                            &request_args,
                            None,
                            timeout_sec,
                            StreamingType::None,
                            tool_name_for_worker,
                        )
                        .await?;

                    let result = if let Some(jr) = job_result {
                        match self.extract_job_result_output(jr) {
                            Ok(bytes) => {
                                match self
                                    .decode_job_result_output(
                                        None,
                                        Some(&runner_name),
                                        &bytes,
                                        using_for_result.as_deref(),
                                    )
                                    .await
                                {
                                    Ok(json) => Some(json),
                                    Err(_) => Some(serde_json::Value::String(
                                        String::from_utf8_lossy(&bytes).to_string(),
                                    )),
                                }
                            }
                            Err(e) => {
                                tracing::warn!("Failed to extract worker result: {:?}", e);
                                None
                            }
                        }
                    } else {
                        None
                    };

                    Ok(EnqueuedFunction {
                        job_id,
                        runner_name,
                        result,
                        is_streaming: false,
                        result_handle: None,
                        using: using_for_result,
                    })
                }
            }
            Err(e) => {
                tracing::error!("enqueue_function_for_llm error: {:#?}", &e);
                Err(e)
            }
        }
    }

    /// Phase B: Await the result of a previously enqueued streaming function.
    /// Uses the pre-started result_handle from EnqueuedFunction to receive
    /// the job result via listen_result_by_job_id (pubsub with DB fallback).
    /// Collects FinalCollected or last Data chunk from the stream.
    async fn await_function_result(
        &self,
        result_handle: tokio::task::JoinHandle<JobListenResult>,
        runner_name: &str,
        using: Option<&str>,
    ) -> Result<serde_json::Value> {
        use futures::StreamExt;
        use proto::jobworkerp::data::result_output_item;

        let (job_result, stream_opt) = result_handle
            .await
            .map_err(|e| anyhow::anyhow!("Result listener task failed: {}", e))??;

        // For streaming jobs: collect from stream (FinalCollected is authoritative)
        if let Some(mut stream) = stream_opt {
            let mut last_data: Option<Vec<u8>> = None;
            while let Some(item) = stream.next().await {
                match item.item {
                    Some(result_output_item::Item::FinalCollected(data)) => {
                        last_data = Some(data);
                        break;
                    }
                    Some(result_output_item::Item::Data(data)) => {
                        last_data = Some(data);
                    }
                    Some(result_output_item::Item::End(_)) => break,
                    None => {}
                }
            }
            if let Some(data) = last_data {
                return self
                    .decode_job_result_output(None, Some(runner_name), &data, using)
                    .await;
            }
        }

        // Fallback: use direct output from job result (non-streaming)
        let output = job_result
            .data
            .as_ref()
            .and_then(|r| r.output.as_ref().map(|o| &o.items));

        if let Some(output_bytes) = output {
            if !output_bytes.is_empty() {
                self.decode_job_result_output(None, Some(runner_name), output_bytes, using)
                    .await
            } else {
                Err(
                    JobWorkerError::RuntimeError("Job completed but output is empty".to_string())
                        .into(),
                )
            }
        } else {
            Err(
                JobWorkerError::RuntimeError("Job completed but no output found".to_string())
                    .into(),
            )
        }
    }

    /// Streaming version of call_function_for_llm.
    ///
    /// Returns a stream of FunctionResult items for progressive output.
    /// Used by MCP Server for streaming tool execution results.
    fn call_function_for_llm_streaming<'a>(
        &'a self,
        meta: Arc<HashMap<String, String>>,
        name: &'a str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
        timeout_sec: u32,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = Result<FunctionResult>> + Send + 'a>> {
        use futures::StreamExt;

        Box::pin(async_stream::stream! {
            tracing::debug!("call_function_for_llm_streaming: {}: {:?}", name, &arguments);

            match self.find_runner_by_name_with_mcp(name).await {
                Ok(Some((
                    RunnerWithSchema {
                        id: Some(rid),
                        data: Some(rdata),
                        ..
                    },
                    tool_name_opt,
                ))) => {
                    let supports_streaming = check_method_supports_streaming(&rdata, tool_name_opt.as_deref());

                    let arguments = self.transform_function_arguments(rdata.runner_type(), arguments);
                    let _runner = RunnerWithSchema {
                        id: Some(rid),
                        data: Some(rdata.clone()),
                        settings_schema: String::new(),
                        method_json_schema_map: None,
                    };
                    let (settings, args) = match Self::prepare_runner_call_arguments(
                        arguments.unwrap_or_default(),
                    ).await {
                        Ok((s, a)) => (s, a),
                        Err(e) => {
                            yield Err(e);
                            return;
                        }
                    };

                    let stream = self.handle_runner_for_front(
                        meta,
                        name,
                        settings,
                        None,
                        args,
                        None,
                        timeout_sec,
                        supports_streaming, // streaming enabled
                    );

                    let mut stream = std::pin::pin!(stream);
                    while let Some(result) = stream.next().await {
                        yield result;
                    }
                }
                Ok(Some((runner, tool_name_opt))) => {
                    if let Some(rdata) = &runner.data {
                        let supports_streaming = check_method_supports_streaming(rdata, tool_name_opt.as_deref());

                        let arguments = self.transform_function_arguments(rdata.runner_type(), arguments);
                        let (settings, args) = match Self::prepare_runner_call_arguments(
                            arguments.unwrap_or_default(),
                        ).await {
                            Ok((s, a)) => (s, a),
                            Err(e) => {
                                yield Err(e);
                                return;
                            }
                        };

                        let stream = self.handle_runner_for_front(
                            meta,
                            name,
                            settings,
                            None,
                            args,
                            None,
                            timeout_sec,
                            supports_streaming, // streaming enabled
                        );

                        let mut stream = std::pin::pin!(stream);
                        while let Some(result) = stream.next().await {
                            yield result;
                        }
                    } else {
                        yield Err(JobWorkerError::NotFound("Runner data not found".to_string()).into());
                    }
                }
                Ok(None) => {
                    // Worker-based call with streaming
                    let args = json!(arguments.unwrap_or_default());
                    let stream = self.handle_worker_call_for_front(
                        meta,
                        name,
                        args,
                        None,
                        timeout_sec,
                        // RunnerData is not available in the worker-based call path,
                        // so streaming is enabled by default. The actual streaming behavior
                        // is determined by the worker's job execution.
                        true,
                    );

                    let mut stream = std::pin::pin!(stream);
                    while let Some(result) = stream.next().await {
                        yield result;
                    }
                }
                Err(e) => {
                    tracing::error!("error: {:#?}", &e);
                    yield Err(e);
                }
            }
        })
    }
}

/// Create overrides for streaming no-wait pattern used by `enqueue_function_for_llm`.
///
/// This intentionally overrides the worker's `response_type` (typically `Direct`) to `NoResult`
/// so that `enqueue_job` / `enqueue_job_with_temp_worker` returns immediately without blocking.
/// The caller then listens for results via a pre-started `listen_result_by_job_id` handle.
///
/// `store_success: true` is required so that `complete_job` persists the `job_result` record,
/// which `listen_result_by_job_id` reads as a fallback when pubsub delivery is missed.
/// Note: `complete_job` → `cleanup_job` deletes the *job* record, but the `job_result` row
/// remains. Accumulated result rows should be cleaned up via the `delete_bulk` API.
///
/// `broadcast_results: true` enables pubsub notification so the pre-started listener can
/// receive the result in real-time without polling.
///
/// Note: These overrides propagate to retry/periodic re-enqueue via `build_retry_job()` /
/// `build_next_periodic_job()`, which snapshot resolved values. This is intentional — the
/// caller manages result delivery independently of the worker's default response_type.
fn streaming_no_wait_overrides() -> Option<proto::jobworkerp::data::JobExecutionOverrides> {
    Some(proto::jobworkerp::data::JobExecutionOverrides {
        response_type: Some(proto::jobworkerp::data::ResponseType::NoResult as i32),
        store_success: Some(true),
        store_failure: Some(true),
        broadcast_results: Some(true),
        retry_policy: None, // Falls back to worker's default retry_policy via resolve_job_params
    })
}

/// Check if the runner method supports streaming output.
/// Returns false only when the method is explicitly marked as NonStreaming.
/// Both, Streaming, and unknown/unset values default to true (streaming supported).
fn check_method_supports_streaming(rdata: &RunnerData, tool_name_opt: Option<&str>) -> bool {
    if let Some(map) = &rdata.method_proto_map {
        let method_name = tool_name_opt.unwrap_or(proto::DEFAULT_METHOD_NAME);
        if let Some(schema) = map.schemas.get(method_name) {
            return schema.output_type
                != proto::jobworkerp::data::StreamingOutputType::NonStreaming as i32;
        }
    }
    true
}

#[derive(Debug)]
pub struct FunctionAppImpl {
    runner_app: Arc<dyn crate::app::runner::RunnerApp>,
    worker_app: Arc<dyn crate::app::worker::WorkerApp>,
    job_app: Arc<dyn crate::app::job::JobApp>,
    job_result_app: Arc<dyn crate::app::job_result::JobResultApp>,
    id_generator: Arc<IdGeneratorWrapper>,
    function_cache: memory_utils::cache::moka::MokaCacheImpl<Arc<String>, Vec<FunctionSpecs>>,
    descriptor_cache: Arc<MokaCacheImpl<Arc<String>, RunnerDataWithDescriptor>>,
    runner_factory: Arc<jobworkerp_runner::runner::factory::RunnerSpecFactory>,
    job_queue_config: infra::infra::JobQueueConfig,
    worker_config: crate::app::WorkerConfig,
    workflow_loader: Arc<infra::workflow::WorkflowLoader>,
}

impl FunctionAppImpl {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        runner_app: Arc<dyn crate::app::runner::RunnerApp>,
        worker_app: Arc<dyn crate::app::worker::WorkerApp>,
        job_app: Arc<dyn crate::app::job::JobApp>,
        job_result_app: Arc<dyn crate::app::job_result::JobResultApp>,
        id_generator: Arc<IdGeneratorWrapper>,
        descriptor_cache: Arc<MokaCacheImpl<Arc<String>, RunnerDataWithDescriptor>>,
        runner_factory: Arc<jobworkerp_runner::runner::factory::RunnerSpecFactory>,
        mc_config: &memory_utils::cache::stretto::MemoryCacheConfig,
        job_queue_config: infra::infra::JobQueueConfig,
        worker_config: crate::app::WorkerConfig,
        workflow_loader: Arc<infra::workflow::WorkflowLoader>,
    ) -> Self {
        let function_cache = memory_utils::cache::moka::MokaCacheImpl::new(
            &memory_utils::cache::moka::MokaCacheConfig {
                num_counters: mc_config.num_counters,
                ttl: Some(std::time::Duration::from_secs(60)), // 60 seconds TTL
            },
        );

        Self {
            runner_app,
            worker_app,
            job_app,
            job_result_app,
            id_generator,
            function_cache,
            descriptor_cache,
            runner_factory,
            job_queue_config,
            worker_config,
            workflow_loader,
        }
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
impl UseIdGenerator for FunctionAppImpl {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        &self.id_generator
    }
}

impl UseMokaCache<Arc<String>, Vec<FunctionSpecs>> for FunctionAppImpl {
    fn cache(&self) -> &memory_utils::cache::moka::MokaCache<Arc<String>, Vec<FunctionSpecs>> {
        self.function_cache.cache()
    }
}
impl converter::FunctionSpecConverter for FunctionAppImpl {}
impl ProtobufHelper for FunctionAppImpl {}
impl McpNameConverter for FunctionAppImpl {}
impl UseRunnerParserWithCache for FunctionAppImpl {
    fn descriptor_cache(&self) -> &MokaCacheImpl<Arc<String>, RunnerDataWithDescriptor> {
        &self.descriptor_cache
    }
}
impl crate::app::job::execute::UseRunnerSpecFactory for FunctionAppImpl {
    fn runner_spec_factory(&self) -> &Arc<jobworkerp_runner::runner::factory::RunnerSpecFactory> {
        &self.runner_factory
    }
}
impl UseJobExecutor for FunctionAppImpl {}
impl infra::infra::UseJobQueueConfig for FunctionAppImpl {
    fn job_queue_config(&self) -> &infra::infra::JobQueueConfig {
        &self.job_queue_config
    }
}
impl crate::app::UseWorkerConfig for FunctionAppImpl {
    fn worker_config(&self) -> &crate::app::WorkerConfig {
        &self.worker_config
    }
}
impl FunctionCallHelper for FunctionAppImpl {
    fn timeout_sec(&self) -> u32 {
        30 * 60 // 30 minutes
    }

    fn job_queue_config(&self) -> &infra::infra::JobQueueConfig {
        infra::infra::UseJobQueueConfig::job_queue_config(self)
    }

    fn worker_config(&self) -> &crate::app::WorkerConfig {
        crate::app::UseWorkerConfig::worker_config(self)
    }
}

impl FunctionApp for FunctionAppImpl {}

impl infra::workflow::UseWorkflowLoader for FunctionAppImpl {
    fn workflow_loader(&self) -> &infra::workflow::WorkflowLoader {
        &self.workflow_loader
    }
}

pub trait UseFunctionApp {
    fn function_app(&self) -> &FunctionAppImpl;
}

/// Wrap flat JSON arguments into the 'input' field for Workflow/ReusableWorkflow runners.
/// Used by transform_job_args to ensure args match protobuf schema before serialization.
pub(crate) fn wrap_workflow_args_if_needed(
    rt: RunnerType,
    arg_json: serde_json::Value,
) -> serde_json::Value {
    if rt != RunnerType::Workflow && rt != RunnerType::ReusableWorkflow {
        return arg_json;
    }
    match &arg_json {
        serde_json::Value::Object(map) if !map.contains_key("input") => {
            let input_json = serde_json::to_string(&arg_json).unwrap_or_else(|_| "{}".to_string());
            serde_json::json!({"input": input_json})
        }
        serde_json::Value::String(s) => {
            serde_json::json!({"input": s.clone()})
        }
        _ => arg_json,
    }
}

pub(crate) fn transform_function_arguments_impl(
    rt: RunnerType,
    arguments: Option<serde_json::Map<String, serde_json::Value>>,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    // For CREATE_WORKFLOW runner, process arguments as workflow JSON
    // When called from LLM, only workflow JSON is expected as arguments for proper workflow creation
    // Settings are specified via worker options
    if rt == RunnerType::CreateWorkflow {
        arguments.map(|mut a| {
            let args = match a.remove("arguments") {
                Some(serde_json::Value::String(v)) => serde_json::from_str(v.as_str()).ok(),
                v => v,
            };
            let worker_opts = a.remove("settings");
            tracing::debug!("transforming arguments (CreateWorkflow): {:#?}", args);
            let n = args.as_ref().map(|v| {
                v.get("document")
                    .and_then(|v| v.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or_else(|| rt.as_str_name())
            });
            let s = json!({
                "name": n,
                "workflow_data": args.map(|v| v.to_string()).unwrap_or_default(),
                "worker_options": worker_opts,
            });
            let mut r = serde_json::Map::new();
            r.insert("arguments".to_string(), s);
            tracing::debug!("transformed arguments (CreateWorkflow): {:#?}", r);
            r
        })
    } else if rt == RunnerType::Workflow || rt == RunnerType::ReusableWorkflow {
        // Wrap LLM-generated flat arguments into the 'input' field expected by protobuf
        arguments.map(|mut args| {
            if args.contains_key("input") {
                return args;
            }
            // Support {settings:{...}, arguments:{...}} structured format
            if let Some(inner) = args.remove("arguments") {
                let mut result = serde_json::Map::new();
                if let Some(settings) = args.remove("settings") {
                    result.insert("settings".to_string(), settings);
                }
                let input_json = serde_json::to_string(&inner).unwrap_or_else(|_| "{}".to_string());
                result.insert("input".to_string(), serde_json::Value::String(input_json));
                result
            } else {
                let input_json = serde_json::to_string(&serde_json::Value::Object(args))
                    .unwrap_or_else(|_| "{}".to_string());
                let mut result = serde_json::Map::new();
                result.insert("input".to_string(), serde_json::Value::String(input_json));
                result
            }
        })
    } else {
        arguments
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_workflow_wraps_flat_args_into_input() {
        let args = serde_json::Map::from_iter([
            ("owner".to_string(), json!("foo")),
            ("repo".to_string(), json!("bar")),
        ]);
        let result = transform_function_arguments_impl(RunnerType::Workflow, Some(args)).unwrap();
        assert!(result.contains_key("input"));
        assert_eq!(result.len(), 1);
        let input_str = result["input"].as_str().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(input_str).unwrap();
        assert_eq!(parsed["owner"], "foo");
        assert_eq!(parsed["repo"], "bar");
    }

    #[test]
    fn test_reusable_workflow_wraps_flat_args_into_input() {
        let args = serde_json::Map::from_iter([("query".to_string(), json!("test"))]);
        let result =
            transform_function_arguments_impl(RunnerType::ReusableWorkflow, Some(args)).unwrap();
        assert!(result.contains_key("input"));
        let input_str = result["input"].as_str().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(input_str).unwrap();
        assert_eq!(parsed["query"], "test");
    }

    #[test]
    fn test_workflow_passthrough_when_input_key_exists() {
        let args =
            serde_json::Map::from_iter([("input".to_string(), json!("{\"owner\":\"foo\"}"))]);
        let result =
            transform_function_arguments_impl(RunnerType::Workflow, Some(args.clone())).unwrap();
        assert_eq!(result, args);
    }

    #[test]
    fn test_workflow_empty_args() {
        let args = serde_json::Map::new();
        let result = transform_function_arguments_impl(RunnerType::Workflow, Some(args)).unwrap();
        assert_eq!(result["input"].as_str().unwrap(), "{}");
    }

    #[test]
    fn test_workflow_none_args() {
        let result = transform_function_arguments_impl(RunnerType::Workflow, None);
        assert!(result.is_none());
    }

    #[test]
    fn test_other_runner_type_passthrough() {
        let args = serde_json::Map::from_iter([("key".to_string(), json!("value"))]);
        let result =
            transform_function_arguments_impl(RunnerType::Command, Some(args.clone())).unwrap();
        assert_eq!(result, args);
    }

    // Tests for structured {settings, arguments} format with Workflow
    #[test]
    fn test_workflow_with_structured_format() {
        let args = serde_json::Map::from_iter([
            ("settings".to_string(), json!({"timeout": 30})),
            (
                "arguments".to_string(),
                json!({"owner": "foo", "repo": "bar"}),
            ),
        ]);
        let result = transform_function_arguments_impl(RunnerType::Workflow, Some(args)).unwrap();
        // settings should be preserved
        assert!(result.contains_key("settings"));
        assert_eq!(result["settings"], json!({"timeout": 30}));
        // arguments should be wrapped into input
        assert!(result.contains_key("input"));
        let input_str = result["input"].as_str().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(input_str).unwrap();
        assert_eq!(parsed["owner"], "foo");
        assert_eq!(parsed["repo"], "bar");
        // No stale 'arguments' key
        assert!(!result.contains_key("arguments"));
    }

    #[test]
    fn test_workflow_with_structured_format_no_settings() {
        let args = serde_json::Map::from_iter([("arguments".to_string(), json!({"owner": "foo"}))]);
        let result = transform_function_arguments_impl(RunnerType::Workflow, Some(args)).unwrap();
        assert!(result.contains_key("input"));
        assert!(!result.contains_key("settings"));
        let input_str = result["input"].as_str().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(input_str).unwrap();
        assert_eq!(parsed["owner"], "foo");
    }

    // Tests for wrap_workflow_args_if_needed (gRPC direct path)
    #[test]
    fn test_wrap_workflow_args_flat() {
        let arg = json!({"owner": "foo", "repo": "bar"});
        let result = wrap_workflow_args_if_needed(RunnerType::Workflow, arg);
        assert!(result.is_object());
        let input_str = result["input"].as_str().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(input_str).unwrap();
        assert_eq!(parsed["owner"], "foo");
        assert_eq!(parsed["repo"], "bar");
    }

    #[test]
    fn test_wrap_workflow_args_already_has_input() {
        let arg = json!({"input": "{\"owner\":\"foo\"}"});
        let result = wrap_workflow_args_if_needed(RunnerType::Workflow, arg.clone());
        assert_eq!(result, arg);
    }

    #[test]
    fn test_wrap_workflow_args_non_workflow_passthrough() {
        let arg = json!({"key": "value"});
        let result = wrap_workflow_args_if_needed(RunnerType::Command, arg.clone());
        assert_eq!(result, arg);
    }

    #[test]
    fn test_wrap_workflow_args_empty_object() {
        let arg = json!({});
        let result = wrap_workflow_args_if_needed(RunnerType::ReusableWorkflow, arg);
        assert_eq!(result["input"].as_str().unwrap(), "{}");
    }

    #[test]
    fn test_wrap_workflow_args_string_value() {
        let arg = json!("hello world");
        let result = wrap_workflow_args_if_needed(RunnerType::Workflow, arg);
        assert_eq!(result["input"].as_str().unwrap(), "hello world");
    }

    #[test]
    fn test_wrap_workflow_args_string_non_workflow_passthrough() {
        let arg = json!("hello world");
        let result = wrap_workflow_args_if_needed(RunnerType::Command, arg.clone());
        assert_eq!(result, arg);
    }

    #[test]
    fn test_workflow_arguments_string_value() {
        // LLM may return arguments as a JSON string instead of an object.
        // The string is serialized as-is into the 'input' field (JSON-encoded string).
        let args = serde_json::Map::from_iter([(
            "arguments".to_string(),
            json!("{\"owner\":\"foo\",\"repo\":\"bar\"}"),
        )]);
        let result = transform_function_arguments_impl(RunnerType::Workflow, Some(args)).unwrap();
        assert!(result.contains_key("input"));
        let input_str = result["input"].as_str().unwrap();
        // The input contains the JSON string value (which itself is a JSON string)
        let parsed: serde_json::Value = serde_json::from_str(input_str).unwrap();
        // The parsed value is a string containing the original JSON
        assert!(parsed.is_string());
        let inner: serde_json::Value = serde_json::from_str(parsed.as_str().unwrap()).unwrap();
        assert_eq!(inner["owner"], "foo");
        assert_eq!(inner["repo"], "bar");
    }

    /// Regression test: Worker path must use transform_function_arguments_impl,
    /// not just wrap_workflow_args_if_needed.
    ///
    /// When LLM generates {"arguments": {"owner": "...", "repo": "...", "pull_number": 42}},
    /// transform_function_arguments_impl extracts the inner object from "arguments" key
    /// and wraps it into "input". Without this, wrap_workflow_args_if_needed would wrap
    /// the entire {"arguments": {...}} as-is, causing workflow input validation to fail
    /// because "owner" is not at the top level.
    #[test]
    fn test_workflow_arguments_key_must_be_unwrapped_by_transform() {
        let args = serde_json::Map::from_iter([(
            "arguments".to_string(),
            json!({"owner": "jobworkerp-rs", "repo": "jobworkerp-rs", "pull_number": 187}),
        )]);

        // transform_function_arguments_impl correctly unwraps "arguments" → "input"
        let transformed =
            transform_function_arguments_impl(RunnerType::ReusableWorkflow, Some(args.clone()))
                .unwrap();
        assert!(transformed.contains_key("input"));
        assert!(!transformed.contains_key("arguments"));
        let input_str = transformed["input"].as_str().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(input_str).unwrap();
        assert_eq!(parsed["owner"], "jobworkerp-rs");
        assert_eq!(parsed["pull_number"], 187);

        // wrap_workflow_args_if_needed does NOT unwrap "arguments" — it wraps everything
        let wrapped = wrap_workflow_args_if_needed(
            RunnerType::ReusableWorkflow,
            serde_json::Value::Object(args),
        );
        let wrapped_input_str = wrapped["input"].as_str().unwrap();
        let wrapped_parsed: serde_json::Value = serde_json::from_str(wrapped_input_str).unwrap();
        // The "arguments" key is still present (this was the bug)
        assert!(
            wrapped_parsed.get("arguments").is_some(),
            "wrap_workflow_args_if_needed should NOT unwrap 'arguments' key — \
             that's why transform_function_arguments_impl must be called in worker path"
        );
    }
}
