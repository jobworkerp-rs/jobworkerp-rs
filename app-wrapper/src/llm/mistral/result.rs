#![cfg(feature = "local_llm")]

use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::{
    message_content::{Content as ChatContent, ToolCall, ToolCalls},
    MessageContent as ChatMessageContent, Usage as ChatUsage,
};
use jobworkerp_runner::jobworkerp::runner::llm::llm_completion_result::{
    message_content::Content as CompletionContent, MessageContent as CompletionMessageContent,
    Usage as CompletionUsage,
};
use jobworkerp_runner::jobworkerp::runner::llm::{LlmChatResult, LlmCompletionResult};

pub trait LLMResultConverter {
    fn convert_chat_completion_result(
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
            reasoning_content: None, // mistralrs doesn't provide reasoning content in standard responses
            done,
            usage,
        }
    }
    fn is_chat_finished(llm_response: &mistralrs::ChatCompletionChunkResponse) -> bool {
        llm_response
            .choices
            .iter()
            .any(|c| c.finish_reason.is_some())
    }

    // for streaming responses
    fn convert_chat_completion_chunk_result(
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
            reasoning_content: None, // mistralrs doesn't provide reasoning content in chunk responses
            done,
            usage,
        }
    }
    fn is_completion_finished(llm_response: &mistralrs::CompletionChunkResponse) -> bool {
        llm_response
            .choices
            .iter()
            .any(|c| c.finish_reason.is_some())
    }
    // completion chunk result conversion
    fn convert_completion_chunk_result(
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

        // Note: mistralrs CompletionChunkResponse doesn't include usage stats
        let usage = None;

        LlmCompletionResult {
            content,
            reasoning_content: None, // mistralrs doesn't provide reasoning content in completion responses
            done,
            context: None, // mistralrs doesn't provide context in chunk responses
            usage,
        }
    }

    fn convert_completion_result(
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
            reasoning_content: None, // mistralrs doesn't provide reasoning content
            done,
            context: None, // mistralrs doesn't provide context in standard responses
            usage,
        }
    }
}

pub struct DefaultLLMResultConverter;
impl LLMResultConverter for DefaultLLMResultConverter {}
