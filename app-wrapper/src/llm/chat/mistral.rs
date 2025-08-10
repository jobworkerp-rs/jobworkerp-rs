use crate::llm::generic_tracing_helper::GenericLLMTracingHelper;
use crate::llm::mistral::core::MistralCoreService;
use crate::llm::mistral::tracing::MistralTracingService;
use crate::llm::mistral::{
    MistralLlmServiceImpl, MistralRSMessage, MistralRSToolCall, SerializableChatResponse,
    ToolCallingConfig, ToolExecutionError,
};
use crate::llm::tracing::mistral_helper::MistralTracingHelper;
use anyhow::Result;
use app::app::function::{FunctionApp, FunctionAppImpl, UseFunctionApp};
use async_stream::stream;
use futures::future;
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole as TextMessageRole;
use jobworkerp_runner::jobworkerp::runner::llm::{
    llm_runner_settings::LocalRunnerSettings, LlmChatArgs, LlmChatResult,
};
use mistralrs::{RequestBuilder, Tool};
use net_utils::trace::attr::{OtelSpanAttributes, OtelSpanBuilder, OtelSpanType};
use net_utils::trace::impls::GenericOtelClient;
use net_utils::trace::otel_span::GenAIOtelClient;
use opentelemetry::Context;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use tokio::time::{timeout, Duration};

#[derive(Clone)]
pub struct MistralRSService {
    pub core_service: Arc<MistralLlmServiceImpl>, // Direct use of concrete type
    pub function_app: Arc<FunctionAppImpl>,       // Tool execution functionality (required)
    pub otel_client: Option<Arc<GenericOtelClient>>, // トレーシング
    pub config: ToolCallingConfig,                // Tool calling設定
}

impl MistralRSService {
    /// Constructor (Composition pattern)
    pub async fn new_with_function_app(
        settings: LocalRunnerSettings,
        function_app: Arc<FunctionAppImpl>,
    ) -> Result<Self> {
        let core_service = Arc::new(MistralLlmServiceImpl::new(&settings).await?);

        Ok(Self {
            core_service,
            function_app,
            otel_client: Some(Arc::new(GenericOtelClient::new(
                "mistralrs.tool_calling_service",
            ))),
            config: ToolCallingConfig::default(),
        })
    }

    /// Main entry point for chat requests with tool calling support
    pub async fn request_chat(
        &self,
        args: LlmChatArgs,
        cx: Context,
        metadata: HashMap<String, String>,
    ) -> Result<LlmChatResult> {
        let span_name = "mistral_chat_request";
        let mut main_span_builder = OtelSpanBuilder::new(span_name)
            .span_type(OtelSpanType::Span)
            .level("INFO");

        // Add metadata information
        let mut span_metadata = std::collections::HashMap::new();
        span_metadata.insert(
            "mistral.request.messages.count".to_string(),
            serde_json::Value::Number(serde_json::Number::from(args.messages.len())),
        );
        span_metadata.insert(
            "mistral.request.has_function_calling".to_string(),
            serde_json::Value::Bool(
                args.function_options
                    .as_ref()
                    .is_some_and(|opts| opts.use_function_calling),
            ),
        );

        // Extract important attributes from metadata
        if let Some(job_id) = metadata.get("job_id") {
            main_span_builder = main_span_builder.session_id(job_id.clone());
        }
        if let Some(user_id) = metadata.get("user_id") {
            main_span_builder = main_span_builder.user_id(user_id.clone());
        }

        let main_attributes = main_span_builder.metadata(span_metadata).build();

        // Pre-process necessary data (avoid self borrowing errors)
        let messages = self.convert_proto_messages(&args).map_err(|e| {
            JobWorkerError::OtherError(format!("Convert proto messages failed: {e}"))
        })?;

        let tools = if let Some(function_opts) = &args.function_options {
            tracing::debug!(
                "Function options found: use_function_calling={}",
                function_opts.use_function_calling
            );
            if function_opts.use_function_calling {
                tracing::debug!(
                    "Creating tools from function options: function_set_name={:?}",
                    function_opts.function_set_name
                );
                let created_tools = self
                    .create_tools_from_options(function_opts)
                    .await
                    .map_err(|e| JobWorkerError::OtherError(format!("Create tools failed: {e}")))?;
                tracing::debug!("Created {} tools successfully", created_tools.len());
                Arc::new(created_tools)
            } else {
                tracing::info!("Function calling disabled in options");
                Arc::new(vec![])
            }
        } else {
            tracing::debug!("No function options provided");
            Arc::new(vec![])
        };

        // Create OpenTelemetry context clone in advance
        let cx_clone = cx.clone();
        let self_arc = Arc::new(self.clone());
        let self_arc_clone = Arc::clone(&self_arc);
        let main_action = async move {
            // Start recursive tool calling processing (wrapped in Arc)
            let final_response = self_arc
                .request_chat_internal_with_tracing(messages, tools, Some(cx), Arc::new(metadata))
                .await
                .map_err(|e| JobWorkerError::OtherError(format!("Internal chat failed: {e}")))?;

            // Convert final result to protobuf format
            Ok(self_arc_clone.convert_mistral_response_to_final_result(&final_response))
        };

        // Execute with OpenTelemetry span (using wrapper struct)
        let (result, _) = self
            .execute_with_span(span_name, main_attributes, Some(cx_clone), main_action)
            .await?;
        Ok(result)
    }

