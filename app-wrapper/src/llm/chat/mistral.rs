use crate::llm::generic_tracing_helper::GenericLLMTracingHelper;
use crate::llm::mistral::{
    MistralLlmServiceImpl, MistralRSMessage, MistralRSToolCall, ToolCallingConfig,
    ToolExecutionError,
};
use crate::llm::tracing::mistral_helper::MistralTracingHelper;
use crate::llm::tracing::LLMTracingHelper;
use anyhow::Result;
use app::app::function::function_set::{FunctionSetAppImpl, UseFunctionSetApp};
use app::app::function::{FunctionApp, FunctionAppImpl, UseFunctionApp};
use async_stream::stream;
use command_utils::trace::impls::GenericOtelClient;
use command_utils::trace::otel_span::GenAIOtelClient;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole as TextMessageRole;
use jobworkerp_runner::jobworkerp::runner::llm::{
    llm_runner_settings::LocalRunnerSettings, LlmChatArgs, LlmChatResult,
};
use mistralrs::{RequestBuilder, Tool};
use opentelemetry::Context;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::time::{timeout, Duration};

#[derive(Clone)]
pub struct MistralRSService {
    pub core_service: Arc<MistralLlmServiceImpl>, // Direct use of concrete type
    pub function_app: Arc<FunctionAppImpl>,       // Tool execution functionality (required)
    pub function_set_app: Arc<FunctionSetAppImpl>, // FunctionSet management (required)
    pub otel_client: Option<Arc<GenericOtelClient>>, // Tracing client
    pub config: ToolCallingConfig,                // Tool calling configuration
}

impl MistralRSService {
    /// Constructor (Composition pattern)
    pub async fn new_with_function_app(
        settings: LocalRunnerSettings,
        function_app: Arc<FunctionAppImpl>,
        function_set_app: Arc<FunctionSetAppImpl>,
    ) -> Result<Self> {
        let core_service = Arc::new(MistralLlmServiceImpl::new(&settings).await?);

        Ok(Self {
            core_service,
            function_app,
            function_set_app,
            otel_client: Some(Arc::new(GenericOtelClient::new(
                "mistralrs.tool_calling_service",
            ))),
            config: ToolCallingConfig::default(),
        })
    }

    /// Main entry point using generic_tracing_helper approach
    pub async fn request_chat(
        &self,
        args: LlmChatArgs,
        cx: Context,
        metadata: HashMap<String, String>,
    ) -> Result<LlmChatResult> {
        // 1. Pre-resolve tool information
        let resolved_tools = if let Some(function_opts) = &args.function_options {
            if function_opts.use_function_calling {
                self.create_tools_from_options(function_opts).await?
            } else {
                vec![]
            }
        } else {
            vec![]
        };

        // 2. Convert messages to MistralRSMessage for processing
        let messages: Vec<_> = args
            .messages
            .iter()
            .map(|m| {
                let role = TextMessageRole::try_from(m.role).unwrap_or(TextMessageRole::User);
                let content = m
                    .content
                    .as_ref()
                    .and_then(|c| c.content.as_ref())
                    .map(|content| match content {
                        jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::Content::Text(text) => text.clone(),
                        _ => String::new(),
                    })
                    .unwrap_or_default();

                crate::llm::mistral::MistralRSMessage {
                    role,
                    content,
                    tool_call_id: None,
                    tool_calls: None,
                }
            })
            .collect();

        // 3. Create span attributes using generic helper
        let model_options = crate::llm::tracing::mistral_helper::MistralModelOptions {
            temperature: args.options.as_ref().and_then(|o| o.temperature),
            max_tokens: args
                .options
                .as_ref()
                .and_then(|o| o.max_tokens.map(|t| t as u32)),
            top_p: args.options.as_ref().and_then(|o| o.top_p),
        };

        let model_parameters = {
            let mut parameters = HashMap::new();
            if let Some(temp) = model_options.temperature {
                parameters.insert("temperature".to_string(), serde_json::json!(temp));
            }
            if let Some(max_tokens) = model_options.max_tokens {
                parameters.insert("max_tokens".to_string(), serde_json::json!(max_tokens));
            }
            if let Some(top_p) = model_options.top_p {
                parameters.insert("top_p".to_string(), serde_json::json!(top_p));
            }
            parameters
        };
        let span_attributes = self.create_chat_completion_span_attributes(
            self.core_service.model_name(),
            self.convert_messages_to_input(&messages),
            Some(&model_parameters),
            &resolved_tools,
            &metadata,
        );

        // 4. Execute chat with tracing using generic helper
        let chat_action = {
            let args_clone = args.clone();
            let resolved_tools_arc = Arc::new(resolved_tools);
            let cx_clone = cx.clone();
            let metadata_clone = metadata.clone();
            let service = self.clone();

            async move {
                service
                    .execute_chat_with_context_preservation(
                        args_clone,
                        resolved_tools_arc,
                        Some(cx_clone),
                        metadata_clone,
                    )
                    .await
                    .map_err(|e| jobworkerp_base::error::JobWorkerError::OtherError(e.to_string()))
            }
        };

        let (mistral_response, _context) = GenericLLMTracingHelper::with_chat_response_tracing(
            self,
            &metadata,
            Some(cx),
            span_attributes,
            chat_action,
        )
        .await?;

        // 5. Convert response to final result
        Ok(self.convert_mistral_response_to_final_result(&mistral_response))
    }

