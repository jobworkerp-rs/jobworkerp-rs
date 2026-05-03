use super::super::generic_tracing_helper::{
    self, ChatResponse, GenericLLMTracingHelper, LLMMessage, ModelOptions as GenericModelOptions,
    StreamingTraceUpdate, ToolInfo as GenericToolInfo, UsageData,
};
use super::conversion::{ToolCallName, ToolConverter};
use crate::llm::ThinkTagHelper;
use crate::llm::tracing::genai_helper::GenaiTracingHelper;
use anyhow::Result;
use app::app::function::function_set::{FunctionSetApp, FunctionSetAppImpl};
use app::app::function::{FunctionApp, FunctionAppImpl};
use command_utils::trace::impls::GenericOtelClient;
use futures::StreamExt;
use futures::stream::BoxStream;
use genai::chat::{
    ChatMessage, ChatOptions, ChatRequest, ChatResponseFormat, ChatStreamEvent, JsonSpec,
    MessageContent as GenaiMessageContent, Tool, ToolResponse,
};
use genai::resolver::{Endpoint, ServiceTargetResolver};
use genai::{Client, ServiceTarget};
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::jobworkerp;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::ToolExecutionRequest;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content;
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::GenaiRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::{
    LlmChatArgs, LlmChatResult, PendingToolCalls, ToolCallRequest, llm_chat_result,
};
use proto::jobworkerp::data::{ResultOutputItem, Trailer, result_output_item};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

impl ToolCallName for genai::chat::ToolCall {
    fn tool_name(&self) -> &str {
        &self.fn_name
    }
}

// Default timeout for tool calls in seconds
const DEFAULT_TIMEOUT_SEC: u32 = 300;

// Maximum number of recursive tool call rounds before aborting
const MAX_TOOL_CALL_DEPTH: u32 = 10;

/// Internal result type for chat operations
enum ChatInternalResult {
    /// Final response from LLM (no more tool calls)
    Final(Box<genai::chat::ChatResponse>),
    /// Pending tool calls that need client approval (manual mode)
    PendingTools {
        tool_calls: Vec<genai::chat::ToolCall>,
    },
}

pub struct GenaiLLMConfig {
    pub model_name: String,
    pub endpoint_url: Option<String>,
}
#[derive(Clone)]
pub struct GenaiChatService {
    pub function_app: Arc<FunctionAppImpl>,
    pub function_set_app: Arc<FunctionSetAppImpl>,
    pub client: Client,
    pub model: String,
    pub system_prompt: Option<String>,
    pub otel_client: Option<Arc<GenericOtelClient>>,
}

impl ThinkTagHelper for GenaiChatService {}

