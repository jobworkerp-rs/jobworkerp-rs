use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use command_utils::trace::attr::{
    OtelSpanAttributes, OtelSpanBuilder, OtelSpanType, langfuse_keys,
};
use command_utils::trace::impls::GenericOtelClient;
use command_utils::trace::otel_span::GenAIOtelClient;
use futures::Stream;
use futures::StreamExt;
use futures::stream::BoxStream;
use jobworkerp_base::error::JobWorkerError;
use opentelemetry::global::BoxedSpan;
use serde_json::json;

/// Generic trait for OpenTelemetry tracing functionality in LLM services
pub trait GenericLLMTracingHelper {
    fn get_otel_client(&self) -> Option<&Arc<GenericOtelClient>>;

    /// Convert messages to proper tracing input format (provider-specific implementation)
    fn convert_messages_to_input(&self, messages: &[impl LLMMessage]) -> serde_json::Value;

    /// Get provider name for span naming
    fn get_provider_name(&self) -> &str;

    /// Create span attributes for chat completion
    fn create_chat_completion_span_attributes(
        &self,
        model: &str,
        input_messages: serde_json::Value,
        model_parameters: Option<&HashMap<String, serde_json::Value>>,
        tools: &[impl ToolInfo],
        metadata: &HashMap<String, String>,
    ) -> OtelSpanAttributes {
        let provider = self.get_provider_name();
        let mut span_builder = OtelSpanBuilder::new(format!("{provider}.chat.completions"))
            .span_type(OtelSpanType::Generation)
            .model(model.to_string())
            .system(provider)
            .operation_name("chat")
            .input(input_messages)
            .openinference_span_kind("LLM");

        if let Some(sid) = metadata.get("session_id").cloned() {
            span_builder = span_builder.session_id(sid);
        }
        if let Some(uid) = metadata.get("user_id").cloned() {
            span_builder = span_builder.user_id(uid);
        }
        if let Some(params) = model_parameters {
            span_builder = span_builder.model_parameters(params.clone());
        }
        if !tools.is_empty() {
            let tools_json = serde_json::json!(
                tools
                    .iter()
                    .map(|t| t.get_name().to_string())
                    .collect::<Vec<_>>()
            );
            let mut metadata = HashMap::new();
            metadata.insert("tools".to_string(), tools_json);
            span_builder = span_builder.metadata(metadata);
        }
        span_builder.build()
    }

    /// Create span attributes for tool call tracing
    fn create_tool_call_span_attributes(
        &self,
        function_name: &str,
        arguments: serde_json::Value,
        metadata: &HashMap<String, String>,
    ) -> OtelSpanAttributes {
        let provider = self.get_provider_name();
        let mut span_builder = OtelSpanBuilder::new(format!("{provider}.tool.{function_name}"))
            .span_type(OtelSpanType::Span)
            .system(provider)
            .operation_name("tool_calls")
            .input(arguments);

        let mut m = HashMap::new();
        m.insert("gen_ai.tool.name".to_string(), json!(function_name));
        span_builder = span_builder.metadata(m);

        if let Some(session_id) = metadata.get("session_id").cloned() {
            span_builder = span_builder.session_id(session_id);
        }
        if let Some(user_id) = metadata.get("user_id").cloned() {
            span_builder = span_builder.user_id(user_id);
        }
        span_builder.build()
    }

    /// Execute chat action with proper parent-child span tracing and response recording
    fn with_chat_response_tracing<F, R>(
        &self,
        _metadata: &HashMap<String, String>,
        parent_context: Option<opentelemetry::Context>,
        span_attributes: OtelSpanAttributes,
        action: F,
    ) -> impl std::future::Future<Output = Result<(R, opentelemetry::Context)>> + Send
    where
        F: std::future::Future<Output = Result<R, JobWorkerError>> + Send + 'static,
        R: ChatResponse + Send + 'static,
    {
        let otel_client = self.get_otel_client().cloned();

        async move {
            let (result, context) = if let Some(client) = &otel_client {
                use opentelemetry::trace::{Span, Status};

                // Start span BEFORE action to capture actual duration
                let mut span = if let Some(ref ctx) = parent_context {
                    client.start_with_context(span_attributes, ctx.clone())
                } else {
                    client.start_new_span(span_attributes)
                };

                let action_result = action.await;

                match action_result {
                    Ok(result) => {
                        let response_output = result.to_json().to_string();
                        span.set_attribute(opentelemetry::KeyValue::new(
                            langfuse_keys::OBSERVATION_OUTPUT,
                            response_output,
                        ));
                        span.set_status(Status::Ok);
                        span.end();

                        (
                            result,
                            parent_context.unwrap_or_else(opentelemetry::Context::current),
                        )
                    }
                    Err(e) => {
                        tracing::error!("LLM action failed: {:?}", e);
                        span.set_status(Status::error(e.to_string()));
                        span.end();
                        return Err(anyhow::anyhow!("Action failed: {}", e));
                    }
                }
            } else {
                let result = action.await?;
                let context = parent_context.unwrap_or_else(opentelemetry::Context::current);
                (result, context)
            };

            Ok((result, context))
        }
    }