    /// Streaming chat processing - Strategy 1 for tool calling
    pub async fn request_stream_chat(
        &self,
        args: LlmChatArgs,
    ) -> Result<futures::stream::BoxStream<'static, LlmChatResult>> {
        use futures::stream::{self, StreamExt};

        let has_tools = args
            .function_options
            .as_ref()
            .is_some_and(|opts| opts.use_function_calling);

        if !has_tools {
            // No tool calling - direct streaming
            return Arc::new(self.clone())
                .request_direct_stream_chat(args)
                .await;
        }

        // Tool calling enabled - Strategy 1: Non-streaming tool calling + final result streaming
        tracing::debug!("Tool calling detected, using non-streaming execution + final streaming");

        // 1. Execute tool calling completely in non-streaming mode
        let final_result = self
            .request_chat(
                args,
                opentelemetry::Context::current(),
                std::collections::HashMap::new(),
            )
            .await?;

        // 2. Return final result as single stream item
        let result_stream = stream::once(async move { final_result });
        Ok(result_stream.boxed())
    }

    /// Direct streaming processing without tool calling
    async fn request_direct_stream_chat(
        self: Arc<Self>,
        args: LlmChatArgs,
    ) -> Result<futures::stream::BoxStream<'static, LlmChatResult>> {
        use async_stream::stream;
        use futures::stream::StreamExt;

        let messages = self.convert_proto_messages(&args)?;
        let request_builder = self.build_request_from_messages(&messages, &[]).await?;

        let mistral_stream: futures::stream::BoxStream<'static, mistralrs::Response> =
            self.core_service.stream_chat(request_builder).await?;

        // Stream converting MistralRS Response to LlmChatResult
        let result_stream = stream! {
            tokio::pin!(mistral_stream);
            while let Some(response) = mistral_stream.next().await {
                match response {
                    mistralrs::Response::Chunk(chunk) => {
                        use crate::llm::mistral::result::{DefaultLLMResultConverter, LLMResultConverter};
                        let result =DefaultLLMResultConverter::convert_chat_completion_chunk_result(&chunk);
                        // let result = self.convert_mistral_chunk_to_result(&chunk);
                        yield result;
                    }
                    mistralrs::Response::Done(completion) => {
                        let result = self.convert_mistral_response_to_final_result(&completion);
                        yield result;
                        break; // End on Done response
                    }
                    _ => {
                        // Other response types (add processing as needed)
                        tracing::debug!("Received other response type in stream");
                    }
                }
            }
        };

        Ok(result_stream.boxed())
    }

    /// Core tool execution logic (tracing separated)
    async fn execute_tool_call_core(
        call: MistralRSToolCall,
        function_app: Arc<FunctionAppImpl>,
        metadata: Arc<HashMap<String, String>>,
        config: ToolCallingConfig,
    ) -> MistralRSMessage {
        let arguments_obj = serde_json::from_str(&call.arguments).map_err(|e| {
            ToolExecutionError::InvalidArguments {
                reason: format!("JSON parse error: {e}"),
            }
        });
        if let Err(e) = arguments_obj {
            tracing::warn!(
                "Invalid arguments for tool call '{}': {}",
                call.function_name,
                e
            );
            return MistralRSMessage {
                role: TextMessageRole::Tool,
                content: format!(
                    "Error parsing arguments for tool '{}': {}",
                    call.function_name, e
                ),
                tool_call_id: Some(call.id),
                tool_calls: None,
            };
        } else {
            tracing::debug!(
                "Parsed arguments for tool call '{}': {:?}",
                call.function_name,
                arguments_obj
            );
        }
        let arguments_obj: serde_json::Map<String, serde_json::Value> = arguments_obj.unwrap();

        // Execute tool with timeout
        let tool_execution = async {
            function_app
                .call_function_for_llm(
                    metadata,
                    &call.function_name,
                    Some(arguments_obj),
                    config.tool_timeout_sec,
                )
                .await
        };

        match timeout(
            Duration::from_secs(config.tool_timeout_sec as u64),
            tool_execution,
        )
        .await
        {
            Ok(Ok(result)) => {
                tracing::debug!("Tool call executed successfully: {}", call.function_name);
                MistralRSMessage {
                    role: TextMessageRole::Tool,
                    content: result.to_string(),
                    tool_call_id: Some(call.id),
                    tool_calls: None,
                }
            }
            Ok(Err(e)) => {
                tracing::warn!("Tool call failed: {}", e);
                MistralRSMessage {
                    role: TextMessageRole::Tool,
                    content: format!("Error executing tool '{}': {}", call.function_name, e),
                    tool_call_id: Some(call.id),
                    tool_calls: None,
                }
            }
            Err(_) => {
                tracing::warn!(
                    "Tool execution timed out after {} seconds",
                    config.tool_timeout_sec
                );
                MistralRSMessage {
                    role: TextMessageRole::Tool,
                    content: format!(
                        "Tool execution timed out after {} seconds",
                        config.tool_timeout_sec
                    ),
                    tool_call_id: Some(call.id),
                    tool_calls: None,
                }
            }
        }
    }

    // Helper methods that delegate to existing implementations
    fn convert_proto_messages(&self, args: &LlmChatArgs) -> Result<Vec<MistralRSMessage>> {
        // Direct access to MistralLlmServiceImpl
        self.core_service.convert_proto_messages(args)
    }

    fn extract_tool_calls_from_response(
        &self,
        response: &mistralrs::ChatCompletionResponse,
    ) -> Result<Vec<MistralRSToolCall>> {
        // Direct access to MistralLlmServiceImpl
        self.core_service.extract_tool_calls_from_response(response)
    }

    async fn create_tools_from_options(
        &self,
        function_opts: &jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::FunctionOptions,
    ) -> Result<Vec<Tool>> {
        use crate::llm::mistral::args::LLMRequestConverter;
        LLMRequestConverter::create_tools_from_options(self, function_opts).await
    }

    async fn build_request_from_messages(
        &self,
        messages: &[MistralRSMessage],
        tools: &[Tool],
    ) -> Result<RequestBuilder> {
        use mistralrs::{TextMessageRole as MistralTextRole, ToolChoice};
        let mut builder = RequestBuilder::new();

        for msg in messages {
            match msg.role {
                TextMessageRole::Tool => {
                    if let Some(tool_call_id) = &msg.tool_call_id {
                        tracing::debug!(
                            "Adding tool message: '{}' with call_id: {}",
                            msg.content,
                            tool_call_id
                        );
                        builder =
                            builder.add_tool_message(msg.content.clone(), tool_call_id.clone());
                    } else {
                        tracing::warn!(
                            "Tool message without tool_call_id, adding as regular message"
                        );
                        let role = match msg.role {
                            TextMessageRole::User => MistralTextRole::User,
                            TextMessageRole::Assistant => MistralTextRole::Assistant,
                            TextMessageRole::System => MistralTextRole::System,
                            TextMessageRole::Tool => MistralTextRole::Tool,
                            _ => MistralTextRole::User,
                        };
                        builder = builder.add_message(role, &msg.content);
                    }
                }
                TextMessageRole::Assistant => {
                    // Special handling for assistant messages containing tool calls
                    if let Some(tool_calls) = &msg.tool_calls {
                        if !tool_calls.is_empty() {
                            tracing::debug!(
                                "Adding Assistant message with {} tool calls: '{}'",
                                tool_calls.len(),
                                msg.content
                            );
                        } else {
                            tracing::debug!("Adding Assistant message: '{}'", msg.content);
                        }
                        builder = builder.add_message(MistralTextRole::Assistant, &msg.content);
                    } else {
                        tracing::debug!("Adding Assistant message: '{}'", msg.content);
                        builder = builder.add_message(MistralTextRole::Assistant, &msg.content);
                    }
                }
                _ => {
                    let role = match msg.role {
                        TextMessageRole::User => MistralTextRole::User,
                        TextMessageRole::System => MistralTextRole::System,
                        _ => MistralTextRole::User,
                    };
                    builder = builder.add_message(role, &msg.content);
                }
            }
        }

        if !tools.is_empty() {
            tracing::debug!("Adding {} tools to MistralRS request", tools.len());
            for (i, tool) in tools.iter().enumerate() {
                tracing::debug!(
                    "Tool {}: {} - {}\n schema: {:#?}",
                    i,
                    tool.function.name,
                    tool.function
                        .description
                        .as_deref()
                        .unwrap_or("No description"),
                    tool.function.parameters.as_ref()
                );
            }
            builder = builder.set_tools(tools.to_vec());
            builder = builder.set_tool_choice(ToolChoice::Auto);
        } else {
            tracing::debug!("No tools provided to MistralRS request");
        }

        Ok(builder)
    }

    fn convert_mistral_response_to_final_result(
        &self,
        response: &mistralrs::ChatCompletionResponse,
    ) -> LlmChatResult {
        use crate::llm::mistral::result::{DefaultLLMResultConverter, LLMResultConverter};
        DefaultLLMResultConverter::convert_chat_completion_result(response)
    }
}

