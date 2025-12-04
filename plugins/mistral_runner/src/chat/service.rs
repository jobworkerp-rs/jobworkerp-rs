//! Chat service for MistralRS plugin
//!
//! Provides chat completion with optional tool calling support

use crate::conversion::{RequestConverter, ResultConverter};
use crate::core::{MistralLlmServiceImpl, MistralRSMessage, ToolCallingConfig};
use crate::grpc::FunctionServiceClient;
use crate::tracing_helper::{extract_parent_context, messages_to_json, MistralOtelClient};
use anyhow::Result;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole as TextMessageRole;
use jobworkerp_runner::jobworkerp::runner::llm::{LlmChatArgs, LlmChatResult};
use mistralrs::{
    CalledFunction, RequestBuilder, TextMessageRole as MistralTextMessageRole, Tool,
    ToolCallResponse, ToolCallType, ToolChoice,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::tool_calling::ToolCallingProcessor;

/// Chat service with optional tool calling support
pub struct MistralChatService {
    core_service: Arc<MistralLlmServiceImpl>,
    grpc_client: Option<Arc<FunctionServiceClient>>,
    config: ToolCallingConfig,
    otel_client: Option<MistralOtelClient>,
}

impl MistralChatService {
    pub fn new(
        core_service: Arc<MistralLlmServiceImpl>,
        grpc_client: Option<Arc<FunctionServiceClient>>,
        config: ToolCallingConfig,
    ) -> Self {
        // Initialize OTel client if OTLP_ADDR is set
        let otel_client = if std::env::var("OTLP_ADDR").is_ok() {
            Some(MistralOtelClient::new())
        } else {
            None
        };

        Self {
            core_service,
            grpc_client,
            config,
            otel_client,
        }
    }

    /// Get model name for tracing
    fn model_name(&self) -> &str {
        self.core_service.model_name()
    }

    /// Extract model options for tracing
    fn extract_model_options(
        &self,
        args: &LlmChatArgs,
    ) -> Option<HashMap<String, serde_json::Value>> {
        args.options.as_ref().map(|opts| {
            let mut params = HashMap::new();
            if let Some(t) = opts.temperature {
                params.insert("temperature".to_string(), serde_json::json!(t));
            }
            if let Some(t) = opts.max_tokens {
                params.insert("max_tokens".to_string(), serde_json::json!(t));
            }
            if let Some(t) = opts.top_p {
                params.insert("top_p".to_string(), serde_json::json!(t));
            }
            params
        })
    }

    /// Execute chat request (non-streaming)
    pub async fn request_chat(
        &self,
        args: LlmChatArgs,
        cancel_token: Option<CancellationToken>,
        metadata: HashMap<String, String>,
    ) -> Result<LlmChatResult> {
        // Check if tool calling is enabled
        let use_tools = args
            .function_options
            .as_ref()
            .is_some_and(|o| o.use_function_calling);

        if use_tools && self.grpc_client.is_some() {
            self.request_chat_with_tools(args, cancel_token, metadata)
                .await
        } else {
            self.request_chat_simple(args, &metadata).await
        }
    }

    /// Simple chat without tool calling
    async fn request_chat_simple(
        &self,
        args: LlmChatArgs,
        metadata: &HashMap<String, String>,
    ) -> Result<LlmChatResult> {
        let request = RequestConverter::build_request(&args, vec![])?;

        // Execute with OpenTelemetry tracing if enabled
        if let Some(otel) = &self.otel_client {
            let messages = self.core_service.convert_proto_messages(&args)?;
            let messages_json = messages_to_json(&messages);
            let model_params = self.extract_model_options(&args);

            let span_attrs = otel.create_chat_span_attributes(
                self.model_name(),
                messages_json,
                model_params.as_ref(),
                &[],
                metadata,
            );

            // Extract parent context from metadata for distributed tracing
            let parent_context = extract_parent_context(metadata);

            // Execute within traced span with parent context propagation
            let core_service = self.core_service.clone();
            let result = otel
                .execute_with_span(span_attrs, Some(parent_context), async move {
                    core_service.request_chat(request).await
                })
                .await?;

            tracing::debug!(
                "Chat completion traced with parent context: model={}, tokens={}",
                self.model_name(),
                result.usage.total_tokens
            );

            Ok(ResultConverter::convert_chat_completion_result(&result))
        } else {
            let response = self.core_service.request_chat(request).await?;
            Ok(ResultConverter::convert_chat_completion_result(&response))
        }
    }

    /// Chat with tool calling support
    async fn request_chat_with_tools(
        &self,
        args: LlmChatArgs,
        cancel_token: Option<CancellationToken>,
        metadata: HashMap<String, String>,
    ) -> Result<LlmChatResult> {
        let grpc_client = self
            .grpc_client
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("gRPC client not configured for tool calling"))?;

        let tool_processor = ToolCallingProcessor::new(
            grpc_client.clone(),
            self.config.clone(),
            self.otel_client.clone(),
        );

        // Load tools from function set
        let tools = if let Some(ref function_opts) = args.function_options {
            if let Some(ref set_name) = function_opts.function_set_name {
                tool_processor.load_tools_from_set(set_name).await?
            } else {
                vec![]
            }
        } else {
            vec![]
        };

        if tools.is_empty() {
            tracing::warn!("No tools loaded, falling back to simple chat");
            return self.request_chat_simple(args, &metadata).await;
        }

        // Initial messages
        let mut messages = self.core_service.convert_proto_messages(&args)?;

        // Tool calling loop
        for iteration in 0..self.config.max_iterations {
            // Check cancellation
            if let Some(ref token) = cancel_token {
                if token.is_cancelled() {
                    tracing::info!("Chat cancelled at iteration {}", iteration);
                    return Ok(LlmChatResult {
                        content: Some(
                            jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::MessageContent {
                                content: Some(
                                    jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(
                                        "Request cancelled".to_string(),
                                    ),
                                ),
                            },
                        ),
                        reasoning_content: None,
                        done: true,
                        usage: None,
                    });
                }
            }

            tracing::debug!("Tool calling iteration {}", iteration);

            // Build request with current messages and tools
            let request = self.build_request_with_messages(&messages, &args, tools.clone())?;

            // Send request
            let response = self.core_service.request_chat(request).await?;

            // Extract tool calls
            let tool_calls = self
                .core_service
                .extract_tool_calls_from_response(&response)?;

            if tool_calls.is_empty() {
                // No more tool calls, return final result
                tracing::debug!("No tool calls in response, returning final result");
                return Ok(ResultConverter::convert_chat_completion_result(&response));
            }

            tracing::debug!("Executing {} tool calls", tool_calls.len());

            // Add assistant message with tool calls
            if let Some(first_choice) = response.choices.first() {
                messages.push(MistralRSMessage {
                    role: TextMessageRole::Assistant,
                    content: first_choice.message.content.clone().unwrap_or_default(),
                    tool_call_id: None,
                    tool_calls: Some(tool_calls.clone()),
                });
            }

            // Execute tool calls
            let tool_results = tool_processor
                .execute_tool_calls(&tool_calls, cancel_token.clone(), metadata.clone())
                .await?;

            // Add tool results to messages
            messages.extend(tool_results);
        }

        // Max iterations reached - return error
        Err(anyhow::anyhow!(
            "Max tool calling iterations ({}) exceeded",
            self.config.max_iterations
        ))
    }

    /// Build request with messages and tools
    ///
    /// Uses appropriate MistralRS API for each message type:
    /// - Tool messages: add_tool_message with tool_call_id
    /// - Assistant messages with tool_calls: add_message_with_tool_call
    /// - Other messages: add_message
    fn build_request_with_messages(
        &self,
        messages: &[MistralRSMessage],
        args: &LlmChatArgs,
        tools: Vec<Tool>,
    ) -> Result<RequestBuilder> {
        let mut builder = RequestBuilder::new();

        // Add messages with proper handling for tool-related messages
        for msg in messages {
            match msg.role {
                TextMessageRole::Tool => {
                    // Tool response messages require tool_call_id
                    let tool_id = msg.tool_call_id.as_deref().unwrap_or("");
                    builder = builder.add_tool_message(&msg.content, tool_id);
                }
                TextMessageRole::Assistant => {
                    // Assistant messages may contain tool calls
                    if let Some(tool_calls) = &msg.tool_calls {
                        if !tool_calls.is_empty() {
                            let mistral_tool_calls: Vec<ToolCallResponse> = tool_calls
                                .iter()
                                .enumerate()
                                .map(|(index, tc)| ToolCallResponse {
                                    index,
                                    id: tc.id.clone(),
                                    tp: ToolCallType::Function,
                                    function: CalledFunction {
                                        name: tc.function_name.clone(),
                                        arguments: tc.arguments.clone(),
                                    },
                                })
                                .collect();
                            builder = builder.add_message_with_tool_call(
                                MistralTextMessageRole::Assistant,
                                &msg.content,
                                mistral_tool_calls,
                            );
                        } else {
                            builder = builder
                                .add_message(MistralTextMessageRole::Assistant, &msg.content);
                        }
                    } else {
                        builder =
                            builder.add_message(MistralTextMessageRole::Assistant, &msg.content);
                    }
                }
                TextMessageRole::User => {
                    builder = builder.add_message(MistralTextMessageRole::User, &msg.content);
                }
                TextMessageRole::System => {
                    builder = builder.add_message(MistralTextMessageRole::System, &msg.content);
                }
                _ => {
                    // Default to User for unknown roles
                    builder = builder.add_message(MistralTextMessageRole::User, &msg.content);
                }
            }
        }

        // Apply options
        if let Some(opts) = &args.options {
            if let Some(max_tokens) = opts.max_tokens {
                builder = builder.set_sampler_max_len(max_tokens as usize);
            }
            if let Some(temperature) = opts.temperature {
                builder = builder.set_sampler_temperature(temperature.into());
            }
            if let Some(top_p) = opts.top_p {
                builder = builder.set_sampler_topp(top_p.into());
            }
        }

        // Set tools
        if !tools.is_empty() {
            builder = builder.set_tools(tools);
            builder = builder.set_tool_choice(ToolChoice::Auto);
        }

        Ok(builder)
    }

    /// Stream chat to a channel
    pub async fn stream_chat(
        &self,
        args: LlmChatArgs,
        tx: mpsc::Sender<Vec<u8>>,
        cancel_token: Option<CancellationToken>,
        metadata: HashMap<String, String>,
    ) -> Result<()> {
        use futures::StreamExt;
        use prost::Message;

        // For tool calling, execute non-streaming first then send final result
        let use_tools = args
            .function_options
            .as_ref()
            .is_some_and(|o| o.use_function_calling);

        if use_tools && self.grpc_client.is_some() {
            let result = self
                .request_chat_with_tools(args, cancel_token, metadata)
                .await?;
            let encoded = result.encode_to_vec();
            let _ = tx.send(encoded).await;
            return Ok(());
        }

        // Simple streaming without tool calling
        let request = RequestConverter::build_request(&args, vec![])?;
        let mut stream = self.core_service.stream_chat(request).await?;

        while let Some(response) = stream.next().await {
            // Check cancellation
            if let Some(ref token) = cancel_token {
                if token.is_cancelled() {
                    tracing::info!("Stream cancelled");
                    break;
                }
            }

            let result = match response {
                mistralrs::Response::Chunk(chunk) => {
                    ResultConverter::convert_chat_completion_chunk_result(&chunk)
                }
                mistralrs::Response::Done(completion) => {
                    ResultConverter::convert_chat_completion_result(&completion)
                }
                _ => continue,
            };

            let encoded = result.encode_to_vec();
            if tx.send(encoded).await.is_err() {
                tracing::debug!("Stream receiver dropped");
                break;
            }

            if result.done {
                break;
            }
        }

        Ok(())
    }
}