    /// ストリーミング版のチャット処理 - Tool calling戦略1採用
    pub async fn request_stream_chat(
        &self,
        args: LlmChatArgs,
    ) -> Result<futures::stream::BoxStream<'static, LlmChatResult>> {
        use futures::stream::{self, StreamExt};

        // Check if tool calling is needed
        let has_tools = args
            .function_options
            .as_ref()
            .is_some_and(|opts| opts.use_function_calling);

        if !has_tools {
            // Tool callingなし - 直接ストリーミング
            return Arc::new(self.clone())
                .request_direct_stream_chat(args)
                .await;
        }

        // Tool callingあり - 戦略1: Non-streaming tool calling + 最終結果streaming
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

    /// Tool callingなしの直接ストリーミング処理
    async fn request_direct_stream_chat(
        self: Arc<Self>,
        args: LlmChatArgs,
    ) -> Result<futures::stream::BoxStream<'static, LlmChatResult>> {
        use async_stream::stream;
        use futures::stream::StreamExt;

        // プロトコルメッセージ変換
        let messages = self.convert_proto_messages(&args)?;
        let request_builder = self.build_request_from_messages(&messages, &[]).await?;

        // Use MistralCoreService's stream_chat
        let mistral_stream: futures::stream::BoxStream<'static, mistralrs::Response> =
            self.core_service.stream_chat(request_builder).await?;

        // MistralRS Response -> LlmChatResult変換ストリーム
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

    /// Recursive tool calling processing (functional approach)
    async fn request_chat_internal_with_tracing(
        self: Arc<Self>,
        messages: Vec<MistralRSMessage>, // イミュータブル管理
        tools: Arc<Vec<Tool>>,
        parent_context: Option<Context>,
        metadata: Arc<HashMap<String, String>>,
    ) -> Result<mistralrs::ChatCompletionResponse> {
        self.request_chat_internal_with_iteration_count(
            messages,
            tools,
            parent_context,
            metadata,
            0,
        )
        .await
    }