impl UseFunctionApp for MistralRSService {
    fn function_app(&self) -> &FunctionAppImpl {
        &self.function_app
    }
}

impl UseFunctionSetApp for MistralRSService {
    fn function_set_app(&self) -> &FunctionSetAppImpl {
        &self.function_set_app
    }
}

impl GenericLLMTracingHelper for MistralRSService {
    fn get_otel_client(&self) -> Option<&Arc<GenericOtelClient>> {
        self.otel_client.as_ref()
    }

    fn convert_messages_to_input(
        &self,
        messages: &[impl crate::llm::generic_tracing_helper::LLMMessage],
    ) -> serde_json::Value {
        serde_json::json!(messages
            .iter()
            .map(|m| serde_json::json!({
                "role": m.get_role(),
                "content": m.get_content()
            }))
            .collect::<Vec<_>>())
    }

    fn get_provider_name(&self) -> &str {
        "mistralrs"
    }
}

impl MistralTracingHelper for MistralRSService {}

impl crate::llm::tracing::LLMTracingHelper for MistralRSService {
    fn get_otel_client(
        &self,
    ) -> Option<&std::sync::Arc<command_utils::trace::impls::GenericOtelClient>> {
        self.otel_client.as_ref()
    }

    fn get_provider_name(&self) -> &str {
        "mistralrs"
    }