    /// Execute tool action with child span tracing and response recording
    fn with_tool_response_tracing<F>(
        &self,
        metadata: &HashMap<String, String>,
        parent_context: opentelemetry::Context,
        tool_attributes: OtelSpanAttributes,
        function_name: &str,
        arguments: serde_json::Value,
        action: F,
    ) -> impl std::future::Future<Output = Result<(serde_json::Value, opentelemetry::Context)>>
    + Send
    + 'static
    where
        F: std::future::Future<Output = Result<serde_json::Value, JobWorkerError>> + Send + 'static,
    {
        let function_name = function_name.to_string();
        let session_id = metadata.get("session_id").cloned();
        let user_id = metadata.get("user_id").cloned();
        let otel_client = self.get_otel_client().cloned();
        let provider = self.get_provider_name().to_string();

        async move {
            let (result, context) = if let Some(client) = otel_client {
                let saved_parent_context = parent_context.clone();
                let response_parser = {
                    let function_name = function_name.clone();
                    let arguments = arguments.clone();
                    let session_id = session_id.clone();
                    let user_id = user_id.clone();
                    let provider = provider.clone();

                    move |result: &serde_json::Value| -> Option<OtelSpanAttributes> {
                        // Detect tool execution errors from result string
                        let is_error = result
                            .as_str()
                            .is_some_and(|s| s.starts_with("Error executing tool"));
                        let execution_status = if is_error { "error" } else { "completed" };

                        let observation_output = serde_json::json!({
                            "function_name": function_name,
                            "arguments": arguments,
                            "result": result,
                            "execution_status": execution_status
                        });

                        let completion_output = result.clone();
                        let trace_output = serde_json::json!({
                            "tool_name": function_name,
                            "output": result,
                            "success": !is_error
                        });

                        let level = if is_error { "ERROR" } else { "INFO" };
                        let mut response_span_builder = OtelSpanBuilder::new(format!(
                            "{provider}.tool.{function_name}.response"
                        ))
                        .span_type(OtelSpanType::Event)
                        .observation_output(observation_output)
                        .completion_output(completion_output)
                        .trace_output(trace_output)
                        .level(level);

                        let mut metadata = HashMap::new();
                        metadata
                            .insert("event_type".to_string(), serde_json::json!("tool_response"));
                        metadata.insert("tool_name".to_string(), serde_json::json!(function_name));
                        metadata.insert(
                            "execution_status".to_string(),
                            serde_json::json!(execution_status),
                        );
                        response_span_builder = response_span_builder.metadata(metadata);

                        if let Some(ref session_id) = session_id {
                            response_span_builder =
                                response_span_builder.session_id(session_id.clone());
                        }
                        if let Some(ref user_id) = user_id {
                            response_span_builder = response_span_builder.user_id(user_id.clone());
                        }

                        Some(response_span_builder.build())
                    }
                };

                let result = client
                    .with_span_result_and_response_parser(
                        tool_attributes,
                        Some(parent_context),
                        action,
                        Some(response_parser),
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!("Error in traced tool span: {}", e))?;

                (result, saved_parent_context)
            } else {
                let result = action.await?;
                (result, parent_context)
            };

            Ok((result, context))
        }
    }