impl GenaiChatService {
    pub async fn new(
        function_app: Arc<FunctionAppImpl>,
        function_set_app: Arc<FunctionSetAppImpl>,
        settings: GenaiRunnerSettings,
    ) -> Result<Self> {
        let model_name = settings.model.clone();
        let endpoint_url = settings.base_url.clone();
        let target_resolver = ServiceTargetResolver::from_resolver_async_fn(
            move |_: ServiceTarget| -> std::pin::Pin<
                Box<
                    dyn std::future::Future<Output = Result<ServiceTarget, genai::resolver::Error>>
                        + Send,
                >,
            > {
                let model_name = model_name.clone();
                let endpoint_url = endpoint_url.clone();
                Box::pin(async move {
                    let client = Client::default();
                    let mut service_target = client
                        .resolve_service_target(&model_name)
                        .await
                        .map_err(|e| {
                            genai::resolver::Error::Custom(format!(
                                "Failed to resolve service target from model={} : {:#?}",
                                &model_name, e
                            ))
                        })?;
                    if let Some(url) = endpoint_url {
                        let mut u = url.parse::<url::Url>().map_err(|e| {
                            genai::resolver::Error::Custom(format!(
                                "Failed to parse endpoint URL={} : {:#?}",
                                &url, e
                            ))
                        })?;
                        // Set the path to "/v1/" to match the GenAI API if it's empty
                        if u.path() == "" || u.path() == "/" {
                            u.set_path("/v1/");
                        } else if !u.path().ends_with('/') {
                            u.set_path(&format!("{}/", u.path()));
                        }
                        service_target.endpoint = Endpoint::from_owned(u.to_string());
                        tracing::debug!(
                            "Genai LLM: resolved service target model: {:?}, endpoint: {:?}",
                            &service_target.model,
                            &service_target.endpoint,
                        );
                    }
                    Ok(service_target)
                })
            },
        );
        // -- Build the new client with this adapter_config
        let client = Client::builder()
            .with_service_target_resolver(target_resolver)
            .build();
        Ok(Self {
            function_app,
            function_set_app,
            client,
            model: settings.model,
            system_prompt: settings.system_prompt,
            otel_client: Some(Arc::new(GenericOtelClient::new("genai.chat_service"))),
        })
    }
    pub(super) fn build_options(args: &LlmChatArgs) -> Option<ChatOptions> {
        let mut chat_opts = ChatOptions::default();
        let mut has_value = false;

        if let Some(opt) = args.options {
            chat_opts.temperature = opt.temperature.map(|v| v as f64);
            chat_opts.max_tokens = opt.max_tokens.map(|v| v as u32);
            chat_opts.top_p = opt.top_p.map(|v| v as f64);
            chat_opts.normalize_reasoning_content = opt.extract_reasoning_content;
            has_value = true;
        }

        if let Some(schema_str) = args.json_schema.as_deref() {
            match serde_json::from_str::<serde_json::Value>(schema_str) {
                Ok(schema) => {
                    chat_opts.response_format = Some(ChatResponseFormat::JsonSpec(JsonSpec::new(
                        crate::llm::GENAI_JSON_SPEC_NAME,
                        schema,
                    )));
                    has_value = true;
                    tracing::debug!("Applied JSON schema to GenAI chat: {}", schema_str);
                }
                Err(e) => {
                    // Mirror Ollama behavior: warn and ignore on parse failure.
                    tracing::warn!("Invalid JSON schema for GenAI chat, ignoring: {}", e);
                }
            }
        }

        if has_value { Some(chat_opts) } else { None }
    }
    fn trans_role(
        &self,
        role: jobworkerp::runner::llm::llm_chat_args::ChatRole,
    ) -> genai::chat::ChatRole {
        match role {
            jobworkerp::runner::llm::llm_chat_args::ChatRole::System => {
                genai::chat::ChatRole::System
            }
            jobworkerp::runner::llm::llm_chat_args::ChatRole::User => genai::chat::ChatRole::User,
            jobworkerp::runner::llm::llm_chat_args::ChatRole::Assistant => {
                genai::chat::ChatRole::Assistant
            }
            jobworkerp::runner::llm::llm_chat_args::ChatRole::Tool => genai::chat::ChatRole::Tool,
            _ => {
                tracing::warn!("Unknown ChatRole {:?}, defaulting to User", role);
                genai::chat::ChatRole::User
            }
        }
    }
    fn trans_messages(&self, args: LlmChatArgs) -> Vec<ChatMessage> {
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::Content as ProtoContent;
        args.messages
            .into_iter()
            .filter_map(|msg| {
                let role = self.trans_role(msg.role());
                let content = match msg.content {
                    Some(content) => match content.content {
                        Some(ProtoContent::Text(text)) => GenaiMessageContent::from_text(text),
                        // TODO pdf
                        Some(ProtoContent::Image(image)) => {
                            let source = match image.source {
                                Some(src) => {
                                    if !src.url.is_empty() {
                                        genai::chat::BinarySource::Url(src.url)
                                    } else if !src.base64.is_empty() {
                                        genai::chat::BinarySource::Base64(Arc::from(src.base64))
                                    } else {
                                        return None;
                                    }
                                }
                                None => return None,
                            };
                            GenaiMessageContent::from_parts(vec![genai::chat::ContentPart::Binary(
                                genai::chat::Binary {
                                    name: None,
                                    content_type: image.content_type,
                                    source,
                                },
                            )])
                        }
                        Some(ProtoContent::ToolCalls(tool_calls)) => {
                            let calls = tool_calls
                                .calls
                                .into_iter()
                                .map(|call| {
                                    let fn_arguments_value =
                                        serde_json::from_str(&call.fn_arguments)
                                            .unwrap_or_else(|_| serde_json::json!({}));
                                    genai::chat::ToolCall {
                                        call_id: call.call_id,
                                        fn_name: call.fn_name,
                                        fn_arguments: fn_arguments_value,
                                        thought_signatures: None,
                                    }
                                })
                                .collect();
                            GenaiMessageContent::from_tool_calls(calls)
                        }
                        Some(ProtoContent::ToolExecutionRequests(_)) => {
                            // Tool execution requests are handled separately in request_chat
                            // They should not be converted to chat messages directly
                            return None;
                        }
                        None => return None,
                    },
                    None => return None,
                };
                Some(ChatMessage {
                    role,
                    content,
                    options: None,
                })
            })
            .collect()
    }
    async fn function_list(
        &self,
        args: &LlmChatArgs,
    ) -> Result<(Vec<Tool>, std::collections::HashSet<String>)> {
        let mut auto_select_names = std::collections::HashSet::new();

        if let Some(function_options) = &args.function_options {
            if function_options.use_function_calling {
                if let Some(set_name) = function_options.function_set_name.as_ref() {
                    // Specific FunctionSet selected
                    match self.function_set_app.find_functions_by_set(set_name).await {
                        Ok(functions) => Ok((
                            ToolConverter::convert_functions_to_genai_tools(functions),
                            auto_select_names,
                        )),
                        Err(e) => {
                            tracing::error!("Error finding functions by set: {}", e);
                            Err(e)
                        }
                    }
                } else if function_options.auto_select_function_set.unwrap_or(false) {
                    // Auto-select mode: inject FunctionSet pseudo-tools
                    match self.function_set_app.find_function_set_all_list(None).await {
                        Ok(function_sets) => {
                            let mut selector_tools = Vec::new();
                            for fs in &function_sets {
                                if let Some(data) = &fs.data {
                                    let tool_summaries =
                                        self.get_tool_summaries_for_set(&data.name).await;
                                    if let Some(tool) =
                                        ToolConverter::convert_function_set_to_selector_tool(
                                            &data.name,
                                            &data.description,
                                            &tool_summaries,
                                        )
                                    {
                                        auto_select_names.insert(tool.name.to_string());
                                        selector_tools.push(tool);
                                    }
                                }
                            }
                            Ok((
                                ToolConverter::convert_function_set_selector_tools_to_genai(
                                    &selector_tools,
                                ),
                                auto_select_names,
                            ))
                        }
                        Err(e) => {
                            tracing::error!("Error finding function sets for auto-select: {}", e);
                            Err(e)
                        }
                    }
                } else {
                    // Default: all functions with runner/worker filter
                    match self
                        .function_app
                        .find_functions(
                            !function_options.use_runners_as_function(),
                            !function_options.use_workers_as_function(),
                        )
                        .await
                    {
                        Ok(functions) => Ok((
                            ToolConverter::convert_functions_to_genai_tools(functions),
                            auto_select_names,
                        )),
                        Err(e) => {
                            tracing::error!("Error finding functions: {}", e);
                            Err(e)
                        }
                    }
                }
            } else {
                Ok((vec![], auto_select_names))
            }
        } else {
            Ok((vec![], auto_select_names))
        }
    }

    async fn get_tool_summaries_for_set(&self, set_name: &str) -> Vec<(String, String)> {
        match self.function_set_app.find_functions_by_set(set_name).await {
            Ok(specs) => ToolConverter::get_tool_summaries(&specs),
            Err(e) => {
                tracing::warn!("Failed to get tool summaries for set '{}': {}", set_name, e);
                vec![]
            }
        }
    }