    fn get_default_model(&self) -> String {
        self.core_service.model_name().to_string()
    }
}

impl crate::llm::mistral::args::LLMRequestConverter for MistralRSService {}

impl MistralRSService {
    /// Tool calling processing with context inheritance
    async fn execute_chat_with_context_preservation(
        &self,
        args: LlmChatArgs,
        resolved_tools: Arc<Vec<mistralrs::Tool>>,
        parent_context: Option<Context>, // Preserve context inheritance
        metadata: HashMap<String, String>,
    ) -> Result<mistralrs::ChatCompletionResponse> {
        let messages = self.convert_proto_messages(&args)?;

        // Preserve existing complex logic (remove only non-standard tracing)
        Arc::new(self.clone())
            .request_chat_internal_recursively(
                messages,
                resolved_tools,
                parent_context, // Context inheritance
                Arc::new(metadata),
                0,
            )
            .await
    }

    /// Internal processing with context inheritance preserved (full functionality maintained)
    async fn request_chat_internal_recursively(
        self: Arc<Self>,
        mut messages: Vec<crate::llm::mistral::MistralRSMessage>,
        tools: Arc<Vec<mistralrs::Tool>>,
        parent_context: Option<Context>, // Preserve inheritance
        metadata: Arc<HashMap<String, String>>,
        iteration_count: usize,
    ) -> Result<mistralrs::ChatCompletionResponse> {
        // Maximum iteration check (preserve existing logic)
        if iteration_count >= self.config.max_iterations {
            return Err(anyhow::Error::from(
                crate::llm::mistral::ToolExecutionError::MaxIterationsExceeded {
                    max_iterations: self.config.max_iterations,
                },
            ));
        }

        // LLM call (use standard logging only, remove custom spans)
        let request_builder = self.build_request_from_messages(&messages, &tools).await?;
        let response = self.core_service.request_chat(request_builder).await?;

        // Tool calls processing (preserve existing logic completely)
        let tool_calls = self.extract_tool_calls_from_response(&response)?;

        if tool_calls.is_empty() {
            Ok(response) // Final response
        } else {
            // Tool execution (context inheritance preserved, supports parallel/sequential)
            let tool_results = if self.config.parallel_execution {
                self.execute_tool_calls_parallel_with_context(
                    &tool_calls,
                    metadata.clone(),
                    parent_context.clone(),
                )
                .await?
            } else {
                self.execute_tool_calls_sequential_with_context(
                    &tool_calls,
                    metadata.clone(),
                    parent_context.clone(),
                )
                .await?
            };

            messages.extend(tool_results);

            // Recursive call (context inheritance preserved)
            Box::pin(self.request_chat_internal_recursively(
                messages,
                tools,
                parent_context, // Preserve context inheritance
                metadata,
                iteration_count + 1,
            ))
            .await
        }
    }