    /// Record usage with tracing
    fn trace_usage<U>(
        &self,
        metadata: &HashMap<String, String>,
        parent_context: opentelemetry::Context,
        name: &str,
        usage_data: &U,
        content: Option<&str>,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'static
    where
        U: UsageData + Clone + Send + 'static,
    {
        let otel_client = self.get_otel_client().cloned();
        let session_id = metadata.get("session_id").cloned();
        let user_id = metadata.get("user_id").cloned();
        let name = name.to_string();
        let usage_data = usage_data.clone();
        let content = content.map(|s| s.to_string());

        async move {
            if let Some(otel_client) = otel_client {
                let usage = usage_data.to_usage_map();
                let output = serde_json::json!({
                    "content": content.as_deref().unwrap_or_default(),
                    "usage": usage_data.to_json()
                });

                let mut span_builder = OtelSpanBuilder::new(&name)
                    .span_type(OtelSpanType::Event)
                    .usage(usage)
                    .output(output)
                    .level("INFO");

                if let Some(session_id) = session_id {
                    span_builder = span_builder.session_id(session_id);
                }
                if let Some(user_id) = user_id {
                    span_builder = span_builder.user_id(user_id);
                }
                let span_attributes = span_builder.build();

                otel_client
                    .with_span_result(span_attributes, Some(parent_context), async {
                        Ok::<(), JobWorkerError>(())
                    })
                    .await
                    .map_err(|e| anyhow::anyhow!("Error in traced span: {}", e))?;
            }
            Ok(())
        }
    }

    /// Open a span for a single tool call as a child of `parent_context`.
    ///
    /// Returns `None` when no otel client is configured. The caller must call
    /// `finish_tool_span` (or drop the span) to terminate it.
    fn open_tool_span(
        &self,
        function_name: &str,
        arguments: serde_json::Value,
        metadata: &HashMap<String, String>,
        parent_context: Option<opentelemetry::Context>,
    ) -> Option<BoxedSpan> {
        let client = self.get_otel_client()?;
        let attrs = self.create_tool_call_span_attributes(function_name, arguments, metadata);
        let span = match parent_context {
            Some(ctx) => client.start_with_context(attrs, ctx),
            None => client.start_new_span(attrs),
        };
        Some(span)
    }

    /// Wrap an async stream with a generation span.
    ///
    /// The span is started before the first chunk is awaited and is guaranteed to end
    /// when the stream terminates (or is dropped). The caller supplies a `finalize`
    /// closure that inspects each chunk and reports either no-op, the final state, or
    /// an error. The final state is recorded onto the span as `langfuse.observation.output`
    /// (and `gen_ai.usage.*` for usage), then the span is closed.
    fn with_streaming_response_tracing<S, T, F>(
        &self,
        parent_context: Option<opentelemetry::Context>,
        span_attributes: OtelSpanAttributes,
        stream: S,
        mut finalize: F,
    ) -> BoxStream<'static, T>
    where
        S: Stream<Item = T> + Send + 'static,
        T: Send + 'static,
        F: FnMut(&T) -> StreamingTraceUpdate + Send + 'static,
    {
        let otel_client = self.get_otel_client().cloned();
        // No tracer configured: short-circuit so the wrapper costs nothing.
        let Some(client) = otel_client else {
            return stream.boxed();
        };

        let span = if let Some(ref ctx) = parent_context {
            client.start_with_context(span_attributes, ctx.clone())
        } else {
            client.start_new_span(span_attributes)
        };
        // The Drop impl on StreamSpanGuard guarantees `end()` runs even if the
        // returned stream is dropped before reaching its `Final` chunk (e.g. the
        // consumer aborts mid-stream).
        let guard = StreamSpanGuard::new(span);

        let traced = async_stream::stream! {
            let mut guard = guard;
            futures::pin_mut!(stream);
            while let Some(item) = stream.next().await {
                match finalize(&item) {
                    StreamingTraceUpdate::None => {}
                    StreamingTraceUpdate::Final { output, usage } => {
                        guard.finish_ok(output, usage);
                    }
                    StreamingTraceUpdate::Error(message) => {
                        guard.finish_error(message);
                    }
                }
                yield item;
            }
        };

        traced.boxed()
    }
}

/// Update emitted by the per-chunk `finalize` callback of
/// [`GenericLLMTracingHelper::with_streaming_response_tracing`].
pub enum StreamingTraceUpdate {
    /// Intermediate chunk — no span change.
    None,
    /// Stream reached its terminal chunk; record the final output and usage and
    /// close the span as Ok.
    Final {
        output: serde_json::Value,
        usage: Option<HashMap<String, i64>>,
    },
    /// Stream errored out; record the message and close the span as Error.
    Error(String),
}

/// RAII guard that owns the streaming generation span and ends it exactly once.
///
/// `finish_ok` / `finish_error` set the recorded outcome and end the span. If neither
/// is called (consumer dropped the stream early), `Drop` ends the span anyway so the
/// tracer never leaks an open span.
struct StreamSpanGuard {
    span: Option<BoxedSpan>,
}

impl StreamSpanGuard {
    fn new(span: BoxedSpan) -> Self {
        Self { span: Some(span) }
    }