    pub async fn request_chat(
        &self,
        args: LlmChatArgs,
        cx: opentelemetry::Context,
        metadata: HashMap<String, String>,
    ) -> Result<LlmChatResult> {
        let metadata = Arc::new(metadata);

        // Check for tool execution requests in messages (manual mode)
        if let Some(tool_exec_requests) = self.extract_tool_execution_requests(&args) {
            return self
                .handle_tool_execution(args, tool_exec_requests, cx, metadata)
                .await;
        }

        // Determine if auto-calling is enabled (default: false = manual mode)
        let is_auto_calling = args
            .function_options
            .as_ref()
            .and_then(|fo| fo.is_auto_calling)
            .unwrap_or(false);

        let options = Self::build_options(&args);
        let (tools_vec, auto_select_names) = self.function_list(&args).await?;
        let is_auto_select = !auto_select_names.is_empty();
        let tools = Arc::new(tools_vec);
        let model = args.model.clone().unwrap_or_else(|| self.model.clone());
        let original_args = if is_auto_select {
            Some(args.clone())
        } else {
            None
        };
        let mut messages = self.trans_messages(args);

        if let Some(system_prompt) = self.system_prompt.clone() {
            messages.retain(|m| !matches!(m.role, genai::chat::ChatRole::System));
            messages.insert(
                0,
                ChatMessage {
                    role: genai::chat::ChatRole::System,
                    content: GenaiMessageContent::from_text(system_prompt),
                    options: None,
                },
            );
        }

        let messages = Arc::new(Mutex::new(messages));

        // For auto-select, force manual mode for the 1st call to intercept the tool call
        let effective_auto_calling = !is_auto_select && is_auto_calling;

        let res = Self::request_chat_internal_with_tracing(
            Arc::new(self.clone()),
            model,
            options,
            messages,
            tools,
            Some(cx.clone()),
            metadata.clone(),
            effective_auto_calling,
            0,
        )
        .await?;

        // Handle the result based on whether it contains pending tool calls
        match res {
            ChatInternalResult::Final(response) => {
                let (prompt, think) =
                    Self::divide_think_tag(response.first_text().unwrap_or("").to_string());

                Ok(LlmChatResult {
                    content: Some(llm_chat_result::MessageContent {
                        content: Some(message_content::Content::Text(prompt)),
                    }),
                    reasoning_content: think,
                    done: true,
                    usage: Some(llm_chat_result::Usage {
                        model: response.model_iden.model_name.to_string(),
                        prompt_tokens: response.usage.prompt_tokens.map(|v| v as u32),
                        completion_tokens: response.usage.completion_tokens.map(|v| v as u32),
                        ..Default::default()
                    }),
                    pending_tool_calls: None,
                    requires_tool_execution: None,
                    tool_execution_results: vec![],
                    tool_execution_started: None,
                })
            }
            ChatInternalResult::PendingTools { tool_calls } => {
                if is_auto_select {
                    let result = ToolConverter::evaluate_auto_select(
                        &tool_calls,
                        &auto_select_names,
                        original_args
                            .expect("original_args must be Some when is_auto_select is true"),
                        "",
                    )?;
                    return Box::pin(self.request_chat(
                        result.second_args,
                        cx,
                        (*metadata).clone(),
                    ))
                    .await;
                }

                // Return tool calls for client approval (manual mode)
                let pending_calls: Vec<ToolCallRequest> = tool_calls
                    .iter()
                    .map(|call| ToolCallRequest {
                        call_id: call.call_id.clone(),
                        fn_name: call.fn_name.clone(),
                        fn_arguments: call.fn_arguments.to_string(),
                    })
                    .collect();

                let tool_calls_content: Vec<llm_chat_result::message_content::ToolCall> =
                    pending_calls
                        .iter()
                        .map(|tc| llm_chat_result::message_content::ToolCall {
                            call_id: tc.call_id.clone(),
                            fn_name: tc.fn_name.clone(),
                            fn_arguments: tc.fn_arguments.clone(),
                        })
                        .collect();

                Ok(LlmChatResult {
                    content: Some(llm_chat_result::MessageContent {
                        content: Some(message_content::Content::ToolCalls(
                            llm_chat_result::message_content::ToolCalls {
                                calls: tool_calls_content,
                            },
                        )),
                    }),
                    reasoning_content: None,
                    done: false,
                    usage: None,
                    pending_tool_calls: Some(PendingToolCalls {
                        calls: pending_calls,
                    }),
                    requires_tool_execution: Some(true),
                    tool_execution_results: vec![],
                    tool_execution_started: None,
                })
            }
        }
    }

    /// Extract tool execution requests from messages (for manual mode)
    fn extract_tool_execution_requests(
        &self,
        args: &LlmChatArgs,
    ) -> Option<Vec<ToolExecutionRequest>> {
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::Content as ProtoContent;

        let requests: Vec<ToolExecutionRequest> = args
            .messages
            .iter()
            .filter(|m| m.role() == ChatRole::Tool)
            .filter_map(|m| m.content.as_ref())
            .filter_map(|c| match &c.content {
                Some(ProtoContent::ToolExecutionRequests(reqs)) => Some(reqs.requests.clone()),
                _ => None,
            })
            .flatten()
            .collect();

        if requests.is_empty() {
            None
        } else {
            Some(requests)
        }
    }