    /// Parallel tool execution with unified span tracing (performance-focused)
    async fn execute_tool_calls_parallel_with_context(
        &self,
        tool_calls: &[crate::llm::mistral::MistralRSToolCall],
        metadata: Arc<HashMap<String, String>>,
        parent_context: Option<Context>,
    ) -> Result<Vec<crate::llm::mistral::MistralRSMessage>> {
        if tool_calls.is_empty() {
            return Ok(vec![]);
        }

        let current_context = parent_context.unwrap_or_else(Context::current);

        if let Some(_client) = LLMTracingHelper::get_otel_client(self) {
            self.execute_parallel_tools_with_unified_span(tool_calls, metadata, current_context)
                .await
        } else {
            self.execute_parallel_tools_without_tracing(tool_calls, metadata)
                .await
        }
    }

    /// Unified span for parallel tool execution (performance-optimized with statistics)
    async fn execute_parallel_tools_with_unified_span(
        &self,
        tool_calls: &[crate::llm::mistral::MistralRSToolCall],
        metadata: Arc<HashMap<String, String>>,
        parent_context: Context,
    ) -> Result<Vec<crate::llm::mistral::MistralRSMessage>> {
        use command_utils::trace::attr::{OtelSpanBuilder, OtelSpanType};

        let span_name = "mistralrs.tool_calls.parallel";
        let mut span_builder = OtelSpanBuilder::new(span_name)
            .span_type(OtelSpanType::Event)
            .level("INFO")
            .input(serde_json::json!({
                "tool_count": tool_calls.len(),
                "tools": tool_calls.iter().map(|call| {
                    serde_json::json!({
                        "name": call.function_name,
                        "id": call.id
                    })
                }).collect::<Vec<_>>()
            }));

        if let Some(session_id) = metadata.get("session_id") {
            span_builder = span_builder.session_id(session_id.clone());
        }
        if let Some(user_id) = metadata.get("user_id") {
            span_builder = span_builder.user_id(user_id.clone());
        }

        let span_attributes = span_builder.build();
        let client = LLMTracingHelper::get_otel_client(self).unwrap().clone();

        // Clone data for async block
        let function_app = self.function_app.clone();
        let config = self.config.clone();
        let tool_calls_owned: Vec<_> = tool_calls.to_vec();

        // Execute tools within unified span
        let results = client.with_span_result(
            span_attributes,
            Some(parent_context),
            async move {
                // Parallel execution
                let handles: Vec<_> = tool_calls_owned
                    .iter()
                    .enumerate()
                    .map(|(idx, call)| {
                        let function_app = function_app.clone();
                        let metadata = metadata.clone();
                        let call = call.clone();
                        let config = config.clone();

                        tokio::spawn(async move {
                            (idx, Self::execute_tool_call_core(call, function_app, metadata, config).await)
                        })
                    })
                    .collect();

                let results = futures::future::join_all(handles).await;
                let mut messages = Vec::new();
                let mut success_count = 0;
                let mut error_count = 0;

                for result in results.into_iter() {
                    match result {
                        Ok((_idx, message)) => {
                            // Count tool execution success/failure
                            if message.content.contains("Error") || message.content.contains("Failed") {
                                error_count += 1;
                            } else {
                                success_count += 1;
                            }
                            messages.push(message);
                        }
                        Err(join_error) => {
                            error_count += 1;
                            let tool_call_id = tool_calls_owned
                                .get(messages.len())
                                .map(|call| call.id.clone())
                                .unwrap_or_else(|| format!("unknown_{}", messages.len()));

                            messages.push(crate::llm::mistral::MistralRSMessage {
                                role: jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole::Tool,
                                content: format!("Task execution failed: {join_error}"),
                                tool_call_id: Some(tool_call_id),
                                tool_calls: None,
                            });
                        }
                    }
                }

                // Log statistics
                tracing::info!(
                    "Parallel tool execution completed: {} success, {} errors, {} total",
                    success_count, error_count, tool_calls_owned.len()
                );

                Ok::<Vec<crate::llm::mistral::MistralRSMessage>, jobworkerp_base::error::JobWorkerError>(messages)
            },
        ).await.map_err(|e| anyhow::anyhow!("Tool execution span error: {e}"))?;

        Ok(results)
    }