    fn finish_ok(&mut self, output: serde_json::Value, usage: Option<HashMap<String, i64>>) {
        let Some(mut span) = self.span.take() else {
            return; // already finalised
        };
        use opentelemetry::trace::{Span, Status};
        if let Ok(output_str) = serde_json::to_string(&output) {
            span.set_attribute(opentelemetry::KeyValue::new(
                langfuse_keys::OBSERVATION_OUTPUT,
                output_str.clone(),
            ));
            span.set_attribute(opentelemetry::KeyValue::new(
                "gen_ai.completion",
                output_str,
            ));
        }
        if let Some(usage) = usage {
            if let Ok(usage_str) = serde_json::to_string(&usage) {
                span.set_attribute(opentelemetry::KeyValue::new(
                    langfuse_keys::OBSERVATION_USAGE_DETAILS,
                    usage_str,
                ));
            }
            for (key, value) in &usage {
                match key.as_str() {
                    "input_tokens" | "prompt_tokens" => {
                        span.set_attribute(opentelemetry::KeyValue::new(
                            "gen_ai.usage.input_tokens",
                            *value,
                        ));
                    }
                    "output_tokens" | "completion_tokens" => {
                        span.set_attribute(opentelemetry::KeyValue::new(
                            "gen_ai.usage.output_tokens",
                            *value,
                        ));
                    }
                    "total_tokens" => {
                        span.set_attribute(opentelemetry::KeyValue::new(
                            "gen_ai.usage.total_tokens",
                            *value,
                        ));
                    }
                    other => {
                        span.set_attribute(opentelemetry::KeyValue::new(
                            format!("gen_ai.usage.{other}"),
                            *value,
                        ));
                    }
                }
            }
        }
        span.set_status(Status::Ok);
        span.end();
    }

    fn finish_error(&mut self, message: String) {
        let Some(mut span) = self.span.take() else {
            return;
        };
        use opentelemetry::trace::{Span, Status};
        span.set_attribute(opentelemetry::KeyValue::new(
            langfuse_keys::OBSERVATION_LEVEL,
            "ERROR",
        ));
        span.set_attribute(opentelemetry::KeyValue::new(
            "error.message",
            message.clone(),
        ));
        span.set_status(Status::error(message));
        span.end();
    }
}

impl Drop for StreamSpanGuard {
    fn drop(&mut self) {
        if let Some(mut span) = self.span.take() {
            use opentelemetry::trace::Span;
            // Stream was dropped before terminating naturally — close the span to
            // avoid leaking it into the tracer's never-ending state.
            span.end();
        }
    }
}

/// Record the result of a tool execution onto its span and end it.
///
/// `success = false` marks the span as Error with `result` as the message; `true`
/// marks it Ok. A `None` span is silently ignored so callers can use this with
/// optional spans returned by `open_tool_span`.
pub fn finish_tool_span(span: Option<BoxedSpan>, result: &str, success: bool) {
    let Some(mut span) = span else {
        return;
    };
    use opentelemetry::trace::{Span, Status};
    span.set_attribute(opentelemetry::KeyValue::new(
        langfuse_keys::OBSERVATION_OUTPUT,
        result.to_string(),
    ));
    if success {
        span.set_status(Status::Ok);
    } else {
        span.set_status(Status::error(result.to_string()));
    }
    span.end();
}

/// Build a streaming usage map from prompt/completion token counts.
///
/// Returns `None` when both inputs are `None` so callers can pass it through to
/// `StreamingTraceUpdate::Final::usage` without an extra wrapping conditional.
pub fn streaming_usage_map(
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
) -> Option<HashMap<String, i64>> {
    if prompt_tokens.is_none() && completion_tokens.is_none() {
        return None;
    }
    let mut m = HashMap::new();
    if let Some(p) = prompt_tokens {
        m.insert("prompt_tokens".to_string(), p as i64);
    }
    if let Some(c) = completion_tokens {
        m.insert("completion_tokens".to_string(), c as i64);
    }
    Some(m)
}

/// Trait for LLM message types
pub trait LLMMessage {
    fn get_role(&self) -> &str;
    fn get_content(&self) -> &str;
}

/// Trait for model options
pub trait ModelOptions {}

/// Trait for tool information
pub trait ToolInfo {
    fn get_name(&self) -> &str;
}

/// Trait for chat response types
pub trait ChatResponse {
    fn to_json(&self) -> serde_json::Value;
}

/// Trait for usage data
pub trait UsageData {
    fn to_usage_map(&self) -> HashMap<String, i64>;
    fn to_json(&self) -> serde_json::Value;
}
