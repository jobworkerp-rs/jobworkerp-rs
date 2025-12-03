//! Result conversion for MistralRS plugin
//!
//! Ported from app-wrapper/src/llm/mistral/result.rs

use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::{
    message_content::{Content as ChatContent, ToolCall, ToolCalls},
    MessageContent as ChatMessageContent, Usage as ChatUsage,
};
use jobworkerp_runner::jobworkerp::runner::llm::llm_completion_result::{
    message_content::Content as CompletionContent, MessageContent as CompletionMessageContent,
    Usage as CompletionUsage,
};
use jobworkerp_runner::jobworkerp::runner::llm::{LlmChatResult, LlmCompletionResult};

/// Result converter for MistralRS responses
pub struct ResultConverter;

impl ResultConverter {
    pub fn convert_chat_completion_result(
        llm_response: &mistralrs::ChatCompletionResponse,
    ) -> LlmChatResult {
        let (content, done) = if let Some(first_choice) = llm_response.choices.first() {
            let content = if let Some(tool_calls) = &first_choice.message.tool_calls {
                if !tool_calls.is_empty() {
                    let converted_tool_calls = tool_calls
                        .iter()
                        .map(|tc| ToolCall {
                            call_id: tc.id.clone(),
                            fn_name: tc.function.name.clone(),
                            fn_arguments: tc.function.arguments.clone(),
                        })
                        .collect();

                    Some(ChatMessageContent {
                        content: Some(ChatContent::ToolCalls(ToolCalls {
                            calls: converted_tool_calls,
                        })),
                    })
                } else {
                    None
                }
            } else {
                first_choice
                    .message
                    .content
                    .as_ref()
                    .map(|text_content| ChatMessageContent {
                        content: Some(ChatContent::Text(text_content.clone())),
                    })
            };

            let done = first_choice.finish_reason == "stop"
                || first_choice.finish_reason == "length"
                || first_choice.finish_reason == "canceled";

            (content, done)
        } else {
            (None, true)
        };

        let usage = Some(ChatUsage {
            model: llm_response.model.clone(),
            prompt_tokens: Some(llm_response.usage.prompt_tokens as u32),
            completion_tokens: Some(llm_response.usage.completion_tokens as u32),
            total_prompt_time_sec: Some(llm_response.usage.total_prompt_time_sec),
            total_completion_time_sec: Some(llm_response.usage.total_completion_time_sec),
        });

        LlmChatResult {
            content,
            reasoning_content: None,
            done,
            usage,
        }
    }

    pub fn is_chat_finished(llm_response: &mistralrs::ChatCompletionChunkResponse) -> bool {
        llm_response
            .choices
            .iter()
            .any(|c| c.finish_reason.is_some())
    }

    pub fn convert_chat_completion_chunk_result(
        llm_response: &mistralrs::ChatCompletionChunkResponse,
    ) -> LlmChatResult {
        let (content, done) = if let Some(first_choice) = llm_response.choices.first() {
            let content = if let Some(tool_calls) = &first_choice.delta.tool_calls {
                if !tool_calls.is_empty() {
                    let converted_delta_tool_calls = tool_calls
                        .iter()
                        .map(|tc| ToolCall {
                            call_id: tc.id.clone(),
                            fn_name: tc.function.name.clone(),
                            fn_arguments: tc.function.arguments.clone(),
                        })
                        .collect();

                    Some(ChatMessageContent {
                        content: Some(ChatContent::ToolCalls(ToolCalls {
                            calls: converted_delta_tool_calls,
                        })),
                    })
                } else {
                    None
                }
            } else {
                first_choice
                    .delta
                    .content
                    .as_ref()
                    .map(|text_content| ChatMessageContent {
                        content: Some(ChatContent::Text(text_content.clone())),
                    })
            };

            let done = first_choice.finish_reason.as_ref().is_some_and(|reason| {
                reason == "stop" || reason == "length" || reason == "canceled"
            });

            (content, done)
        } else {
            (None, true)
        };

        let usage = llm_response.usage.as_ref().map(|u| ChatUsage {
            model: llm_response.model.clone(),
            prompt_tokens: Some(u.prompt_tokens as u32),
            completion_tokens: Some(u.completion_tokens as u32),
            total_prompt_time_sec: Some(u.total_prompt_time_sec),
            total_completion_time_sec: Some(u.total_completion_time_sec),
        });

        LlmChatResult {
            content,
            reasoning_content: None,
            done,
            usage,
        }
    }

    pub fn is_completion_finished(llm_response: &mistralrs::CompletionChunkResponse) -> bool {
        llm_response
            .choices
            .iter()
            .any(|c| c.finish_reason.is_some())
    }