    /// Handle tool execution requests from client (manual mode)
    async fn handle_tool_execution(
        &self,
        mut args: LlmChatArgs,
        requests: Vec<ToolExecutionRequest>,
        cx: opentelemetry::Context,
        metadata: Arc<HashMap<String, String>>,
    ) -> Result<LlmChatResult> {
        // Execute each requested tool
        for req in &requests {
            if ToolConverter::skip_selector_tool_execution(req, &mut args.messages) {
                continue;
            }

            let arguments: Option<serde_json::Map<String, serde_json::Value>> =
                serde_json::from_str(&req.fn_arguments).ok();

            // Execute tool via call_function_for_llm:
            //   1. Find RunnerWithSchema by fn_name
            //   2. Encode arguments based on Runner definition
            //   3. Create Worker and enqueue job
            //   4. Get job execution result
            let result = self
                .function_app
                .call_function_for_llm(
                    metadata.clone(),
                    &req.fn_name,
                    arguments,
                    DEFAULT_TIMEOUT_SEC,
                )
                .await;

            let tool_result = match result {
                Ok(value) => value.to_string(),
                Err(e) => format!("Error: {}", e),
            };

            // Replace tool_execution_requests TOOL message with text result
            ToolConverter::replace_tool_execution_with_result(
                &mut args.messages,
                &req.call_id,
                &tool_result,
            );
        }

        // Continue chat with updated messages (tool results added)
        Box::pin(self.request_chat(args, cx, (*metadata).clone())).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn request_chat_internal_with_tracing(
        self: Arc<Self>,
        model: String,
        options: Option<ChatOptions>,
        messages: Arc<Mutex<Vec<ChatMessage>>>,
        tools: Arc<Vec<Tool>>,
        parent_context: Option<opentelemetry::Context>,
        metadata: Arc<HashMap<String, String>>,
        is_auto_calling: bool,
        tool_call_depth: u32,
    ) -> Result<ChatInternalResult> {
        if tool_call_depth >= MAX_TOOL_CALL_DEPTH {
            return Err(anyhow::anyhow!(
                "Maximum tool call depth ({MAX_TOOL_CALL_DEPTH}) exceeded. Aborting to prevent infinite recursion."
            ));
        }
        let current_messages = messages.lock().await.clone();

        // Execute with tracing using generic_tracing_helper approach
        let (res, current_context) = if GenericLLMTracingHelper::get_otel_client(&*self).is_some() {
            let input_messages = Self::convert_messages_to_input_genai(&current_messages);

            let chat_req = ChatRequest::new(current_messages.clone());
            let chat_req = if tools.is_empty() {
                chat_req
            } else {
                chat_req.with_tools((*tools).clone())
            };

            // Execute chat API call
            let client_clone = self.client.clone();
            let model_clone = model.clone();
            let options_clone = options.clone();
            let chat_req_clone = chat_req.clone();
            let chat_api_action = async move {
                client_clone
                    .exec_chat(&model_clone, chat_req_clone, options_clone.as_ref())
                    .await
                    .map_err(|e| JobWorkerError::OtherError(format!("Chat API error: {e}")))
            };

            let model_parameters = {
                let mut params = HashMap::new();
                if let Some(ref opts) = options {
                    if let Some(temp) = opts.temperature {
                        params.insert("temperature".to_string(), serde_json::json!(temp));
                    }
                    if let Some(max_tokens) = opts.max_tokens {
                        params.insert("max_tokens".to_string(), serde_json::json!(max_tokens));
                    }
                    if let Some(top_p) = opts.top_p {
                        params.insert("top_p".to_string(), serde_json::json!(top_p));
                    }
                }
                params
            };

            let span_attributes = self.create_chat_completion_span_attributes(
                &model,
                input_messages,
                Some(&model_parameters),
                &tools,
                &metadata,
            );

            let parent_ctx = parent_context.unwrap_or_else(opentelemetry::Context::current);

            // Execute chat API call with generic tracing
            GenericLLMTracingHelper::with_chat_response_tracing(
                &*self,
                &metadata,
                Some(parent_ctx),
                span_attributes,
                chat_api_action,
            )
            .await?
        } else {
            // No tracing - execute directly
            let chat_req = ChatRequest::new(current_messages);
            let chat_req = if tools.is_empty() {
                chat_req
            } else {
                chat_req.with_tools((*tools).clone())
            };

            let res = self
                .client
                .exec_chat(&model, chat_req, options.as_ref())
                .await
                .map_err(|e| JobWorkerError::OtherError(format!("Chat API error: {e}")))?;
            let context = parent_context.unwrap_or_else(opentelemetry::Context::current);
            (res, context)
        };

        tracing::debug!("GenAI chat response: {:#?}", &res);

        if let Some(tool_calls) = Self::extract_tool_calls(&res)
            && !tool_calls.is_empty()
        {
            tracing::debug!("Tool calls in response: {:#?}", &tool_calls);

            // Check if auto-calling is enabled
            if !is_auto_calling {
                // Manual mode: return tool calls for client approval
                tracing::debug!("Manual mode: returning tool calls for client approval");
                return Ok(ChatInternalResult::PendingTools { tool_calls });
            }

            // Auto mode: process tool calls automatically
            // Filter out selector pseudo-tools to prevent infinite loops —
            // LLM may hallucinate selector tool calls from conversation history
            let tool_calls = ToolConverter::filter_selector_tools(tool_calls);

            // If only selector tools were called, return as final response
            if tool_calls.is_empty() {
                tracing::warn!(
                    "All tool calls were selector pseudo-tools, treating as final response"
                );
                return Ok(ChatInternalResult::Final(Box::new(res)));
            }

            // Add assistant message with filtered tool calls to conversation history
            messages
                .lock()
                .await
                .push(ChatMessage::from(tool_calls.clone()));

            let updated_context = if GenericLLMTracingHelper::get_otel_client(&*self).is_some() {
                self.process_tool_calls_with_tracing(
                    messages.clone(),
                    &tool_calls,
                    Some(current_context),
                    metadata.clone(),
                )
                .await?
            } else {
                self.process_tool_calls_without_tracing(
                    messages.clone(),
                    &tool_calls,
                    metadata.clone(),
                )
                .await?;
                current_context
            };

            // Recursive call with updated context
            tracing::debug!(
                "Recursing into request_chat_internal_with_tracing after tool execution"
            );
            return Box::pin(self.request_chat_internal_with_tracing(
                model,
                options,
                messages,
                tools,
                Some(updated_context),
                metadata,
                is_auto_calling,
                tool_call_depth + 1,
            ))
            .await;
        }

        Ok(ChatInternalResult::Final(Box::new(res)))
    }

    fn extract_tool_calls(
        response: &genai::chat::ChatResponse,
    ) -> Option<Vec<genai::chat::ToolCall>> {
        let tools = response.content.tool_calls();
        if tools.is_empty() {
            None
        } else {
            Some(tools.into_iter().cloned().collect())
        }
    }

    async fn process_tool_calls_with_tracing(
        &self,
        messages: Arc<Mutex<Vec<ChatMessage>>>,
        tool_calls: &[genai::chat::ToolCall],
        parent_context: Option<opentelemetry::Context>,
        metadata: Arc<HashMap<String, String>>,
    ) -> Result<opentelemetry::Context> {
        if parent_context.is_none() && GenericLLMTracingHelper::get_otel_client(self).is_some() {
            tracing::warn!("No parent context provided for tool calls, using current context");
        }
        let mut current_context = parent_context.unwrap_or_else(opentelemetry::Context::current);

        for call in tool_calls.iter() {
            tracing::debug!("Tool call: {:?}", call);
            tracing::debug!("Tool arguments: {:?}", call.fn_arguments);
            tracing::debug!(
                "Tool arguments as object: {:?}",
                call.fn_arguments.as_object()
            );

            // Clone necessary data to avoid lifetime issues
            let function_name = call.fn_name.clone();
            let arguments = call.fn_arguments.clone();
            let function_app = self.function_app.clone();
            let metadata_clone = metadata.clone();

            let tool_action = async move {
                // Handle empty or null arguments by providing an empty object
                let arguments_obj = arguments.as_object().cloned().unwrap_or_else(|| {
                    tracing::debug!("Tool call has null arguments, using empty object");
                    serde_json::Map::new()
                });

                // Execute tool and convert any error to a string result for LLM to handle
                let result = function_app
                    .call_function_for_llm(
                        metadata_clone,
                        &function_name,
                        Some(arguments_obj),
                        DEFAULT_TIMEOUT_SEC,
                    )
                    .await;

                let tool_result = match result {
                    Ok(success_result) => success_result,
                    Err(error) => serde_json::Value::String(format!(
                        "Error executing tool '{function_name}': {error}"
                    )),
                };

                Ok(tool_result) // Always return Ok so processing continues
            };

            // Execute individual tool call as child span and get updated context
            let tool_attributes = self.create_tool_call_span_attributes(
                &call.fn_name,
                call.fn_arguments.clone(),
                &metadata,
            );

            // Execute tool call with response tracing and get both result and updated context
            let (tool_result, updated_context) =
                GenericLLMTracingHelper::with_tool_response_tracing(
                    self,
                    &metadata,
                    current_context,
                    tool_attributes,
                    &call.fn_name,
                    call.fn_arguments.clone(),
                    tool_action,
                )
                .await?;

            tracing::debug!("Tool response: {}", &tool_result);

            // Use ToolResponse with call_id so LLM can match response to request
            let tool_response = ToolResponse::new(&call.call_id, tool_result.to_string());
            messages.lock().await.push(ChatMessage::from(tool_response));

            // Update context for next tool call
            current_context = updated_context;
        }

        Ok(current_context)
    }

    async fn process_tool_calls_without_tracing(
        &self,
        messages: Arc<Mutex<Vec<ChatMessage>>>,
        tool_calls: &[genai::chat::ToolCall],
        metadata: Arc<HashMap<String, String>>,
    ) -> Result<()> {
        for call in tool_calls {
            tracing::debug!("Tool call: {:?}", call);
            tracing::debug!("Tool arguments: {:?}", call.fn_arguments);
            tracing::debug!(
                "Tool arguments as object: {:?}",
                call.fn_arguments.as_object()
            );

            // Handle empty or null arguments by providing an empty object
            let arguments_obj = call.fn_arguments.as_object().cloned().unwrap_or_else(|| {
                tracing::debug!("Tool call has null arguments, using empty object");
                serde_json::Map::new()
            });

            // Execute tool and convert any error to a string result for LLM to handle
            let result = self
                .function_app
                .call_function_for_llm(
                    metadata.clone(),
                    &call.fn_name,
                    Some(arguments_obj),
                    DEFAULT_TIMEOUT_SEC,
                )
                .await;

            let tool_result = match result {
                Ok(success_result) => success_result,
                Err(error) => serde_json::Value::String(format!(
                    "Error executing tool '{}': {error}",
                    call.fn_name
                )),
            };

            tracing::debug!("Tool response: {}", &tool_result);

            // Use ToolResponse with call_id so LLM can match response to request
            let tool_response = ToolResponse::new(&call.call_id, tool_result.to_string());
            messages.lock().await.push(ChatMessage::from(tool_response));
        }

        Ok(())
    }

    pub async fn request_chat_stream(
        &self,
        mut args: LlmChatArgs,
        metadata: HashMap<String, String>,
        parent_context: Option<opentelemetry::Context>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        // Check for tool execution requests first (highest priority, manual mode continuation)
        let metadata_arc = Arc::new(metadata.clone());
        if let Some(tool_exec_requests) = self.extract_tool_execution_requests(&args) {
            return self
                .handle_tool_execution_stream(
                    args,
                    tool_exec_requests,
                    metadata_arc,
                    metadata,
                    parent_context,
                )
                .await;
        }

        let is_auto_select = args
            .function_options
            .as_ref()
            .is_some_and(|fo| fo.auto_select_function_set.unwrap_or(false));

        // auto_select_function_set: Phase 1 (non-streaming) selects FunctionSet, Phase 2 streams
        if is_auto_select {
            let (tools_vec, auto_select_names) = self.function_list(&args).await?;
            if auto_select_names.is_empty() {
                tracing::warn!(
                    "auto_select_function_set is true but no selector tools available, falling back to normal streaming"
                );
                return self
                    .create_chat_stream(args, metadata, parent_context)
                    .await;
            }
            // Insert system_prompt into args before cloning for Phase 2
            if let Some(ref system_prompt) = self.system_prompt {
                use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
                    ChatMessage as ProtoChatMessage, MessageContent, message_content,
                };
                args.messages.retain(|m| m.role() != ChatRole::System);
                args.messages.insert(
                    0,
                    ProtoChatMessage {
                        role: ChatRole::System.into(),
                        content: Some(MessageContent {
                            content: Some(message_content::Content::Text(system_prompt.clone())),
                        }),
                    },
                );
            }
            let original_args = args.clone();

            let options = Self::build_options(&args);
            let model = args.model.clone().unwrap_or_else(|| self.model.clone());
            let messages = Arc::new(Mutex::new(self.trans_messages(args)));
            let tools = Arc::new(tools_vec);

            let res = Self::request_chat_internal_with_tracing(
                Arc::new(self.clone()),
                model,
                options,
                messages,
                tools,
                parent_context.clone(),
                metadata_arc.clone(),
                false, // manual mode to intercept selector tool call
                0,
            )
            .await?;

            match res {
                ChatInternalResult::PendingTools { tool_calls } => {
                    let result = ToolConverter::evaluate_auto_select(
                        &tool_calls,
                        &auto_select_names,
                        original_args,
                        " (stream)",
                    )?;
                    return Box::pin(self.request_chat_stream(
                        result.second_args,
                        metadata,
                        parent_context,
                    ))
                    .await;
                }
                ChatInternalResult::Final(response) => {
                    // LLM responded with text instead of calling a selector tool
                    // (e.g. casual conversation). Return the text as a stream.
                    tracing::debug!(
                        "Auto-select (stream): LLM responded with text instead of tool call, returning as stream"
                    );
                    let (content_text, reasoning) =
                        Self::divide_think_tag(response.first_text().unwrap_or("").to_string());
                    let metadata_trailer = Trailer { metadata };
                    let stream = async_stream::stream! {
                        let llm_result = LlmChatResult {
                            content: Some(llm_chat_result::MessageContent {
                                content: Some(message_content::Content::Text(content_text)),
                            }),
                            reasoning_content: reasoning,
                            done: true,
                            usage: Some(llm_chat_result::Usage {
                                model: response.model_iden.model_name.to_string(),
                                prompt_tokens: response.usage.prompt_tokens.map(|v| v as u32),
                                completion_tokens: response.usage.completion_tokens.map(|v| v as u32),
                                ..Default::default()
                            }),
                            pending_tool_calls: None,
                            requires_tool_execution: None,
                            tool_execution_results: vec![],
                            tool_execution_started: None,
                        };
                        let bytes = prost::Message::encode_to_vec(&llm_result);
                        yield ResultOutputItem {
                            item: Some(result_output_item::Item::Data(bytes)),
                        };
                        yield ResultOutputItem {
                            item: Some(result_output_item::Item::End(metadata_trailer)),
                        };
                    };
                    return Ok(Box::pin(stream));
                }
            }
        }

        // Normal streaming flow
        self.create_chat_stream(args, metadata, parent_context)
            .await
    }