    /// Parallel execution without tracing (preserves existing logic)
    async fn execute_parallel_tools_without_tracing(
        &self,
        tool_calls: &[crate::llm::mistral::MistralRSToolCall],
        metadata: Arc<HashMap<String, String>>,
    ) -> Result<Vec<crate::llm::mistral::MistralRSMessage>> {
        // Existing implementation preserved
        let handles: Vec<_> = tool_calls
            .iter()
            .map(|call| {
                let function_app = self.function_app.clone();
                let metadata = metadata.clone();
                let call = call.clone();
                let config = self.config.clone();
                tokio::spawn(async move {
                    Self::execute_tool_call_core(call, function_app, metadata, config).await
                })
            })
            .collect();

        let results = futures::future::join_all(handles).await;
        let mut messages = Vec::new();

        for (i, result) in results.into_iter().enumerate() {
            let tool_call_id = tool_calls
                .get(i)
                .map(|call| call.id.clone())
                .unwrap_or_else(|| format!("unknown_{i}"));

            match result {
                Ok(message) => messages.push(message),
                Err(join_error) => {
                    messages.push(crate::llm::mistral::MistralRSMessage {
                        role: jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole::Tool,
                        content: format!("Task execution failed: {join_error}"),
                        tool_call_id: Some(tool_call_id),
                        tool_calls: None,
                    });
                }
            }
        }

        Ok(messages)
    }

