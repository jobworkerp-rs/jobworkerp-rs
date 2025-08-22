use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use anyhow::Result;
use genai::chat::Usage as GenaiUsage;
use jobworkerp_base::error::JobWorkerError;
use net_utils::trace::attr::{OtelSpanAttributes, OtelSpanBuilder, OtelSpanType};
use net_utils::trace::impls::GenericOtelClient;
use net_utils::trace::otel_span::GenAIOtelClient;
use ollama_rs::generation::chat::{ChatMessageFinalResponseData, ChatMessageResponse};
use opentelemetry::Context;
use serde_json;

pub mod genai_helper;
pub mod mistral_helper;
pub mod ollama_helper;

/// Tracing context - For internal state management
#[derive(Debug)]
pub struct LLMTracingContext {
    parent_context: Option<Context>,
    metadata: HashMap<String, String>,
    api_type: LLMApiType,
    main_span_attributes: Option<OtelSpanAttributes>,
}

/// API type enumeration
#[derive(Debug, Clone)]
pub enum LLMApiType {
    Chat,
    Completion,
}

impl LLMApiType {
    pub fn to_string(&self) -> &'static str {
        match self {
            LLMApiType::Chat => "chat",
            LLMApiType::Completion => "completion",
        }
    }
}

/// Unified LLM input data
#[derive(Debug, Clone)]
pub struct LLMInput {
    pub messages: serde_json::Value,
    pub prompt: Option<String>,
}

/// Unified LLM options
#[derive(Debug, Clone)]
pub struct LLMOptions {
    pub parameters: HashMap<String, serde_json::Value>,
}

/// Unified LLM tool information
#[derive(Debug, Clone)]
pub struct LLMTool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Trait for request data - Provider-specific data extraction interface
pub trait LLMRequestData: Send + Sync {
    /// Extract tracing input data from the request
    fn extract_input(&self) -> LLMInput;

    /// Extract model options from the request
    fn extract_options(&self) -> Option<LLMOptions>;

    /// Extract tool information from the request
    fn extract_tools(&self) -> Vec<LLMTool>;

    /// Extract model name from the request
    fn extract_model(&self) -> Option<String>;
}

/// Trait for response data - Provider-specific data extraction interface
pub trait LLMResponseData: Send + Sync {
    /// Convert response to tracing JSON
    fn to_trace_output(&self) -> serde_json::Value;

    /// Extract usage data from the response
    fn extract_usage(&self) -> Option<Box<dyn UsageData>>;

    /// Extract content from the response
    fn extract_content(&self) -> Option<String>;
}

/// Trait for usage data
pub trait UsageData: Send + Sync {
    fn to_usage_map(&self) -> HashMap<String, i64>;
    fn to_json(&self) -> serde_json::Value;
}

/// Trait for unified LLM tracing
pub trait LLMTracingHelper: Send + Sync {
    // === Basic Settings ===
    fn get_otel_client(&self) -> Option<&Arc<GenericOtelClient>>;
    fn get_provider_name(&self) -> &str;

    // === Main Tracing API ===

    /// Start tracing - Called before executing business logic
    fn start_llm_tracing<T>(
        &self,
        api_type: LLMApiType,
        request_data: &T,
        metadata: &HashMap<String, String>,
        parent_context: Option<Context>,
    ) -> impl Future<Output = Result<LLMTracingContext, JobWorkerError>> + Send
    where
        T: LLMRequestData + 'static,
    {
        async move {
            let input = request_data.extract_input();
            let options = request_data.extract_options();
            let tools = request_data.extract_tools();
            let model = request_data
                .extract_model()
                .unwrap_or_else(|| self.get_default_model());

            let span_attributes = self.create_span_attributes(
                api_type.clone(),
                &model,
                &input,
                options.as_ref(),
                &tools,
                metadata,
            );

            // Store span attributes for later use in finish_llm_tracing
            if let Some(_client) = self.get_otel_client() {
                let span_name = format!(
                    "{}.{}.completions",
                    self.get_provider_name(),
                    api_type.to_string()
                );
            }

            Ok(LLMTracingContext {
                parent_context,
                metadata: metadata.clone(),
                api_type,
                main_span_attributes: Some(span_attributes),
            })
        }
    }

