use anyhow::Result;
use net_utils::trace::impls::GenericOtelClient;
use net_utils::trace::attr::OtelSpanAttributes;
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
    async fn execute_with_tracing<F, T>(
        &self,
        action: F,
        context: Option<Context>,
    ) -> Result<T>
    where
        F: Future<Output = Result<T>> + Send,
        T: Send;
    
    /// Create a new tracing span for MistralRS operations
    fn create_mistral_span(&self, operation_name: &str, context: &Context) -> opentelemetry::global::BoxedSpan {
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