    /// Handle tool execution requests and continue LLM conversation in streaming mode
    async fn handle_tool_execution_stream(
        &self,
        args: LlmChatArgs,
        requests: Vec<ToolExecutionRequest>,
        metadata_arc: Arc<HashMap<String, String>>,
        metadata: HashMap<String, String>,
        parent_context: Option<opentelemetry::Context>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        use jobworkerp_runner::jobworkerp::runner::llm::{
            ToolExecutionResult, ToolExecutionStarted,
        };

        let self_clone = self.clone();
        let args_clone = args.clone();
        let requests_clone = requests.clone();
        let metadata_arc_clone = metadata_arc.clone();
        let metadata_clone = metadata.clone();
        let parent_context_clone = parent_context.clone();

        let metadata_trailer = Trailer {
            metadata: metadata.clone(),
        };

        let stream = async_stream::stream! {
            let mut updated_args = args_clone;
            let mut tool_results_cache: Vec<(String, String, bool)> = Vec::new();

            tracing::debug!("handle_tool_execution_stream: starting Phase 1 with {} tool requests", requests_clone.len());

            // Phase 1: Execute tools with 2-stage split (enqueue → yield started → await → yield result)
            for req in &requests_clone {
                if ToolConverter::skip_selector_tool_execution(req, &mut updated_args.messages) {
                    continue;
                }

                let arguments: Option<serde_json::Map<String, serde_json::Value>> =
                    serde_json::from_str(&req.fn_arguments).ok();

                tracing::debug!("Executing tool: {} with args: {:?}", req.fn_name, arguments);

                // Open a tool-call span (child of the streaming generation span) so wall-clock
                // duration is captured even though the tool runs inside the async stream.
                let arg_value = arguments
                    .as_ref()
                    .map(|m| serde_json::Value::Object(m.clone()))
                    .unwrap_or(serde_json::Value::Null);
                let tool_span = self_clone.open_tool_span(
                    &req.fn_name,
                    arg_value,
                    &metadata_clone,
                    parent_context_clone.clone(),
                );

                // Phase A: Enqueue and get job_id immediately
                let enqueued = self_clone
                    .function_app
                    .enqueue_function_for_llm(
                        metadata_arc_clone.clone(),
                        &req.fn_name,
                        arguments,
                        DEFAULT_TIMEOUT_SEC,
                    )
                    .await;

                let (tool_result, success, job_id_opt) = match enqueued {
                    Ok(enq) => {
                        // Yield ToolExecutionStarted for streaming runners
                        if enq.is_streaming {
                            let started = LlmChatResult {
                                tool_execution_started: Some(ToolExecutionStarted {
                                    call_id: req.call_id.clone(),
                                    fn_name: req.fn_name.clone(),
                                    job_id: enq.job_id.value,
                                    fn_arguments: req.fn_arguments.clone(),
                                }),
                                ..Default::default()
                            };
                            let bytes = prost::Message::encode_to_vec(&started);
                            if !bytes.is_empty() {
                                yield ResultOutputItem {
                                    item: Some(result_output_item::Item::Data(bytes)),
                                };
                            }
                        }

                        let job_id_val = enq.job_id.value;

                        // Phase B: Await result
                        let result = if let Some(val) = enq.result {
                            Ok(val)
                        } else if let Some(handle) = enq.result_handle {
                            self_clone
                                .function_app
                                .await_function_result(handle, &enq.runner_name, enq.using.as_deref())
                                .await
                        } else {
                            Err(anyhow::anyhow!("No result or result_handle available"))
                        };

                        match result {
                            Ok(value) => (value.to_string(), true, Some(job_id_val)),
                            Err(e) => (format!("Error: {}", e), false, Some(job_id_val)),
                        }
                    }
                    Err(e) => {
                        tracing::error!("Failed to enqueue tool {}: {}", req.fn_name, e);
                        (format!("Error: {}", e), false, None)
                    }
                };

                tracing::debug!("Tool {} result: {}", req.fn_name, tool_result);

                generic_tracing_helper::finish_tool_span(tool_span, &tool_result, success);

                // Cache result for Phase 2
                tool_results_cache.push((req.call_id.clone(), tool_result.clone(), success));

                // Yield tool execution result with job_id
                let llm_result = LlmChatResult {
                    tool_execution_results: vec![ToolExecutionResult {
                        call_id: req.call_id.clone(),
                        fn_name: req.fn_name.clone(),
                        result: tool_result.clone(),
                        error: if success { None } else { Some(tool_result.clone()) },
                        success,
                        job_id: job_id_opt,
                    }],
                    ..Default::default()
                };
                let bytes = prost::Message::encode_to_vec(&llm_result);
                if !bytes.is_empty() {
                    yield ResultOutputItem {
                        item: Some(result_output_item::Item::Data(bytes)),
                    };
                }
            }

            // Phase 2: Update args with cached tool results
            tracing::debug!("handle_tool_execution_stream: Phase 2 — updating args with {} tool results", tool_results_cache.len());
            for (call_id, tool_result, _success) in &tool_results_cache {
                ToolConverter::replace_tool_execution_with_result(
                    &mut updated_args.messages,
                    call_id,
                    tool_result,
                );
            }

            // Phase 3: Continue with LLM streaming using updated args
            tracing::debug!("handle_tool_execution_stream: Phase 3 — creating continuation stream");
            match self_clone.create_chat_stream(updated_args, metadata_clone, parent_context.clone()).await {
                Ok(mut continuation_stream) => {
                    tracing::debug!("handle_tool_execution_stream: Phase 3 — continuation stream created, forwarding chunks");
                    let mut chunk_count = 0u64;
                    while let Some(item) = continuation_stream.next().await {
                        chunk_count += 1;
                        yield item;
                    }
                    tracing::debug!("handle_tool_execution_stream: Phase 3 — forwarded {} chunks", chunk_count);
                }
                Err(e) => {
                    tracing::error!("handle_tool_execution_stream: Phase 3 — failed to create continuation stream: {}", e);
                    let llm_result = LlmChatResult {
                        content: Some(llm_chat_result::MessageContent {
                            content: Some(message_content::Content::Text(
                                format!("Continuation error: {}", e),
                            )),
                        }),
                        done: true,
                        ..Default::default()
                    };
                    let bytes = prost::Message::encode_to_vec(&llm_result);
                    if !bytes.is_empty() {
                        yield ResultOutputItem {
                            item: Some(result_output_item::Item::Data(bytes)),
                        };
                    }
                    yield ResultOutputItem {
                        item: Some(result_output_item::Item::End(metadata_trailer.clone())),
                    };
                }
            }
        };

        Ok(Box::pin(stream))
    }