    /// Finish tracing - Called after executing business logic
    fn finish_llm_tracing<T>(
        &self,
        context: LLMTracingContext,
        response_data: &T,
    ) -> impl Future<Output = Result<(), JobWorkerError>> + Send
    where
        T: LLMResponseData + 'static,
    {
        let otel_client = self.get_otel_client().cloned();
        let response_output = response_data.to_trace_output();
        let usage = response_data.extract_usage();
        let content = response_data.extract_content();

        async move {
            // Create and execute main span containing all LLM operation details
            if let Some(client) = otel_client {
                if let Some(ref main_span_attributes) = context.main_span_attributes {
                    // Add response output to main span attributes (only one field)
                    let mut updated_attributes = main_span_attributes.clone();
                    
                    // Create assistant message in the same format as input messages
                    // Input uses: [{"role": "user", "content": "..."}, ...]
                    // Output should use: [{"role": "assistant", "content": "..."}]
                    
                    let assistant_content = if let serde_json::Value::String(content) = &response_output {
                        content.clone()
                    } else {
                        serde_json::to_string(&response_output).unwrap_or_default()
                    };
                    
                    // Create completion message array in same format as input messages
                    let completion_messages = serde_json::json!([{
                        "role": "assistant",
                        "content": assistant_content
                    }]);
                    
                    // Set both output (for langfuse.observation.output) and completion messages
                    updated_attributes.data.output = Some(completion_messages.clone());
                    
                    // DEBUG: Log what we're about to send
                    tracing::info!("FINAL TRACE: data.output = {:?}", completion_messages);
                    tracing::info!("FINAL TRACE: span attributes about to be sent = {:?}", updated_attributes.name);
                    
                    // Clear any interfering metadata
                    updated_attributes.data.metadata = None;

                    // Add usage information to main span if available
                    if let Some(usage_ref) = usage.as_ref() {
                        updated_attributes.usage = Some(usage_ref.to_usage_map());
                    }

                    // Use with_span_result_and_response_parser to preserve our response data
                    client
                        .with_span_result_and_response_parser(
                            updated_attributes,
                            context.parent_context.clone(),
                            async move { Ok::<(), JobWorkerError>(()) },
                            Some(|_: &()| None), // Parser that returns None to skip default output overwrite
                        )
                        .await
                        .map_err(|e| {
                            tracing::error!("Failed to create main LLM span: {}", e);
                            JobWorkerError::OtherError(format!("Error in main LLM span: {e}"))
                        })?;

                    tracing::debug!("Main LLM span created successfully with response data");
                } else {
                    tracing::warn!("Main span attributes not available for tracing completion");
                }
            }

            // Also execute separate response tracing for detailed breakdown
            self.trace_response(&context, response_data).await?;

            // Execute usage tracing
            if let Some(usage_ref) = usage {
                self.trace_usage_internal(&context, usage_ref.as_ref(), content.as_deref())
                    .await?;
            }

            Ok(())
        }
    }

