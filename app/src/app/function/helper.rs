use crate::app::{job::execute::UseJobExecutor, WorkerConfig};
use anyhow::Result;
use command_utils::{protobuf::ProtobufDescriptor, util::datetime};
use infra::infra::runner::rows::RunnerWithSchema;
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{
    JobResult, Priority, QueueType, ResponseType, RetryPolicy, RetryType, RunnerId, RunnerType,
    WorkerData,
};
use proto::DEFAULT_METHOD_NAME;
use serde_json::{Map, Value};
use std::{
    collections::HashMap,
    future::Future,
    hash::{DefaultHasher, Hasher},
    sync::Arc,
};
use tracing;

pub trait McpNameConverter {
    const DELIMITER: &str = "___";
    fn combine_names(server_name: &str, tool_name: &str) -> String {
        format!("{}{}{}", server_name, Self::DELIMITER, tool_name)
    }

    fn divide_names(combined: &str) -> Option<(String, String)> {
        let delimiter = Self::DELIMITER;
        let mut v: std::collections::VecDeque<&str> = combined.split(delimiter).collect();
        match v.len().cmp(&2) {
            std::cmp::Ordering::Less => {
                tracing::debug!("Failed to parse combined name: {:#?}", &combined);
                None
            }
            std::cmp::Ordering::Equal => Some((v[0].to_string(), v[1].to_string())),
            std::cmp::Ordering::Greater => {
                let server_name = v.pop_front();
                Some((
                    server_name.unwrap_or_default().to_string(),
                    v.into_iter()
                        .map(|s| s.to_string())
                        .reduce(|acc, n| format!("{acc}{delimiter}{n}"))
                        .unwrap_or_default(),
                ))
            }
        }
    }
}

pub trait FunctionCallHelper: UseJobExecutor + McpNameConverter + Send + Sync {
    const DEFAULT_RETRY_POLICY: RetryPolicy = RetryPolicy {
        r#type: RetryType::Exponential as i32,
        interval: 1000,
        max_interval: 60000,
        max_retry: 1,
        basis: 1.3,
    };

    fn timeout_sec(&self) -> u32;
    fn job_queue_config(&self) -> &infra::infra::JobQueueConfig;
    fn worker_config(&self) -> &WorkerConfig;

