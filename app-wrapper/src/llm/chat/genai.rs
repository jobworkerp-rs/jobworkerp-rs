use super::super::generic_tracing_helper::{
    ChatResponse, GenericLLMTracingHelper, LLMMessage, ModelOptions as GenericModelOptions,
    ToolInfo as GenericToolInfo, UsageData,
};
use super::conversion::ToolConverter;
use crate::llm::tracing::genai_helper::GenaiTracingHelper;
use crate::llm::ThinkTagHelper;
use anyhow::Result;
use app::app::function::{FunctionApp, FunctionAppImpl};
use futures::stream::BoxStream;
use futures::StreamExt;
use genai::chat::{
    ChatMessage, ChatOptions, ChatRequest, ChatStreamEvent, MessageContent as GenaiMessageContent,
    Tool,
};
use genai::resolver::{Endpoint, ServiceTargetResolver};
use genai::{Client, ServiceTarget};
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::jobworkerp;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content;
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::GenaiRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::{llm_chat_result, LlmChatArgs, LlmChatResult};
use net_utils::trace::impls::GenericOtelClient;
use proto::jobworkerp::data::{result_output_item, ResultOutputItem, Trailer};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

// Default timeout for tool calls in seconds
const DEFAULT_TIMEOUT_SEC: u32 = 300;

pub struct GenaiLLMConfig {
    pub model_name: String,
    pub endpoint_url: Option<String>,
}
#[derive(Clone)]
pub struct GenaiChatService {
    pub function_app: Arc<FunctionAppImpl>,
    pub client: Client,
    pub model: String,
    pub system_prompt: Option<String>,
    pub otel_client: Option<Arc<GenericOtelClient>>,
}

impl ThinkTagHelper for GenaiChatService {}