    pub fn convert_completion_chunk_result(
        llm_response: &mistralrs::CompletionChunkResponse,
    ) -> LlmCompletionResult {
        let (content, done) = if let Some(first_choice) = llm_response.choices.first() {
            let content = if !first_choice.text.is_empty() {
                Some(CompletionMessageContent {
                    content: Some(CompletionContent::Text(first_choice.text.clone())),
                })
            } else {
                None
            };

            let done = first_choice.finish_reason.as_ref().is_some_and(|reason| {
                reason == "stop" || reason == "length" || reason == "canceled"
            });

            (content, done)
        } else {
            (None, true)
        };

        LlmCompletionResult {
            content,
            reasoning_content: None,
            done,
            context: None,
            usage: None,
        }
    }

    pub fn convert_completion_result(
        llm_response: &mistralrs::CompletionResponse,
    ) -> LlmCompletionResult {
        let (content, done) = if let Some(first_choice) = llm_response.choices.first() {
            let content = if !first_choice.text.is_empty() {
                Some(CompletionMessageContent {
                    content: Some(CompletionContent::Text(first_choice.text.clone())),
                })
            } else {
                None
            };

            let done = first_choice.finish_reason == "stop"
                || first_choice.finish_reason == "length"
                || first_choice.finish_reason == "canceled";

            (content, done)
        } else {
            (None, true)
        };

        let usage = Some(CompletionUsage {
            model: llm_response.model.clone(),
            prompt_tokens: Some(llm_response.usage.prompt_tokens as u32),
            completion_tokens: Some(llm_response.usage.completion_tokens as u32),
            total_prompt_time_sec: Some(llm_response.usage.total_prompt_time_sec),
            total_completion_time_sec: Some(llm_response.usage.total_completion_time_sec),
        });

        LlmCompletionResult {
            content,
            reasoning_content: None,
            done,
            context: None,
            usage,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mistralrs::{
        ChatCompletionChunkResponse, ChatCompletionResponse, Choice, ChunkChoice, CompletionChoice,
        CompletionChunkChoice, CompletionChunkResponse, CompletionResponse, Delta, ResponseMessage,
        Usage,
    };

    fn create_mock_usage() -> Usage {
        Usage {
            prompt_tokens: 10,
            completion_tokens: 20,
            total_tokens: 30,
            avg_tok_per_sec: 75.0,
            avg_prompt_tok_per_sec: 100.0,
            avg_compl_tok_per_sec: 50.0,
            total_prompt_time_sec: 0.1,
            total_completion_time_sec: 0.4,
            total_time_sec: 0.5,
        }
    }

    #[test]
    fn test_convert_chat_completion_result_with_text() {
        let response = ChatCompletionResponse {
            id: "test-id".to_string(),
            choices: vec![Choice {
                message: ResponseMessage {
                    content: Some("Hello, world!".to_string()),
                    role: "assistant".to_string(),
                    tool_calls: None,
                },
                finish_reason: "stop".to_string(),
                index: 0,
                logprobs: None,
            }],
            created: 0,
            model: "test-model".to_string(),
            system_fingerprint: "test".to_string(),
            object: "chat.completion".to_string(),
            usage: create_mock_usage(),
        };

        let result = ResultConverter::convert_chat_completion_result(&response);

        assert!(result.done);
        assert!(result.content.is_some());
        if let Some(content) = result.content {
            if let Some(ChatContent::Text(text)) = content.content {
                assert_eq!(text, "Hello, world!");
            } else {
                panic!("Expected text content");
            }
        }
        assert!(result.usage.is_some());
        let usage = result.usage.unwrap();
        assert_eq!(usage.prompt_tokens, Some(10));
        assert_eq!(usage.completion_tokens, Some(20));
    }

    #[test]
    fn test_convert_chat_completion_result_with_tool_calls() {
        let response = ChatCompletionResponse {
            id: "test-id".to_string(),
            choices: vec![Choice {
                message: ResponseMessage {
                    content: None,
                    role: "assistant".to_string(),
                    tool_calls: Some(vec![mistralrs::ToolCallResponse {
                        index: 0,
                        id: "call-1".to_string(),
                        function: mistralrs::CalledFunction {
                            name: "get_weather".to_string(),
                            arguments: r#"{"city": "Tokyo"}"#.to_string(),
                        },
                        tp: mistralrs::ToolCallType::Function,
                    }]),
                },
                finish_reason: "tool_calls".to_string(),
                index: 0,
                logprobs: None,
            }],
            created: 0,
            model: "test-model".to_string(),
            system_fingerprint: "test".to_string(),
            object: "chat.completion".to_string(),
            usage: create_mock_usage(),
        };

        let result = ResultConverter::convert_chat_completion_result(&response);

        assert!(!result.done);
        assert!(result.content.is_some());
        if let Some(content) = result.content {
            if let Some(ChatContent::ToolCalls(tool_calls)) = content.content {
                assert_eq!(tool_calls.calls.len(), 1);
                assert_eq!(tool_calls.calls[0].fn_name, "get_weather");
                assert_eq!(tool_calls.calls[0].call_id, "call-1");
            } else {
                panic!("Expected tool calls content");
            }
        }
    }

    #[test]
    fn test_convert_chat_completion_result_empty_choices() {
        let response = ChatCompletionResponse {
            id: "test-id".to_string(),
            choices: vec![],
            created: 0,
            model: "test-model".to_string(),
            system_fingerprint: "test".to_string(),
            object: "chat.completion".to_string(),
            usage: create_mock_usage(),
        };

        let result = ResultConverter::convert_chat_completion_result(&response);

        assert!(result.done);
        assert!(result.content.is_none());
    }

    #[test]
    fn test_convert_chat_completion_chunk_result() {
        let chunk = ChatCompletionChunkResponse {
            id: "test-id".to_string(),
            choices: vec![ChunkChoice {
                delta: Delta {
                    content: Some("Hello".to_string()),
                    role: "assistant".to_string(),
                    tool_calls: None,
                },
                finish_reason: None,
                index: 0,
                logprobs: None,
            }],
            created: 0,
            model: "test-model".to_string(),
            system_fingerprint: "test".to_string(),
            object: "chat.completion.chunk".to_string(),
            usage: None,
        };

        let result = ResultConverter::convert_chat_completion_chunk_result(&chunk);

        assert!(!result.done);
        assert!(result.content.is_some());
        if let Some(content) = result.content {
            if let Some(ChatContent::Text(text)) = content.content {
                assert_eq!(text, "Hello");
            } else {
                panic!("Expected text content");
            }
        }
    }

    #[test]
    fn test_is_chat_finished() {
        let not_finished = ChatCompletionChunkResponse {
            id: "test-id".to_string(),
            choices: vec![ChunkChoice {
                delta: Delta {
                    content: Some("text".to_string()),
                    role: String::new(),
                    tool_calls: None,
                },
                finish_reason: None,
                index: 0,
                logprobs: None,
            }],
            created: 0,
            model: "test-model".to_string(),
            system_fingerprint: "test".to_string(),
            object: "chat.completion.chunk".to_string(),
            usage: None,
        };

        assert!(!ResultConverter::is_chat_finished(&not_finished));

        let finished = ChatCompletionChunkResponse {
            id: "test-id".to_string(),
            choices: vec![ChunkChoice {
                delta: Delta {
                    content: None,
                    role: String::new(),
                    tool_calls: None,
                },
                finish_reason: Some("stop".to_string()),
                index: 0,
                logprobs: None,
            }],
            created: 0,
            model: "test-model".to_string(),
            system_fingerprint: "test".to_string(),
            object: "chat.completion.chunk".to_string(),
            usage: None,
        };

        assert!(ResultConverter::is_chat_finished(&finished));
    }

    #[test]
    fn test_convert_completion_result() {
        let response = CompletionResponse {
            id: "test-id".to_string(),
            choices: vec![CompletionChoice {
                text: "Once upon a time".to_string(),
                finish_reason: "stop".to_string(),
                index: 0,
                logprobs: None,
            }],
            created: 0,
            model: "test-model".to_string(),
            system_fingerprint: "test".to_string(),
            object: "text_completion".to_string(),
            usage: create_mock_usage(),
        };

        let result = ResultConverter::convert_completion_result(&response);

        assert!(result.done);
        assert!(result.content.is_some());
        if let Some(content) = result.content {
            if let Some(CompletionContent::Text(text)) = content.content {
                assert_eq!(text, "Once upon a time");
            } else {
                panic!("Expected text content");
            }
        }
        assert!(result.usage.is_some());
    }

    #[test]
    fn test_convert_completion_chunk_result() {
        let chunk = CompletionChunkResponse {
            id: "test-id".to_string(),
            choices: vec![CompletionChunkChoice {
                text: "Hello".to_string(),
                finish_reason: None,
                index: 0,
                logprobs: None,
            }],
            created: 0,
            model: "test-model".to_string(),
            system_fingerprint: "test".to_string(),
            object: "text_completion.chunk".to_string(),
        };

        let result = ResultConverter::convert_completion_chunk_result(&chunk);

        assert!(!result.done);
        assert!(result.content.is_some());
    }

    #[test]
    fn test_is_completion_finished() {
        let not_finished = CompletionChunkResponse {
            id: "test-id".to_string(),
            choices: vec![CompletionChunkChoice {
                text: "text".to_string(),
                finish_reason: None,
                index: 0,
                logprobs: None,
            }],
            created: 0,
            model: "test-model".to_string(),
            system_fingerprint: "test".to_string(),
            object: "text_completion.chunk".to_string(),
        };

        assert!(!ResultConverter::is_completion_finished(&not_finished));

        let finished = CompletionChunkResponse {
            id: "test-id".to_string(),
            choices: vec![CompletionChunkChoice {
                text: "".to_string(),
                finish_reason: Some("stop".to_string()),
                index: 0,
                logprobs: None,
            }],
            created: 0,
            model: "test-model".to_string(),
            system_fingerprint: "test".to_string(),
            object: "text_completion.chunk".to_string(),
        };

        assert!(ResultConverter::is_completion_finished(&finished));
    }
}
