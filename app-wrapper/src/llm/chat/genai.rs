use super::super::generic_tracing_helper::{
    ChatResponse, GenericLLMTracingHelper, LLMMessage, ModelOptions as GenericModelOptions,
    ToolInfo as GenericToolInfo, UsageData,
};
use super::conversion::ToolConverter;
use crate::llm::ThinkTagHelper;
use crate::llm::tracing::genai_helper::GenaiTracingHelper;
use anyhow::Result;
use app::app::function::function_set::{FunctionSetApp, FunctionSetAppImpl};
use app::app::function::{FunctionApp, FunctionAppImpl};
use command_utils::trace::impls::GenericOtelClient;
use futures::StreamExt;
use futures::stream::BoxStream;
use genai::chat::{
    ChatMessage, ChatOptions, ChatRequest, ChatStreamEvent, MessageContent as GenaiMessageContent,
    Tool, ToolResponse,
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
    fn options(&self, args: &LlmChatArgs) -> Option<ChatOptions> {
        args.options.map(|opt| {
            // XXX
            ChatOptions {
                temperature: opt.temperature.map(|v| v as f64),
                max_tokens: opt.max_tokens.map(|v| v as u32),
                top_p: opt.top_p.map(|v| v as f64),
                normalize_reasoning_content: opt.extract_reasoning_content,
                ..Default::default()
            }
        })
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

        let options = self.options(&args);
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
                })
            }
            ChatInternalResult::PendingTools { tool_calls } => {
                // Auto-select mode: intercept pseudo tool call and execute 2nd request_chat
                if is_auto_select
                    && let Some(selected) = tool_calls
                        .iter()
                        .find(|tc| auto_select_names.contains(&tc.fn_name))
                {
                    // Strip the selector prefix to recover the original FunctionSet name
                    let selected_set_name = selected
                        .fn_name
                        .strip_prefix(ToolConverter::SELECTOR_TOOL_PREFIX)
                        .unwrap_or(&selected.fn_name)
                        .to_string();
                    tracing::info!(
                        function_set = %selected_set_name,
                        "Auto-select: LLM selected FunctionSet"
                    );

                    let extra_selectors: Vec<&str> = tool_calls
                        .iter()
                        .filter(|tc| {
                            auto_select_names.contains(&tc.fn_name)
                                && tc.fn_name != selected.fn_name
                        })
                        .map(|tc| tc.fn_name.as_str())
                        .collect();
                    if !extra_selectors.is_empty() {
                        tracing::warn!(
                            selected = %selected.fn_name,
                            ?extra_selectors,
                            "Auto-select: LLM called multiple selector tools, using the first one"
                        );
                    }

                    // INVARIANT: original_args is Some when is_auto_select is true
                    let mut second_args = original_args
                        .expect("original_args must be Some when is_auto_select is true");
                    if let Some(ref mut fo) = second_args.function_options {
                        fo.function_set_name = Some(selected_set_name);
                        fo.auto_select_function_set = Some(false);
                    }

                    return Box::pin(self.request_chat(second_args, cx, (*metadata).clone())).await;
                }

                // Auto-select mode but no selector tool was called — LLM hallucinated
                if is_auto_select {
                    let attempted_names: Vec<&str> =
                        tool_calls.iter().map(|tc| tc.fn_name.as_str()).collect();
                    tracing::warn!(
                        ?attempted_names,
                        "Auto-select: LLM did not call any selector tool, returning error"
                    );
                    return Err(JobWorkerError::OtherError(format!(
                        "Auto-select failed: LLM called {:?} instead of selector tools",
                        attempted_names
                    ))
                    .into());
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
            // TODO: Extract shared selector-tool filtering logic (see docs/issues/genai-ollama-code-dedup.md)
            // Skip selector pseudo-tools — they cannot be executed as real tools
            if ToolConverter::is_selector_tool(&req.fn_name) {
                tracing::warn!(
                    tool = %req.fn_name,
                    "Skipping selector pseudo-tool in tool execution"
                );
                Self::replace_tool_execution_with_result(
                    &mut args.messages,
                    &req.call_id,
                    "Tool selection completed",
                );
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
            Self::replace_tool_execution_with_result(
                &mut args.messages,
                &req.call_id,
                &tool_result,
            );
        }

        // Continue chat with updated messages (tool results added)
        Box::pin(self.request_chat(args, cx, (*metadata).clone())).await
    }

    /// Replace tool execution request message with actual result
    fn replace_tool_execution_with_result(
        messages: &mut [jobworkerp::runner::llm::llm_chat_args::ChatMessage],
        call_id: &str,
        result: &str,
    ) {
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::MessageContent;
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::Content as ProtoContent;

        for msg in messages.iter_mut() {
            if msg.role() != ChatRole::Tool {
                continue;
            }
            if let Some(ref content) = msg.content
                && let Some(ProtoContent::ToolExecutionRequests(reqs)) = &content.content
            {
                // Check if this message contains the target call_id
                if reqs.requests.iter().any(|r| r.call_id == call_id) {
                    // Replace with text result
                    msg.content = Some(MessageContent {
                        content: Some(ProtoContent::Text(result.to_string())),
                    });
                    return;
                }
            }
        }
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
            let tool_calls: Vec<_> = tool_calls
                .into_iter()
                .filter(|tc| {
                    if ToolConverter::is_selector_tool(&tc.fn_name) {
                        tracing::warn!(
                            tool = %tc.fn_name,
                            "Skipping selector pseudo-tool call in auto-calling mode"
                        );
                        false
                    } else {
                        true
                    }
                })
                .collect();

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
        args: LlmChatArgs,
        metadata: HashMap<String, String>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        // Check for tool execution requests first (highest priority, manual mode continuation)
        let metadata_arc = Arc::new(metadata.clone());
        if let Some(tool_exec_requests) = self.extract_tool_execution_requests(&args) {
            return self
                .handle_tool_execution_stream(args, tool_exec_requests, metadata_arc, metadata)
                .await;
        }

        let is_auto_select = args
            .function_options
            .as_ref()
            .is_some_and(|fo| fo.auto_select_function_set.unwrap_or(false));

        // auto_select_function_set: Phase 1 (non-streaming) selects FunctionSet, Phase 2 streams
        if is_auto_select {
            let original_args = args.clone();
            let (tools_vec, auto_select_names) = self.function_list(&args).await?;
            let options = self.options(&args);
            let model = args.model.clone().unwrap_or_else(|| self.model.clone());
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
            let tools = Arc::new(tools_vec);

            let res = Self::request_chat_internal_with_tracing(
                Arc::new(self.clone()),
                model,
                options,
                messages,
                tools,
                None,
                metadata_arc.clone(),
                false, // manual mode to intercept selector tool call
                0,
            )
            .await?;

            match res {
                ChatInternalResult::PendingTools { tool_calls } => {
                    if let Some(selected) = tool_calls
                        .iter()
                        .find(|tc| auto_select_names.contains(&tc.fn_name))
                    {
                        let selected_set_name = selected
                            .fn_name
                            .strip_prefix(ToolConverter::SELECTOR_TOOL_PREFIX)
                            .unwrap_or(&selected.fn_name)
                            .to_string();
                        tracing::info!(
                            function_set = %selected_set_name,
                            "Auto-select (stream): LLM selected FunctionSet"
                        );

                        let extra_selectors: Vec<&str> = tool_calls
                            .iter()
                            .filter(|tc| {
                                auto_select_names.contains(&tc.fn_name)
                                    && tc.fn_name != selected.fn_name
                            })
                            .map(|tc| tc.fn_name.as_str())
                            .collect();
                        if !extra_selectors.is_empty() {
                            tracing::warn!(
                                selected = %selected.fn_name,
                                ?extra_selectors,
                                "Auto-select (stream): LLM called multiple selector tools, using the first one"
                            );
                        }

                        let mut second_args = original_args;
                        if let Some(ref mut fo) = second_args.function_options {
                            fo.function_set_name = Some(selected_set_name);
                            fo.auto_select_function_set = Some(false);
                        }
                        // Use request_chat_stream (not create_chat_stream) so that
                        // is_auto_calling and tool execution handling are fully active in Phase 2
                        return Box::pin(self.request_chat_stream(second_args, metadata)).await;
                    }

                    let attempted_names: Vec<&str> =
                        tool_calls.iter().map(|tc| tc.fn_name.as_str()).collect();
                    tracing::warn!(
                        ?attempted_names,
                        "Auto-select (stream): LLM did not call any selector tool"
                    );
                    return Err(JobWorkerError::OtherError(format!(
                        "Auto-select failed: LLM called {:?} instead of selector tools",
                        attempted_names
                    ))
                    .into());
                }
                ChatInternalResult::Final(_) => {
                    return Err(JobWorkerError::OtherError(
                        "Auto-select failed: LLM did not call any selector tool".to_string(),
                    )
                    .into());
                }
            }
        }

        // Normal streaming flow
        self.create_chat_stream(args, metadata).await
    }

    /// Handle tool execution requests and continue LLM conversation in streaming mode
    async fn handle_tool_execution_stream(
        &self,
        args: LlmChatArgs,
        requests: Vec<ToolExecutionRequest>,
        metadata_arc: Arc<HashMap<String, String>>,
        metadata: HashMap<String, String>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        use jobworkerp_runner::jobworkerp::runner::llm::ToolExecutionResult;

        let self_clone = self.clone();
        let args_clone = args.clone();
        let requests_clone = requests.clone();
        let metadata_arc_clone = metadata_arc.clone();
        let metadata_clone = metadata.clone();

        let metadata_trailer = Trailer {
            metadata: metadata.clone(),
        };

        let stream = async_stream::stream! {
            let mut updated_args = args_clone;
            let mut tool_results_cache: Vec<(String, String, bool)> = Vec::new();

            tracing::debug!("handle_tool_execution_stream: starting Phase 1 with {} tool requests", requests_clone.len());

            // Phase 1: Execute tools, yield results, and cache for later
            for req in &requests_clone {
                // Skip selector pseudo-tools — they cannot be executed as real tools
                if ToolConverter::is_selector_tool(&req.fn_name) {
                    tracing::warn!(
                        tool = %req.fn_name,
                        "Skipping selector pseudo-tool in tool execution stream"
                    );
                    continue;
                }

                let arguments: Option<serde_json::Map<String, serde_json::Value>> =
                    serde_json::from_str(&req.fn_arguments).ok();

                tracing::debug!("Executing tool: {} with args: {:?}", req.fn_name, arguments);

                let result = self_clone
                    .function_app
                    .call_function_for_llm(
                        metadata_arc_clone.clone(),
                        &req.fn_name,
                        arguments,
                        DEFAULT_TIMEOUT_SEC,
                    )
                    .await;

                let (tool_result, success) = match result {
                    Ok(value) => (value.to_string(), true),
                    Err(e) => (format!("Error: {}", e), false),
                };

                tracing::debug!("Tool {} result: {}", req.fn_name, tool_result);

                // Cache result for Phase 2
                tool_results_cache.push((req.call_id.clone(), tool_result.clone(), success));

                // Yield tool execution result
                let llm_result = LlmChatResult {
                    content: None,
                    reasoning_content: None,
                    done: false,
                    usage: None,
                    pending_tool_calls: None,
                    requires_tool_execution: None,
                    tool_execution_results: vec![ToolExecutionResult {
                        call_id: req.call_id.clone(),
                        fn_name: req.fn_name.clone(),
                        result: tool_result.clone(),
                        error: if success { None } else { Some(tool_result.clone()) },
                        success,
                    }],
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
                GenaiChatService::replace_tool_execution_with_result(
                    &mut updated_args.messages,
                    call_id,
                    tool_result,
                );
            }

            // Phase 3: Continue with LLM streaming using updated args
            tracing::debug!("handle_tool_execution_stream: Phase 3 — creating continuation stream");
            match self_clone.create_chat_stream(updated_args, metadata_clone).await {
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
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        let options = self.options(&args);
        let (tools, _auto_select_names) = self.function_list(&args).await?;
        let messages = self.trans_messages(args);
        let chat_req = if tools.is_empty() {
            ChatRequest::new(messages)
        } else {
            ChatRequest::new(messages).with_tools(tools)
        };
        tracing::debug!(
            "Genai LLM(stream): model: {}, Chat request: {:?}, options: {:?}",
            &self.model,
            &chat_req,
            &options
        );
        let res = self
            .client
            .exec_chat_stream(&self.model, chat_req, options.as_ref())
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
                            // Filter out selector pseudo-tools before exposing as pending
                            accumulated_tool_calls.retain(|tc| {
                                if ToolConverter::is_selector_tool(&tc.fn_name) {
                                    tracing::warn!(
                                        tool = %tc.fn_name,
                                        "Filtering out selector pseudo-tool from pending tool calls"
                                    );
                                    false
                                } else {
                                    true
                                }
                            });
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

        Ok(Box::pin(stream))
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
