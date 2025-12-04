//! Core MistralRS service implementation
//!
//! Ported from app-wrapper/src/llm/mistral.rs

use super::model::MistralModelLoader;
use super::types::{MistralRSMessage, MistralRSToolCall};
use anyhow::Result;
use futures::stream::StreamExt;
use jobworkerp_runner::jobworkerp::runner::llm::{
    llm_chat_args::{message_content, ChatRole},
    llm_runner_settings::LocalRunnerSettings,
    LlmChatArgs,
};
use mistralrs::Model;
use std::sync::Arc;

use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole as TextMessageRole;

pub struct MistralLlmServiceImpl {
    model_name: String,
    pub model: Arc<Model>,
}

impl MistralModelLoader for MistralLlmServiceImpl {}

impl MistralLlmServiceImpl {
    /// Create a new MistralRS service from settings
    pub async fn new(settings: &LocalRunnerSettings) -> Result<Self> {
        let model = Arc::new(Self::load_model(settings).await?);

        match &settings.model_settings {
            Some(
                jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::local_runner_settings::ModelSettings::TextModel(
                    text_model,
                ),
            ) => {
                let model_name = text_model.model_name_or_path.clone();
                Ok(Self { model, model_name })
            }
            Some(
                jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::local_runner_settings::ModelSettings::GgufModel(
                    gguf_model,
                ),
            ) => {
                let model_name = gguf_model.model_name_or_path.clone();
                Ok(Self { model, model_name })
            }
            None => Err(anyhow::anyhow!("No model settings provided")),
        }
    }

    /// Send a chat request to the MistralRS model
    pub async fn request_chat(
        &self,
        request_builder: mistralrs::RequestBuilder,
    ) -> Result<mistralrs::ChatCompletionResponse> {
        let response = self.model.send_chat_request(request_builder).await?;
        Ok(response)
    }

    /// Stream a chat request to the MistralRS model
    pub async fn stream_chat(
        &self,
        request_builder: mistralrs::RequestBuilder,
    ) -> Result<futures::stream::BoxStream<'static, mistralrs::Response>> {
        let model = self.model.clone();

        let (tx, rx) = futures::channel::mpsc::unbounded();

        tokio::spawn(async move {
            let result = async {
                let mut stream = model.stream_chat_request(request_builder).await?;

                while let Some(response) = stream.next().await {
                    if tx.unbounded_send(response).is_err() {
                        break;
                    }
                }

                anyhow::Result::<()>::Ok(())
            }
            .await;

            if let Err(e) = result {
                tracing::error!("Error in MistralRS stream: {}", e);
            }
        });

        Ok(rx.boxed())
    }

    /// Get reference to the underlying model
    pub fn model(&self) -> &Arc<Model> {
        &self.model
    }

    /// Get the model name for identification purposes
    pub fn model_name(&self) -> &str {
        &self.model_name
    }

    /// Convert protocol messages to MistralRS format
    pub fn convert_proto_messages(&self, args: &LlmChatArgs) -> Result<Vec<MistralRSMessage>> {
        args.messages
            .iter()
            .map(|msg| {
                let role = match ChatRole::try_from(msg.role)? {
                    ChatRole::User => TextMessageRole::User,
                    ChatRole::Assistant => TextMessageRole::Assistant,
                    ChatRole::System => TextMessageRole::System,
                    ChatRole::Tool => TextMessageRole::Tool,
                    ChatRole::Unspecified => TextMessageRole::User,
                };

                match &msg.content {
                    Some(content) => match &content.content {
                        Some(message_content::Content::Text(text)) => Ok(MistralRSMessage {
                            role,
                            content: text.clone(),
                            tool_call_id: None,
                            tool_calls: None,
                        }),
                        Some(message_content::Content::ToolCalls(tool_calls)) => {
                            let converted_tool_calls: Vec<MistralRSToolCall> = tool_calls
                                .calls
                                .iter()
                                .map(|tc| MistralRSToolCall {
                                    id: tc.call_id.clone(),
                                    function_name: tc.fn_name.clone(),
                                    arguments: tc.fn_arguments.clone(),
                                })
                                .collect();

                            Ok(MistralRSMessage {
                                role: TextMessageRole::Assistant,
                                content: String::new(),
                                tool_call_id: None,
                                tool_calls: Some(converted_tool_calls),
                            })
                        }
                        _ => Ok(MistralRSMessage {
                            role,
                            content: String::new(),
                            tool_call_id: None,
                            tool_calls: None,
                        }),
                    },
                    None => Ok(MistralRSMessage {
                        role,
                        content: String::new(),
                        tool_call_id: None,
                        tool_calls: None,
                    }),
                }
            })
            .collect()
    }

    /// Extract tool calls from MistralRS API response
    pub fn extract_tool_calls_from_response(
        &self,
        response: &mistralrs::ChatCompletionResponse,
    ) -> Result<Vec<MistralRSToolCall>> {
        if let Some(first_choice) = response.choices.first() {
            if let Some(tool_calls) = &first_choice.message.tool_calls {
                return Ok(tool_calls
                    .iter()
                    .map(|tc| MistralRSToolCall {
                        id: tc.id.clone(),
                        function_name: tc.function.name.clone(),
                        arguments: tc.function.arguments.clone(),
                    })
                    .collect());
            }
        }
        Ok(vec![])
    }

    /// Extract tool calls for streaming
    pub fn extract_tool_calls_from_chunk_response(
        &self,
        response: &mistralrs::ChatCompletionChunkResponse,
    ) -> Result<Vec<MistralRSToolCall>> {
        if let Some(first_choice) = response.choices.first() {
            if let Some(tool_calls) = &first_choice.delta.tool_calls {
                return Ok(tool_calls
                    .iter()
                    .map(|tc| MistralRSToolCall {
                        id: tc.id.clone(),
                        function_name: tc.function.name.clone(),
                        arguments: tc.function.arguments.clone(),
                    })
                    .collect());
            }
        }
        Ok(vec![])
    }
}