    /// Create the base streaming chat (without tool execution request handling)
    async fn create_chat_stream(
        &self,
        args: LlmChatArgs,
        metadata: HashMap<String, String>,
        parent_context: Option<opentelemetry::Context>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        let options = Self::build_options(&args);
        let (tools, _auto_select_names) = self.function_list(&args).await?;
        // Honour args.model when supplied so the span and the actual request
        // target the same model. Falling back to self.model only when args
        // omits it keeps parity with request_chat_internal_with_tracing.
        let model = args.model.clone().unwrap_or_else(|| self.model.clone());
        let messages = self.trans_messages(args);
        let chat_req = if tools.is_empty() {
            ChatRequest::new(messages)
        } else {
            ChatRequest::new(messages).with_tools(tools.clone())
        };
        tracing::debug!(
            "Genai LLM(stream): model: {}, Chat request: {:?}, options: {:?}",
            &model,
            &chat_req,
            &options
        );

        // Build span attributes BEFORE consuming chat_req into exec_chat_stream so the
        // generation span captures the same input the model receives.
        let span_attributes = if GenericLLMTracingHelper::get_otel_client(self).is_some() {
            Some(
                self.create_chat_span_from_request(&model, &chat_req, &options, &tools, &metadata)
                    .await,
            )
        } else {
            None
        };

        let res = self
            .client
            .exec_chat_stream(&model, chat_req, options.as_ref())
            .await
            .map_err(|e| anyhow::anyhow!("Failed to request generation: {:#?}", e))?;

        // Clone the model name to use inside the closure
        let model_name = res.model_iden.model_name.to_string();

        let metadata_trailer = Trailer {
            metadata: metadata.clone(),
        };

        // Use async_stream to accumulate tool calls during streaming
        let mut base_stream = res.stream;
        let stream = async_stream::stream! {
            let mut accumulated_tool_calls: Vec<genai::chat::ToolCall> = Vec::new();
            let mut stream_error = false;

            while let Some(event_result) = base_stream.next().await {
                match event_result {
                    Ok(event) => match event {
                        ChatStreamEvent::Start => {
                            // Ignore start event
                        }
                        ChatStreamEvent::Chunk(chunk) => {
                            let llm_result = LlmChatResult {
                                content: Some(llm_chat_result::MessageContent {
                                    content: Some(message_content::Content::Text(
                                        chunk.content,
                                    )),
                                }),
                                done: false,
                                ..Default::default()
                            };
                            let bytes = prost::Message::encode_to_vec(&llm_result);
                            if !bytes.is_empty() {
                                yield ResultOutputItem {
                                    item: Some(result_output_item::Item::Data(bytes)),
                                };
                            }
                        }
                        ChatStreamEvent::ReasoningChunk(chunk) => {
                            let llm_result = LlmChatResult {
                                reasoning_content: Some(chunk.content),
                                done: false,
                                ..Default::default()
                            };
                            let bytes = prost::Message::encode_to_vec(&llm_result);
                            if !bytes.is_empty() {
                                yield ResultOutputItem {
                                    item: Some(result_output_item::Item::Data(bytes)),
                                };
                            }
                        }
                        ChatStreamEvent::ThoughtSignatureChunk(chunk) => {
                            let llm_result = LlmChatResult {
                                reasoning_content: Some(chunk.content),
                                done: false,
                                ..Default::default()
                            };
                            let bytes = prost::Message::encode_to_vec(&llm_result);
                            if !bytes.is_empty() {
                                yield ResultOutputItem {
                                    item: Some(result_output_item::Item::Data(bytes)),
                                };
                            }
                        }
                        ChatStreamEvent::ToolCallChunk(tool_chunk) => {
                            // Accumulate tool calls during streaming
                            // GenAI sends complete ToolCall in each chunk (not partial like OpenAI)
                            tracing::debug!(
                                "Accumulating tool call: {:?}",
                                &tool_chunk.tool_call
                            );
                            accumulated_tool_calls.push(tool_chunk.tool_call);
                        }
                        ChatStreamEvent::End(end) => {
                            ToolConverter::retain_non_selector_tools(&mut accumulated_tool_calls);
                            // If we have accumulated tool calls, yield them as pending_tool_calls
                            if !accumulated_tool_calls.is_empty() {
                                let pending_calls: Vec<ToolCallRequest> = accumulated_tool_calls
                                    .iter()
                                    .map(|call| ToolCallRequest {
                                        call_id: call.call_id.clone(),
                                        fn_name: call.fn_name.clone(),
                                        fn_arguments: call.fn_arguments.to_string(),
                                    })
                                    .collect();

                                let tool_calls_content: Vec<llm_chat_result::message_content::ToolCall> =
                                    pending_calls
                                        .iter()
                                        .map(|tc| llm_chat_result::message_content::ToolCall {
                                            call_id: tc.call_id.clone(),
                                            fn_name: tc.fn_name.clone(),
                                            fn_arguments: tc.fn_arguments.clone(),
                                        })
                                        .collect();

                                let llm_result = LlmChatResult {
                                    content: Some(llm_chat_result::MessageContent {
                                        content: Some(message_content::Content::ToolCalls(
                                            llm_chat_result::message_content::ToolCalls {
                                                calls: tool_calls_content,
                                            },
                                        )),
                                    }),
                                    done: false,
                                    pending_tool_calls: Some(PendingToolCalls {
                                        calls: pending_calls,
                                    }),
                                    requires_tool_execution: Some(true),
                                    ..Default::default()
                                };
                                let bytes = prost::Message::encode_to_vec(&llm_result);
                                if !bytes.is_empty() {
                                    yield ResultOutputItem {
                                        item: Some(result_output_item::Item::Data(bytes)),
                                    };
                                }
                            }

                            // End event - send LlmChatResult with done flag set to true
                            let mut llm_result = LlmChatResult {
                                done: true,
                                ..Default::default()
                            };
                            if let Some(usage) = end.captured_usage {
                                llm_result.usage = Some(llm_chat_result::Usage {
                                    model: model_name.clone(),
                                    prompt_tokens: usage.prompt_tokens.map(|v| v as u32),
                                    completion_tokens: usage
                                        .completion_tokens
                                        .map(|v| v as u32),
                                    ..Default::default()
                                });
                            }
                            if let Some(text) =
                                end.captured_content.as_ref().and_then(|c| c.first_text())
                            {
                                llm_result.content = Some(llm_chat_result::MessageContent {
                                    content: Some(message_content::Content::Text(
                                        text.to_string(),
                                    )),
                                });
                            }
                            if let Some(reasoning) = end.captured_reasoning_content {
                                llm_result.reasoning_content = Some(reasoning);
                            }
                            let bytes = prost::Message::encode_to_vec(&llm_result);
                            if !bytes.is_empty() {
                                yield ResultOutputItem {
                                    item: Some(result_output_item::Item::Data(bytes)),
                                };
                            }
                        }
                    },
                    Err(e) => {
                        tracing::error!("Error in chat stream: {:?}", e);
                        stream_error = true;
                        break;
                    }
                }
            }

            // Always send End item at the end of the stream
            if stream_error {
                yield ResultOutputItem {
                    item: Some(result_output_item::Item::End(metadata_trailer.clone())),
                };
            } else {
                yield ResultOutputItem {
                    item: Some(result_output_item::Item::End(metadata_trailer)),
                };
            }
        };

        // Wrap the stream with the generation span. The closure decodes each Data item
        // (a serialized LlmChatResult) so we can detect the `done=true` terminal chunk
        // and record final usage / output text on the span.
        let traced = if let Some(span_attrs) = span_attributes {
            use prost::Message as _;
            let mut accumulated_text = String::with_capacity(4096);
            GenericLLMTracingHelper::with_streaming_response_tracing(
                self,
                parent_context,
                span_attrs,
                stream,
                move |item: &ResultOutputItem| {
                    let bytes = match item.item.as_ref() {
                        Some(result_output_item::Item::Data(b)) => b,
                        _ => return StreamingTraceUpdate::None,
                    };
                    let parsed = match LlmChatResult::decode(bytes.as_slice()) {
                        Ok(p) => p,
                        Err(_) => return StreamingTraceUpdate::None,
                    };
                    // The `done=true` chunk carries `End.captured_content` (the
                    // full text already aggregated by GenAI) — adding it to
                    // `accumulated_text`, which already holds the per-chunk
                    // concatenation, would duplicate the body in the trace.
                    // Skip accumulation on the terminal chunk and prefer the
                    // captured full text only as a fallback when no per-chunk
                    // text was streamed.
                    let chunk_text =
                        parsed
                            .content
                            .as_ref()
                            .and_then(|c| match c.content.as_ref() {
                                Some(message_content::Content::Text(text)) => Some(text.as_str()),
                                _ => None,
                            });
                    if !parsed.done {
                        if let Some(text) = chunk_text {
                            accumulated_text.push_str(text);
                        }
                        return StreamingTraceUpdate::None;
                    }
                    let usage = parsed.usage.as_ref().and_then(|u| {
                        generic_tracing_helper::streaming_usage_map(
                            u.prompt_tokens,
                            u.completion_tokens,
                        )
                    });
                    let final_text = if accumulated_text.is_empty() {
                        chunk_text.unwrap_or("").to_string()
                    } else {
                        std::mem::take(&mut accumulated_text)
                    };
                    let output = serde_json::json!({
                        "role": "assistant",
                        "content": final_text,
                    });
                    StreamingTraceUpdate::Final { output, usage }
                },
            )
        } else {
            Box::pin(stream)
        };

        Ok(traced)
    }
}

