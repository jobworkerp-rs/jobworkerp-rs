pub mod args;
pub mod model;
pub mod result;

use self::model::MistralModelLoader;
use crate::mistral_runner::MistralChatResult;
use crate::mistral_runner::{MistralChatArgs, MistralRunnerSettings};
use anyhow::Result;
use futures::stream::StreamExt;
use jobworkerp_runner::jobworkerp::runner::llm::LlmChatResult;
use mistralrs::Model;
pub use result::{DefaultLLMResultConverter, LLMResultConverter};
use std::sync::Arc;

// ... (structs MistralRSMessage, MistralRSToolCall, etc. remain unchanged) ...

use crate::mistral::args::RequestBuilderUtils;

pub struct MistralRSService {
    model: Arc<Model>,
}

impl MistralModelLoader for MistralRSService {}

impl MistralRSService {
    pub async fn new(settings: &MistralRunnerSettings) -> Result<Self> {
        let model = Self::load_model(settings).await?;
        Ok(Self {
            model: Arc::new(model),
        })
    }

    pub async fn request_chat(&self, args: &MistralChatArgs) -> Result<MistralChatResult> {
        let request_builder = RequestBuilderUtils::build_request(args)?;
        let response = self.model.send_chat_request(request_builder).await?;

        // Convert to LlmChatResult first using the existing converter
        let llm_result = DefaultLLMResultConverter::convert_chat_completion_result(&response);

        // Convert LlmChatResult to MistralChatResult (proto)
        // We need to serialize/deserialize or map manually.
        // Since they are proto-generated structs from different crates but same proto definition (mostly),
        // manual mapping is safer.
        // However, for simplicity and to avoid huge mapping code, we can try to use serde if derived,
        // but proto structs usually don't derive serde by default unless configured.
        // Let's assume manual mapping for now or use a helper if available.
        // Actually, MistralChatResult is generated in this crate. LlmChatResult is in jobworkerp_runner.
        // They should be identical.

        // Let's implement a simple mapper here or in a helper.
        let result = Self::convert_llm_result_to_mistral_result(llm_result);
        Ok(result)
    }

    pub async fn stream_chat(
        &self,
        args: &MistralChatArgs,
    ) -> Result<futures::stream::BoxStream<'static, MistralChatResult>> {
        let request_builder = RequestBuilderUtils::build_request(args)?;

        // Clone the model to avoid lifetime issues
        let model = self.model.clone();

        // Use channel-based streaming to handle lifetime issues
        let (tx, rx) = futures::channel::mpsc::unbounded();

