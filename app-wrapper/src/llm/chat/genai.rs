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
use jobworkerp_runner::jobworkerp;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content;
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::GenaiRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::{llm_chat_result, LlmChatArgs, LlmChatResult};
use proto::jobworkerp::data::{result_output_item, Empty, ResultOutputItem};
use std::sync::Arc;

use super::conversion::ToolConverter;

pub struct GenaiLLMConfig {
    pub model_name: String,
    pub endpoint_url: Option<String>,
}
#[derive(Clone)]
pub struct GenaiService {
    pub function_app: Arc<FunctionAppImpl>,
    pub client: Client,
    pub model: String,
    pub system_prompt: Option<String>,
}
impl GenaiService {
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

    pub async fn request_chat(&self, args: LlmChatArgs) -> Result<LlmChatResult> {
        let options = self.options(&args);
        let tools = self.function_list(&args).await?;
        let messages = self.trans_messages(args);
        let chat_req = ChatRequest::new(messages);
        let chat_req = if tools.is_empty() {
            chat_req
        } else {
            // Add tools to the chat request
            chat_req.with_tools(tools)
        };
        tracing::debug!(
            "Genai LLM: model: {}, Chat request: {:?}, options: {:?}",
            &self.model,
            &chat_req,
            &options,
        );
        let res = self
            .client
            .exec_chat(&self.model, chat_req, options.as_ref())
            .await
            .map_err(|e| anyhow::anyhow!("Failed to request generation: {:#?}", e))?;
        Ok(LlmChatResult {
            content: match res.content {
                Some(GenaiMessageContent::Text(text)) => Some(llm_chat_result::MessageContent {
                    content: Some(message_content::Content::Text(text)),
                }),
                Some(_) => {
                    tracing::error!("Unsupported message content type: {:#?}", &res.content);
                    None
                }
                None => None,
            },
            reasoning_content: res.reasoning_content,
            done: true,
            usage: Some(llm_chat_result::Usage {
                model: res.model_iden.model_name.to_string(),
                prompt_tokens: res.usage.prompt_tokens.map(|v| v as u32),
                completion_tokens: res.usage.completion_tokens.map(|v| v as u32),
                ..Default::default()
            }),
        })
    }
    pub async fn request_chat_stream(
        &self,
        args: LlmChatArgs,
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

        // Use flatmap to allow returning multiple ResultOutputItems from a single event
        let stream = res
            .stream
            .filter_map(move |event_result| {
                let value = model_name.clone();
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
                                if let Some(GenaiMessageContent::Text(text)) = end.captured_content
                                {
                                    llm_result.content = Some(llm_chat_result::MessageContent {
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
                                            item: Some(result_output_item::Item::End(Empty {})),
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
                                item: Some(result_output_item::Item::End(Empty {})),
                            })
                        }
                    }
                }
            })
            .chain(futures::stream::once(async {
                // Always send End item at the end of the stream
                ResultOutputItem {
                    item: Some(result_output_item::Item::End(Empty {})),
                }
            }))
            .boxed();

        Ok(stream)
    }
}
