use crate::config::McpServerConfig;
use crate::grpc_tool::{
    call_fixed_grpc_tool, call_generic_grpc_tool, fixed_grpc_tools, generic_grpc_output_schema,
};
use app::app::function::function_set::{FunctionSetApp, FunctionSetAppImpl};
use app::app::function::{FunctionApp, FunctionAppImpl};
use app_wrapper::llm::chat::conversion::ToolConverter;
use futures::StreamExt;
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler, model::*, service::NotificationContext,
    service::RequestContext,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// MCP Server Handler that bridges jobworkerp's FunctionApp to MCP protocol.
///
/// Implements rmcp::ServerHandler to expose jobworkerp runners and workers
/// as MCP tools, enabling integration with MCP clients like Claude Desktop.
#[derive(Clone)]
pub struct McpHandler {
    function_app: Arc<FunctionAppImpl>,
    function_set_app: Arc<FunctionSetAppImpl>,
    config: McpServerConfig,
    instructions: String,
}

impl McpHandler {
    /// Create a new McpHandler with the given app modules and configuration
    pub fn new(
        function_app: Arc<FunctionAppImpl>,
        function_set_app: Arc<FunctionSetAppImpl>,
        config: McpServerConfig,
    ) -> Self {
        Self {
            function_app,
            function_set_app,
            config,
            instructions: fallback_instructions(),
        }
    }

    /// Create a handler with the initialize instructions resolved once for the
    /// connection/server lifetime. A FunctionSet description is intentionally
    /// only used when that set is also the configured publication boundary.
    pub async fn new_resolved(
        function_app: Arc<FunctionAppImpl>,
        function_set_app: Arc<FunctionSetAppImpl>,
        config: McpServerConfig,
    ) -> Self {
        let mut handler = Self::new(function_app, function_set_app, config);
        if let Some(name) = handler.config.set_name.as_deref() {
            match handler
                .function_set_app
                .find_function_set_by_name(name)
                .await
            {
                Ok(Some(set)) => {
                    if let Some(description) = set.data.map(|data| data.description)
                        && !description.trim().is_empty()
                    {
                        handler.instructions = description;
                    }
                }
                Ok(None) => tracing::warn!(
                    set_name = name,
                    "MCP FunctionSet for instructions was not found"
                ),
                Err(error) => {
                    tracing::warn!(set_name = name, error = %error, "failed to resolve MCP FunctionSet instructions")
                }
            }
        }
        handler
    }

    async fn function_set_tools(&self, set_name: &str) -> Result<ListToolsResult, McpError> {
        let functions = self
            .function_set_app
            .find_functions_by_set(set_name)
            .await
            .map_err(Self::map_error)?;
        self.project_tools(functions).await
    }

    async fn project_tools(
        &self,
        functions: Vec<proto::jobworkerp::function::data::FunctionSpecs>,
    ) -> Result<ListToolsResult, McpError> {
        let mut tools = Vec::new();
        let timeout = Duration::from_millis(self.config.grpc_schema_timeout_ms);
        for function in functions {
            match fixed_grpc_tools(
                &self.function_app,
                &function,
                timeout,
                self.config.proto_schema_max_depth,
            )
            .await
            .map_err(Self::map_error)?
            {
                Some(projected) => tools.extend(projected),
                None => {
                    let mut projected = ToolConverter::convert_normal_function(&function);
                    if proto::jobworkerp::data::RunnerType::try_from(function.runner_type).ok()
                        == Some(proto::jobworkerp::data::RunnerType::Grpc)
                    {
                        for tool in &mut projected {
                            tool.output_schema = Some(Arc::new(generic_grpc_output_schema()));
                        }
                    }
                    tools.extend(projected);
                }
            }
        }
        Ok(ListToolsResult {
            tools,
            next_cursor: None,
            meta: None,
        })
    }

    async fn ensure_tool_is_published(&self, name: &str) -> Result<(), McpError> {
        // Calling a tool must not resolve every fixed gRPC worker in the
        // publication set.  Those resolutions may perform independent remote
        // reflection requests, so only inspect the function that could own
        // this exact tool name.
        for function in self.publication_functions().await? {
            let is_grpc = proto::jobworkerp::data::RunnerType::try_from(function.runner_type).ok()
                == Some(proto::jobworkerp::data::RunnerType::Grpc);
            let could_be_fixed = is_grpc && name.starts_with(&format!("{}___", function.name));
            if could_be_fixed {
                let timeout = Duration::from_millis(self.config.grpc_schema_timeout_ms);
                match fixed_grpc_tools(
                    &self.function_app,
                    &function,
                    timeout,
                    self.config.proto_schema_max_depth,
                )
                .await
                .map_err(Self::map_error)?
                {
                    Some(tools) if tools.iter().any(|tool| tool.name == name) => return Ok(()),
                    Some(_) => continue,
                    None => {}
                }
            }
            if ToolConverter::convert_normal_function(&function)
                .iter()
                .any(|tool| tool.name == name)
            {
                return Ok(());
            }
        }
        Err(McpError::method_not_found::<CallToolRequestMethod>())
    }