        // Spawn task to handle MistralRS streaming
        tokio::spawn(async move {
            let result = async {
                let mut stream = model.stream_chat_request(request_builder).await?;

                while let Some(response) = stream.next().await {
                    let result = match response {
                        mistralrs::Response::Chunk(chunk) => {
                            let llm_result =
                                DefaultLLMResultConverter::convert_chat_completion_chunk_result(
                                    &chunk,
                                );
                            Self::convert_llm_result_to_mistral_result(llm_result)
                        }
                        mistralrs::Response::Done(completion) => {
                            let llm_result =
                                DefaultLLMResultConverter::convert_chat_completion_result(
                                    &completion,
                                );
                            Self::convert_llm_result_to_mistral_result(llm_result)
                        }
                        _ => MistralChatResult {
                            content: None,
                            reasoning_content: None,
                            done: true,
                            usage: None,
                        },
                    };

                    if tx.unbounded_send(result).is_err() {
                        // Receiver dropped, stop streaming
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

        // Convert receiver to BoxStream
        Ok(rx.boxed())
    }

    fn convert_llm_result_to_mistral_result(llm_result: LlmChatResult) -> MistralChatResult {
        // This is a bit tedious but necessary.
        // We need to map jobworkerp_runner::...::LlmChatResult to crate::mistral_runner::MistralChatResult

        use crate::mistral_runner::mistral_chat_result::{
            message_content::{Content, ToolCall, ToolCalls},
            MessageContent, Usage,
        };

        let content = llm_result.content.map(|c| {
            let inner_content = match c.content {
                Some(jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(t)) => {
                    Some(Content::Text(t))
                }
                Some(jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::ToolCalls(tc)) => {
                    Some(Content::ToolCalls(ToolCalls {
                        calls: tc.calls.into_iter().map(|c| ToolCall {
                            call_id: c.call_id,
                            fn_name: c.fn_name,
                            fn_arguments: c.fn_arguments,
                        }).collect()
                    }))
                }
                Some(jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Image(_)) => {
                    // Image content not supported in Mistral runner output yet
                    None
                }
                None => None,
            };
            MessageContent {
                content: inner_content,
            }
        });

        let usage = llm_result.usage.map(|u| {
            let prompt_tokens = u.prompt_tokens.unwrap_or(0) as i32;
            let completion_tokens = u.completion_tokens.unwrap_or(0) as i32;
            let total_tokens = prompt_tokens + completion_tokens;

            Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
            }
        });

        MistralChatResult {
            content,
            reasoning_content: llm_result.reasoning_content,
            done: llm_result.done,
            usage,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    use crate::mistral_runner::{MistralChatArgs, MistralRunnerSettings};
    use anyhow::Result;
    use futures::stream::StreamExt;
    

    // Helper to create settings (mock)
    fn create_mistral_settings() -> Result<MistralRunnerSettings> {
        // Mock settings
        Ok(MistralRunnerSettings {
            model_settings: Some(
                crate::mistral_runner::mistral_runner_settings::ModelSettings::TextModel(
                    crate::mistral_runner::mistral_runner_settings::TextModelSettings {
                        model_name_or_path: "test-model".to_string(),
                        isq_type: None,
                        with_logging: false,
                        with_paged_attn: false,
                        chat_template: None,
                    },
                ),
            ),
            auto_device_map: None,
        })
    }

    // Helper to create chat args (mock)
    fn create_chat_args(_stream: bool) -> Result<MistralChatArgs> {
        use crate::mistral_runner::mistral_chat_args::{
            message_content, ChatMessage, ChatRole, LlmOptions, MessageContent,
        };
        Ok(MistralChatArgs {
            messages: vec![ChatMessage {
                role: ChatRole::User as i32,
                content: Some(MessageContent {
                    content: Some(message_content::Content::Text("Hello".to_string())),
                }),
            }],
            options: Some(LlmOptions {
                max_tokens: Some(10),
                temperature: Some(0.1),
                ..Default::default()
            }),
            ..Default::default()
        })
    }

    // Helper to create GGUF settings (mock)
    fn create_gguf_mistral_settings() -> Result<MistralRunnerSettings> {
        Ok(MistralRunnerSettings {
            model_settings: Some(
                crate::mistral_runner::mistral_runner_settings::ModelSettings::GgufModel(
                    crate::mistral_runner::mistral_runner_settings::GgufModelSettings {
                        model_name_or_path: "test-gguf-model".to_string(),
                        gguf_files: vec!["test.gguf".to_string()],
                        tok_model_id: None,
                        with_logging: false,
                        with_paged_attn: false,
                        chat_template: None,
                    },
                ),
            ),
            auto_device_map: None,
        })
    }

    #[ignore]
    #[tokio::test]
    async fn test_chat_llm_runner() -> Result<()> {
        // Create settings
        let settings = create_mistral_settings()?;

        // Create service instance
        let service = MistralRSService::new(&settings).await?;

        // Create chat args
        let args = create_chat_args(false)?;

        // Send request directly
        let result = service.request_chat(&args).await?;

        // Verify response
        assert!(result.done || result.content.is_some());
        if let Some(content) = result.content {
            println!("Chat response: {:?}", content);
        }

        Ok(())
    }

    #[ignore]
    #[tokio::test]
    async fn test_gguf_model() -> Result<()> {
        // Create GGUF settings
        let settings = create_gguf_mistral_settings()?;

        // Create service instance
        let service = MistralRSService::new(&settings).await?;

        // Create chat args
        let args = create_chat_args(false)?;

        // Send request directly
        let result = service.request_chat(&args).await?;

        // Verify response
        assert!(result.done || result.content.is_some());
        if let Some(content) = result.content {
            println!("GGUF response: {:?}", content);
        }

        Ok(())
    }

    #[ignore]
    #[tokio::test]
    async fn test_llm_runner_stream() -> Result<()> {
        // Create settings
        let settings = create_mistral_settings()?;

        // Create service instance
        let service = MistralRSService::new(&settings).await?;

        // Create chat args for streaming
        let args = create_chat_args(true)?;

        // Use MistralRSService stream_chat method
        let stream = service.stream_chat(&args).await?;

        let mut stream = stream;
        let mut count = 0;
        let mut output = Vec::new();

        while let Some(result) = stream.next().await {
            println!("Stream item done: {}", result.done);

            if let Some(content) = &result.content {
                // Check content
                println!("Content: {:?}", content);
                output.push(format!("{:?}", content));
            }

            count += 1;

            if result.done {
                break;
            }
        }

        println!("Stream output: {output:?}");
        assert!(count > 0, "Expected at least one stream item");

        Ok(())
    }

    #[ignore]
    #[tokio::test]
    async fn test_gguf_llm_runner_stream() -> Result<()> {
        // Create GGUF settings
        let settings = create_gguf_mistral_settings()?;

        // Create service instance
        let service = MistralRSService::new(&settings).await?;

        // Create chat args for streaming
        let args = create_chat_args(true)?;

        // Use MistralRSService stream_chat method
        let stream = service.stream_chat(&args).await?;

        let mut stream = stream;
        let mut count = 0;
        let mut output = Vec::new();

        while let Some(result) = stream.next().await {
            if let Some(content) = &result.content {
                output.push(format!("{:?}", content));
            }

            count += 1;

            if result.done {
                break;
            }
        }

        println!("GGUF stream output: {output:?}");
        assert!(count > 0, "Expected at least one stream item");

        Ok(())
    }
}