    /// Sequential tool execution with detailed individual span tracing (Ollama-style)
    async fn execute_tool_calls_sequential_with_context(
        &self,
        tool_calls: &[crate::llm::mistral::MistralRSToolCall],
        metadata: Arc<HashMap<String, String>>,
        parent_context: Option<Context>,
    ) -> Result<Vec<crate::llm::mistral::MistralRSMessage>> {
        if tool_calls.is_empty() {
            return Ok(vec![]);
        }

        let mut current_context = parent_context.unwrap_or_else(Context::current);
        let mut messages = Vec::new();

        for call in tool_calls.iter() {
            if let Some(_client) = LLMTracingHelper::get_otel_client(self) {
                // Individual span for detailed tracing (Ollama pattern)
                let (message, updated_context) = self
                    .execute_single_tool_with_detailed_tracing(call, &metadata, current_context)
                    .await?;

                messages.push(message);
                current_context = updated_context;
            } else {
                // Execution without tracing
                let message = Self::execute_tool_call_core(
                    call.clone(),
                    self.function_app.clone(),
                    metadata.clone(),
                    self.config.clone(),
                )
                .await;
                messages.push(message);
            }
        }

        Ok(messages)
    }

    /// Execute individual tool with detailed span creation
    async fn execute_single_tool_with_detailed_tracing(
        &self,
        call: &crate::llm::mistral::MistralRSToolCall,
        metadata: &Arc<HashMap<String, String>>,
        parent_context: Context,
    ) -> Result<(crate::llm::mistral::MistralRSMessage, Context)> {
        let tool_attributes = self.create_tool_call_span_from_mistral_call(
            &call.function_name,
            &call.arguments,
            metadata,
        );

        // Tool execution logic
        let function_app = self.function_app.clone();
        let metadata_clone = metadata.clone();
        let call_clone = call.clone();
        let config = self.config.clone();

        let tool_action = async move {
            let message =
                Self::execute_tool_call_core(call_clone, function_app, metadata_clone, config)
                    .await;

            Ok::<serde_json::Value, jobworkerp_base::error::JobWorkerError>(serde_json::json!({
                "content": message.content,
                "tool_call_id": message.tool_call_id,
                "role": "tool"
            }))
        };

        let (tool_result, updated_context) = MistralTracingHelper::with_tool_response_tracing(
            self,
            metadata,
            parent_context,
            tool_attributes,
            &call.function_name,
            &call.arguments,
            tool_action,
        )
        .await?;

        let message = crate::llm::mistral::MistralRSMessage {
            role: jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole::Tool,
            content: tool_result
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string(),
            tool_call_id: call.id.clone().into(),
            tool_calls: None,
        };

        Ok((message, updated_context))
    }
}
