use super::{CollectStreamFuture, RunnerSpec};
use crate::jobworkerp::runner::llm::LlmCompletionResult;
use futures::stream::BoxStream;
use futures::StreamExt;
use prost::Message;
use proto::jobworkerp::data::{ResultOutputItem, RunnerType};
use proto::DEFAULT_METHOD_NAME;
use std::collections::HashMap;

pub struct LLMCompletionRunnerSpecImpl {}

impl LLMCompletionRunnerSpecImpl {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for LLMCompletionRunnerSpecImpl {
    fn default() -> Self {
        Self::new()
    }
}

pub trait LLMCompletionRunnerSpec {
    fn name(&self) -> String {
        RunnerType::LlmCompletion.as_str_name().to_string()
    }
    fn runner_settings_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/llm/runner.proto").to_string()
    }
    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        let mut schemas = HashMap::new();
        schemas.insert(
            DEFAULT_METHOD_NAME.to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: include_str!(
                    "../../protobuf/jobworkerp/runner/llm/completion_args.proto"
                )
                .to_string(),
                result_proto: include_str!(
                    "../../protobuf/jobworkerp/runner/llm/completion_result.proto"
                )
                .to_string(),
                description: Some("Generate text completion using LLM".to_string()),
                output_type: proto::jobworkerp::data::StreamingOutputType::Both as i32,
            },
        );
        schemas
    }

    // Reason: Protobuf oneof fields in GenerationContext require oneOf constraints
    fn method_json_schema_map(&self) -> HashMap<String, super::MethodJsonSchema> {
        let mut schemas = HashMap::new();
        schemas.insert(
            DEFAULT_METHOD_NAME.to_string(),
            super::MethodJsonSchema {
                args_schema: include_str!("../../schema/llm/LLMCompletionArgs.json").to_string(),
                result_schema: Some(
                    include_str!("../../schema/llm/LLMCompletionResult.json").to_string(),
                ),
            },
        );
        schemas
    }

    fn settings_schema(&self) -> String {
        include_str!("../../schema/llm/LLMRunnerSettings.json").to_string()
    }
}

impl LLMCompletionRunnerSpec for LLMCompletionRunnerSpecImpl {}

impl RunnerSpec for LLMCompletionRunnerSpecImpl {
    fn name(&self) -> String {
        LLMCompletionRunnerSpec::name(self)
    }

    fn runner_settings_proto(&self) -> String {
        LLMCompletionRunnerSpec::runner_settings_proto(self)
    }

    fn method_proto_map(
        &self,
    ) -> std::collections::HashMap<String, proto::jobworkerp::data::MethodSchema> {
        LLMCompletionRunnerSpec::method_proto_map(self)
    }

    fn method_json_schema_map(&self) -> HashMap<String, super::MethodJsonSchema> {
        LLMCompletionRunnerSpec::method_json_schema_map(self)
    }

    fn settings_schema(&self) -> String {
        LLMCompletionRunnerSpec::settings_schema(self)
    }

