use jobworkerp_runner::jobworkerp::runner::llm_result::{
    self, ChatCompletionResponse, Choice, Logprobs, ResponseLogprob, ResponseMessage,
    ToolCallResponse, TopLogprob, Usage,
};
use jobworkerp_runner::jobworkerp::runner::LlmResult;

pub trait LLMResultConverter {
    fn convert_chat_completion_result(
        llm_response: &mistralrs::ChatCompletionResponse,
    ) -> LlmResult {
        // Convert choices
        let choices = llm_response
            .choices
            .iter()
            .map(|choice| {
                // Convert message
                let message = ResponseMessage {
                    content: choice.message.content.clone(),
                    role: choice.message.role.clone(),
                    tool_calls: choice
                        .message
                        .tool_calls
                        .iter()
                        .map(|tc| ToolCallResponse {
                            id: tc.id.clone(),
                            r#type: tc.tp.to_string().clone(),
                            function_name: tc.function.name.clone(),
                            function_arguments: tc.function.arguments.clone(),
                        })
                        .collect(),
                };

                // Convert logprobs if present
                let logprobs = choice.logprobs.as_ref().map(|lp| Logprobs {
                    content: lp
                        .content
                        .as_ref()
                        .map(|content| {
                            content
                                .iter()
                                .map(|logprob| ResponseLogprob {
                                    token: logprob.token.clone(),
                                    logprob: logprob.logprob,
                                    bytes: logprob.bytes.clone(),
                                    top_logprobs: logprob
                                        .top_logprobs
                                        .iter()
                                        .map(|top| TopLogprob {
                                            token: top.token.clone(),
                                            logprob: top.logprob,
                                            bytes: top.bytes.clone(),
                                        })
                                        .collect(),
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                });

                Choice {
                    finish_reason: match choice.finish_reason.as_str() {
                        "stop" => llm_result::FinishReason::Stop as i32,
                        "length" => llm_result::FinishReason::Length as i32,
                        "canceled" => llm_result::FinishReason::Canceled as i32,
                        "generated-image" => llm_result::FinishReason::GeneratedImage as i32,
                        _ => llm_result::FinishReason::Unknown as i32,
                    },
                    index: choice.index as u32,
                    message: Some(message),
                    logprobs,
                }
            })
            .collect();

        // Convert usage stats
        let usage = Usage {
            completion_tokens: llm_response.usage.completion_tokens as u32,
            prompt_tokens: llm_response.usage.prompt_tokens as u32,
            total_tokens: llm_response.usage.total_tokens as u32,
            avg_tok_per_sec: llm_response.usage.avg_tok_per_sec,
            avg_prompt_tok_per_sec: llm_response.usage.avg_prompt_tok_per_sec,
            avg_compl_tok_per_sec: llm_response.usage.avg_compl_tok_per_sec,
            total_time_sec: llm_response.usage.total_time_sec,
            total_prompt_time_sec: llm_response.usage.total_prompt_time_sec,
            total_completion_time_sec: llm_response.usage.total_completion_time_sec,
        };

        // Create chat completion response
        let chat_completion = ChatCompletionResponse {
            id: llm_response.id.clone(),
            choices,
            created: llm_response.created,
            model: llm_response.model.clone(),
            system_fingerprint: llm_response.system_fingerprint.clone(),
            object: llm_response.object.clone(),
            usage: Some(usage),
        };

        // Create final LlmResult with ChatCompletionResponse
        LlmResult {
            result: Some(llm_result::Result::ChatCompletion(chat_completion)),
        }
    }
    fn is_chat_finished(llm_response: &mistralrs::ChatCompletionChunkResponse) -> bool {
        llm_response
            .choices
            .iter()
            .any(|c| c.finish_reason.is_some())
    }
    fn convert_chat_completion_chunk_result(
        llm_response: &mistralrs::ChatCompletionChunkResponse,
    ) -> LlmResult {
        // Convert choices
        let choices = llm_response
            .choices
            .iter()
            .map(|choice| {
                // Convert delta message
                let delta = llm_result::Delta {
                    content: choice.delta.content.clone(),
                    role: choice.delta.role.clone(),
                    tool_calls: choice
                        .delta
                        .tool_calls
                        .as_ref()
                        .map(|tool_calls| {
                            tool_calls
                                .iter()
                                .map(|tc| ToolCallResponse {
                                    id: tc.id.clone(),
                                    r#type: tc.tp.to_string(),
                                    function_name: tc.function.name.clone(),
                                    function_arguments: tc.function.arguments.clone(),
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                };

                // Convert logprobs if present
                let logprobs = choice.logprobs.as_ref().map(|logprob| ResponseLogprob {
                    token: logprob.token.clone(),
                    logprob: logprob.logprob,
                    bytes: logprob.bytes.clone().map(|b| b),
                    top_logprobs: logprob
                        .top_logprobs
                        .iter()
                        .map(|top| TopLogprob {
                            token: top.token.clone(),
                            logprob: top.logprob,
                            bytes: top.bytes.clone(),
                        })
                        .collect(),
                });

                llm_result::ChunkChoice {
                    finish_reason: choice.finish_reason.as_ref().map(|c| match c.as_str() {
                        "stop" => llm_result::FinishReason::Stop as i32,
                        "length" => llm_result::FinishReason::Length as i32,
                        "canceled" => llm_result::FinishReason::Canceled as i32,
                        "generated-image" => llm_result::FinishReason::GeneratedImage as i32,
                        _ => llm_result::FinishReason::Unknown as i32,
                    }),
                    index: choice.index as u32,
                    delta: Some(delta),
                    logprobs,
                }
            })
            .collect();

        // Convert usage stats if present
        let usage = llm_response.usage.as_ref().map(|u| Usage {
            completion_tokens: u.completion_tokens as u32,
            prompt_tokens: u.prompt_tokens as u32,
            total_tokens: u.total_tokens as u32,
            avg_tok_per_sec: u.avg_tok_per_sec,
            avg_prompt_tok_per_sec: u.avg_prompt_tok_per_sec,
            avg_compl_tok_per_sec: u.avg_compl_tok_per_sec,
            total_time_sec: u.total_time_sec,
            total_prompt_time_sec: u.total_prompt_time_sec,
            total_completion_time_sec: u.total_completion_time_sec,
        });

        // Create chat completion chunk response
        let chat_completion_chunk = llm_result::ChatCompletionChunkResponse {
            id: llm_response.id.clone(),
            choices,
            created: llm_response.created as u64, // XXX u128 to u64 (nanoseconds?)
            model: llm_response.model.clone(),
            system_fingerprint: llm_response.system_fingerprint.clone(),
            object: llm_response.object.clone(),
            usage,
        };

        // Create final LlmResult with ChatCompletionChunkResponse
        LlmResult {
            result: Some(llm_result::Result::ChatCompletionChunk(
                chat_completion_chunk,
            )),
        }
    }
    fn is_completion_finished(llm_response: &mistralrs::CompletionChunkResponse) -> bool {
        llm_response
            .choices
            .iter()
            .any(|c| c.finish_reason.is_some())
    }
    fn convert_completion_chunk_result(
        llm_response: &mistralrs::CompletionChunkResponse,
    ) -> LlmResult {
        // Convert choices
        let choices = llm_response
            .choices
            .iter()
            .map(|choice| {
                // Convert delta message
                let text = choice.text.clone();

                // Convert logprobs if present
                let logprobs = choice.logprobs.as_ref().map(|logprob| ResponseLogprob {
                    token: logprob.token.clone(),
                    logprob: logprob.logprob,
                    bytes: logprob.bytes.clone().map(|b| b),
                    top_logprobs: logprob
                        .top_logprobs
                        .iter()
                        .map(|top| TopLogprob {
                            token: top.token.clone(),
                            logprob: top.logprob,
                            bytes: top.bytes.clone(),
                        })
                        .collect(),
                });

                llm_result::CompletionChunkChoice {
                    finish_reason: choice.finish_reason.as_ref().map(|c| match c.as_str() {
                        "stop" => llm_result::FinishReason::Stop as i32,
                        "length" => llm_result::FinishReason::Length as i32,
                        "canceled" => llm_result::FinishReason::Canceled as i32,
                        "generated-image" => llm_result::FinishReason::GeneratedImage as i32,
                        _ => llm_result::FinishReason::Unknown as i32,
                    }),
                    index: choice.index as u32,
                    text,
                    logprobs,
                }
            })
            .collect();

        // Create completion chunk response
        let completion_chunk = llm_result::CompletionChunkResponse {
            id: llm_response.id.clone(),
            choices,
            created: llm_response.created as u64, // XXX u128 to u64 (nanoseconds?)
            model: llm_response.model.clone(),
            system_fingerprint: llm_response.system_fingerprint.clone(),
            object: llm_response.object.clone(),
        };

        // Create final LlmResult with CompletionChunkResponse
        LlmResult {
            result: Some(llm_result::Result::CompletionChunk(completion_chunk)),
        }
    }
}

pub struct DefaultLLMResultConverter;
impl LLMResultConverter for DefaultLLMResultConverter {}