impl GenaiChatService {
    pub async fn new(
        function_app: Arc<FunctionAppImpl>,
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
                        Some(ProtoContent::Text(text)) => GenaiMessageContent::Text(text),
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
                            GenaiMessageContent::Parts(vec![genai::chat::ContentPart::Binary {
                                name: None,
                                content_type: image.content_type,
                                source,
                            }])
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
                                    }
                                })
                                .collect();
                            GenaiMessageContent::ToolCalls(calls)
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
    async fn function_list(&self, args: &LlmChatArgs) -> Result<Vec<Tool>> {
        if let Some(function_options) = &args.function_options {
            if function_options.use_function_calling {
                let list_future =
                    if let Some(set_name) = function_options.function_set_name.as_ref() {
                        self.function_app.find_functions_by_set(set_name)
                    } else {
                        self.function_app.find_functions(
                            !function_options.use_runners_as_function(),
                            !function_options.use_workers_as_function(),
                        )
                    };
                match list_future.await {
                    Ok(functions) => Ok(ToolConverter::convert_functions_to_genai_tools(
                        functions.clone(),
                    )),
                    Err(e) => {
                        tracing::error!("Error finding functions: {}", e);
                        Ok(vec![])
                    }
                }
            } else {
                Ok(vec![])
            }
        } else {
            Ok(vec![])
        }
    }

    pub async fn request_chat(
        &self,
        args: LlmChatArgs,
        cx: opentelemetry::Context,
        metadata: HashMap<String, String>,
    ) -> Result<LlmChatResult> {
        let metadata = Arc::new(metadata);
        let options = self.options(&args);
        let tools = Arc::new(self.function_list(&args).await?);
        let model = args.model.clone().unwrap_or_else(|| self.model.clone());
        let mut messages = self.trans_messages(args);

        // Add system prompt if exists
        if let Some(system_prompt) = self.system_prompt.clone() {
            messages.retain(|m| !matches!(m.role, genai::chat::ChatRole::System));
            messages.insert(
                0,
                ChatMessage {
                    role: genai::chat::ChatRole::System,
                    content: GenaiMessageContent::Text(system_prompt),
                    options: None,
                },
            );
        }

        let messages = Arc::new(Mutex::new(messages));

        // Use internal method with tool call support
        let res = Self::request_chat_internal_with_tracing(
            Arc::new(self.clone()),
            model,
            options,
            messages,
            tools,
            Some(cx.clone()),
            metadata.clone(),
        )
        .await?;

        // Convert response to LlmChatResult
        let (prompt, think) = Self::divide_think_tag(res.first_text().unwrap_or("").to_string());

        let chat_result = LlmChatResult {
            content: Some(llm_chat_result::MessageContent {
                content: Some(message_content::Content::Text(prompt)),
            }),
            reasoning_content: think,
            done: true,
            usage: Some(llm_chat_result::Usage {
                model: res.model_iden.model_name.to_string(),
                prompt_tokens: res.usage.prompt_tokens.map(|v| v as u32),
                completion_tokens: res.usage.completion_tokens.map(|v| v as u32),
                ..Default::default()
            }),
        };

        Ok(chat_result)
    }

    async fn request_chat_internal_with_tracing(
        self: Arc<Self>,
        model: String,
        options: Option<ChatOptions>,
        messages: Arc<Mutex<Vec<ChatMessage>>>,
        tools: Arc<Vec<Tool>>,
        parent_context: Option<opentelemetry::Context>,
        metadata: Arc<HashMap<String, String>>,
    ) -> Result<genai::chat::ChatResponse> {
        let current_messages = messages.lock().await.clone();
        let chat_req = ChatRequest::new(current_messages);
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

        // Execute with tracing
        let (res, current_context) = if let Some(_otel_client) = self.get_otel_client() {
            // Use existing tracing
            let tracing_context = crate::llm::tracing::LLMTracingHelper::start_llm_tracing(
                &*self,
                crate::llm::tracing::LLMApiType::Chat,
                &chat_req,
                &metadata,
                parent_context.clone(),
            )
            .await?;

            let api_result = chat_api_action.await;

            let res = match api_result {
                Ok(response) => {
                    let _ = crate::llm::tracing::LLMTracingHelper::finish_llm_tracing(
                        &*self,
                        tracing_context,
                        &response,
                    )
                    .await;
                    response
                }
                Err(error) => {
                    let _ = crate::llm::tracing::LLMTracingHelper::finish_llm_tracing_with_error(
                        &*self,
                        tracing_context,
                        &error,
                    )
                    .await;
                    return Err(error.into());
                }
            };

            let context = parent_context.unwrap_or_else(opentelemetry::Context::current);
            (res, context)
        } else {
            let res = chat_api_action.await?;
            let context = parent_context.unwrap_or_else(opentelemetry::Context::current);
            (res, context)
        };

        tracing::debug!("GenAI chat response: {:#?}", &res);

        // Check for tool calls in response
        if let Some(tool_calls) = Self::extract_tool_calls(&res) {
            if !tool_calls.is_empty() {
                tracing::debug!("Tool calls in response: {:#?}", &tool_calls);

                // Process tool calls
                let updated_context = if self.get_otel_client().is_some() {
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
                return Box::pin(self.request_chat_internal_with_tracing(
                    model,
                    options,
                    messages,
                    tools,
                    Some(updated_context),
                    metadata,
                ))
                .await;
            }
        }

        Ok(res)
    }

    fn extract_tool_calls(
        response: &genai::chat::ChatResponse,
    ) -> Option<Vec<genai::chat::ToolCall>> {
        // Check if response contains tool calls
        for content in &response.content {
            if let GenaiMessageContent::ToolCalls(calls) = content {
                return Some(calls.clone());
            }
        }
        None
    }

    async fn process_tool_calls_with_tracing(
        &self,
        messages: Arc<Mutex<Vec<ChatMessage>>>,
        tool_calls: &[genai::chat::ToolCall],
        parent_context: Option<opentelemetry::Context>,
        metadata: Arc<HashMap<String, String>>,
    ) -> Result<opentelemetry::Context> {
        let current_context = parent_context.unwrap_or_else(opentelemetry::Context::current);

        for call in tool_calls {
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

                function_app
                    .call_function_for_llm(
                        metadata_clone,
                        &function_name,
                        Some(arguments_obj),
                        DEFAULT_TIMEOUT_SEC,
                    )
                    .await
                    .map_err(|e| JobWorkerError::OtherError(format!("Tool execution error: {e}")))
            };

            // For now, execute without detailed tracing (can be enhanced later)
            let tool_result = tool_action.await?;

            tracing::debug!("Tool response: {}", &tool_result);

            // Add tool result to messages
            messages.lock().await.push(ChatMessage {
                role: genai::chat::ChatRole::Tool,
                content: GenaiMessageContent::Text(tool_result.to_string()),
                options: None,
            });
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

            let tool_result = self
                .function_app
                .call_function_for_llm(
                    metadata.clone(),
                    &call.fn_name,
                    Some(arguments_obj),
                    DEFAULT_TIMEOUT_SEC,
                )
                .await?;

            tracing::debug!("Tool response: {}", &tool_result);

            // Add tool result to messages
            messages.lock().await.push(ChatMessage {
                role: genai::chat::ChatRole::Tool,
                content: GenaiMessageContent::Text(tool_result.to_string()),
                options: None,
            });
        }

        Ok(())
    }

    pub async fn request_chat_stream(
        &self,
        args: LlmChatArgs,
        metadata: HashMap<String, String>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        let options = self.options(&args);
        let messages = self.trans_messages(args);
        let chat_req = ChatRequest::new(messages);
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
        let model_name = Arc::new(res.model_iden.model_name.to_string().clone());

        // XXX TODO tracing metadata
        let metadata = Trailer { metadata };
        let metadata_clone = metadata.clone();
        // Use flatmap to allow returning multiple ResultOutputItems from a single event
        let stream = res
            .stream
            .filter_map(move |event_result| {
                let value = model_name.clone();
                let metadata = metadata.clone();
                async move {
                    match event_result {
                        Ok(event) => match event {
                            ChatStreamEvent::Start => {
                                // Ignore start event
                                None
                            }
                            ChatStreamEvent::Chunk(chunk) => {
                                // Convert text chunk to LlmChatResult and serialize
                                let llm_result = LlmChatResult {
                                    content: Some(llm_chat_result::MessageContent {
                                        content: Some(message_content::Content::Text(
                                            chunk.content,
                                        )),
                                    }),
                                    done: false,
                                    // Set other fields as needed
                                    ..Default::default()
                                };
                                // Encode LlmChatResult to Protobuf
                                let bytes = match prost::Message::encode_to_vec(&llm_result) {
                                    bytes if !bytes.is_empty() => bytes,
                                    _ => {
                                        tracing::error!(
                                            "Failed to encode LlmChatResult to protobuf"
                                        );
                                        return None;
                                    }
                                };
                                Some(ResultOutputItem {
                                    item: Some(result_output_item::Item::Data(bytes)),
                                })
                            }
                            ChatStreamEvent::ReasoningChunk(chunk) => {
                                // Convert reasoning chunk to LlmChatResult and serialize
                                let llm_result = LlmChatResult {
                                    reasoning_content: Some(chunk.content),
                                    done: false,
                                    // Set other fields as needed
                                    ..Default::default()
                                };
                                // Encode LlmChatResult to Protobuf
                                let bytes = match prost::Message::encode_to_vec(&llm_result) {
                                    bytes if !bytes.is_empty() => bytes,
                                    _ => {
                                        tracing::error!(
                                            "Failed to encode reasoning LlmChatResult to protobuf"
                                        );
                                        return None;
                                    }
                                };
                                Some(ResultOutputItem {
                                    item: Some(result_output_item::Item::Data(bytes)),
                                })
                            }
                            ChatStreamEvent::ToolCallChunk(c) => {
                                // Tool calls in streaming should be handled differently
                                // For now, we'll log and ignore, but this could be enhanced
                                tracing::warn!(
                                    "Tool call chunks in streaming not yet supported: {c:?}",
                                );
                                None
                            }
                            ChatStreamEvent::End(end) => {
                                // End event - send LlmChatResult with done flag set to true
                                let mut llm_result = LlmChatResult {
                                    done: true,
                                    ..Default::default()
                                };
                                // Add usage if available
                                if let Some(usage) = end.captured_usage {
                                    llm_result.usage = Some(llm_chat_result::Usage {
                                        model: value.to_string(),
                                        prompt_tokens: usage.prompt_tokens.map(|v| v as u32),
                                        completion_tokens: usage
                                            .completion_tokens
                                            .map(|v| v as u32),
                                        ..Default::default()
                                    });
                                }
                                // Add final content if available
                                if let Some(text) = end
                                    .captured_content
                                    .as_ref()
                                    .and_then(|c| c.first())
                                    .and_then(|mc| match mc {
                                        GenaiMessageContent::Text(text) => Some(text.clone()),
                                        _ => None,
                                    })
                                {
                                    llm_result.content = Some(llm_chat_result::MessageContent {
                                        content: Some(message_content::Content::Text(
                                            text.to_string(),
                                        )),
                                    });
                                }
                                // Add final reasoning content if available
                                if let Some(reasoning) = end.captured_reasoning_content {
                                    llm_result.reasoning_content = Some(reasoning);
                                }
                                // Encode completion message to Protobuf
                                let bytes = match prost::Message::encode_to_vec(&llm_result) {
                                    bytes if !bytes.is_empty() => bytes,
                                    _ => {
                                        // If encoding fails, send an empty End signal
                                        return Some(ResultOutputItem {
                                            item: Some(result_output_item::Item::End(
                                                metadata.clone(),
                                            )),
                                        });
                                    }
                                };
                                // Return only data item here, flat_map will add subsequent End item
                                Some(ResultOutputItem {
                                    item: Some(result_output_item::Item::Data(bytes)),
                                })
                            }
                        },
                        Err(e) => {
                            tracing::error!("Error in chat stream: {:?}", e);
                            // On error, send termination signal
                            Some(ResultOutputItem {
                                item: Some(result_output_item::Item::End(metadata.clone())),
                            })
                        }
                    }
                }
            })
            .chain(futures::stream::once(async move {
                // Always send End item at the end of the stream
                ResultOutputItem {
                    item: Some(result_output_item::Item::End(metadata_clone)),
                }
            }))
            .boxed();

        Ok(stream)
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
        match &self.content {
            GenaiMessageContent::Text(text) => text,
            _ => "", // For non-text content, return empty string
        }
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
        // Return only the message content for trace output, not the full structure
        serde_json::json!(self.first_text())
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
                content: GenaiMessageContent::Text(m.get_content().to_string()),
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