    async fn publication_functions(
        &self,
    ) -> Result<Vec<proto::jobworkerp::function::data::FunctionSpecs>, McpError> {
        if let Some(set_name) = self.config.set_name.as_deref() {
            self.function_set_app
                .find_functions_by_set(set_name)
                .await
                .map_err(Self::map_error)
        } else {
            self.function_app
                .find_functions(
                    self.config.exclude_runner_as_tool,
                    self.config.exclude_worker_as_tool,
                )
                .await
                .map_err(Self::map_error)
        }
    }

    async fn is_generic_grpc_tool(&self, name: &str) -> Result<bool, McpError> {
        let functions = self.publication_functions().await?;
        Ok(functions.into_iter().any(|function| {
            proto::jobworkerp::data::RunnerType::try_from(function.runner_type).ok()
                == Some(proto::jobworkerp::data::RunnerType::Grpc)
                && ToolConverter::convert_normal_function(&function)
                    .iter()
                    .any(|tool| tool.name == name)
        }))
    }

    /// Map internal errors to MCP ErrorData
    fn map_error(e: anyhow::Error) -> McpError {
        use jobworkerp_base::error::JobWorkerError;

        if let Some(jwe) = e.downcast_ref::<JobWorkerError>() {
            match jwe {
                JobWorkerError::NotFound(_) => {
                    McpError::method_not_found::<CallToolRequestMethod>()
                }
                JobWorkerError::InvalidParameter(msg) => {
                    McpError::invalid_params(msg.clone(), None)
                }
                JobWorkerError::WorkerNotFound(_) => {
                    McpError::method_not_found::<CallToolRequestMethod>()
                }
                _ => McpError::internal_error(e.to_string(), None),
            }
        } else {
            McpError::internal_error(e.to_string(), None)
        }
    }
}

impl ServerHandler for McpHandler {
    fn get_info(&self) -> ServerInfo {
        // ServerInfo (InitializeResult) is #[non_exhaustive] in rmcp 2.x; build via constructor.
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::LATEST)
            .with_instructions(self.instructions.clone())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        if let Some(name) = &self.config.set_name {
            self.function_set_tools(name).await
        } else {
            let functions = self
                .function_app
                .find_functions(
                    self.config.exclude_runner_as_tool,
                    self.config.exclude_worker_as_tool,
                )
                .await
                .map_err(Self::map_error)?;
            self.project_tools(functions).await
        }
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        // Tool name is passed directly to call_function_for_llm().
        // The internal find_runner_by_name_with_mcp() handles the "runner___method" format
        // via divide_names(), so no pre-processing is needed here.
        let name = request.name.as_ref();
        self.ensure_tool_is_published(name).await?;

        let request_meta = request.meta.map(|meta| serde_json::Value::Object(meta.0));
        let raw_arguments = request.arguments;
        let arguments = raw_arguments.clone().unwrap_or_default();
        if let Some(result) = call_fixed_grpc_tool(
            &self.function_app,
            name,
            arguments,
            request_meta.clone(),
            Duration::from_millis(self.config.grpc_schema_timeout_ms),
            self.config.timeout_sec,
        )
        .await
        .map_err(Self::map_error)?
        {
            return Ok(result);
        }
        let is_generic_grpc = self.is_generic_grpc_tool(name).await?;
        if is_generic_grpc
            && let Some(result) = call_generic_grpc_tool(
                &self.function_app,
                name,
                raw_arguments.clone().unwrap_or_default(),
                request_meta,
                Duration::from_millis(self.config.grpc_schema_timeout_ms),
                self.config.timeout_sec,
            )
            .await
            .map_err(Self::map_error)?
        {
            return Ok(result);
        }

        // Arguments from the MCP client follow the per-tool schema generated by
        // ToolConverter, which differs by target:
        // - Runner tools: { "settings": {...}, "arguments": {...} }; resolved via
        //   prepare_runner_call_arguments().
        // - Worker tools: the input fields at the top level (no wrapper); WORKFLOW
        //   workers receive the workflow input directly. Resolved via
        //   prepare_worker_call_arguments().
        // call_function_for_llm() dispatches to the right path by name.
        let arguments = raw_arguments;

        let meta = Arc::new(HashMap::new());

