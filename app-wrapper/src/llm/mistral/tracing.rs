use anyhow::Result;
use jobworkerp_base::error::JobWorkerError;
use net_utils::trace::attr::OtelSpanAttributes;
use net_utils::trace::impls::GenericOtelClient;
use net_utils::trace::otel_span::GenAIOtelClient;
use net_utils::trace::otel_span::RemoteSpanClient;
use opentelemetry::trace::Tracer;
use opentelemetry::Context;
use std::future::Future;
use std::sync::Arc;

/// Trait for OpenTelemetry tracing integration with MistralRS services
///
/// This trait provides a consistent interface for distributed tracing across
/// MistralRS-based LLM operations, enabling observability and monitoring.
pub trait MistralTracingService {
    /// Get the OpenTelemetry client instance
    fn get_otel_client(&self) -> Option<&Arc<GenericOtelClient>>;

    /// Execute an operation with tracing context
    ///
    /// This method wraps the given async operation with appropriate tracing spans
    /// and context propagation for distributed tracing.
    fn execute_with_tracing<F, T>(
        &self,
        action: F,
        context: Option<Context>,
    ) -> impl Future<Output = Result<T>> + Send
    where
        F: Future<Output = Result<T, anyhow::Error>> + Send,
        T: Send,
        Self: std::marker::Sync;

    /// Execute an operation with OpenTelemetry span
    fn execute_with_span<F, T>(
        &self,
        _span_name: &str,
        attributes: OtelSpanAttributes,
        parent_context: Option<Context>,
        action: F,
    ) -> impl Future<Output = Result<(T, Context)>> + Send
    where
        F: Future<Output = Result<T, JobWorkerError>> + Send + 'static,
        T: Send + serde::Serialize + 'static,
        Self: std::marker::Sync,
    {
        async move {
            if let Some(otel_client) = self.get_otel_client() {
                let parent_ctx = parent_context.unwrap_or_else(opentelemetry::Context::current);
                let result = otel_client
                    .with_span_result(attributes, Some(parent_ctx.clone()), action)
                    .await
                    .map_err(|e| anyhow::anyhow!("OpenTelemetry span execution failed: {e}"))?;
                Ok((result, parent_ctx))
            } else {
                let result = action
                    .await
                    .map_err(|e| anyhow::anyhow!("Action execution failed: {e}"))?;
                let context = parent_context.unwrap_or_else(opentelemetry::Context::current);
                Ok((result, context))
            }
        }
    }

    /// Create a new tracing span for MistralRS operations
    fn create_mistral_span(
        &self,
        operation_name: &str,
        context: &Context,
    ) -> opentelemetry::global::BoxedSpan {
        if let Some(otel_client) = self.get_otel_client() {
            // Create empty attributes for now - can be extended later
            let attributes = OtelSpanAttributes::default();
            otel_client.create_child_span_from_context(attributes, context)
        } else {
            // Create a no-op span if tracing is not available
            opentelemetry::global::tracer("mistral").start(operation_name.to_string())
        }
    }
}
