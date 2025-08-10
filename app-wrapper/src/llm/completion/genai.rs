use anyhow::Result;
use futures::stream::BoxStream;
use futures::StreamExt;
use genai::chat::{
    ChatMessage, ChatOptions, ChatRequest, ChatStreamEvent, MessageContent as GenaiMessageContent,
};
use genai::resolver::{Endpoint, ServiceTargetResolver};
use genai::{Client, ServiceTarget};
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::jobworkerp::runner::llm::llm_completion_result::message_content;
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::GenaiRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::{
    llm_completion_result, LlmCompletionArgs, LlmCompletionResult,
};
use net_utils::trace::impls::GenericOtelClient;
use proto::jobworkerp::data::{result_output_item, ResultOutputItem, Trailer};
use std::collections::HashMap;
use std::sync::Arc;

use super::super::generic_tracing_helper::{
    GenericLLMTracingHelper, LLMMessage,
};
use super::super::tracing::genai_helper::GenaiCompletionTracingHelper;

pub struct GenaiLLMConfig {
    pub model_name: String,
    pub endpoint_url: Option<String>,
}
#[derive(Clone)]
pub struct GenaiCompletionService {
    pub client: Client,
    pub model: String,
    pub system_prompt: Option<String>,
    pub otel_client: Option<Arc<GenericOtelClient>>,
}
impl GenaiCompletionService {
    pub async fn new(settings: GenaiRunnerSettings) -> Result<Self> {
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
            client,
            model: settings.model,
            system_prompt: settings.system_prompt,
            otel_client: Some(Arc::new(GenericOtelClient::new("genai.completion_service"))),
        })
    }
    fn options(&self, args: &LlmCompletionArgs) -> Option<ChatOptions> {
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
    fn messages(&self, args: LlmCompletionArgs) -> Vec<ChatMessage> {
        let mut messages = Vec::new();
        if let Some(s) = args.system_prompt.or_else(|| self.system_prompt.clone()) {
            messages.push(ChatMessage::system(s))
        }
        messages.push(ChatMessage::user(args.prompt));
        messages
    }
    pub fn with_otel_client(mut self, client: Arc<GenericOtelClient>) -> Self {
        self.otel_client = Some(client);
        self
    }

    pub async fn request_chat(
        &self,
        args: LlmCompletionArgs,
        cx: opentelemetry::Context,
        metadata: HashMap<String, String>,
    ) -> Result<LlmCompletionResult> {
        let metadata = Arc::new(metadata);
        let options = self.options(&args);
        let messages = self.messages(args);
        let chat_req = ChatRequest::new(messages);

        // Use tracing-enabled internal call
        let res = Self::request_completion_internal_with_tracing(
            Arc::new(self.clone()),
            chat_req,
            options,
            Some(cx.clone()),
            metadata.clone(),
        )
        .await?;

        let completion_result = LlmCompletionResult {
            content: if let Some(text) = res.first_text() {
                Some(llm_completion_result::MessageContent {
                    content: Some(message_content::Content::Text(text.to_string())),
                })
            } else {
                tracing::error!("No text content found in response: {:#?}", &res.content);
                None
            },
            reasoning_content: res.reasoning_content,
            done: true,
            usage: Some(llm_completion_result::Usage {
                model: res.model_iden.model_name.to_string(),
                prompt_tokens: res.usage.prompt_tokens.map(|v| v as u32),
                completion_tokens: res.usage.completion_tokens.map(|v| v as u32),
                ..Default::default()
            }),
            ..Default::default()
        };

        // Record usage if available
        let usage = &res.usage;
        if true {
            let content = completion_result
                .content
                .as_ref()
                .and_then(|c| c.content.as_ref())
                .map(|content| match content {
                    message_content::Content::Text(text) => text.as_str(),
                    // _ => None,
                });
            let _ = GenericLLMTracingHelper::trace_usage(
                self,
                &metadata,
                cx.clone(),
                "genai.completion.usage",
                usage,
                content,
            )
            .await;
        }

        Ok(completion_result)
    }

    async fn request_completion_internal_with_tracing(
        self: Arc<Self>,
        chat_req: ChatRequest,
        options: Option<ChatOptions>,
        parent_context: Option<opentelemetry::Context>,
        metadata: Arc<HashMap<String, String>>,
    ) -> Result<genai::chat::ChatResponse> {
        let model = self.model.clone();
        let model_for_span = model.clone();
        let chat_req_for_span = chat_req.clone();
        let options_for_span = options.clone();

        tracing::debug!(
            "Genai LLM: model: {}, Chat request: {:?}, options: {:?}",
            &model,
            &chat_req,
            &options,
        );

        let client_clone = self.client.clone();
        let completion_api_action = async move {
            client_clone
                .exec_chat(&model, chat_req, options.as_ref())
                .await
                .map_err(|e| JobWorkerError::OtherError(format!("Completion API error: {e}")))
        };

        // Execute completion API call and get both result and context
        let (res, _current_context) = if let Some(_otel_client) = self.get_otel_client() {
            // Create span attributes for completion API call
            let span_attributes = self
                .create_completion_span_from_request(
                    &model_for_span,
                    &chat_req_for_span,
                    &options_for_span,
                    &metadata,
                )
                .await;

            // Execute completion API call with response tracing
            self.with_completion_response_tracing(
                &metadata,
                parent_context.clone(),
                span_attributes,
                completion_api_action,
            )
            .await?
        } else {
            let result = completion_api_action.await?;
            let context = opentelemetry::Context::current();
            (result, context)
        };

        Ok(res)
    }
    pub async fn request_chat_stream(
        &self,
        args: LlmCompletionArgs,
        metadata: HashMap<String, String>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        let options = self.options(&args);
        let messages = self.messages(args);
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

        let metadata_clone = metadata.clone();
        // Use flatmap to allow returning multiple ResultOutputItems from a single event
        let stream = res.stream
            .filter_map(move |event_result| {
            let value = model_name.clone();
            let metadata_clone = metadata_clone.clone();
            async move {
                match event_result {
                    Ok(event) => match event {
                        ChatStreamEvent::Start => {
                            // Ignore start event
                            None
                        },
                        ChatStreamEvent::Chunk(chunk) => {
                            // Convert text chunk to LlmCompletionResult and serialize
                            let llm_result = LlmCompletionResult {
                                content: Some(llm_completion_result::MessageContent {
                                    content: Some(message_content::Content::Text(chunk.content)),
                                }),
                                done: false,
                                // Set other fields as needed
                                ..Default::default()
                            };
                            // Encode LlmCompletionResult to Protobuf
                            let bytes = match prost::Message::encode_to_vec(&llm_result) {
                                bytes if !bytes.is_empty() => bytes,
                                _ => {
                                    tracing::error!("Failed to encode LlmCompletionResult to protobuf");
                                    return None;
                                }
                            };
                            Some(ResultOutputItem {
                                item: Some(result_output_item::Item::Data(bytes)),
                            })
                        },
                        ChatStreamEvent::ReasoningChunk(chunk) => {
                            // Convert reasoning chunk to LlmCompletionResult and serialize
                            let llm_result = LlmCompletionResult {
                                reasoning_content: Some(chunk.content),
                                done: false,
                                // Set other fields as needed
                                ..Default::default()
                            };
                            // Encode LlmCompletionResult to Protobuf
                            let bytes = match prost::Message::encode_to_vec(&llm_result) {
                                bytes if !bytes.is_empty() => bytes,
                                _ => {
                                    tracing::error!("Failed to encode reasoning LlmCompletionResult to protobuf");
                                    return None;
                                }
                            };
                            Some(ResultOutputItem {
                                item: Some(result_output_item::Item::Data(bytes)),
                            })
                        },
                        ChatStreamEvent::ToolCallChunk(_) => {
                            // Handle tool call chunks - for now, we'll ignore them
                            // as they're not directly supported in the current LlmCompletionResult structure
                            None
                        },
                        ChatStreamEvent::End(end) => {
                            // End event - send LlmCompletionResult with done flag set to true
                            let mut llm_result = LlmCompletionResult {
                                done: true,
                                ..Default::default()
                            };
                            // Add usage if available
                            if let Some(usage) = end.captured_usage {
                                llm_result.usage = Some(llm_completion_result::Usage {
                                    model: value.to_string(),
                                    prompt_tokens: usage.prompt_tokens.map(|v| v as u32),
                                    completion_tokens: usage.completion_tokens.map(|v| v as u32),
                                    ..Default::default()
                                });
                            }
                            // Add final content if available
                            if let Some(text) = end.captured_content.as_ref().and_then(|c| c.first()).and_then(|mc| {
                                match mc {
                                    GenaiMessageContent::Text(text) => Some(text.clone()),
                                    _ => None,
                                }
                            }) {
                                llm_result.content = Some(llm_completion_result::MessageContent {
                                    content: Some(message_content::Content::Text(text)),
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
                                        item: Some(result_output_item::Item::End(Trailer {
                                            metadata: metadata_clone,
                                        })),
                                    });
                                }
                            };
                            // Return only data item here, flat_map will add subsequent End item
                            Some(ResultOutputItem {
                                item: Some(result_output_item::Item::Data(bytes)),
                            })
                        },
                    },
                    Err(e) => {
                        tracing::error!("Error in chat stream: {:?}", e);
                        // On error, send termination signal
                        Some(ResultOutputItem {
                            item: Some(result_output_item::Item::End(Trailer {
                                metadata: metadata_clone,
                            })),
                        })
                    },
                }
            }
            })
            .chain(futures::stream::once(async move {
                // Always send End item at the end of the stream
                ResultOutputItem {
                    item: Some(result_output_item::Item::End(Trailer {
                        metadata,
                    })),
                }
            }))
            .boxed();

        Ok(stream)
    }
}

// Implement traits for GenaiService (completion version)
impl GenericLLMTracingHelper for GenaiCompletionService {
    fn get_otel_client(&self) -> Option<&Arc<GenericOtelClient>> {
        self.otel_client.as_ref()
    }

    fn convert_messages_to_input(&self, messages: &[impl LLMMessage]) -> serde_json::Value {
        use super::super::tracing::genai_helper::GenaiTracingHelper;
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
        super::super::chat::genai::GenaiChatService::convert_messages_to_input_genai(
            &genai_messages,
        )
    }

    fn get_provider_name(&self) -> &str {
        "genai"
    }
}

impl GenaiCompletionTracingHelper for GenaiCompletionService {}