        if self.config.streaming {
            // Streaming execution: collect all results from the stream
            let stream = self.function_app.call_function_for_llm_streaming(
                meta,
                name,
                arguments,
                self.config.timeout_sec,
            );

            let mut collected_output = Vec::new();
            let mut last_error: Option<anyhow::Error> = None;
            let mut final_info = None;

            futures::pin_mut!(stream);
            while let Some(result) = stream.next().await {
                match result {
                    Ok(function_result) => {
                        // Collect non-empty output chunks
                        if !function_result.output.is_empty() {
                            collected_output.push(function_result.output);
                        }
                        // Preserve execution info from final chunk
                        if function_result.last_info.is_some() {
                            final_info = function_result.last_info;
                        }
                    }
                    Err(e) => {
                        last_error = Some(e);
                        break;
                    }
                }
            }

            if let Some(e) = last_error {
                if is_generic_grpc {
                    return Ok(generic_grpc_error(e));
                }
                return Err(Self::map_error(e));
            }

            // Combine all collected outputs
            let combined_output = if collected_output.is_empty() {
                serde_json::json!({
                    "status": "success",
                    "output": "",
                    "execution_info": final_info
                })
            } else {
                serde_json::json!({
                    "status": "success",
                    "output": collected_output.join(""),
                    "execution_info": final_info
                })
            };

            if is_generic_grpc {
                Ok(CallToolResult::structured(
                    serde_json::json!({"output": combined_output}),
                ))
            } else {
                Ok(CallToolResult::success(vec![ContentBlock::json(
                    combined_output,
                )?]))
            }
        } else {
            // Non-streaming execution
            let result = self
                .function_app
                .call_function_for_llm(meta, name, arguments, self.config.timeout_sec)
                .await;
            let result = match result {
                Ok(result) => result,
                Err(error) if is_generic_grpc => return Ok(generic_grpc_error(error)),
                Err(error) => return Err(Self::map_error(error)),
            };

            if is_generic_grpc {
                Ok(CallToolResult::structured(
                    serde_json::json!({"output": result}),
                ))
            } else {
                Ok(CallToolResult::success(vec![ContentBlock::json(result)?]))
            }
        }
    }

    async fn initialize(
        &self,
        _request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        // Headers can contain credentials, so initialization audit logs keep
        // only the request method and path.
        if let Some(http_parts) = context.extensions.get::<axum::http::request::Parts>() {
            tracing::info!(
                method = %http_parts.method,
                path = %http_parts.uri.path(),
                "MCP initialize from HTTP client"
            );
        }
        Ok(self.get_info())
    }

    async fn on_cancelled(
        &self,
        notification: CancelledNotificationParam,
        _context: NotificationContext<RoleServer>,
    ) {
        // Log cancellation request for debugging/auditing
        // Note: Full job cancellation requires request_id to job_id mapping,
        // which is a future enhancement
        // request_id is Option<NumberOrString> in rmcp 2.x, which is not Display; use Debug.
        tracing::info!(
            request_id = ?notification.request_id,
            reason = ?notification.reason,
            "MCP request cancelled"
        );

        // TODO: Implement job cancellation when request_id to job_id mapping is available
        // This would involve:
        // 1. Tracking active requests with their job IDs
        // 2. Calling job_app.cancel(job_id) when cancellation is requested
    }

    async fn on_initialized(&self, _context: NotificationContext<RoleServer>) {
        tracing::info!("MCP client initialized successfully");
    }
}

fn fallback_instructions() -> String {
    std::env::var("MCP_INSTRUCTIONS")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            "jobworkerp MCP Server - Asynchronous job processing with various runners. \
             Available runners include COMMAND, HTTP_REQUEST, PYTHON_COMMAND, DOCKER, \
             LLM, WORKFLOW, GRPC, and custom plugins."
                .to_string()
        })
}

fn generic_grpc_error(error: anyhow::Error) -> CallToolResult {
    use jobworkerp_base::error::JobWorkerError;

    let stage = if error
        .downcast_ref::<JobWorkerError>()
        .is_some_and(|error| matches!(error, JobWorkerError::InvalidParameter(_)))
    {
        "input"
    } else {
        "execution"
    };
    CallToolResult::structured_error(
        serde_json::json!({"error":{"stage":stage,"message":error.to_string()}}),
    )
}

#[cfg(test)]
mod tests {
    use super::generic_grpc_error;
    use jobworkerp_base::error::JobWorkerError;

    #[test]
    fn generic_grpc_invalid_parameter_is_a_tool_input_error() {
        let result =
            generic_grpc_error(JobWorkerError::InvalidParameter("bad input".to_string()).into());
        assert!(result.is_error.unwrap_or_default());
        assert_eq!(
            result
                .structured_content
                .as_ref()
                .and_then(|value| value.get("error"))
                .and_then(|value| value.get("stage"))
                .and_then(serde_json::Value::as_str),
            Some("input")
        );
    }
}
