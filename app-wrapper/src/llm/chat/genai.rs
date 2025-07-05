use super::super::generic_tracing_helper::{
    ChatResponse, GenericLLMTracingHelper, LLMMessage, ModelOptions as GenericModelOptions,
    ToolInfo as GenericToolInfo, UsageData,
};
use super::conversion::ToolConverter;
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
use infra_utils::infra::trace::impls::GenericOtelClient;
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::jobworkerp;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content;
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::GenaiRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::{llm_chat_result, LlmChatArgs, LlmChatResult};
use proto::jobworkerp::data::{result_output_item, ResultOutputItem, Trailer};
use std::collections::HashMap;
use std::sync::Arc;

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
        tracing::debug!("=== Genai LLM: target_resolver: {:?}", &target_resolver,);
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
            jobworkerp::runner::llm::llm_chat_args::ChatRole::RoleSystem => {
                genai::chat::ChatRole::System
            }
            jobworkerp::runner::llm::llm_chat_args::ChatRole::RoleUser => {
                genai::chat::ChatRole::User
            }
            jobworkerp::runner::llm::llm_chat_args::ChatRole::RoleAssistant => {
                genai::chat::ChatRole::Assistant
            }
            jobworkerp::runner::llm::llm_chat_args::ChatRole::RoleTool => {
                genai::chat::ChatRole::Tool
            }
            _ => genai::chat::ChatRole::User,
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
                        Some(ProtoContent::Image(image)) => {
                            let source = match image.source {
                                Some(src) => {
                                    if !src.url.is_empty() {
                                        genai::chat::ImageSource::Url(src.url)
                                    } else if !src.base64.is_empty() {
                                        genai::chat::ImageSource::Base64(Arc::from(src.base64))
                                    } else {
                                        return None;
                                    }
                                }
                                None => return None,
                            };
                            GenaiMessageContent::Parts(vec![genai::chat::ContentPart::Image {
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

    pub fn with_otel_client(mut self, client: Arc<GenericOtelClient>) -> Self {
        self.otel_client = Some(client);
        self
    }

    pub async fn request_chat(
        &self,
        args: LlmChatArgs,
        cx: opentelemetry::Context,
        metadata: HashMap<String, String>,
    ) -> Result<LlmChatResult> {
        let metadata = Arc::new(metadata);
        let options = self.options(&args);
        let tools = self.function_list(&args).await?;
        let messages = self.trans_messages(args);
        let chat_req = ChatRequest::new(messages);
        let chat_req = if tools.is_empty() {
            chat_req
        } else {
            // Add tools to the chat request
            chat_req.with_tools(tools.clone())
        };

        // 新しい統一トレーシングAPIを使用
        let tracing_context = crate::llm::tracing::LLMTracingHelper::start_llm_tracing(
            self,
            crate::llm::tracing::LLMApiType::Chat,
            &chat_req,
            &metadata,
            Some(cx.clone()),
        )
        .await?;

        // APIリクエストの実行
        let api_result = self
            .client
            .exec_chat(&self.model, chat_req, options.as_ref())
            .await
            .map_err(|e| JobWorkerError::OtherError(format!("Chat API error: {e}")));

        // 結果に応じたトレーシング終了
        let res = match api_result {
            Ok(response) => {
                // 成功時のトレーシング終了
                let _ = crate::llm::tracing::LLMTracingHelper::finish_llm_tracing(
                    self,
                    tracing_context,
                    &response,
                )
                .await;
                response
            }
            Err(error) => {
                // エラー時のトレーシング終了
                let _ = crate::llm::tracing::LLMTracingHelper::finish_llm_tracing_with_error(
                    self,
                    tracing_context,
                    &error,
                )
                .await;
                return Err(error.into());
            }
        };

        let chat_result = LlmChatResult {
            content: if let Some(text) = res.first_text() {
                Some(llm_chat_result::MessageContent {
                    content: Some(message_content::Content::Text(text.to_string())),
                })
            } else {
                tracing::warn!("No text content found in response: {:#?}", &res.content);
                None
            },
            reasoning_content: res.reasoning_content,
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
                            ChatStreamEvent::ToolCallChunk(_) => {
                                // Handle tool call chunks - for now, we'll ignore them
                                // as they're not directly supported in the current LlmChatResult structure
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
        serde_json::json!({
            "content": self.content,
            "content_text": self.first_text(),
            "model": self.model_iden.model_name,
            "usage": {
                "prompt_tokens": self.usage.prompt_tokens,
                "completion_tokens": self.usage.completion_tokens,
                "total_tokens": self.usage.total_tokens.unwrap_or(0)
            },
            "reasoning_content": self.reasoning_content
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

/// Trait for OpenTelemetry tracing functionality in GenAI services
pub trait GenaiTracingHelper: GenericLLMTracingHelper {
    /// Convert ChatMessage vector to proper tracing input format
    fn convert_messages_to_input_genai(messages: &[ChatMessage]) -> serde_json::Value {
        serde_json::json!(messages
            .iter()
            .map(|m| {
                let mut msg_json = serde_json::json!({
                    "role": m.get_role(),
                    "content": m.get_content()
                });

                // Add additional content info for non-text messages
                match &m.content {
                    GenaiMessageContent::Parts(parts) => {
                        msg_json["parts_count"] = serde_json::json!(parts.len());
                    }
                    GenaiMessageContent::ToolCalls(calls) => {
                        msg_json["tool_calls"] = serde_json::json!(calls
                            .iter()
                            .map(|tc| serde_json::json!({
                                "call_id": tc.call_id,
                                "fn_name": tc.fn_name,
                                "fn_arguments": tc.fn_arguments
                            }))
                            .collect::<Vec<_>>());
                    }
                    _ => {}
                }

                msg_json
            })
            .collect::<Vec<_>>())
    }

    /// Convert ChatOptions to proper model parameters format
    fn convert_model_options_to_parameters_genai(
        options: &Option<ChatOptions>,
    ) -> HashMap<String, serde_json::Value> {
        let mut parameters = HashMap::new();

        if let Some(opts) = options {
            if let Some(temp) = opts.temperature {
                parameters.insert("temperature".to_string(), serde_json::json!(temp));
            }
            if let Some(max_tokens) = opts.max_tokens {
                parameters.insert("max_tokens".to_string(), serde_json::json!(max_tokens));
            }
            if let Some(top_p) = opts.top_p {
                parameters.insert("top_p".to_string(), serde_json::json!(top_p));
            }
            if let Some(normalize) = opts.normalize_reasoning_content {
                parameters.insert(
                    "normalize_reasoning_content".to_string(),
                    serde_json::json!(normalize),
                );
            }
        }

        parameters
    }

    /// Create chat completion span attributes from GenAI request components
    #[allow(async_fn_in_trait)]
    async fn create_chat_span_from_request(
        &self,
        model: &str,
        chat_req: &ChatRequest,
        options: &Option<ChatOptions>,
        tools: &[Tool],
        metadata: &HashMap<String, String>,
    ) -> infra_utils::infra::trace::attr::OtelSpanAttributes {
        let input_messages = Self::convert_messages_to_input_genai(&chat_req.messages);
        let model_parameters = Self::convert_model_options_to_parameters_genai(options);

        self.create_chat_completion_span_attributes(
            model,
            input_messages,
            Some(&model_parameters),
            tools,
            metadata,
        )
    }

    fn trace_usage(
        &self,
        metadata: &HashMap<String, String>,
        parent_context: opentelemetry::Context,
        name: &str,
        usage_data: &genai::chat::Usage,
        content: Option<&str>,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'static {
        GenericLLMTracingHelper::trace_usage(
            self,
            metadata,
            parent_context,
            name,
            usage_data,
            content,
        )
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
                    _ => genai::chat::ChatRole::User,
                },
                content: GenaiMessageContent::Text(m.get_content().to_string()),
                options: None,
            })
            .collect();
        Self::convert_messages_to_input_genai(&genai_messages)
    }

    fn convert_model_options_to_parameters(
        &self,
        _options: &impl GenericModelOptions,
    ) -> HashMap<String, serde_json::Value> {
        // For GenAI, we can't directly convert from the generic trait
        // This would need to be implemented with specific knowledge of the options
        HashMap::new()
    }

    fn get_provider_name(&self) -> &str {
        "genai"
    }
}

impl GenaiTracingHelper for GenaiChatService {}

// GenAI Chat Request 用の LLMRequestData trait 実装
impl crate::llm::tracing::LLMRequestData for genai::chat::ChatRequest {
    fn extract_input(&self) -> crate::llm::tracing::LLMInput {
        crate::llm::tracing::LLMInput {
            messages: serde_json::json!(self
                .messages
                .iter()
                .map(|m| {
                    let mut msg_json = serde_json::json!({
                        "role": match m.role {
                            genai::chat::ChatRole::User => "user",
                            genai::chat::ChatRole::Assistant => "assistant",
                            genai::chat::ChatRole::System => "system",
                            genai::chat::ChatRole::Tool => "tool",
                        },
                        "content": match &m.content {
                            genai::chat::MessageContent::Text(text) => text,
                            _ => "",
                        }
                    });

                    // Add additional content info for non-text messages
                    match &m.content {
                        genai::chat::MessageContent::Parts(parts) => {
                            msg_json["parts_count"] = serde_json::json!(parts.len());
                        }
                        genai::chat::MessageContent::ToolCalls(calls) => {
                            msg_json["tool_calls"] = serde_json::json!(calls
                                .iter()
                                .map(|tc| serde_json::json!({
                                    "call_id": tc.call_id,
                                    "fn_name": tc.fn_name,
                                    "fn_arguments": tc.fn_arguments
                                }))
                                .collect::<Vec<_>>());
                        }
                        _ => {}
                    }

                    msg_json
                })
                .collect::<Vec<_>>()),
            prompt: None,
        }
    }

    fn extract_options(&self) -> Option<crate::llm::tracing::LLMOptions> {
        // GenAIの場合、オプションはChatRequestに直接含まれていない
        // ChatOptionsは別途渡される構造になっているため、ここではNoneを返す
        None
    }

    fn extract_tools(&self) -> Vec<crate::llm::tracing::LLMTool> {
        self.tools.as_ref().map_or(vec![], |tools| {
            tools
                .iter()
                .map(|tool| crate::llm::tracing::LLMTool {
                    name: tool.name.clone(),
                    description: tool.description.clone().unwrap_or_default(),
                    parameters: serde_json::json!(tool),
                })
                .collect()
        })
    }

    fn extract_model(&self) -> Option<String> {
        None // GenAIの場合、モデル名はリクエストに含まれていない
    }
}

// GenAI Chat Response 用の LLMResponseData trait 実装
impl crate::llm::tracing::LLMResponseData for genai::chat::ChatResponse {
    fn to_trace_output(&self) -> serde_json::Value {
        serde_json::json!({
            "content": self.content,
            "content_text": self.first_text(),
            "model": self.model_iden.model_name,
            "usage": {
                "prompt_tokens": self.usage.prompt_tokens,
                "completion_tokens": self.usage.completion_tokens,
                "total_tokens": self.usage.total_tokens.unwrap_or(0)
            },
            "reasoning_content": self.reasoning_content
        })
    }

    fn extract_usage(&self) -> Option<Box<dyn crate::llm::tracing::UsageData>> {
        Some(Box::new(self.usage.clone()) as Box<dyn crate::llm::tracing::UsageData>)
    }

    fn extract_content(&self) -> Option<String> {
        self.first_text().map(|s| s.to_string())
    }
}

// 新しい統一LLMTracingHelper実装
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