    /// Collect streaming LLM completion results into a single LlmCompletionResult
    ///
    /// Strategy:
    /// - Concatenates text content from all chunks
    /// - Concatenates reasoning content from all chunks
    /// - Uses context and usage from the final chunk (done=true)
    /// - Returns error if data items were received but all decodes failed
    fn collect_stream(
        &self,
        stream: BoxStream<'static, ResultOutputItem>,
        _using: Option<&str>,
    ) -> CollectStreamFuture {
        use crate::jobworkerp::runner::llm::llm_completion_result::{
            message_content, GenerationContext, MessageContent, Usage,
        };
        use proto::jobworkerp::data::result_output_item;

        Box::pin(async move {
            let mut combined_text = String::new();
            let mut combined_reasoning = String::new();
            let mut final_context: Option<GenerationContext> = None;
            let mut final_usage: Option<Usage> = None;
            let mut metadata = HashMap::new();
            let mut stream = stream;
            let mut final_collected: Option<Vec<u8>> = None;
            let mut decode_failure_count = 0;
            let mut data_item_count = 0;
            let mut successful_decode_count = 0;

            while let Some(item) = stream.next().await {
                match item.item {
                    Some(result_output_item::Item::Data(data)) => {
                        data_item_count += 1;
                        match LlmCompletionResult::decode(data.as_slice()) {
                            Ok(chunk) => {
                                successful_decode_count += 1;
                                // Concatenate text content
                                if let Some(content) = chunk.content {
                                    if let Some(message_content::Content::Text(text)) =
                                        content.content
                                    {
                                        combined_text.push_str(&text);
                                    }
                                }

                                // Concatenate reasoning content
                                if let Some(reasoning) = chunk.reasoning_content {
                                    combined_reasoning.push_str(&reasoning);
                                }

                                // Use final chunk's context and usage
                                if chunk.done {
                                    final_context = chunk.context;
                                    final_usage = chunk.usage;
                                }
                            }
                            Err(e) => {
                                decode_failure_count += 1;
                                tracing::warn!(
                                    "Failed to decode LlmCompletionResult in collect_stream (chunk {}/{}): {:?}",
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
                        final_collected = Some(data);
                    }
                    None => {}
                }
            }

            if let Some(data) = final_collected {
                return Ok((data, metadata));
            }

            // Return error if we received data items but all decodes failed
            if data_item_count > 0 && successful_decode_count == 0 {
                return Err(anyhow::anyhow!(
                    "All {} LlmCompletionResult decode attempts failed in collect_stream",
                    decode_failure_count
                ));
            }

            // Build collected result
            let result = LlmCompletionResult {
                content: if combined_text.is_empty() {
                    None
                } else {
                    Some(MessageContent {
                        content: Some(message_content::Content::Text(combined_text)),
                    })
                },
                reasoning_content: if combined_reasoning.is_empty() {
                    None
                } else {
                    Some(combined_reasoning)
                },
                done: true,
                context: final_context,
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
    use crate::jobworkerp::runner::llm::llm_completion_result::{
        message_content, GenerationContext, MessageContent, Usage,
    };
    use futures::stream;
    use proto::jobworkerp::data::{result_output_item, Trailer};

    fn create_completion_result(
        text: &str,
        reasoning: Option<&str>,
        done: bool,
        context: Option<GenerationContext>,
        usage: Option<Usage>,
    ) -> LlmCompletionResult {
        LlmCompletionResult {
            content: if text.is_empty() {
                None
            } else {
                Some(MessageContent {
                    content: Some(message_content::Content::Text(text.to_string())),
                })
            },
            reasoning_content: reasoning.map(|r| r.to_string()),
            done,
            context,
            usage,
        }
    }

    fn create_data_item(result: &LlmCompletionResult) -> ResultOutputItem {
        ResultOutputItem {
            item: Some(result_output_item::Item::Data(result.encode_to_vec())),
        }
    }

    fn create_end_item(metadata: HashMap<String, String>) -> ResultOutputItem {
        ResultOutputItem {
            item: Some(result_output_item::Item::End(Trailer { metadata })),
        }
    }

    fn create_final_collected_item(data: Vec<u8>) -> ResultOutputItem {
        ResultOutputItem {
            item: Some(result_output_item::Item::FinalCollected(data)),
        }
    }

    #[tokio::test]
    async fn test_llm_completion_collect_stream_single_chunk() {
        let runner = LLMCompletionRunnerSpecImpl::new();
        let result = create_completion_result("Hello, world!", None, true, None, None);

        let items = vec![create_data_item(&result), create_end_item(HashMap::new())];
        let stream = stream::iter(items).boxed();

        let (bytes, _) = runner.collect_stream(stream, None).await.unwrap();

        let decoded = LlmCompletionResult::decode(bytes.as_slice()).unwrap();
        assert!(decoded.done);
        if let Some(content) = decoded.content {
            if let Some(message_content::Content::Text(text)) = content.content {
                assert_eq!(text, "Hello, world!");
            } else {
                panic!("Expected text content");
            }
        } else {
            panic!("Expected content");
        }
    }

    #[tokio::test]
    async fn test_llm_completion_collect_stream_concatenates_text() {
        let runner = LLMCompletionRunnerSpecImpl::new();
        let chunk1 = create_completion_result("Hello, ", None, false, None, None);
        let chunk2 = create_completion_result("world", None, false, None, None);
        let chunk3 = create_completion_result(
            "!",
            None,
            true,
            None,
            Some(Usage {
                prompt_tokens: Some(10),
                completion_tokens: Some(5),
                model: String::new(),
                total_prompt_time_sec: None,
                total_completion_time_sec: None,
            }),
        );

        let items = vec![
            create_data_item(&chunk1),
            create_data_item(&chunk2),
            create_data_item(&chunk3),
            create_end_item(HashMap::new()),
        ];
        let stream = stream::iter(items).boxed();

        let (bytes, _) = runner.collect_stream(stream, None).await.unwrap();

        let decoded = LlmCompletionResult::decode(bytes.as_slice()).unwrap();
        assert!(decoded.done);
        if let Some(content) = decoded.content {
            if let Some(message_content::Content::Text(text)) = content.content {
                assert_eq!(text, "Hello, world!");
            } else {
                panic!("Expected text content");
            }
        }
        assert!(decoded.usage.is_some());
        let usage = decoded.usage.unwrap();
        assert_eq!(usage.prompt_tokens, Some(10));
        assert_eq!(usage.completion_tokens, Some(5));
    }

    #[tokio::test]
    async fn test_llm_completion_collect_stream_concatenates_reasoning() {
        let runner = LLMCompletionRunnerSpecImpl::new();
        let chunk1 = create_completion_result("", Some("First thought. "), false, None, None);
        let chunk2 = create_completion_result("", Some("Second thought."), false, None, None);
        let chunk3 = create_completion_result("Final answer", None, true, None, None);

        let items = vec![
            create_data_item(&chunk1),
            create_data_item(&chunk2),
            create_data_item(&chunk3),
            create_end_item(HashMap::new()),
        ];
        let stream = stream::iter(items).boxed();

        let (bytes, _) = runner.collect_stream(stream, None).await.unwrap();

        let decoded = LlmCompletionResult::decode(bytes.as_slice()).unwrap();
        assert_eq!(
            decoded.reasoning_content,
            Some("First thought. Second thought.".to_string())
        );
    }

    #[tokio::test]
    async fn test_llm_completion_collect_stream_final_collected_takes_precedence() {
        let runner = LLMCompletionRunnerSpecImpl::new();
        let chunk = create_completion_result("intermediate", None, false, None, None);
        let final_result = create_completion_result("final result", None, true, None, None);

        let mut metadata = HashMap::new();
        metadata.insert("key".to_string(), "value".to_string());

        let items = vec![
            create_data_item(&chunk),
            create_final_collected_item(final_result.encode_to_vec()),
            create_end_item(metadata.clone()),
        ];
        let stream = stream::iter(items).boxed();

        let (bytes, returned_metadata) = runner.collect_stream(stream, None).await.unwrap();

        let decoded = LlmCompletionResult::decode(bytes.as_slice()).unwrap();
        if let Some(content) = decoded.content {
            if let Some(message_content::Content::Text(text)) = content.content {
                assert_eq!(text, "final result");
            }
        }
        assert_eq!(returned_metadata.get("key"), Some(&"value".to_string()));
    }

    #[tokio::test]
    async fn test_llm_completion_collect_stream_empty_returns_empty_content() {
        let runner = LLMCompletionRunnerSpecImpl::new();

        let items = vec![create_end_item(HashMap::new())];
        let stream = stream::iter(items).boxed();

        let (bytes, _) = runner.collect_stream(stream, None).await.unwrap();

        let decoded = LlmCompletionResult::decode(bytes.as_slice()).unwrap();
        assert!(decoded.content.is_none());
        assert!(decoded.done);
    }
}