    /// Finish tracing with error
    fn finish_llm_tracing_with_error(
        &self,
        context: LLMTracingContext,
        error: &JobWorkerError,
    ) -> impl Future<Output = Result<(), JobWorkerError>> + Send {
        let otel_client = self.get_otel_client().cloned();
        let error_message = error.to_string();

        async move {
            // Create main span with error information
            if let Some(client) = otel_client {
                if let Some(ref main_span_attributes) = context.main_span_attributes {
                    // Add error information to main span attributes (only one field)
                    let mut updated_attributes = main_span_attributes.clone();
                    let error_output = serde_json::json!({
                        "error": error_message,
                        "error_type": "llm_api_error"
                    });
                    updated_attributes.data.output = Some(error_output); // Use only OpenInference standard field
                    updated_attributes.level = Some("ERROR".to_string());

                    // Use with_span_result_and_response_parser to preserve our error data
                    client
                        .with_span_result_and_response_parser(
                            updated_attributes,
                            context.parent_context.clone(),
                            async move { Ok::<(), JobWorkerError>(()) },
                            Some(|_: &()| None), // Parser that returns None to skip default output overwrite
                        )
                        .await
                        .map_err(|e| {
                            tracing::error!("Failed to create main LLM error span: {}", e);
                            JobWorkerError::OtherError(format!("Error in main LLM error span: {e}"))
                        })?;

                    tracing::debug!("Main LLM error span created successfully");
                } else {
                    tracing::warn!("Main span attributes not available for error tracing");
                }
            }

            // Also execute separate error tracing
            self.trace_error(&context, error).await?;
            Ok(())
        }
    }

    // === Provider-specific settings (mandatory implementation) ===

    /// Get default model name
    fn get_default_model(&self) -> String;

    // === Internal tracing implementation (default provided) ===