// Trait implementations for GenAI-specific types
impl LLMMessage for ChatMessage {
    fn get_role(&self) -> &str {
        match self.role {
            genai::chat::ChatRole::User => "user",
            genai::chat::ChatRole::Assistant => "assistant",
            genai::chat::ChatRole::System => "system",
            genai::chat::ChatRole::Tool => "tool",
            // _ => "unknown",
        }
    }

    fn get_content(&self) -> &str {
        if self.content.len() > 1 {
            tracing::warn!(
                "!! Message content has multiple parts ({}), returning first text part only",
                self.content.len()
            );
        }
        self.content.first_text().unwrap_or("")
    }
}

impl GenericModelOptions for ChatOptions {}

impl GenericToolInfo for Tool {
    fn get_name(&self) -> &str {
        &self.name
    }
}

impl ChatResponse for genai::chat::ChatResponse {
    fn to_json(&self) -> serde_json::Value {
        // Follow MistralRS/Ollama approach - include comprehensive response information
        serde_json::json!({
            "role": "assistant",
            "content": self.first_text().unwrap_or(""),
            "model": self.model_iden.model_name,
            "usage": {
                "prompt_tokens": self.usage.prompt_tokens,
                "completion_tokens": self.usage.completion_tokens,
                "total_tokens": self.usage.total_tokens
            },
            "reasoning_content": self.reasoning_content.as_deref().unwrap_or(""),
            "finish_reason": "stop"
        })
    }
}

