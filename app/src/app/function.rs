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
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::jobworkerp::runner::ReusableWorkflowRunnerSettings;
use memory_utils::cache::moka::{MokaCacheImpl, UseMokaCache};
use proto::jobworkerp::data::{RunnerData, RunnerType, StreamingOutputType, WorkerData, WorkerId};
use proto::jobworkerp::function::data::{
    function_specs, FunctionResult, FunctionSchema, FunctionSpecs, McpToolList, WorkerOptions,
};
use proto::ProtobufHelper;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

pub mod function_set;
pub mod helper;

#[async_trait]
pub trait FunctionApp:
    UseWorkerApp
    + UseRunnerApp
    + UseMokaCache<Arc<String>, Vec<FunctionSpecs>>
    + FunctionSpecConverter
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

        // Get runners if not excluded
        if !exclude_runner {
            let runners = self
                .runner_app()
                .find_runner_list(include_full, None, None)
                .await?;
            for runner in runners {
                functions.push(Self::convert_runner_to_function_specs(runner));
            }
        }

        // Get workers if not excluded
        if !exclude_worker {
            let workers = self
                .worker_app()
                .find_list(vec![], None, None, None, None, None, vec![], None, None)
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
                        None, // using not used via front API (only for LLM tool calling)
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
                            None, // using not used via front API
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

            // Extract MCP tool name from combined name if present (e.g., "server___tool")
            let (worker_name, tool_name_opt) = if let Some((server_name, tool_name)) = Self::divide_names(name) {
                (server_name, Some(tool_name))
            } else {
                (name.to_string(), None)
            };

            let result = self
                .enqueue_with_worker_name(
                    meta.clone(),
                    &worker_name,
                    &arguments,
                    unique_key,
                    job_timeout_sec,
                    streaming,
                    tool_name_opt.clone(), // Pass using parameter for MCP worker tools
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

                    // Use the common stream processing method
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
        using: Option<String>, // Phase 6.6.7: Add using for method-specific decoding
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
        } else {
            arguments
        }
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

        // Return None if function_id is not set (will be skipped by caller)
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

        // Validate and serialize settings_json (use existing setup_runner_and_settings with descriptor cache)
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

        // Validate WorkerData (periodic/direct/channel etc.)
        self.validate_worker_options(&worker_data)?;

        // Create Worker via WorkerApp
        let worker_id = self.worker_app().create(&worker_data).await?;

        Ok((worker_id, name))
    }

    /// Create a REUSABLE_WORKFLOW Worker from workflow definition
    async fn create_workflow_from_definition(
        &self,
        workflow_data: Option<String>,
        workflow_url: Option<String>,
        name: Option<String>,
        worker_options: Option<WorkerOptions>,
    ) -> Result<(WorkerId, String, Option<String>)> {
        // Load workflow definition (data or URL)
        // Use WorkflowLoader via DI (UseWorkflowLoader trait)
        let workflow_schema = if let Some(data) = workflow_data {
            self.workflow_loader()
                .load_workflow(None, Some(&data), true)
                .await?
        } else if let Some(url) = workflow_url {
            self.workflow_loader()
                .load_workflow(Some(&url), None, true)
                .await?
        } else {
            return Err(JobWorkerError::InvalidParameter(
                "Either workflow_data or workflow_url is required".to_string(),
            )
            .into());
        };

        // Get workflow name (document.name)
        let workflow_name = workflow_schema.document.name.to_string();

        // Determine worker name (from name parameter or workflow definition)
        let worker_name = name.unwrap_or_else(|| workflow_name.clone());

        // Create ReusableWorkflowRunnerSettings
        // Serialize workflow_schema to JSON string
        let workflow_json_str = serde_json::to_string(&workflow_schema).map_err(|e| {
            JobWorkerError::InvalidParameter(format!("Failed to serialize workflow schema: {}", e))
        })?;

        let runner_settings = ReusableWorkflowRunnerSettings {
            json_data: workflow_json_str,
        };
        let runner_settings_bytes = ProstMessageCodec::serialize_message(&runner_settings)?;

        // Get REUSABLE_WORKFLOW Runner ID (XXX same as db schema now)
        let runner_id = proto::jobworkerp::data::RunnerId {
            value: proto::jobworkerp::data::RunnerType::ReusableWorkflow as i64,
        };

        // Build WorkerData
        let description = workflow_schema.document.summary.unwrap_or_default();

        let worker_data = self.build_worker_data_from_options(
            worker_name.clone(),
            description,
            runner_id,
            runner_settings_bytes,
            worker_options,
        )?;

        // Validate WorkerData (periodic/direct/channel etc.)
        self.validate_worker_options(&worker_data)?;

        // Create Worker via WorkerApp
        let worker_id = self.worker_app().create(&worker_data).await?;

        Ok((worker_id, worker_name, Some(workflow_name)))
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
                // Re-create runner object for standard handling
                let runner = RunnerWithSchema {
                    id: Some(rid),
                    data: Some(rdata),
                    settings_schema: String::new(),
                    arguments_schema: String::new(),
                    output_schema: None,
                    tools: Vec::new(),
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
}

#[derive(Debug)]
pub struct FunctionAppImpl {
    runner_app: Arc<dyn crate::app::runner::RunnerApp>,
    worker_app: Arc<dyn crate::app::worker::WorkerApp>,
    job_app: Arc<dyn crate::app::job::JobApp>,
    job_result_app: Arc<dyn crate::app::job_result::JobResultApp>,
    function_cache: memory_utils::cache::moka::MokaCacheImpl<Arc<String>, Vec<FunctionSpecs>>,
    descriptor_cache: Arc<MokaCacheImpl<Arc<String>, RunnerDataWithDescriptor>>,
    job_queue_config: infra::infra::JobQueueConfig,
    worker_config: crate::app::WorkerConfig,
    workflow_loader: Arc<infra::workflow::WorkflowLoader>, // Workflow definition loader (DI pattern)
}

impl FunctionAppImpl {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        runner_app: Arc<dyn crate::app::runner::RunnerApp>,
        worker_app: Arc<dyn crate::app::worker::WorkerApp>,
        job_app: Arc<dyn crate::app::job::JobApp>,
        job_result_app: Arc<dyn crate::app::job_result::JobResultApp>,
        descriptor_cache: Arc<MokaCacheImpl<Arc<String>, RunnerDataWithDescriptor>>,
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
            function_cache,
            descriptor_cache,
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

impl UseMokaCache<Arc<String>, Vec<FunctionSpecs>> for FunctionAppImpl {
    fn cache(&self) -> &memory_utils::cache::moka::MokaCache<Arc<String>, Vec<FunctionSpecs>> {
        self.function_cache.cache()
    }
}
impl FunctionSpecConverter for FunctionAppImpl {}
impl ProtobufHelper for FunctionAppImpl {}
impl McpNameConverter for FunctionAppImpl {}
impl UseRunnerParserWithCache for FunctionAppImpl {
    fn descriptor_cache(&self) -> &MokaCacheImpl<Arc<String>, RunnerDataWithDescriptor> {
        &self.descriptor_cache
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

pub trait FunctionSpecConverter {
    // Helper function to get output_type from RunnerData
    // Phase 6.6: Prefer method_proto_map over deprecated output_type field
    #[allow(deprecated)]
    fn get_runner_output_type(runner_data: &RunnerData) -> i32 {
        // Phase 6.6.4: Get output_type from method_proto_map (required for all runners)
        if let Some(ref method_proto_map) = runner_data.method_proto_map {
            if let Some(schema) = method_proto_map.schemas.values().next() {
                return schema.output_type;
            }
        }
        // If no schemas found, default to NON_STREAMING
        StreamingOutputType::NonStreaming as i32
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
                schema: Some(function_specs::Schema::McpTools(McpToolList {
                    list: runner.tools,
                })),
                output_type: runner
                    .data
                    .as_ref()
                    .map(|data| Self::get_runner_output_type(data))
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
                    .map(|data| Self::get_runner_output_type(data))
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
                    .map(|data| Self::get_runner_output_type(data))
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
                    .map(|data| Self::get_runner_output_type(data))
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
                    .map(|data| Self::get_runner_output_type(data))
                    .unwrap_or(StreamingOutputType::NonStreaming as i32),
            })
        }
    }

    /// Convert Runner + specific using to FunctionSpecs
    ///
    /// Returns a FunctionSpecs with a single tool (the specified using).
    /// For MCP runners, extracts the specific tool from the McpToolList.
    /// Returns error if the runner doesn't support usings or the using doesn't exist.
    fn convert_runner_using_to_function_specs(
        runner: RunnerWithSchema,
        using: &str,
    ) -> Result<FunctionSpecs> {
        let runner_data = runner
            .data
            .as_ref()
            .ok_or_else(|| JobWorkerError::NotFound("Runner data not found".to_string()))?;

        // Check if this is an MCP server runner with tools
        if runner_data.runner_type == RunnerType::McpServer as i32 {
            // Find the specific tool in the McpToolList
            let tool = runner
                .tools
                .iter()
                .find(|t| t.name == using)
                .ok_or_else(|| {
                    JobWorkerError::NotFound(format!(
                        "using '{}' not found in MCP server '{}'. Available: {:?}",
                        using,
                        runner_data.name,
                        runner.tools.iter().map(|t| &t.name).collect::<Vec<_>>()
                    ))
                })?;

            // Return FunctionSpecs with single tool
            Ok(FunctionSpecs {
                runner_type: RunnerType::McpServer as i32,
                runner_id: runner.id,
                worker_id: None,
                // Use combined name for LLM tool calling: "server_name___tool_name"
                name: format!("{}___{}", runner_data.name, using),
                description: tool
                    .description
                    .clone()
                    .unwrap_or_else(|| format!("{} - {}", runner_data.description, using)),
                schema: Some(function_specs::Schema::SingleSchema(FunctionSchema {
                    settings: None,
                    arguments: tool.input_schema.clone(),
                    result_output_schema: runner.output_schema.clone(),
                })),
                output_type: Self::get_runner_output_type(runner_data),
            })
        } else if runner_data.method_proto_map.is_some() {
            // Plugin or other runner with method_proto_map
            let proto_map = runner_data.method_proto_map.as_ref().unwrap();
            let method_schema = proto_map.schemas.get(using).ok_or_else(|| {
                JobWorkerError::NotFound(format!(
                    "using '{}' not found in runner '{}'. Available: {:?}",
                    using,
                    runner_data.name,
                    proto_map.schemas.keys().collect::<Vec<_>>()
                ))
            })?;

            Ok(FunctionSpecs {
                runner_type: runner_data.runner_type,
                runner_id: runner.id,
                worker_id: None,
                name: format!("{}___{}", runner_data.name, using),
                description: method_schema
                    .description
                    .clone()
                    .unwrap_or_else(|| format!("{} - {}", runner_data.description, using)),
                schema: Some(function_specs::Schema::SingleSchema(FunctionSchema {
                    settings: None,
                    arguments: method_schema.args_proto.clone(),
                    // Rev.8.1: result_proto is now required (not optional)
                    result_output_schema: Some(method_schema.result_proto.clone()),
                })),
                output_type: Self::get_runner_output_type(runner_data),
            })
        } else {
            // Runner doesn't support usings
            Err(JobWorkerError::InvalidParameter(format!(
                "Runner '{}' does not support usings",
                runner_data.name
            ))
            .into())
        }
    }

    /// Convert Worker + specific using to FunctionSpecs
    ///
    /// Returns a FunctionSpecs with a single tool (the specified using).
    /// For Workers backed by MCP/Plugin runners, extracts the specific tool.
    /// For normal runners, returns error with warning that using is ignored.
    fn convert_worker_using_to_function_specs(
        worker_id: WorkerId,
        worker_data: WorkerData,
        runner: RunnerWithSchema,
        using: &str,
    ) -> Result<FunctionSpecs> {
        let runner_data = runner
            .data
            .as_ref()
            .ok_or_else(|| JobWorkerError::NotFound("Runner data not found".to_string()))?;

        // Check if this is an MCP server runner with tools
        if runner_data.runner_type == RunnerType::McpServer as i32 {
            // Find the specific tool in the McpToolList
            let tool = runner
                .tools
                .iter()
                .find(|t| t.name == using)
                .ok_or_else(|| {
                    JobWorkerError::NotFound(format!(
                        "using '{}' not found in Worker '{}' (MCP server '{}'). Available: {:?}",
                        using,
                        worker_data.name,
                        runner_data.name,
                        runner.tools.iter().map(|t| &t.name).collect::<Vec<_>>()
                    ))
                })?;

            // Return FunctionSpecs with single tool
            Ok(FunctionSpecs {
                runner_type: RunnerType::McpServer as i32,
                runner_id: runner.id,
                worker_id: Some(worker_id),
                // Use worker name + tool name for clarity
                name: format!("{}___{}", worker_data.name, using),
                description: tool
                    .description
                    .clone()
                    .unwrap_or_else(|| format!("{} - {}", worker_data.description, using)),
                schema: Some(function_specs::Schema::SingleSchema(FunctionSchema {
                    settings: None, // Workers don't have config (already set)
                    arguments: tool.input_schema.clone(),
                    result_output_schema: runner.output_schema.clone(),
                })),
                output_type: Self::get_runner_output_type(runner_data),
            })
        } else if runner_data.method_proto_map.is_some() {
            // Plugin or other runner with method_proto_map
            let proto_map = runner_data.method_proto_map.as_ref().unwrap();
            let method_schema = proto_map.schemas.get(using).ok_or_else(|| {
                JobWorkerError::NotFound(format!(
                    "using '{}' not found in Worker '{}' (runner '{}'). Available: {:?}",
                    using,
                    worker_data.name,
                    runner_data.name,
                    proto_map.schemas.keys().collect::<Vec<_>>()
                ))
            })?;

            Ok(FunctionSpecs {
                runner_type: runner_data.runner_type,
                runner_id: runner.id,
                worker_id: Some(worker_id),
                name: format!("{}___{}", worker_data.name, using),
                description: method_schema
                    .description
                    .clone()
                    .unwrap_or_else(|| format!("{} - {}", worker_data.description, using)),
                schema: Some(function_specs::Schema::SingleSchema(FunctionSchema {
                    settings: None, // Workers don't have config (already set)
                    arguments: method_schema.args_proto.clone(),
                    // Rev.8.1: result_proto is now required (not optional)
                    result_output_schema: Some(method_schema.result_proto.clone()),
                })),
                output_type: Self::get_runner_output_type(runner_data),
            })
        } else {
            // Worker→Runner doesn't support usings - log warning and return error
            tracing::warn!(
                "Worker '{}' (ID={}) does not support using. Worker is backed by runner '{}' which has no MCP tools or method_proto_map. Ignoring using='{}'",
                worker_data.name,
                worker_id.value,
                runner_data.name,
                using
            );
            Err(JobWorkerError::InvalidParameter(format!(
                "Worker '{}' does not support usings (backed by runner '{}' with no tools/methods)",
                worker_data.name, runner_data.name
            ))
            .into())
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