    /// Create unified span attributes for API types
    fn create_span_attributes(
        &self,
        api_type: LLMApiType,
        model: &str,
        input: &LLMInput,
        options: Option<&LLMOptions>,
        tools: &[LLMTool],
        metadata: &HashMap<String, String>,
    ) -> OtelSpanAttributes {
        let operation = api_type.to_string();

        let mut span_builder = OtelSpanBuilder::new(format!(
            "{}.{}.completions",
            self.get_provider_name(),
            operation
        ))
        .span_type(OtelSpanType::Generation)
        .model(model.to_string())
        .system(self.get_provider_name())
        .operation_name(operation)
        .input(input.messages.clone())
        .openinference_span_kind("LLM");

        // Provider-specific detailed settings
        if let Some(opts) = options {
            span_builder = span_builder.model_parameters(opts.parameters.clone());
        }

        if !tools.is_empty() {
            let tools_json = serde_json::json!(tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters
                    })
                })
                .collect::<Vec<_>>());
            tracing::debug!(
                "TODO: Should Be Adding tools to span: {}",
                tools_json.to_string()
            );
            // Note: tools method may not exist, skip for now
            // span_builder = span_builder.tools(tools_json);
        }

        // Add metadata
        if let Some(sid) = metadata.get("session_id").cloned() {
            span_builder = span_builder.session_id(sid);
        }
        if let Some(uid) = metadata.get("user_id").cloned() {
            span_builder = span_builder.user_id(uid);
        }

        span_builder.build()
    }

    /// Execute response tracing
    fn trace_response<T>(
        &self,
        context: &LLMTracingContext,
        response_data: &T,
    ) -> impl Future<Output = Result<(), JobWorkerError>> + Send
    where
        T: LLMResponseData + 'static,
    {
        let session_id = context.metadata.get("session_id").cloned();
        let user_id = context.metadata.get("user_id").cloned();
        let otel_client = self.get_otel_client().cloned();
        let provider = self.get_provider_name().to_string();
        let api_type = context.api_type.to_string().to_string();
        let _response_output = response_data.to_trace_output(); // Not used in trace_response
        let parent_context = context.parent_context.clone();

        async move {
            if let Some(client) = otel_client {
                let mut response_span_builder =
                    OtelSpanBuilder::new(format!("{provider}.{api_type}.response"))
                        .span_type(OtelSpanType::Event)
                        .output(serde_json::json!("DUMMY_RESPONSE_EVENT_OUTPUT_TEST")) // TESTING: Dummy data
                        .trace_output(serde_json::json!("DUMMY_RESPONSE_TRACE_OUTPUT_TEST")) // TESTING: Also add to trace_output
                        .level("INFO")
                        .openinference_span_kind("LLM");

                let mut metadata = HashMap::new();
                metadata.insert(
                    "event_type".to_string(),
                    serde_json::json!(format!("{api_type}_response")),
                );
                response_span_builder = response_span_builder.metadata(metadata);

                if let Some(session_id) = session_id {
                    response_span_builder = response_span_builder.session_id(session_id);
                }
                if let Some(user_id) = user_id {
                    response_span_builder = response_span_builder.user_id(user_id);
                }

                let span_attributes = response_span_builder.build();

                client
                    .with_span_result(span_attributes, parent_context, async move {
                        Ok::<(), JobWorkerError>(())
                    })
                    .await
                    .map_err(|e| {
                        JobWorkerError::OtherError(format!("Error in response tracing: {e}"))
                    })?;
            }
            Ok(())
        }
    }

    /// Execute usage tracing (internal use)
    fn trace_usage_internal(
        &self,
        context: &LLMTracingContext,
        usage_data: &dyn UsageData,
        content: Option<&str>,
    ) -> impl Future<Output = Result<(), JobWorkerError>> + Send {
        let session_id = context.metadata.get("session_id").cloned();
        let user_id = context.metadata.get("user_id").cloned();
        let otel_client = self.get_otel_client().cloned();
        let usage_json = usage_data.to_json();
        let usage_map = usage_data.to_usage_map();
        let content = content.map(|s| s.to_string());
        let name = format!(
            "{}.{}.usage",
            self.get_provider_name(),
            context.api_type.to_string()
        );
        let parent_context = context.parent_context.clone();

        async move {
            if let Some(client) = otel_client {
                let output = serde_json::json!({
                    "content": content,
                    "usage": usage_json
                });

                let mut span_builder = OtelSpanBuilder::new(&name)
                    .span_type(OtelSpanType::Event)
                    .usage(usage_map)
                    .output(output)
                    .level("INFO");

                if let Some(session_id) = session_id {
                    span_builder = span_builder.session_id(session_id);
                }
                if let Some(user_id) = user_id {
                    span_builder = span_builder.user_id(user_id);
                }
                let span_attributes = span_builder.build();

                client
                    .with_span_result(span_attributes, parent_context, async move {
                        Ok::<(), JobWorkerError>(())
                    })
                    .await
                    .map_err(|e| {
                        JobWorkerError::OtherError(format!("Error in usage tracing: {e}"))
                    })?;
            }
            Ok(())
        }
    }

    /// Execute error tracing
    fn trace_error(
        &self,
        context: &LLMTracingContext,
        error: &JobWorkerError,
    ) -> impl Future<Output = Result<(), JobWorkerError>> + Send {
        let session_id = context.metadata.get("session_id").cloned();
        let user_id = context.metadata.get("user_id").cloned();
        let otel_client = self.get_otel_client().cloned();
        let error_message = error.to_string();
        let provider = self.get_provider_name().to_string();
        let api_type = context.api_type.to_string().to_string();
        let parent_context = context.parent_context.clone();

        async move {
            if let Some(client) = otel_client {
                let error_output = serde_json::json!({
                    "error": error_message,
                    "error_type": "llm_api_error"
                });

                let mut span_builder = OtelSpanBuilder::new(format!("{provider}.{api_type}.error"))
                    .span_type(OtelSpanType::Event)
                    .output(error_output)
                    .level("ERROR");

                if let Some(session_id) = session_id {
                    span_builder = span_builder.session_id(session_id);
                }
                if let Some(user_id) = user_id {
                    span_builder = span_builder.user_id(user_id);
                }
                let span_attributes = span_builder.build();

                client
                    .with_span_result(span_attributes, parent_context, async move {
                        Ok::<(), JobWorkerError>(())
                    })
                    .await
                    .map_err(|e| {
                        JobWorkerError::OtherError(format!("Error in error tracing: {e}"))
                    })?;
            }
            Ok(())
        }
    }
}

