use super::super::generic_tracing_helper::GenericLLMTracingHelper;
use anyhow::Result;
use genai::chat::{ChatOptions, ChatRequest};
use jobworkerp_base::error::JobWorkerError;
use std::collections::HashMap;

///
///
/// Trait for OpenTelemetry tracing functionality in GenAI completion services
pub trait GenaiCompletionTracingHelper: GenericLLMTracingHelper {
    /// Create completion span attributes from GenAI request components
    #[allow(async_fn_in_trait)]
    async fn create_completion_span_from_request(
        &self,
        model: &str,
        chat_req: &ChatRequest,
        options: &Option<ChatOptions>,
        metadata: &HashMap<String, String>,
    ) -> infra_utils::infra::trace::attr::OtelSpanAttributes {
        use super::super::chat::genai::GenaiTracingHelper;
        let input_messages =
            super::super::chat::genai::GenaiChatService::convert_messages_to_input_genai(
                &chat_req.messages,
            );
        let model_parameters =
            super::super::chat::genai::GenaiChatService::convert_model_options_to_parameters_genai(
                options,
            );

        // Create completion-specific span attributes
        let mut span_builder = infra_utils::infra::trace::attr::OtelSpanBuilder::new(format!(
            "{}.completions",
            self.get_provider_name()
        ))
        .span_type(infra_utils::infra::trace::attr::OtelSpanType::Generation)
        .model(model.to_string())
        .system(self.get_provider_name())
        .operation_name("completion")
        .input(input_messages)
        .openinference_span_kind("LLM");

        if let Some(sid) = metadata.get("session_id").cloned() {
            span_builder = span_builder.session_id(sid);
        }
        if let Some(uid) = metadata.get("user_id").cloned() {
            span_builder = span_builder.user_id(uid);
        }
        if !model_parameters.is_empty() {
            span_builder = span_builder.model_parameters(model_parameters);
        }

        span_builder.build()
    }

    /// Execute completion action with proper parent-child span tracing and response recording
    fn with_completion_response_tracing<F>(
        &self,
        metadata: &HashMap<String, String>,
        parent_context: Option<opentelemetry::Context>,
        span_attributes: infra_utils::infra::trace::attr::OtelSpanAttributes,
        action: F,
    ) -> impl std::future::Future<Output = Result<(genai::chat::ChatResponse, opentelemetry::Context)>>
           + Send
    where
        F: std::future::Future<Output = Result<genai::chat::ChatResponse, JobWorkerError>>
            + Send
            + 'static,
    {
        GenericLLMTracingHelper::with_chat_response_tracing(
            self,
            metadata,
            parent_context,
            span_attributes,
            action,
        )
    }

    fn trace_usage(
        &self,
        metadata: &HashMap<String, String>,
        parent_context: opentelemetry::Context,
        name: &str,
        usage_data: &genai::chat::Usage,
        content: Option<&str>,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'static {
        GenericLLMTracingHelper::trace_usage(
            self,
            metadata,
            parent_context,
            name,
            usage_data,
            content,
        )
    }
}
