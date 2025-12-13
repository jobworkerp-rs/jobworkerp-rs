use crate::jobworkerp::runner::llm::llm_chat_result::{message_content, MessageContent, Usage};
use crate::jobworkerp::runner::llm::LlmChatResult;
use crate::{jobworkerp::runner::llm::LlmRunnerSettings, schema_to_json_string};
use futures::stream::BoxStream;
use prost::Message;
use proto::DEFAULT_METHOD_NAME;

use super::{CollectStreamFuture, RunnerSpec};
use proto::jobworkerp::data::RunnerType;
use std::collections::HashMap;

pub struct LLMChatRunnerSpecImpl {}

impl LLMChatRunnerSpecImpl {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for LLMChatRunnerSpecImpl {
    fn default() -> Self {
        Self::new()
    }
}

pub trait LLMChatRunnerSpec {
    fn name(&self) -> String {
        RunnerType::LlmChat.as_str_name().to_string()
    }
    fn runner_settings_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/llm/runner.proto").to_string()
    }
    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        let mut schemas = HashMap::new();
        schemas.insert(
            DEFAULT_METHOD_NAME.to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: include_str!("../../protobuf/jobworkerp/runner/llm/chat_args.proto")
                    .to_string(),
                result_proto: include_str!(
                    "../../protobuf/jobworkerp/runner/llm/chat_result.proto"
                )
                .to_string(),
                description: Some(
                    "Generate chat response using LLM with conversation history".to_string(),
                ),
                output_type: proto::jobworkerp::data::StreamingOutputType::Both as i32,
            },
        );
        schemas
    }
    fn settings_schema(&self) -> String {
        // include_str!("../../schema/llm/LLMRunnerSettings.json").to_string()
        schema_to_json_string!(LlmRunnerSettings, "settings_schema")
    }
}

impl LLMChatRunnerSpec for LLMChatRunnerSpecImpl {}

impl RunnerSpec for LLMChatRunnerSpecImpl {
    fn name(&self) -> String {
        LLMChatRunnerSpec::name(self)
    }

    fn runner_settings_proto(&self) -> String {
        LLMChatRunnerSpec::runner_settings_proto(self)
    }

    fn method_proto_map(
        &self,
    ) -> std::collections::HashMap<String, proto::jobworkerp::data::MethodSchema> {
        LLMChatRunnerSpec::method_proto_map(self)
    }

    fn settings_schema(&self) -> String {
        LLMChatRunnerSpec::settings_schema(self)
    }