    fn find_runner_by_name_with_mcp<'a>(
        &'a self,
        name: &'a str,
    ) -> impl Future<Output = Result<Option<(RunnerWithSchema, Option<String>)>>> + Send + 'a {
        async move {
            match self.runner_app().find_runner_by_name(name).await {
                Ok(Some(runner)) => {
                    tracing::debug!("found runner: {:?}", &runner);
                    Ok(Some((runner, None)))
                }
                Ok(None) => match Self::divide_names(name) {
                    Some((server_name, tool_name)) => {
                        tracing::debug!(
                            "found calling to mcp server: {}:{}",
                            &server_name,
                            &tool_name
                        );
                        self.runner_app()
                            .find_runner_by_name(&server_name)
                            .await
                            .map(|res| res.map(|r| (r, Some(tool_name))))
                    }
                    None => Ok(None),
                },
                Err(e) => Err(e),
            }
        }
    }

    fn find_worker_by_name_with_mcp<'a>(
        &'a self,
        name: &'a str,
    ) -> impl Future<Output = Result<Option<(WorkerData, Option<String>)>>> + Send + 'a {
        async move {
            match self.worker_app().find_by_name(name).await {
                Ok(Some(worker)) => {
                    tracing::debug!("found worker: {:?}", &worker);
                    Ok(worker.data.map(|w| (w, None)))
                }
                Ok(None) => match Self::divide_names(name) {
                    Some((server_name, tool_name)) => {
                        tracing::debug!(
                            "found calling to mcp server: {}:{}",
                            &server_name,
                            &tool_name
                        );
                        self.worker_app()
                            .find_by_name(&server_name)
                            .await
                            .map(|res| res.and_then(|r| r.data.map(|d| (d, Some(tool_name)))))
                    }
                    None => Ok(None),
                },
                Err(e) => Err(e),
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_runner_call_from_llm(
        &self,
        meta: Arc<HashMap<String, String>>,
        arguments: Option<Map<String, Value>>,
        runner: RunnerWithSchema,
        tool_name_opt: Option<String>,
        worker_params: Option<serde_json::Value>,
        unique_key: Option<String>,
        timeout_sec: u32,
        streaming: bool, // TODO if true, use streaming job
    ) -> impl Future<Output = Result<Value>> + Send + '_ {
        async move {
            tracing::debug!("found runner: {:?}, tool: {:?}", &runner, &tool_name_opt);

            // Phase 6.7: Validate MCP tool name if present using method_json_schema_map
            if let Some(ref tool_name) = tool_name_opt {
                if runner
                    .data
                    .as_ref()
                    .is_some_and(|d| d.runner_type() == RunnerType::McpServer)
                {
                    // Check if tool exists in the MCP server's method_json_schema_map
                    let tool_exists = runner
                        .method_json_schema_map
                        .as_ref()
                        .map(|m| m.schemas.contains_key(tool_name))
                        .unwrap_or(false);

                    if !tool_exists {
                        let available_tools: Vec<_> = runner
                            .method_json_schema_map
                            .as_ref()
                            .map(|m| m.schemas.keys().collect())
                            .unwrap_or_default();

                        return Err(JobWorkerError::InvalidParameter(format!(
                            "Tool '{}' not found in MCP server '{}'. Available tools: {:?}",
                            tool_name,
                            runner
                                .data
                                .as_ref()
                                .map(|d| &d.name)
                                .unwrap_or(&"unknown".to_string()),
                            available_tools
                        ))
                        .into());
                    }
                }
            }

            let (settings, arguments) = Self::prepare_runner_call_arguments(
                arguments.unwrap_or_default(),
                &runner,
                tool_name_opt.clone(), // Clone because we use it again below
            )
            .await?;
            if let RunnerWithSchema {
                id: Some(_id),
                data: Some(runner_data),
                ..
            } = &runner
            {
                let worker_data = self
                    .create_worker_data(&runner, settings, worker_params)
                    .await?;

                self.setup_worker_and_enqueue_with_json(
                    meta,
                    runner_data.name.as_str(),
                    worker_data,
                    arguments,
                    unique_key,
                    timeout_sec,
                    streaming,
                    tool_name_opt, // Pass using parameter for MCP/Plugin runners
                )
                .await
            } else {
                tracing::error!("Runner not found");
                Err(JobWorkerError::NotFound("Runner not found".to_string()).into())
            }
        }
    }

    // for LLM_CHAT, mcp proxy
    fn handle_worker_call_for_llm<'a>(
        &'a self,
        meta: Arc<HashMap<String, String>>,
        name: &'a str,
        arguments: Option<Map<String, Value>>,
        unique_key: Option<String>,
        streaming: bool, // TODO if true, use streaming job
    ) -> impl Future<Output = Result<Value>> + Send + 'a {
        async move {
            tracing::info!("runner not found, run as worker: {:?}", &name);
            let request_args = arguments.unwrap_or_default();
            let (worker_data, tool_name_opt) = self
                .find_worker_by_name_with_mcp(name)
                .await
                .inspect_err(|e| {
                    tracing::error!("Failed to find worker: {}", e);
                })?
                .ok_or_else(|| {
                    JobWorkerError::WorkerNotFound(format!("worker or mcp tool not found: {name}"))
                })?;
            // Phase 6.8: Arguments are now passed directly without MCP-specific correction
            // tool_name is passed via 'using' parameter to enqueue_temp_worker_with_json
            self.enqueue_temp_worker_with_json(
                meta,
                &worker_data,
                Value::Object(request_args),
                unique_key,
                streaming,
                tool_name_opt, // Pass using parameter for MCP worker tools
            )
            .await
            .map(|r| r.unwrap_or_default())
        }
    }
    fn create_worker_data(
        &self,
        runner: &RunnerWithSchema,
        runner_settings: Option<serde_json::Value>,
        worker_params: Option<serde_json::Value>,
    ) -> impl Future<Output = Result<WorkerData>> + Send {
        async move {
            let settings = self
                .setup_runner_and_settings(runner, runner_settings)
                .await?;

            if let RunnerWithSchema {
                id: Some(sid),
                data: Some(sdata),
                ..
            } = runner
            {
                let worker_data =
                    Self::create_default_worker_data(*sid, &sdata.name, settings, worker_params);
                Ok(worker_data)
            } else {
                Err(JobWorkerError::InvalidParameter(format!("illegal runner: {runner:#?}")).into())
            }
        }
    }
    fn enqueue_temp_worker_with_json<'a>(
        &'a self,
        meta: Arc<HashMap<String, String>>,
        temp_worker_data: &'a WorkerData,
        arguments: Value,
        uniq_key: Option<String>,
        _streaming: bool,      // TODO if true, use streaming job
        using: Option<String>, // Pass using parameter for MCP/Plugin workers
    ) -> impl Future<Output = Result<Option<Value>>> + Send + 'a {
        async move {
            if let Some(runner_id) = temp_worker_data.runner_id.as_ref() {
                let runner = self.runner_app().find_runner(runner_id).await?;
                if let Some(RunnerWithSchema {
                    id: Some(rid),
                    data: Some(rdata),
                    ..
                }) = runner
                {
                    // Phase 6.6.7: Parse runner proto schemas with cache and use method-specific descriptor
                    let runner_with_descriptor = self.parse_proto_with_cache(&rid, &rdata).await?;
                    let args_descriptor = runner_with_descriptor
                        .get_job_args_message_for_method(using.as_deref())
                        .map_err(|e| {
                            anyhow::anyhow!(
                                "Failed to get args descriptor for method '{}': {:#?}",
                                using.as_deref().unwrap_or(DEFAULT_METHOD_NAME),
                                e
                            )
                        })?;
                    tracing::debug!("job args (using: {:?}): {:#?}", using, &arguments);
                    let job_args = if let Some(desc) = args_descriptor {
                        ProtobufDescriptor::json_value_to_message(desc, &arguments, true).map_err(
                            |e| anyhow::anyhow!("Failed to parse job_args schema: {:#?}", e),
                        )?
                    } else {
                        serde_json::to_string(&arguments)
                            .map_err(|e| anyhow::anyhow!("Failed to serialize job_args: {:#?}", e))?
                            .as_bytes()
                            .to_vec()
                    };
                    // Clone using to keep a copy for result parsing after move
                    let using_for_result = using.clone();
                    let res = self
                        .job_app()
                        .enqueue_job_with_temp_worker(
                            meta,
                            temp_worker_data.clone(),
                            job_args,
                            uniq_key,
                            0,
                            Priority::Medium as i32,
                            (self.timeout_sec() * 1000) as u64,
                            None,
                            false,
                            !temp_worker_data.use_static,
                            using, // Pass using parameter to job execution (moved here)
                        )
                        .await
                        .map(|res| {
                            tracing::debug!("enqueue job result: {:#?}", &res.1);
                            res.1
                        })?;
                    if let Some(r) = res {
                        // Reuse cached runner_with_descriptor for result parsing
                        Self::parse_job_result_with_descriptor(
                            r,
                            &runner_with_descriptor,
                            using_for_result.as_deref(),
                        )
                    } else {
                        Ok(None)
                    }
                } else {
                    Err(JobWorkerError::NotFound("Runner data not found".to_string()).into())
                }
            } else {
                Err(JobWorkerError::NotFound("Runner ID not found".to_string()).into())
            }
        }
    }

    fn create_default_worker_data(
        runner_id: RunnerId,
        runner_name: &str,
        runner_settings: Vec<u8>,
        worker_params: Option<serde_json::Value>,
    ) -> WorkerData {
        let mut worker: WorkerData = if let Some(serde_json::Value::Object(obj)) = worker_params {
            // override values with workflow metadata
            WorkerData {
                name: obj
                    .get("name")
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_else(|| runner_name.to_string()),
                description: obj
                    .get("description")
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_else(|| "".to_string()),
                runner_id: Some(runner_id),
                runner_settings,
                periodic_interval: 0,
                channel: obj
                    .get("channel")
                    .and_then(|v| v.as_str().map(|s| s.to_string())),
                queue_type: obj
                    .get("queue_type")
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .and_then(|s| QueueType::from_str_name(&s).map(|q| q as i32))
                    .unwrap_or(QueueType::Normal as i32),
                response_type: obj
                    .get("response_type")
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .and_then(|s| ResponseType::from_str_name(&s).map(|r| r as i32))
                    .unwrap_or(ResponseType::Direct as i32),
                store_success: obj
                    .get("store_success")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                store_failure: obj
                    .get("store_failure")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true), //
                use_static: obj
                    .get("use_static")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                retry_policy: obj
                    .get("retry_policy")
                    .and_then(|v| v.as_object())
                    .and_then(|o| serde_json::from_value(Value::Object(o.clone())).ok()) // ignore parse errors
                    .or(Some(Self::DEFAULT_RETRY_POLICY)),
                broadcast_results: obj
                    .get("broadcast_results")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true),
            }
        } else {
            // default values
            WorkerData {
                name: runner_name.to_string(),
                description: "Should not use other(maybe temporary)".to_string(),
                runner_id: Some(runner_id),
                runner_settings,
                periodic_interval: 0,
                channel: None,
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::Direct as i32,
                store_success: false,
                store_failure: false,
                use_static: false,
                retry_policy: Some(Self::DEFAULT_RETRY_POLICY), //TODO
                broadcast_results: true,
            }
        };
        // random name (temporary name for not static worker)
        if !worker.use_static {
            let mut hasher = DefaultHasher::default();
            hasher.write_i64(datetime::now_millis());
            hasher.write_i64(rand::random()); // random
            worker.name = format!("{}_{:x}", worker.name, hasher.finish());
            tracing::debug!("Worker name with hash: {}", &worker.name);
        }
        worker
    }

    fn prepare_runner_call_arguments(
        mut request_args: Map<String, Value>,
        _runner: &RunnerWithSchema, // Reserved for future validation logic
        _tool_name_opt: Option<String>, // tool_name now passed via 'using' parameter
    ) -> impl Future<Output = Result<(Option<Value>, Value)>> + Send + '_ {
        async move {
            let settings = request_args.remove("settings");

            // All runners now use unified 'arguments' field
            // tool_name is passed separately via 'using' parameter to worker layer
            let arguments = request_args.get("arguments").cloned().ok_or_else(|| {
                JobWorkerError::InvalidParameter(format!(
                    "Failed to find 'arguments' in request_args: {:#?}",
                    &request_args
                ))
            })?;

            tracing::debug!(
                "runner settings: {:#?}, arguments: {:#?}",
                settings,
                arguments
            );

            Ok((settings, arguments))
        }
    }

    // Build WorkerData from WorkerOptions
    fn build_worker_data_from_options(
        &self,
        name: String,
        description: String,
        runner_id: RunnerId,
        runner_settings: Vec<u8>,
        worker_options: Option<proto::jobworkerp::function::data::WorkerOptions>,
    ) -> Result<WorkerData> {
        let opts = worker_options.unwrap_or_default();

        // queue_type: optional so explicit handling
        // If not specified, default to NORMAL (fast, memory only)
        let queue_type = if let Some(qt) = opts.queue_type {
            QueueType::try_from(qt).map_err(|_| {
                JobWorkerError::InvalidParameter(format!("Invalid queue_type: {}", qt))
            })?
        } else {
            QueueType::Normal // default
        };

        // response_type: optional so explicit handling
        // If not specified, default to DIRECT (synchronous execution, result return)
        let response_type = if let Some(rt) = opts.response_type {
            ResponseType::try_from(rt).map_err(|_| {
                JobWorkerError::InvalidParameter(format!("Invalid response_type: {}", rt))
            })?
        } else {
            ResponseType::Direct // default
        };

        // retry_policy: optional so pass as is
        let retry_policy = opts.retry_policy;

        Ok(WorkerData {
            name,
            description,
            runner_id: Some(runner_id),
            runner_settings,
            retry_policy,
            periodic_interval: 0, // CreateWorker always sets to 0 (periodic execution not supported)
            channel: opts.channel,
            queue_type: queue_type as i32,
            response_type: response_type as i32,
            // bool types: use proto3 default (false) as is
            store_success: opts.store_success,
            store_failure: opts.store_failure,
            use_static: opts.use_static,
            broadcast_results: opts.broadcast_results,
        })
    }

    // Validate worker options (reusing existing validation logic from grpc-front/src/service/worker.rs)
    fn validate_worker_options(&self, worker_data: &WorkerData) -> Result<()> {
        // 1. Periodic + Direct禁止
        if worker_data.periodic_interval != 0
            && worker_data.response_type == ResponseType::Direct as i32
        {
            return Err(JobWorkerError::InvalidParameter(
                "periodic and direct_response can't be set at the same time".to_string(),
            )
            .into());
        }

        // 2. check Periodic interval and fetch_interval
        if worker_data.periodic_interval != 0
            && worker_data.periodic_interval <= self.job_queue_config().fetch_interval
        {
            return Err(JobWorkerError::InvalidParameter(format!(
                "periodic interval can't be set lesser than {}msec",
                self.job_queue_config().fetch_interval
            ))
            .into());
        }

        // 3. no RDB + Direct
        if worker_data.queue_type == QueueType::DbOnly as i32
            && worker_data.response_type == ResponseType::Direct as i32
        {
            return Err(JobWorkerError::InvalidParameter(
                "can't use db queue in direct_response".to_string(),
            )
            .into());
        }

        // 4. Name verification
        if worker_data.name.is_empty() {
            return Err(
                JobWorkerError::InvalidParameter("name should not be empty".to_string()).into(),
            );
        }

        // 5. Retry policy verification
        if let Some(rp) = &worker_data.retry_policy {
            if rp.basis < 1.0 {
                return Err(JobWorkerError::InvalidParameter(
                    "retry_basis should be greater than 1.0".to_string(),
                )
                .into());
            }
        }

        // 6. Channel verification
        if let Some(channel) = &worker_data.channel {
            self.validate_channel(channel)?;
        }

        Ok(())
    }

    // Validate channel existence
    fn validate_channel(&self, channel: &str) -> Result<()> {
        if channel.is_empty() {
            return Err(JobWorkerError::InvalidParameter(
                "channel name cannot be empty".to_string(),
            )
            .into());
        }

        let available_channels: std::collections::HashSet<String> =
            self.worker_config().get_channels().into_iter().collect();

        if !available_channels.contains(channel) {
            return Err(JobWorkerError::InvalidParameter(format!(
                "specified channel '{}' does not exist. Available channels: {:?}",
                channel,
                available_channels.into_iter().collect::<Vec<_>>()
            ))
            .into());
        }

        Ok(())
    }

    // Find runner by name or id
    fn find_runner(
        &self,
        runner_name: Option<String>,
        runner_id: Option<RunnerId>,
    ) -> impl Future<Output = Result<RunnerWithSchema>> + Send + '_ {
        async move {
            if let Some(name) = runner_name {
                self.runner_app()
                    .find_runner_by_name(&name)
                    .await?
                    .ok_or_else(|| {
                        JobWorkerError::NotFound(format!("Runner with name '{}' not found", name))
                            .into()
                    })
            } else if let Some(id) = runner_id {
                self.runner_app().find_runner(&id).await?.ok_or_else(|| {
                    JobWorkerError::NotFound(format!("Runner with id '{}' not found", id.value))
                        .into()
                })
            } else {
                Err(JobWorkerError::InvalidParameter(
                    "Either runner_name or runner_id is required".to_string(),
                )
                .into())
            }
        }
    }

    /// Parse job result using pre-cached runner descriptor
    ///
    /// This method reuses the already-parsed RunnerDataWithDescriptor to avoid
    /// redundant Proto schema parsing. This is more efficient than parse_job_result()
    /// when the descriptor is already available.
    ///
    /// # Arguments
    /// * `job_result` - The job result to parse
    /// * `runner_with_descriptor` - Pre-cached runner descriptor with method schemas
    /// * `using` - Optional method name for MCP/Plugin runners
    fn parse_job_result_with_descriptor(
        job_result: JobResult,
        runner_with_descriptor: &crate::app::runner::RunnerDataWithDescriptor,
        using: Option<&str>,
    ) -> Result<Option<serde_json::Value>> {
        let output = job_result
            .data
            .as_ref()
            .and_then(|r| r.output.as_ref().map(|o| &o.items));

        if let Some(output) = output {
            let method_name = using.unwrap_or(DEFAULT_METHOD_NAME);

            // Use cached result descriptor for the specified method
            let result_descriptor = runner_with_descriptor
                .get_job_result_message_descriptor_for_method(Some(method_name))
                .map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to get result descriptor for method '{}': {:#?}",
                        method_name,
                        e
                    )
                })?;

            if let Some(desc) = result_descriptor {
                match ProtobufDescriptor::get_message_from_bytes(desc, output) {
                    Ok(m) => {
                        let j = ProtobufDescriptor::message_to_json_value(&m)?;
                        tracing::debug!(
                            "Result schema exists (method: {}). decode message with proto: {:#?}",
                            method_name,
                            j
                        );
                        Ok(Some(j))
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to parse result schema for method '{}': {:#?}",
                            method_name,
                            e
                        );
                        Err(JobWorkerError::RuntimeError(format!(
                            "Failed to parse result schema for method '{}': {e:#?}",
                            method_name
                        )))
                    }
                }
            } else {
                let text = String::from_utf8_lossy(output);
                tracing::debug!("No result schema for method '{}': {}", method_name, text);
                Ok(Some(serde_json::Value::String(text.to_string())))
            }
            .map_err(|e| {
                JobWorkerError::RuntimeError(format!("Failed to parse output: {e:#?}")).into()
            })
        } else {
            tracing::warn!("No output found");
            Ok(None)
        }
    }
}