    /// Internal implementation with iteration count control
    async fn request_chat_internal_with_iteration_count(
        self: Arc<Self>,
        mut messages: Vec<MistralRSMessage>,
        tools: Arc<Vec<Tool>>,
        parent_context: Option<Context>,
        metadata: Arc<HashMap<String, String>>,
        iteration_count: usize,
    ) -> Result<mistralrs::ChatCompletionResponse> {
        // 最大反復チェック
        if iteration_count >= self.config.max_iterations {
            tracing::error!(
                "Maximum tool calling iterations ({}) exceeded",
                self.config.max_iterations
            );
            return Err(anyhow::Error::from(
                ToolExecutionError::MaxIterationsExceeded {
                    max_iterations: self.config.max_iterations,
                },
            ));
        }

        tracing::info!(
            "Tool calling iteration {}/{}",
            iteration_count + 1,
            self.config.max_iterations
        );

        // Debug: Log conversation history
        tracing::debug!("Building request with {} messages:", messages.len());
        for (i, msg) in messages.iter().enumerate() {
            tracing::debug!(
                "Message {}: {:?} - '{}' (tool_call_id: {:?}, tool_calls: {})",
                i,
                msg.role,
                if msg.content.len() > 100 {
                    &msg.content[..100]
                } else {
                    &msg.content
                },
                msg.tool_call_id,
                msg.tool_calls.as_ref().map_or(0, |tc| tc.len())
            );
        }

        let iteration_attributes = self.create_tool_calling_iteration_attributes(
            iteration_count,
            messages.len(),
            tools.len(),
            &metadata,
        );

        // Execute API request once and cache result
        let request_builder = self.build_request_from_messages(&messages, &tools).await?;
        let response = self.core_service.request_chat(request_builder).await?;

        // トレーシング用のwrapper作成
        let serializable_response = SerializableChatResponse::from(&response);
        let iteration_action =
            async move { Ok::<SerializableChatResponse, JobWorkerError>(serializable_response) };

        let (_serializable_response, current_context) = self
            .execute_with_span(
                &format!("mistral_tool_calling_iteration_{}", iteration_count + 1),
                iteration_attributes,
                parent_context.clone(),
                iteration_action,
            )
            .await?;

        // MistralRS APIから直接tool calls抽出
        let tool_calls = self.extract_tool_calls_from_response(&response)?;

        // debug output
        if let Some(first_choice) = response.choices.first() {
            if let Some(content) = &first_choice.message.content {
                tracing::debug!("Response content: {}", content);
            }
            tracing::debug!("Response finish_reason: {}", first_choice.finish_reason);
        }

        if tool_calls.is_empty() {
            tracing::debug!("No tool calls found, returning final response");
            Ok(response)
        } else {
            tracing::debug!("Processing {} tool calls", tool_calls.len());
            for (i, call) in tool_calls.iter().enumerate() {
                tracing::debug!(
                    "Tool call {}: {} with args: {}",
                    i,
                    call.function_name,
                    call.arguments
                );
            }

            // Parallel tool execution
            let tool_results = if self.config.parallel_execution {
                self.execute_tool_calls_parallel_with_tracing(
                    &tool_calls,
                    metadata.clone(),
                    Some(current_context.clone()),
                )
                .await?
            } else {
                self.execute_tool_calls_sequential_with_tracing(
                    &tool_calls,
                    metadata.clone(),
                    Some(current_context.clone()),
                )
                .await?
            };

            // Fix 2: Add tool result messages while preserving order (extend)
            messages.extend(tool_results);

            // Recursive call
            Box::pin(self.request_chat_internal_with_iteration_count(
                messages, // Pass updated messages
                tools,
                parent_context, // Inherit parent context
                metadata,
                iteration_count + 1, // Increment iteration count
            ))
            .await
        }
    }

    /// Parallel tool execution (spawn + join pattern)
    async fn execute_tool_calls_parallel_with_tracing(
        &self,
        tool_calls: &[MistralRSToolCall],
        metadata: Arc<HashMap<String, String>>,
        parent_context: Option<Context>,
    ) -> Result<Vec<MistralRSMessage>> {
        if tool_calls.is_empty() {
            return Ok(vec![]);
        }

        let span_name = "mistral_parallel_tool_execution";
        let span_builder = OtelSpanBuilder::new(span_name)
            .span_type(OtelSpanType::Span)
            .level("INFO");

        let mut span_metadata = std::collections::HashMap::new();
        span_metadata.insert(
            "tool.execution.mode".to_string(),
            serde_json::Value::String("parallel".to_string()),
        );
        span_metadata.insert(
            "tool.calls.count".to_string(),
            serde_json::Value::Number(serde_json::Number::from(tool_calls.len())),
        );

        let attributes = span_builder.metadata(span_metadata).build();

        // Clone parent_context to prepare for move semantics
        let parent_context_clone = parent_context.clone();
        let tool_calls_vec = tool_calls.to_vec(); // 借用回避のためベクタークローン
                                                  // Clone necessary fields of self in advance
        let function_app = self.function_app.clone();
        let config = self.config.clone();
        let otel_client = self.otel_client.clone();
        let parallel_action = async move {
            // Execute all tool calls in parallel
            let handles: Vec<_> = tool_calls_vec
                .iter()
                .enumerate()
                .map(|(idx, call)| {
                    let function_app = function_app.clone();
                    let metadata = metadata.clone();
                    let call = call.clone();
                    let config = config.clone();
                    let otel_client = otel_client.clone();
                    let parent_ctx = parent_context_clone.clone();

                    tokio::spawn(async move {
                        Self::execute_single_tool_call_with_enhanced_tracing(
                            call,
                            function_app,
                            metadata,
                            config,
                            otel_client,
                            parent_ctx,
                            idx,
                        )
                        .await
                    })
                })
                .collect();

            // Wait for all tasks to complete
            let results = future::join_all(handles).await;

            // Combine results while preserving order (using correct tool_call_id)
            let mut messages = Vec::new();
            for (i, result) in results.into_iter().enumerate() {
                let tool_call_id = tool_calls_vec
                    .get(i)
                    .map(|call| call.id.clone())
                    .unwrap_or_else(|| format!("unknown_{i}"));

                match result {
                    Ok(message) => {
                        tracing::debug!("Tool call {i} succeeded");
                        messages.push(message);
                    }
                    Err(join_error) => {
                        tracing::error!("Tool call {i} task failed: {join_error}");
                        messages.push(MistralRSMessage {
                            role: TextMessageRole::Tool,
                            content: format!("Task execution failed: {join_error}"),
                            tool_call_id: Some(tool_call_id),
                            tool_calls: None,
                        });
                    }
                }
            }

            Ok::<Vec<MistralRSMessage>, JobWorkerError>(messages)
        };

        let (result, _) = self
            .execute_with_span(span_name, attributes, parent_context, parallel_action)
            .await?;
        Ok(result)
    }