    /// Collect streaming LLM chat results into a single LlmChatResult
    ///
    /// Strategy:
    /// - Concatenates text content from all chunks
    /// - Collects all tool_calls (tool_calls takes precedence over text in final result)
    /// - Concatenates reasoning content from all chunks
    /// - Uses usage from the final chunk (done=true)
    /// - Returns error if data items were received but all decodes failed
    fn collect_stream(
        &self,
        stream: BoxStream<'static, proto::jobworkerp::data::ResultOutputItem>,
    ) -> CollectStreamFuture {
        use futures::StreamExt;
        use proto::jobworkerp::data::result_output_item;

        Box::pin(async move {
            let mut combined_text = String::new();
            let mut combined_reasoning = String::new();
            let mut collected_tool_calls: Vec<message_content::ToolCall> = Vec::new();
            let mut final_usage: Option<Usage> = None;
            let mut metadata = HashMap::new();
            let mut stream = stream;
            // Store FinalCollected data if received, to return after End(trailer)
            let mut final_collected: Option<Vec<u8>> = None;
            let mut decode_failure_count = 0;
            let mut data_item_count = 0;
            let mut successful_decode_count = 0;

            while let Some(item) = stream.next().await {
                match item.item {
                    Some(result_output_item::Item::Data(data)) => {
                        data_item_count += 1;
                        match LlmChatResult::decode(data.as_slice()) {
                            Ok(chunk) => {
                                successful_decode_count += 1;
                                // Handle content (text, image, or tool_calls)
                                if let Some(content) = chunk.content {
                                    match content.content {
                                        Some(message_content::Content::Text(text)) => {
                                            combined_text.push_str(&text);
                                        }
                                        Some(message_content::Content::ToolCalls(tc)) => {
                                            collected_tool_calls.extend(tc.calls);
                                        }
                                        Some(message_content::Content::Image(_)) => {
                                            // Image content cannot be meaningfully merged
                                            tracing::warn!(
                                                "Image content in streaming response is not supported"
                                            );
                                        }
                                        None => {}
                                    }
                                }

                                // Concatenate reasoning content
                                if let Some(reasoning) = chunk.reasoning_content {
                                    combined_reasoning.push_str(&reasoning);
                                }

                                // Only update final_usage if chunk.usage is Some
                                // to preserve previously recorded usage
                                if chunk.done && chunk.usage.is_some() {
                                    final_usage = chunk.usage;
                                }
                            }
                            Err(e) => {
                                decode_failure_count += 1;
                                tracing::warn!(
                                    "Failed to decode LlmChatResult in collect_stream (chunk {}/{}): {:?}",
                                    decode_failure_count,
                                    data_item_count,
                                    e
                                );
                            }
                        }
                    }
                    Some(result_output_item::Item::End(trailer)) => {
                        metadata = trailer.metadata;
                        break;
                    }
                    Some(result_output_item::Item::FinalCollected(data)) => {
                        // Store FinalCollected data and continue loop to capture End(trailer) metadata
                        final_collected = Some(data);
                    }
                    None => {}
                }
            }

            // If FinalCollected was received, return it with collected metadata
            if let Some(data) = final_collected {
                return Ok((data, metadata));
            }

            // Return error if we received data items but all decodes failed
            if data_item_count > 0 && successful_decode_count == 0 {
                return Err(anyhow::anyhow!(
                    "All {} LlmChatResult decode attempts failed in collect_stream",
                    decode_failure_count
                ));
            }

            // Determine final content type: tool_calls takes precedence if present
            let final_content = if !collected_tool_calls.is_empty() {
                Some(MessageContent {
                    content: Some(message_content::Content::ToolCalls(
                        message_content::ToolCalls {
                            calls: collected_tool_calls,
                        },
                    )),
                })
            } else if !combined_text.is_empty() {
                Some(MessageContent {
                    content: Some(message_content::Content::Text(combined_text)),
                })
            } else {
                None
            };

            // Build collected result
            let result = LlmChatResult {
                content: final_content,
                reasoning_content: if combined_reasoning.is_empty() {
                    None
                } else {
                    Some(combined_reasoning)
                },
                done: true,
                usage: final_usage,
            };

            let bytes = result.encode_to_vec();
            Ok((bytes, metadata))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use futures::StreamExt;
    use proto::jobworkerp::data::{result_output_item, Trailer};

    fn create_text_chat_result(text: &str, reasoning: Option<&str>, done: bool) -> LlmChatResult {
        LlmChatResult {
            content: if text.is_empty() {
                None
            } else {
                Some(MessageContent {
                    content: Some(message_content::Content::Text(text.to_string())),
                })
            },
            reasoning_content: reasoning.map(|r| r.to_string()),
            done,
            usage: None,
        }
    }

    fn create_tool_calls_chat_result(
        calls: Vec<message_content::ToolCall>,
        done: bool,
    ) -> LlmChatResult {
        LlmChatResult {
            content: Some(MessageContent {
                content: Some(message_content::Content::ToolCalls(
                    message_content::ToolCalls { calls },
                )),
            }),
            reasoning_content: None,
            done,
            usage: None,
        }
    }

    fn create_data_item(result: &LlmChatResult) -> proto::jobworkerp::data::ResultOutputItem {
        proto::jobworkerp::data::ResultOutputItem {
            item: Some(result_output_item::Item::Data(result.encode_to_vec())),
        }
    }

    fn create_end_item(
        metadata: HashMap<String, String>,
    ) -> proto::jobworkerp::data::ResultOutputItem {
        proto::jobworkerp::data::ResultOutputItem {
            item: Some(result_output_item::Item::End(Trailer { metadata })),
        }
    }

    fn create_final_collected_item(data: Vec<u8>) -> proto::jobworkerp::data::ResultOutputItem {
        proto::jobworkerp::data::ResultOutputItem {
            item: Some(result_output_item::Item::FinalCollected(data)),
        }
    }

    #[tokio::test]
    async fn test_llm_chat_collect_stream_single_text_chunk() {
        let runner = LLMChatRunnerSpecImpl::new();
        let result = create_text_chat_result("Hello!", None, true);

        let items = vec![create_data_item(&result), create_end_item(HashMap::new())];
        let stream = stream::iter(items).boxed();

        let (bytes, _) = runner.collect_stream(stream).await.unwrap();

        let decoded = LlmChatResult::decode(bytes.as_slice()).unwrap();
        assert!(decoded.done);
        if let Some(content) = decoded.content {
            if let Some(message_content::Content::Text(text)) = content.content {
                assert_eq!(text, "Hello!");
            } else {
                panic!("Expected text content");
            }
        } else {
            panic!("Expected content");
        }
    }

    #[tokio::test]
    async fn test_llm_chat_collect_stream_concatenates_text() {
        let runner = LLMChatRunnerSpecImpl::new();
        let chunk1 = create_text_chat_result("Hello, ", None, false);
        let chunk2 = create_text_chat_result("world", None, false);
        let chunk3 = create_text_chat_result("!", None, true);

        let items = vec![
            create_data_item(&chunk1),
            create_data_item(&chunk2),
            create_data_item(&chunk3),
            create_end_item(HashMap::new()),
        ];
        let stream = stream::iter(items).boxed();

        let (bytes, _) = runner.collect_stream(stream).await.unwrap();

        let decoded = LlmChatResult::decode(bytes.as_slice()).unwrap();
        if let Some(content) = decoded.content {
            if let Some(message_content::Content::Text(text)) = content.content {
                assert_eq!(text, "Hello, world!");
            } else {
                panic!("Expected text content");
            }
        }
    }

    #[tokio::test]
    async fn test_llm_chat_collect_stream_tool_calls_precedence() {
        let runner = LLMChatRunnerSpecImpl::new();
        let text_chunk = create_text_chat_result("Some text", None, false);
        let tool_call = message_content::ToolCall {
            call_id: "call_1".to_string(),
            fn_name: "get_weather".to_string(),
            fn_arguments: r#"{"location": "Tokyo"}"#.to_string(),
        };
        let tool_chunk = create_tool_calls_chat_result(vec![tool_call.clone()], true);

        let items = vec![
            create_data_item(&text_chunk),
            create_data_item(&tool_chunk),
            create_end_item(HashMap::new()),
        ];
        let stream = stream::iter(items).boxed();

        let (bytes, _) = runner.collect_stream(stream).await.unwrap();

        let decoded = LlmChatResult::decode(bytes.as_slice()).unwrap();
        if let Some(content) = decoded.content {
            if let Some(message_content::Content::ToolCalls(tc)) = content.content {
                assert_eq!(tc.calls.len(), 1);
                assert_eq!(tc.calls[0].fn_name, "get_weather");
            } else {
                panic!("Expected tool_calls content");
            }
        } else {
            panic!("Expected content");
        }
    }

    #[tokio::test]
    async fn test_llm_chat_collect_stream_multiple_tool_calls() {
        let runner = LLMChatRunnerSpecImpl::new();
        let tool1 = message_content::ToolCall {
            call_id: "call_1".to_string(),
            fn_name: "get_weather".to_string(),
            fn_arguments: r#"{"location": "Tokyo"}"#.to_string(),
        };
        let tool2 = message_content::ToolCall {
            call_id: "call_2".to_string(),
            fn_name: "get_time".to_string(),
            fn_arguments: r#"{"timezone": "JST"}"#.to_string(),
        };
        let chunk1 = create_tool_calls_chat_result(vec![tool1], false);
        let chunk2 = create_tool_calls_chat_result(vec![tool2], true);

        let items = vec![
            create_data_item(&chunk1),
            create_data_item(&chunk2),
            create_end_item(HashMap::new()),
        ];
        let stream = stream::iter(items).boxed();

        let (bytes, _) = runner.collect_stream(stream).await.unwrap();

        let decoded = LlmChatResult::decode(bytes.as_slice()).unwrap();
        if let Some(content) = decoded.content {
            if let Some(message_content::Content::ToolCalls(tc)) = content.content {
                assert_eq!(tc.calls.len(), 2);
                assert_eq!(tc.calls[0].fn_name, "get_weather");
                assert_eq!(tc.calls[1].fn_name, "get_time");
            } else {
                panic!("Expected tool_calls content");
            }
        }
    }

    #[tokio::test]
    async fn test_llm_chat_collect_stream_concatenates_reasoning() {
        let runner = LLMChatRunnerSpecImpl::new();
        let chunk1 = create_text_chat_result("", Some("Step 1. "), false);
        let chunk2 = create_text_chat_result("", Some("Step 2."), false);
        let chunk3 = create_text_chat_result("Answer", None, true);

        let items = vec![
            create_data_item(&chunk1),
            create_data_item(&chunk2),
            create_data_item(&chunk3),
            create_end_item(HashMap::new()),
        ];
        let stream = stream::iter(items).boxed();

        let (bytes, _) = runner.collect_stream(stream).await.unwrap();

        let decoded = LlmChatResult::decode(bytes.as_slice()).unwrap();
        assert_eq!(
            decoded.reasoning_content,
            Some("Step 1. Step 2.".to_string())
        );
    }

    #[tokio::test]
    async fn test_llm_chat_collect_stream_final_collected_takes_precedence() {
        let runner = LLMChatRunnerSpecImpl::new();
        let chunk = create_text_chat_result("intermediate", None, false);
        let final_result = create_text_chat_result("final result", None, true);

        let mut metadata = HashMap::new();
        metadata.insert("key".to_string(), "value".to_string());

        let items = vec![
            create_data_item(&chunk),
            create_final_collected_item(final_result.encode_to_vec()),
            create_end_item(metadata.clone()),
        ];
        let stream = stream::iter(items).boxed();

        let (bytes, returned_metadata) = runner.collect_stream(stream).await.unwrap();

        let decoded = LlmChatResult::decode(bytes.as_slice()).unwrap();
        if let Some(content) = decoded.content {
            if let Some(message_content::Content::Text(text)) = content.content {
                assert_eq!(text, "final result");
            }
        }
        assert_eq!(returned_metadata.get("key"), Some(&"value".to_string()));
    }

    #[tokio::test]
    async fn test_llm_chat_collect_stream_empty_returns_empty_content() {
        let runner = LLMChatRunnerSpecImpl::new();

        let items = vec![create_end_item(HashMap::new())];
        let stream = stream::iter(items).boxed();

        let (bytes, _) = runner.collect_stream(stream).await.unwrap();

        let decoded = LlmChatResult::decode(bytes.as_slice()).unwrap();
        assert!(decoded.content.is_none());
        assert!(decoded.done);
    }
}