impl UsageData for genai::chat::Usage {
    fn to_usage_map(&self) -> HashMap<String, i64> {
        let mut usage = HashMap::new();
        usage.insert(
            "prompt_tokens".to_string(),
            self.prompt_tokens.unwrap_or(0) as i64,
        );
        usage.insert(
            "completion_tokens".to_string(),
            self.completion_tokens.unwrap_or(0) as i64,
        );
        usage.insert(
            "total_tokens".to_string(),
            self.total_tokens.unwrap_or(0) as i64,
        );
        usage
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "prompt_tokens": self.prompt_tokens.unwrap_or(0),
            "completion_tokens": self.completion_tokens.unwrap_or(0),
            "total_tokens": self.total_tokens.unwrap_or(0)
        })
    }
}

// Implement traits for GenaiService
impl GenericLLMTracingHelper for GenaiChatService {
    fn get_otel_client(&self) -> Option<&Arc<GenericOtelClient>> {
        self.otel_client.as_ref()
    }

    fn convert_messages_to_input(&self, messages: &[impl LLMMessage]) -> serde_json::Value {
        let genai_messages: Vec<ChatMessage> = messages
            .iter()
            .map(|m| ChatMessage {
                role: match m.get_role() {
                    "user" => genai::chat::ChatRole::User,
                    "assistant" => genai::chat::ChatRole::Assistant,
                    "system" => genai::chat::ChatRole::System,
                    "tool" => genai::chat::ChatRole::Tool,
                    unknown_role => {
                        tracing::warn!(
                            "Unknown role string '{}', defaulting to User",
                            unknown_role
                        );
                        genai::chat::ChatRole::User
                    }
                },
                content: GenaiMessageContent::from_text(m.get_content().to_string()),
                options: None,
            })
            .collect();
        Self::convert_messages_to_input_genai(&genai_messages)
    }

    fn get_provider_name(&self) -> &str {
        "genai"
    }
}

impl GenaiTracingHelper for GenaiChatService {}

impl crate::llm::tracing::LLMTracingHelper for GenaiChatService {
    fn get_otel_client(&self) -> Option<&Arc<GenericOtelClient>> {
        self.otel_client.as_ref()
    }

    fn get_provider_name(&self) -> &str {
        "genai"
    }

    fn get_default_model(&self) -> String {
        self.model.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::LlmOptions;

    #[test]
    fn build_options_sets_json_spec_response_format() {
        let schema = r#"{"type":"object","properties":{"x":{"type":"integer"}}}"#;
        let args = LlmChatArgs {
            json_schema: Some(schema.to_string()),
            ..Default::default()
        };
        let opts = GenaiChatService::build_options(&args).expect("options must be Some");
        match opts.response_format {
            Some(ChatResponseFormat::JsonSpec(spec)) => {
                assert_eq!(spec.name, "structured_output");
                let expected: serde_json::Value = serde_json::from_str(schema).unwrap();
                assert_eq!(spec.schema, expected);
            }
            other => panic!("expected JsonSpec response_format, got {other:?}"),
        }
    }

    #[test]
    fn build_options_returns_some_when_only_json_schema() {
        let args = LlmChatArgs {
            options: None,
            json_schema: Some(r#"{"type":"object"}"#.to_string()),
            ..Default::default()
        };
        let opts = GenaiChatService::build_options(&args);
        assert!(opts.is_some());
        assert!(opts.unwrap().response_format.is_some());
    }

    #[test]
    fn build_options_ignores_invalid_json_schema() {
        let args = LlmChatArgs {
            options: None,
            json_schema: Some("not a json".to_string()),
            ..Default::default()
        };
        assert!(GenaiChatService::build_options(&args).is_none());
    }

    #[test]
    fn build_options_preserves_existing_fields() {
        let args = LlmChatArgs {
            options: Some(LlmOptions {
                temperature: Some(0.42),
                max_tokens: Some(128),
                top_p: Some(0.9),
                extract_reasoning_content: Some(true),
                ..Default::default()
            }),
            json_schema: Some(r#"{"type":"object"}"#.to_string()),
            ..Default::default()
        };
        let opts = GenaiChatService::build_options(&args).expect("options must be Some");
        assert!((opts.temperature.unwrap() - 0.42_f64).abs() < 1e-6);
        assert_eq!(opts.max_tokens, Some(128));
        assert!((opts.top_p.unwrap() - 0.9_f64).abs() < 1e-6);
        assert_eq!(opts.normalize_reasoning_content, Some(true));
        assert!(matches!(
            opts.response_format,
            Some(ChatResponseFormat::JsonSpec(_))
        ));
    }
}