    /// Sequential tool execution
    async fn execute_tool_calls_sequential_with_tracing(
        &self,
        tool_calls: &[MistralRSToolCall],
        metadata: Arc<HashMap<String, String>>,
        parent_context: Option<Context>,
    ) -> Result<Vec<MistralRSMessage>> {
        if tool_calls.is_empty() {
            return Ok(vec![]);
        }

        let span_name = "mistral_sequential_tool_execution";
        let span_builder = OtelSpanBuilder::new(span_name)
            .span_type(OtelSpanType::Span)
            .level("INFO");

        let mut span_metadata = std::collections::HashMap::new();
        span_metadata.insert(
            "tool.execution.mode".to_string(),
            serde_json::Value::String("sequential".to_string()),
        );
        span_metadata.insert(
            "tool.calls.count".to_string(),
            serde_json::Value::Number(serde_json::Number::from(tool_calls.len())),
        );

        let attributes = span_builder.metadata(span_metadata).build();

        // Clone parent_context to prepare for move semantics
        let parent_context_clone = parent_context.clone();
        let tool_calls_vec = tool_calls.to_vec(); // 借用回避のためベクタークローン
                                                  // Clone necessary fields of self in advance
        let function_app = self.function_app.clone();
        let config = self.config.clone();
        let otel_client = self.otel_client.clone();
        let sequential_action = async move {
            let mut messages = Vec::new();

            for (idx, call) in tool_calls_vec.iter().enumerate() {
                let res = Self::execute_single_tool_call_with_enhanced_tracing(
                    call.clone(),
                    function_app.clone(),
                    metadata.clone(),
                    config.clone(),
                    otel_client.clone(),
                    parent_context_clone.clone(),
                    idx,
                )
                .await;
                messages.push(res);
            }

            Ok::<Vec<MistralRSMessage>, JobWorkerError>(messages)
        };

        let (result, _) = self
            .execute_with_span(span_name, attributes, parent_context, sequential_action)
            .await?;
        Ok(result)
    }

