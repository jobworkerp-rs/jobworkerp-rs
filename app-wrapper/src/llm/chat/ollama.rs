use super::super::tracing::ollama_helper::OllamaTracingHelper;
use super::conversion::ToolConverter;
use crate::llm::ThinkTagHelper;
use crate::llm::generic_tracing_helper::GenericLLMTracingHelper;
use anyhow::Result;
use app::app::function::function_set::{FunctionSetApp, FunctionSetAppImpl};
use app::app::function::{FunctionApp, FunctionAppImpl};
use command_utils::trace::impls::GenericOtelClient;
use futures::StreamExt;
use futures::stream::BoxStream;
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::ToolExecutionRequest;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content;
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::OllamaRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::{
    self, LlmChatArgs, LlmChatResult, PendingToolCalls, ToolCallRequest,
};
use ollama_rs::generation::chat::ChatMessageResponse;
use ollama_rs::generation::parameters::{FormatType, JsonStructure};
use ollama_rs::generation::tools::{ToolCall as OllamaToolCall, ToolInfo};
use ollama_rs::{
    Ollama,
    generation::chat::{ChatMessage, MessageRole, request::ChatMessageRequest},
    generation::images::Image as OllamaImage,
    models::ModelOptions,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
// use tokio::sync::Mutex;

// Maximum number of recursive tool call rounds before aborting
const MAX_TOOL_CALL_DEPTH: u32 = 10;

#[derive(Debug, Clone)]
pub struct OllamaChatService {
    pub function_app: Arc<FunctionAppImpl>,
    pub function_set_app: Arc<FunctionSetAppImpl>,
    pub ollama: Arc<Ollama>,
    pub model: String,
    pub system_prompt: Option<String>,
    pub otel_client: Option<Arc<GenericOtelClient>>,
}

impl OllamaTracingHelper for OllamaChatService {}

impl crate::llm::tracing::LLMTracingHelper for OllamaChatService {
    fn get_otel_client(&self) -> Option<&Arc<GenericOtelClient>> {
        self.otel_client.as_ref()
    }

    fn get_provider_name(&self) -> &str {
        "ollama"
    }

    fn get_default_model(&self) -> String {
        self.model.clone()
    }
}

impl super::super::generic_tracing_helper::GenericLLMTracingHelper for OllamaChatService {
    fn get_otel_client(&self) -> Option<&Arc<GenericOtelClient>> {
        self.otel_client.as_ref()
    }

    fn convert_messages_to_input(
        &self,
        messages: &[impl super::super::generic_tracing_helper::LLMMessage],
    ) -> serde_json::Value {
        use super::super::tracing::ollama_helper::OllamaTracingHelper;
        let ollama_messages: Vec<ChatMessage> = messages
            .iter()
            .map(|m| ChatMessage {
                role: match m.get_role() {
                    "user" => MessageRole::User,
                    "assistant" => MessageRole::Assistant,
                    "system" => MessageRole::System,
                    "tool" => MessageRole::Tool,
                    unknown_role => {
                        tracing::warn!(
                            "Unknown role string '{}', defaulting to User",
                            unknown_role
                        );
                        MessageRole::User
                    }
                },
                content: m.get_content().to_string(),
                tool_calls: vec![],
                images: Some(vec![]),
                thinking: None,
            })
            .collect();
        Self::convert_messages_to_input_ollama(&ollama_messages)
    }

    fn get_provider_name(&self) -> &str {
        "ollama"
    }
}

impl ThinkTagHelper for OllamaChatService {}

// TODO set from job.timeout
const DEFAULT_TIMEOUT_SEC: u32 = 300; // Default timeout for Ollama chat requests in seconds

/// Internal result type for chat operations
enum ChatInternalResult {
    /// Final response from LLM (no more tool calls)
    Final(Box<ChatMessageResponse>),
    /// Pending tool calls that need client approval (manual mode)
    PendingTools {
        tool_calls: Vec<ollama_rs::generation::tools::ToolCall>,
    },
}

impl OllamaChatService {
    pub fn new(
        function_app: Arc<FunctionAppImpl>,
        function_set_app: Arc<FunctionSetAppImpl>,
        settings: OllamaRunnerSettings,
    ) -> Result<Self> {
        let ollama = Arc::new(Ollama::try_new(
            settings
                .base_url
                .unwrap_or_else(|| "http://localhost:11434".to_string()),
        )?);

        Ok(Self {
            function_app,
            function_set_app,
            ollama,
            model: settings.model,
            system_prompt: settings.system_prompt,
            otel_client: Some(Arc::new(GenericOtelClient::new("ollama.chat_service"))),
        })
    }

    pub fn with_otel_client(mut self, client: Arc<GenericOtelClient>) -> Self {
        self.otel_client = Some(client);
        self
    }

    fn create_chat_options(args: &LlmChatArgs) -> ModelOptions {
        let mut options = ModelOptions::default();
        if let Some(opts) = args.options.as_ref() {
            if let Some(max_tokens) = opts.max_tokens {
                options = options.num_predict(max_tokens);
            } else {
                options = options.num_predict(-2);
            }
            if let Some(temperature) = opts.temperature {
                options = options.temperature(temperature);
            }
            if let Some(top_p) = opts.top_p {
                options = options.top_p(top_p);
            }
            if let Some(repeat_penalty) = opts.repeat_penalty {
                options = options.repeat_penalty(repeat_penalty);
            }
            if let Some(repeat_last_n) = opts.repeat_last_n {
                options = options.repeat_last_n(repeat_last_n);
            }
            if let Some(seed) = opts.seed {
                options = options.seed(seed);
            }
        }
        options
    }

    fn role_to_enum(role: ChatRole) -> MessageRole {
        match role {
            ChatRole::System => MessageRole::System,
            ChatRole::User => MessageRole::User,
            ChatRole::Assistant => MessageRole::Assistant,
            ChatRole::Tool => MessageRole::Tool,
            _ => {
                tracing::warn!("Unknown ChatRole {:?}, defaulting to User", role);
                MessageRole::User
            }
        }
    }

    async fn function_list(
        &self,
        args: &LlmChatArgs,
    ) -> Result<(Vec<ToolInfo>, std::collections::HashSet<String>)> {
        let mut auto_select_names = std::collections::HashSet::new();

        if let Some(function_options) = &args.function_options {
            if function_options.use_function_calling {
                if let Some(set_name) = function_options.function_set_name.as_ref() {
                    tracing::debug!("Use functions by set: {}", set_name);
                    match self.function_set_app.find_functions_by_set(set_name).await {
                        Ok(functions) => {
                            tracing::debug!("Functions found: {}", functions.len());
                            let converted =
                                ToolConverter::convert_functions_to_ollama_tools(functions);
                            Ok((converted, auto_select_names))
                        }
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
                                ToolConverter::convert_function_set_selector_tools_to_ollama(
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
                    tracing::debug!(
                        "Use all functions from {}",
                        if function_options.use_runners_as_function()
                            && function_options.use_workers_as_function()
                        {
                            "all"
                        } else if function_options.use_workers_as_function() {
                            "workers"
                        } else if function_options.use_runners_as_function() {
                            "runners"
                        } else {
                            "none"
                        }
                    );
                    match self
                        .function_app
                        .find_functions(
                            !function_options.use_runners_as_function(),
                            !function_options.use_workers_as_function(),
                        )
                        .await
                    {
                        Ok(functions) => {
                            tracing::debug!("Functions found: {}", functions.len());
                            let converted =
                                ToolConverter::convert_functions_to_ollama_tools(functions);
                            tracing::debug!(
                                "Converted functions: {:?}",
                                converted
                                    .iter()
                                    .map(|f| f.function.name.as_str())
                                    .collect::<Vec<&str>>()
                            );
                            Ok((converted, auto_select_names))
                        }
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

    async fn convert_messages(args: &LlmChatArgs) -> Vec<ChatMessage> {
        let mut messages = Vec::with_capacity(args.messages.len());
        for m in &args.messages {
            let role = Self::role_to_enum(m.role());
            let (content, images, tool_calls) = match &m.content {
                Some(content) => match &content.content {
                    Some(llm::llm_chat_args::message_content::Content::Text(t)) => {
                        (t.clone(), vec![], vec![])
                    }
                    Some(llm::llm_chat_args::message_content::Content::Image(image)) => {
                        match Self::convert_proto_image_to_ollama(image).await {
                            Some(ollama_image) => (String::new(), vec![ollama_image], vec![]),
                            None => {
                                tracing::warn!("Failed to convert image, skipping");
                                (String::new(), vec![], vec![])
                            }
                        }
                    }
                    Some(llm::llm_chat_args::message_content::Content::ToolCalls(tc)) => {
                        let calls: Vec<OllamaToolCall> = tc
                            .calls
                            .iter()
                            .map(|call| OllamaToolCall {
                                function: ollama_rs::generation::tools::ToolCallFunction {
                                    name: call.fn_name.clone(),
                                    arguments: serde_json::from_str(&call.fn_arguments)
                                        .unwrap_or_else(|_| serde_json::json!({})),
                                },
                            })
                            .collect();
                        (String::new(), vec![], calls)
                    }
                    _ => (String::new(), vec![], vec![]),
                },
                None => (String::new(), vec![], vec![]),
            };
            messages.push(ChatMessage {
                role,
                content,
                tool_calls,
                images: if images.is_empty() {
                    None
                } else {
                    Some(images)
                },
                thinking: None,
            });
        }
        messages
    }

    /// Convert proto Image to ollama_rs Image (base64 string)
    async fn convert_proto_image_to_ollama(
        image: &llm::llm_chat_args::message_content::Image,
    ) -> Option<OllamaImage> {
        let source = image.source.as_ref()?;
        if !source.base64.is_empty() {
            return Some(OllamaImage::from_base64(&source.base64));
        }
        if !source.url.is_empty() {
            // Ollama only accepts base64, so fetch the image from URL and encode
            // TODO adhoc reqwest get(set timeout, )
            match reqwest::get(&source.url).await {
                Ok(response) => match response.bytes().await {
                    Ok(bytes) => {
                        use base64::Engine;
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                        Some(OllamaImage::from_base64(b64))
                    }
                    Err(e) => {
                        tracing::error!(
                            "Failed to read image bytes from URL {}: {}",
                            source.url,
                            e
                        );
                        None
                    }
                },
                Err(e) => {
                    tracing::error!("Failed to fetch image from URL {}: {}", source.url, e);
                    None
                }
            }
        } else {
            tracing::warn!("Image has no base64 or URL source");
            None
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

        let options = Self::create_chat_options(&args);
        let model = args.model.clone().unwrap_or_else(|| self.model.clone());
        let mut messages = Self::convert_messages(&args).await;

        if let Some(system_prompt) = self.system_prompt.clone() {
            messages.retain(|m| m.role != MessageRole::System);
            messages.insert(0, ChatMessage::new(MessageRole::System, system_prompt));
        }

        let (tools_vec, auto_select_names) = self.function_list(&args).await?;
        let is_auto_select = !auto_select_names.is_empty();
        let original_args = if is_auto_select {
            Some(args.clone())
        } else {
            None
        };
        let tools = Arc::new(tools_vec);
        let messages = Arc::new(Mutex::new(messages));
        let think = args.options.as_ref().map(|o| o.extract_reasoning_content());

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
            args.json_schema,
            effective_auto_calling,
            think,
            0,
        )
        .await?;

        // Handle the result based on whether it contains pending tool calls
        match res {
            ChatInternalResult::Final(response) => {
                // Use ollama-rs thinking field if available, otherwise fall back to tag parsing
                let (content_text, reasoning) =
                    if let Some(thinking) = response.message.thinking.clone() {
                        // ollama-rs provides thinking separately
                        (response.message.content.clone(), Some(thinking))
                    } else {
                        // Fall back to manual tag parsing for older versions
                        Self::divide_think_tag(response.message.content.clone())
                    };

                Ok(LlmChatResult {
                    content: Some(llm::llm_chat_result::MessageContent {
                        content: Some(message_content::Content::Text(content_text)),
                    }),
                    reasoning_content: reasoning,
                    done: true,
                    usage: None,
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
                        .find(|tc| auto_select_names.contains(&tc.function.name))
                {
                    // Strip the selector prefix to recover the original FunctionSet name
                    let selected_set_name = selected
                        .function
                        .name
                        .strip_prefix(ToolConverter::SELECTOR_TOOL_PREFIX)
                        .unwrap_or(&selected.function.name)
                        .to_string();
                    tracing::info!(
                        function_set = %selected_set_name,
                        "Auto-select: LLM selected FunctionSet"
                    );

                    let extra_selectors: Vec<&str> = tool_calls
                        .iter()
                        .filter(|tc| {
                            auto_select_names.contains(&tc.function.name)
                                && tc.function.name != selected.function.name
                        })
                        .map(|tc| tc.function.name.as_str())
                        .collect();
                    if !extra_selectors.is_empty() {
                        tracing::warn!(
                            selected = %selected.function.name,
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
                    let attempted_names: Vec<&str> = tool_calls
                        .iter()
                        .map(|tc| tc.function.name.as_str())
                        .collect();
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
                        call_id: uuid::Uuid::new_v4().to_string(),
                        fn_name: call.function.name.clone(),
                        fn_arguments: call.function.arguments.to_string(),
                    })
                    .collect();

                let tool_calls_content: Vec<llm::llm_chat_result::message_content::ToolCall> =
                    pending_calls
                        .iter()
                        .map(|tc| llm::llm_chat_result::message_content::ToolCall {
                            call_id: tc.call_id.clone(),
                            fn_name: tc.fn_name.clone(),
                            fn_arguments: tc.fn_arguments.clone(),
                        })
                        .collect();

                Ok(LlmChatResult {
                    content: Some(llm::llm_chat_result::MessageContent {
                        content: Some(message_content::Content::ToolCalls(
                            llm::llm_chat_result::message_content::ToolCalls {
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
        use llm::llm_chat_args::message_content::Content as ProtoContent;

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
        messages: &mut [llm::llm_chat_args::ChatMessage],
        call_id: &str,
        result: &str,
    ) {
        use llm::llm_chat_args::message_content::Content as ProtoContent;

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
                    msg.content = Some(llm::llm_chat_args::MessageContent {
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
        options: ModelOptions,
        messages: Arc<Mutex<Vec<ChatMessage>>>,
        tools: Arc<Vec<ToolInfo>>,
        parent_context: Option<opentelemetry::Context>,
        metadata: Arc<HashMap<String, String>>,
        json_schema: Option<String>,
        is_auto_calling: bool,
        think: Option<bool>,
        tool_call_depth: u32,
    ) -> Result<ChatInternalResult> {
        if tool_call_depth >= MAX_TOOL_CALL_DEPTH {
            return Err(anyhow::anyhow!(
                "Maximum tool call depth ({MAX_TOOL_CALL_DEPTH}) exceeded. Aborting to prevent infinite recursion."
            ));
        }
        let mut req = ChatMessageRequest::new(model.clone(), messages.lock().await.clone());
        req = req.options(options.clone());
        if let Some(t) = think {
            req = req.think(t);
        }

        let mut schema_applied = false;
        if let Some(ref schema_str) = json_schema {
            match serde_json::from_str(schema_str) {
                Ok(schema) => {
                    let format =
                        FormatType::StructuredJson(Box::new(JsonStructure::new_for_schema(schema)));
                    req = req.format(format);
                    schema_applied = true;
                    tracing::debug!("Applied JSON schema format: {}", schema_str);
                }
                Err(e) => {
                    tracing::warn!("Invalid JSON schema, ignoring format: {}", e);
                }
            }
        }

        if tools.is_empty() {
            tracing::debug!("No tools found");
        } else {
            tracing::debug!("Tools found: {:#?}", &tools);
            req = req.tools((*tools).clone());
        }

        let ollama_clone = self.ollama.clone();
        let json_schema_clone = json_schema.clone();
        let model_clone = model.clone();
        let tools_clone = tools.clone();

        // closure of Execute chat API call
        let chat_api_action = async move {
            tracing::debug!(
                "Sending Ollama chat request: model={}, tools_count={}, schema_applied={}",
                model_clone,
                tools_clone.len(),
                schema_applied
            );
            let result = ollama_clone.send_chat_messages(req).await.map_err(|e| {
                // Detailed error analysis for Reqwest errors
                let error_details = if e.to_string().contains("Connection refused") {
                    "Ollama server connection refused. Check if Ollama is running and accessible."
                } else if e.to_string().contains("timeout") {
                    "Ollama request timeout. Consider increasing timeout or checking server load."
                } else if e.to_string().contains("invalid JSON schema") {
                    "Invalid JSON schema format. Check if the schema is compatible with Ollama."
                } else {
                    "Unknown Ollama API error"
                };

                let schema_info = if let Some(ref schema_str) = json_schema_clone {
                    format!("schema_size: {}", schema_str.len())
                } else {
                    "no_schema".to_string()
                };

                let context_info = format!(
                    "model: {}, tools: {}, {}",
                    model_clone,
                    tools_clone.len(),
                    schema_info
                );

                tracing::error!(
                    "Ollama chat API error - {}: {:?} [{}]",
                    error_details,
                    e,
                    context_info
                );
                JobWorkerError::OtherError(format!(
                    "Chat API error: {error_details} ({e:?}) [{context_info}]"
                ))
            });

            match &result {
                Ok(_) => tracing::debug!("Ollama chat request successful"),
                Err(e) => tracing::error!("Ollama chat request failed: {:?}", e),
            }

            result
        };

        // Execute chat API call using generic_tracing_helper approach
        let (res, current_context) = if GenericLLMTracingHelper::get_otel_client(&*self).is_some() {
            let messages_locked = messages.lock().await;
            let input_messages = Self::convert_messages_to_input_ollama(&messages_locked);
            let model_parameters = Self::convert_model_options_to_parameters_ollama(&options);

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
            let result = chat_api_action.await?;
            let context = parent_context.unwrap_or_else(opentelemetry::Context::current);
            (result, context)
        };

        tracing::debug!("Ollama chat response: {:#?}", &res);

        if res.message.tool_calls.is_empty() {
            tracing::debug!("No tool calls in response");
            Ok(ChatInternalResult::Final(Box::new(res)))
        } else {
            tracing::debug!("Tool calls in response: {:#?}", &res.message.tool_calls);

            // Check if auto-calling is enabled
            if !is_auto_calling {
                // Manual mode: return tool calls for client approval
                tracing::debug!("Manual mode: returning tool calls for client approval");
                return Ok(ChatInternalResult::PendingTools {
                    tool_calls: res.message.tool_calls.clone(),
                });
            }

            // Auto mode: process tool calls automatically
            // Filter out selector pseudo-tools to prevent infinite loops —
            // LLM may hallucinate selector tool calls from conversation history
            let tool_calls: Vec<_> = res
                .message
                .tool_calls
                .iter()
                .filter(|tc| {
                    if ToolConverter::is_selector_tool(&tc.function.name) {
                        tracing::warn!(
                            tool = %tc.function.name,
                            "Skipping selector pseudo-tool call in auto-calling mode"
                        );
                        false
                    } else {
                        true
                    }
                })
                .cloned()
                .collect();

            // If only selector tools were called, treat as text response or error
            if tool_calls.is_empty() {
                tracing::warn!(
                    "All tool calls were selector pseudo-tools, treating as final response"
                );
                return Ok(ChatInternalResult::Final(Box::new(res)));
            }

            // Add assistant message with filtered tool calls to conversation history
            let mut filtered_message = res.message.clone();
            filtered_message.tool_calls = tool_calls.clone();
            messages.lock().await.push(filtered_message);

            // Process tool calls and get updated context for each tool call
            let mut updated_context = current_context;
            if GenericLLMTracingHelper::get_otel_client(&*self).is_some() {
                // Process tool calls with tracing using current context as parent
                updated_context = self
                    .process_tool_calls_with_tracing(
                        messages.clone(),
                        &tool_calls,
                        Some(updated_context),
                        metadata.clone(),
                    )
                    .await?;
            } else {
                self.process_tool_calls_without_tracing(
                    messages.clone(),
                    &tool_calls,
                    metadata.clone(),
                )
                .await?;
                // Keep current context unchanged when not tracing
                // updated_context = current_context;
            }

            // Recursive call with updated context from tool execution
            tracing::debug!(
                "Recursing into request_chat_internal_with_tracing after tool execution"
            );
            Box::pin(self.request_chat_internal_with_tracing(
                model,
                options,
                messages,
                tools,
                Some(updated_context),
                metadata,
                None, // json_schema is not used in recursive calls to avoid conflicts
                is_auto_calling,
                think,
                tool_call_depth + 1,
            ))
            .await
        }
    }

    async fn process_tool_calls_with_tracing(
        &self,
        messages: Arc<Mutex<Vec<ChatMessage>>>,
        tool_calls: &[ollama_rs::generation::tools::ToolCall],
        parent_context: Option<opentelemetry::Context>,
        metadata: Arc<HashMap<String, String>>,
    ) -> Result<opentelemetry::Context> {
        if parent_context.is_none() && GenericLLMTracingHelper::get_otel_client(self).is_some() {
            tracing::warn!("No parent context provided for tool calls, using current context");
        }
        let mut current_context = parent_context.unwrap_or_else(opentelemetry::Context::current);

        for call in tool_calls.iter() {
            tracing::debug!("Tool call: {:?}", call.function);
            tracing::debug!("Tool arguments: {:?}", call.function.arguments);
            tracing::debug!(
                "Tool arguments as object: {:?}",
                call.function.arguments.as_object()
            );

            // Clone necessary data to avoid lifetime issues
            let function_name = call.function.name.clone();
            let arguments = call.function.arguments.clone();
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
            let tool_attributes = self.create_tool_call_span_from_call(call, &metadata);

            // Execute tool call with response tracing and get both result and updated context
            let (tool_result, updated_context) = OllamaTracingHelper::with_tool_response_tracing(
                self,
                &metadata,
                current_context,
                tool_attributes,
                call,
                tool_action,
            )
            .await?;

            tracing::debug!("Tool response: {}", &tool_result);
            messages
                .lock()
                .await
                .push(ChatMessage::tool(tool_result.to_string()));

            // Update context for next tool call
            current_context = updated_context;
        }
        Ok(current_context)
    }

    async fn process_tool_calls_without_tracing(
        &self,
        messages: Arc<Mutex<Vec<ChatMessage>>>,
        tool_calls: &[ollama_rs::generation::tools::ToolCall],
        metadata: Arc<HashMap<String, String>>,
    ) -> Result<()> {
        for call in tool_calls {
            tracing::debug!("Tool call: {:?}", call.function);
            tracing::debug!("Tool arguments: {:?}", call.function.arguments);
            tracing::debug!(
                "Tool arguments as object: {:?}",
                call.function.arguments.as_object()
            );

            // Handle empty or null arguments by providing an empty object
            let arguments_obj = call
                .function
                .arguments
                .as_object()
                .cloned()
                .unwrap_or_else(|| {
                    tracing::debug!("Tool call has null arguments, using empty object");
                    serde_json::Map::new()
                });

            // Execute tool and convert any error to a string result for LLM to handle
            let result = self
                .function_app
                .call_function_for_llm(
                    metadata.clone(),
                    call.function.name.as_str(),
                    Some(arguments_obj),
                    DEFAULT_TIMEOUT_SEC,
                )
                .await;

            let tool_result = match result {
                Ok(success_result) => {
                    tracing::debug!("Tool execution succeeded: {}", &success_result);
                    success_result
                }
                Err(error) => {
                    tracing::info!(
                        "Tool execution failed for: {}, error: {}",
                        call.function.name,
                        error
                    );
                    serde_json::Value::String(format!(
                        "Error executing tool '{}': {}",
                        call.function.name, error
                    ))
                }
            };

            tracing::debug!("Tool response: {}", &tool_result);
            messages
                .lock()
                .await
                .push(ChatMessage::tool(tool_result.to_string()));
        }
        Ok(())
    }

    /// Streaming chat with tool call support (manual mode with ToolExecutionRequests)
    ///
    /// Flow:
    /// 1. First request: LLM may return pending_tool_calls
    /// 2. Client sends ToolExecutionRequests to execute tools
    /// 3. Server executes tools, yields results, then continues LLM conversation
    pub async fn request_stream_chat(
        self: Arc<Self>,
        mut args: LlmChatArgs,
        metadata: HashMap<String, String>,
    ) -> Result<BoxStream<'static, LlmChatResult>> {
        // Check for tool execution requests first (highest priority, manual mode continuation)
        let metadata_arc = Arc::new(metadata);
        if let Some(tool_exec_requests) = self.extract_tool_execution_requests(&args) {
            return self
                .handle_tool_execution_stream(args, tool_exec_requests, metadata_arc)
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
                return self.create_streaming_chat(args, metadata_arc).await;
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

            let options = Self::create_chat_options(&args);
            let model = args.model.clone().unwrap_or_else(|| self.model.clone());
            let messages = Arc::new(Mutex::new(Self::convert_messages(&args).await));

            let tools = Arc::new(tools_vec);
            let think = args.options.as_ref().map(|o| o.extract_reasoning_content());
            let res = Self::request_chat_internal_with_tracing(
                self.clone(),
                model,
                options,
                messages,
                tools,
                None,
                metadata_arc.clone(),
                args.json_schema.clone(),
                false, // manual mode to intercept selector tool call
                think,
                0,
            )
            .await?;

            match res {
                ChatInternalResult::PendingTools { tool_calls } => {
                    if let Some(selected) = tool_calls
                        .iter()
                        .find(|tc| auto_select_names.contains(&tc.function.name))
                    {
                        let selected_set_name = selected
                            .function
                            .name
                            .strip_prefix(ToolConverter::SELECTOR_TOOL_PREFIX)
                            .unwrap_or(&selected.function.name)
                            .to_string();
                        tracing::info!(
                            function_set = %selected_set_name,
                            "Auto-select (stream): LLM selected FunctionSet"
                        );

                        let extra_selectors: Vec<&str> = tool_calls
                            .iter()
                            .filter(|tc| {
                                auto_select_names.contains(&tc.function.name)
                                    && tc.function.name != selected.function.name
                            })
                            .map(|tc| tc.function.name.as_str())
                            .collect();
                        if !extra_selectors.is_empty() {
                            tracing::warn!(
                                selected = %selected.function.name,
                                ?extra_selectors,
                                "Auto-select (stream): LLM called multiple selector tools, using the first one"
                            );
                        }

                        let mut second_args = original_args;
                        if let Some(ref mut fo) = second_args.function_options {
                            fo.function_set_name = Some(selected_set_name);
                            fo.auto_select_function_set = Some(false);
                        }
                        // Use request_stream_chat (not create_streaming_chat) so that
                        // is_auto_calling and tool execution handling are fully active in Phase 2
                        return Box::pin(
                            self.request_stream_chat(second_args, (*metadata_arc).clone()),
                        )
                        .await;
                    }

                    let attempted_names: Vec<&str> = tool_calls
                        .iter()
                        .map(|tc| tc.function.name.as_str())
                        .collect();
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

        // Normal streaming flow (first request or no tool execution)
        self.create_streaming_chat(args, metadata_arc).await
    }

    /// Wrapper method for non-Arc callers (backward compatibility)
    pub async fn request_stream_chat_ref(
        &self,
        args: LlmChatArgs,
        metadata: HashMap<String, String>,
    ) -> Result<BoxStream<'static, LlmChatResult>> {
        // Clone self into Arc for internal use
        let self_arc = Arc::new(self.clone());
        self_arc.request_stream_chat(args, metadata).await
    }

    /// Handle tool execution requests and continue LLM conversation in streaming mode
    async fn handle_tool_execution_stream(
        self: Arc<Self>,
        args: LlmChatArgs,
        requests: Vec<ToolExecutionRequest>,
        metadata: Arc<HashMap<String, String>>,
    ) -> Result<BoxStream<'static, LlmChatResult>> {
        use jobworkerp_runner::jobworkerp::runner::llm::ToolExecutionResult;

        let self_clone = self.clone();
        let args_clone = args.clone();
        let requests_clone = requests.clone();
        let metadata_clone = metadata.clone();

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
                    OllamaChatService::replace_tool_execution_with_result(
                        &mut updated_args.messages,
                        &req.call_id,
                        "Tool selection completed",
                    );
                    continue;
                }

                let arguments: Option<serde_json::Map<String, serde_json::Value>> =
                    serde_json::from_str(&req.fn_arguments).ok();

                tracing::debug!("Executing tool: {} with args: {:?}", req.fn_name, arguments);

                let result = self_clone
                    .function_app
                    .call_function_for_llm(
                        metadata_clone.clone(),
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
                yield LlmChatResult {
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
            }

            // Phase 2: Update args with cached tool results
            tracing::debug!("handle_tool_execution_stream: Phase 2 — updating args with {} tool results", tool_results_cache.len());
            for (call_id, tool_result, _success) in &tool_results_cache {
                OllamaChatService::replace_tool_execution_with_result(
                    &mut updated_args.messages,
                    call_id,
                    tool_result,
                );
            }

            // Phase 3: Continue with LLM streaming using updated args
            tracing::debug!("handle_tool_execution_stream: Phase 3 — creating continuation stream");
            match self_clone.clone().create_streaming_chat(updated_args, metadata_clone.clone()).await {
                Ok(mut continuation_stream) => {
                    tracing::debug!("handle_tool_execution_stream: Phase 3 — continuation stream created, forwarding chunks");
                    let mut chunk_count = 0u64;
                    while let Some(chunk) = continuation_stream.next().await {
                        chunk_count += 1;
                        yield chunk;
                    }
                    tracing::debug!("handle_tool_execution_stream: Phase 3 — forwarded {} chunks", chunk_count);
                }
                Err(e) => {
                    tracing::error!("handle_tool_execution_stream: Phase 3 — failed to create continuation stream: {}", e);
                    yield LlmChatResult {
                        content: Some(llm::llm_chat_result::MessageContent {
                            content: Some(message_content::Content::Text(
                                format!("Continuation error: {}", e),
                            )),
                        }),
                        reasoning_content: None,
                        done: true,
                        usage: None,
                        pending_tool_calls: None,
                        requires_tool_execution: None,
                        tool_execution_results: vec![],
                    };
                }
            }
        };

        Ok(Box::pin(stream))
    }

    /// Create the base streaming chat (without tool execution request handling)
    async fn create_streaming_chat(
        self: Arc<Self>,
        args: LlmChatArgs,
        _metadata: Arc<HashMap<String, String>>,
    ) -> Result<BoxStream<'static, LlmChatResult>> {
        let use_function_calling = args
            .function_options
            .as_ref()
            .map(|fo| fo.use_function_calling)
            .unwrap_or(false);

        // Load tools if function calling is enabled
        let tools: Vec<ToolInfo> = if use_function_calling {
            let (tools, _auto_select_names) = self.function_list(&args).await?;
            tools
        } else {
            vec![]
        };

        let options = Self::create_chat_options(&args);
        let model_name = args.model.clone().unwrap_or_else(|| self.model.clone());
        let messages = Self::convert_messages(&args).await;

        let mut req = ChatMessageRequest::new(model_name, messages);
        req = req.options(options);
        if let Some(t) = args.options.as_ref().map(|o| o.extract_reasoning_content()) {
            req = req.think(t);
        }

        if let Some(system_prompt) = self.system_prompt.clone() {
            req = req.template(system_prompt);
        }

        // Add tools to request if available
        if !tools.is_empty() {
            req = req.tools(tools);
        }

        let ollama = self.ollama.clone();

        // Use async_stream for stateful stream processing
        let stream = async_stream::stream! {
            let mut accumulated_tool_calls: Vec<OllamaToolCall> = Vec::new();

            // Create base stream with tools
            let base_stream_result = ollama
                .send_chat_messages_stream(req)
                .await;

            match base_stream_result {
                Ok(mut base_stream) => {
                    while let Some(result) = base_stream.next().await {
                        match result {
                            Ok(chunk) => {
                                // Accumulate tool calls
                                if !chunk.message.tool_calls.is_empty() {
                                    accumulated_tool_calls.extend(chunk.message.tool_calls.clone());
                                }

                                // Yield text content as it arrives
                                if !chunk.message.content.is_empty() {
                                    yield LlmChatResult {
                                        content: Some(llm::llm_chat_result::MessageContent {
                                            content: Some(message_content::Content::Text(
                                                chunk.message.content.clone(),
                                            )),
                                        }),
                                        reasoning_content: None,
                                        done: false,
                                        usage: None,
                                        pending_tool_calls: None,
                                        requires_tool_execution: None,
                                        tool_execution_results: vec![],
                                    };
                                }
                            }
                            Err(_) => {
                                tracing::error!("Error in stream chat");
                                yield LlmChatResult {
                                    content: Some(llm::llm_chat_result::MessageContent {
                                        content: Some(message_content::Content::Text(
                                            "Stream error".to_string(),
                                        )),
                                    }),
                                    reasoning_content: None,
                                    done: true,
                                    usage: None,
                                    pending_tool_calls: None,
                                    requires_tool_execution: None,
                                    tool_execution_results: vec![],
                                };
                                return;
                            }
                        }
                    }

                    // After stream ends, process any accumulated tool calls
                    // Filter out selector pseudo-tools — they are internal to auto-select
                    // and must not be exposed as pending tool calls to clients/workflows
                    accumulated_tool_calls.retain(|tc| {
                        if ToolConverter::is_selector_tool(&tc.function.name) {
                            tracing::warn!(
                                tool = %tc.function.name,
                                "Filtering out selector pseudo-tool from pending tool calls"
                            );
                            false
                        } else {
                            true
                        }
                    });
                    if !accumulated_tool_calls.is_empty() {
                        // Convert to pending tool calls format
                        let pending_calls: Vec<ToolCallRequest> = accumulated_tool_calls
                            .iter()
                            .map(|call| ToolCallRequest {
                                call_id: uuid::Uuid::new_v4().to_string(),
                                fn_name: call.function.name.clone(),
                                fn_arguments: call.function.arguments.to_string(),
                            })
                            .collect();

                        let tool_calls_content: Vec<llm::llm_chat_result::message_content::ToolCall> =
                            pending_calls
                                .iter()
                                .map(|tc| llm::llm_chat_result::message_content::ToolCall {
                                    call_id: tc.call_id.clone(),
                                    fn_name: tc.fn_name.clone(),
                                    fn_arguments: tc.fn_arguments.clone(),
                                })
                                .collect();

                        // Yield tool calls with pending status
                        yield LlmChatResult {
                            content: Some(llm::llm_chat_result::MessageContent {
                                content: Some(message_content::Content::ToolCalls(
                                    llm::llm_chat_result::message_content::ToolCalls {
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
                        };
                    }

                    // Yield final done signal
                    yield LlmChatResult {
                        content: None,
                        reasoning_content: None,
                        done: true,
                        usage: None,
                        pending_tool_calls: None,
                        requires_tool_execution: None,
                        tool_execution_results: vec![],
                    };
                }
                Err(e) => {
                    tracing::error!("Failed to create stream: {}", e);
                    yield LlmChatResult {
                        content: Some(llm::llm_chat_result::MessageContent {
                            content: Some(message_content::Content::Text(
                                format!("Stream creation error: {}", e),
                            )),
                        }),
                        reasoning_content: None,
                        done: true,
                        usage: None,
                        pending_tool_calls: None,
                        requires_tool_execution: None,
                        tool_execution_results: vec![],
                    };
                }
            }
        };

        Ok(Box::pin(stream))
    }
}