// UsageData implementation for Ollama
impl UsageData for ChatMessageFinalResponseData {
    fn to_usage_map(&self) -> HashMap<String, i64> {
        let mut usage = HashMap::new();
        usage.insert("prompt_tokens".to_string(), self.prompt_eval_count as i64);
        usage.insert("completion_tokens".to_string(), self.eval_count as i64);
        usage.insert(
            "total_tokens".to_string(),
            (self.prompt_eval_count + self.eval_count) as i64,
        );
        usage
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "prompt_tokens": self.prompt_eval_count,
            "completion_tokens": self.eval_count,
            "total_tokens": self.prompt_eval_count + self.eval_count
        })
    }
}

// UsageData implementation for GenAI
impl UsageData for GenaiUsage {
    fn to_usage_map(&self) -> HashMap<String, i64> {
        let mut usage = HashMap::new();
        usage.insert(
            "prompt_tokens".to_string(),
            self.prompt_tokens.unwrap_or(0) as i64,
        );
        usage.insert(
            "completion_tokens".to_string(),
            self.completion_tokens.unwrap_or(0) as i64,
        );
        usage.insert(
            "total_tokens".to_string(),
            self.total_tokens.unwrap_or(0) as i64,
        );
        usage
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "prompt_tokens": self.prompt_tokens.unwrap_or(0),
            "completion_tokens": self.completion_tokens.unwrap_or(0),
            "total_tokens": self.total_tokens.unwrap_or(0)
        })
    }
}

impl crate::llm::tracing::LLMRequestData
    for ollama_rs::generation::chat::request::ChatMessageRequest
{
    fn extract_input(&self) -> crate::llm::tracing::LLMInput {
        crate::llm::tracing::LLMInput {
            messages: serde_json::json!(self
                .messages
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "role": match m.role {
                            ollama_rs::generation::chat::MessageRole::User => "user",
                            ollama_rs::generation::chat::MessageRole::Assistant => "assistant",
                            ollama_rs::generation::chat::MessageRole::System => "system",
                            ollama_rs::generation::chat::MessageRole::Tool => "tool",
                        },
                        "content": m.content,
                        "tool_calls": if !m.tool_calls.is_empty() {
                            Some(serde_json::json!(m.tool_calls))
                        } else {
                            None
                        },
                        "images": m.images.as_ref().map(|imgs| imgs.len())
                    })
                })
                .collect::<Vec<_>>()),
            prompt: None,
        }
    }

    fn extract_options(&self) -> Option<crate::llm::tracing::LLMOptions> {
        self.options
            .as_ref()
            .map(|options| crate::llm::tracing::LLMOptions {
                parameters: if let Ok(value) = serde_json::to_value(options) {
                    if let Some(obj) = value.as_object() {
                        obj.iter()
                            .filter_map(|(k, v)| {
                                if !v.is_null() {
                                    Some((k.clone(), v.clone()))
                                } else {
                                    None
                                }
                            })
                            .collect()
                    } else {
                        HashMap::new()
                    }
                } else {
                    HashMap::new()
                },
            })
    }

    fn extract_tools(&self) -> Vec<crate::llm::tracing::LLMTool> {
        if !self.tools.is_empty() {
            self.tools
                .iter()
                .map(|tool| crate::llm::tracing::LLMTool {
                    name: tool.function.name.clone(),
                    description: tool.function.description.clone(),
                    parameters: serde_json::json!(tool.function.parameters),
                })
                .collect()
        } else {
            vec![]
        }
    }

    fn extract_model(&self) -> Option<String> {
        Some(self.model_name.clone())
    }
}

impl crate::llm::tracing::LLMResponseData for ChatMessageResponse {
    fn to_trace_output(&self) -> serde_json::Value {
        // Return only the message content for trace output, not the full structure
        serde_json::json!(self.message.content)
    }

    fn extract_usage(&self) -> Option<Box<dyn crate::llm::tracing::UsageData>> {
        self.final_data
            .as_ref()
            .map(|fd| Box::new(fd.clone()) as Box<dyn crate::llm::tracing::UsageData>)
    }

    fn extract_content(&self) -> Option<String> {
        Some(self.message.content.clone())
    }
}