    /// Individual tool execution with tracing
    async fn execute_single_tool_call_with_enhanced_tracing(
        call: MistralRSToolCall,
        function_app: Arc<FunctionAppImpl>,
        metadata: Arc<HashMap<String, String>>,
        config: ToolCallingConfig,
        otel_client: Option<Arc<GenericOtelClient>>,
        parent_context: Option<Context>,
        call_index: usize,
    ) -> MistralRSMessage {
        // OpenTelemetryトレーシング統合
        if let Some(client) = &otel_client {
            let mut span_builder =
                OtelSpanBuilder::new(format!("tool_execution_{}", call.function_name))
                    .span_type(OtelSpanType::Span)
                    .level("INFO");

            let mut span_metadata = std::collections::HashMap::new();
            span_metadata.insert(
                "tool.call.id".to_string(),
                serde_json::Value::String(call.id.clone()),
            );
            span_metadata.insert(
                "tool.function.name".to_string(),
                serde_json::Value::String(call.function_name.clone()),
            );
            span_metadata.insert(
                "tool.arguments.length".to_string(),
                serde_json::Value::Number(serde_json::Number::from(call.arguments.len())),
            );
            span_metadata.insert(
                "tool.call.index".to_string(),
                serde_json::Value::Number(serde_json::Number::from(call_index)),
            );

            // Extract important attributes from metadata
            if let Some(job_id) = metadata.get("job_id") {
                span_builder = span_builder.session_id(job_id.clone());
            }

            let attributes = span_builder.metadata(span_metadata).build();

            let parent_ctx = parent_context.unwrap_or_else(opentelemetry::Context::current);
            let _span_name = format!("tool_execution_{}", call.function_name);

            let tool_action = async move {
                let result =
                    Self::execute_tool_call_core(call, function_app, metadata, config).await;
                Ok::<MistralRSMessage, JobWorkerError>(result)
            };

            // Execute tool within span
            match client
                .with_span_result(attributes, Some(parent_ctx), tool_action)
                .await
            {
                Ok(result) => result,
                Err(e) => {
                    tracing::error!("Tool execution failed with error: {}", e);
                    MistralRSMessage {
                        role: TextMessageRole::Tool,
                        content: format!("Tool execution failed: {e}"),
                        tool_call_id: Some(call_index.to_string()),
                        tool_calls: None,
                    }
                }
            }
        } else {
            // トレーシングなしの従来実行
            Self::execute_tool_call_core(call, function_app, metadata, config).await
        }
    }

    /// Core tool execution logic (tracing separated)
    async fn execute_tool_call_core(
        call: MistralRSToolCall,
        function_app: Arc<FunctionAppImpl>,
        metadata: Arc<HashMap<String, String>>,
        config: ToolCallingConfig,
    ) -> MistralRSMessage {
        // 引数パース
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

    /// Create span attributes for tracing
    fn create_tool_calling_iteration_attributes(
        &self,
        iteration_count: usize,
        messages_count: usize,
        tools_count: usize,
        metadata: &HashMap<String, String>,
    ) -> OtelSpanAttributes {
        let mut span_builder = OtelSpanBuilder::new(format!(
            "mistral_tool_calling_iteration_{}",
            iteration_count + 1
        ))
        .span_type(OtelSpanType::Span)
        .level("INFO");

        let mut span_metadata = std::collections::HashMap::new();
        span_metadata.insert(
            "mistral.tool_calling.iteration".to_string(),
            serde_json::Value::Number(serde_json::Number::from(iteration_count)),
        );
        span_metadata.insert(
            "mistral.messages.count".to_string(),
            serde_json::Value::Number(serde_json::Number::from(messages_count)),
        );
        span_metadata.insert(
            "mistral.tools.count".to_string(),
            serde_json::Value::Number(serde_json::Number::from(tools_count)),
        );

        // Extract important attributes from metadata
        if let Some(job_id) = metadata.get("job_id") {
            span_builder = span_builder.session_id(job_id.clone());
        }
        if let Some(user_id) = metadata.get("user_id") {
            span_builder = span_builder.user_id(user_id.clone());
        }

        span_builder.metadata(span_metadata).build()
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

        // MistralRSMessage → MistralRS RequestBuilder変換
        for msg in messages {
            match msg.role {
                TextMessageRole::Tool => {
                    // Add tool message as result
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

        // Tools追加
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

impl MistralTracingService for MistralRSService {
    fn get_otel_client(&self) -> Option<&Arc<GenericOtelClient>> {
        self.otel_client.as_ref()
    }

    fn execute_with_tracing<F, T>(
        &self,
        action: F,
        context: Option<Context>,
    ) -> impl Future<Output = Result<T>> + Send
    where
        F: Future<Output = Result<T, anyhow::Error>> + Send,
        T: Send,
    {
        // Simple implementation: OpenTelemetry tracing handled by execute_with_span
        let _ = context; // Context is used in other methods
        action
    }
}

// GenericLLMTracingHelperトレイトの実装
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

// MistralTracingHelperトレイトの実装
impl MistralTracingHelper for MistralRSService {}

// LLMRequestConverterトレイトの実装
impl crate::llm::mistral::args::LLMRequestConverter for MistralRSService {}
