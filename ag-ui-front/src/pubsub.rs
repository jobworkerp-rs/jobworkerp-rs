//! Pub/Sub integration for AG-UI event streaming.
//!
//! This module provides utilities to integrate with jobworkerp-rs Pub/Sub
//! infrastructure for real-time job result notifications and LLM streaming.
//!
//! # Design
//!
//! Two main integration points:
//! 1. **Job Result Notifications**: Subscribe to job completions via `JobResultSubscriber`
//! 2. **LLM Streaming**: Subscribe to streaming results via `subscribe_result_stream`
//!
//! Both use the existing Pub/Sub infrastructure (Redis for Scalable, Channel for Standalone).

use crate::events::llm::result_output_stream_to_ag_ui_events_with_end_guarantee;
use crate::events::{AgUiEvent, SharedWorkflowEventAdapter};
use crate::types::ids::MessageId;
use app::app::job_result::JobResultApp;
use futures::stream::BoxStream;
use proto::jobworkerp::data::{JobId, ResultOutputItem};
use std::sync::Arc;

/// Subscribe to LLM streaming results and convert to AG-UI events.
///
/// This function combines the job result listening with AG-UI event conversion:
/// 1. Calls `listen_result(streaming=true)` to get both result and stream
/// 2. Converts the stream to AG-UI TEXT_MESSAGE_* events
/// 3. Returns both the event stream and the collected result
///
/// # Arguments
/// * `job_result_app` - JobResultApp for listening to results
/// * `job_id` - The job ID to listen for
/// * `worker_name` - Optional worker name for filtering
/// * `timeout` - Optional timeout in milliseconds
///
/// # Returns
/// Tuple of (JobResult, Option<AG-UI event stream>)
pub async fn subscribe_llm_stream(
    job_result_app: &Arc<dyn JobResultApp>,
    job_id: &JobId,
    worker_name: Option<&String>,
    timeout: Option<u64>,
) -> anyhow::Result<(
    proto::jobworkerp::data::JobResult,
    Option<BoxStream<'static, AgUiEvent>>,
)> {
    let (job_result, stream_opt) = job_result_app
        .listen_result(job_id, None, worker_name, timeout, true)
        .await?;

    let event_stream = stream_opt.map(|stream| {
        let message_id = MessageId::random();
        let events = result_output_stream_to_ag_ui_events_with_end_guarantee(stream, message_id);
        Box::pin(events) as BoxStream<'static, AgUiEvent>
    });

    Ok((job_result, event_stream))
}

/// Subscribe to job result notifications and convert to AG-UI TOOL_CALL_RESULT events.
///
/// This function listens for job completion notifications and converts them
/// to AG-UI tool call result events.
///
/// # Arguments
/// * `job_result_app` - JobResultApp for listening to results
/// * `job_id` - The job ID to listen for
/// * `worker_name` - Optional worker name for filtering
/// * `timeout` - Optional timeout in milliseconds
/// * `adapter` - Shared workflow event adapter for event generation
///
/// # Returns
/// AG-UI event (TOOL_CALL_RESULT or TOOL_CALL_END with error)
pub async fn subscribe_job_result_as_tool_call(
    job_result_app: &Arc<dyn JobResultApp>,
    job_id: &JobId,
    worker_name: Option<&String>,
    timeout: Option<u64>,
    adapter: SharedWorkflowEventAdapter,
) -> anyhow::Result<Vec<AgUiEvent>> {
    let (job_result, _stream) = job_result_app
        .listen_result(job_id, None, worker_name, timeout, false)
        .await?;

    let mut events = Vec::new();
    let adapter_lock = adapter.lock().await;

    if let Some(data) = &job_result.data {
        // Extract result output
        let result_value = if let Some(output) = &data.output {
            // Try to parse as JSON, fall back to string representation
            serde_json::from_slice(&output.items).unwrap_or_else(|_| {
                serde_json::Value::String(String::from_utf8_lossy(&output.items).to_string())
            })
        } else {
            serde_json::Value::Null
        };

        // Generate TOOL_CALL_RESULT event with explicit job ID
        let event = adapter_lock.job_completed_with_id(job_id.value, result_value);
        events.push(event);
    }

    Ok(events)
}

/// Create a merged stream of workflow events and LLM streaming events.
///
/// This function merges the main workflow context stream with any
/// LLM streaming events, ensuring proper ordering and event sequencing.
///
/// # Arguments
/// * `workflow_stream` - Main workflow event stream
/// * `llm_stream` - Optional LLM streaming event stream
///
/// # Returns
/// Merged event stream with all events
pub fn merge_workflow_and_llm_streams<W, L>(
    workflow_stream: W,
    llm_stream: Option<L>,
) -> BoxStream<'static, AgUiEvent>
where
    W: futures::Stream<Item = AgUiEvent> + Send + 'static,
    L: futures::Stream<Item = AgUiEvent> + Send + 'static,
{
    match llm_stream {
        Some(llm) => {
            // Use select to interleave events from both streams
            let merged = futures::stream::select(workflow_stream, llm);
            Box::pin(merged)
        }
        None => Box::pin(workflow_stream),
    }
}

/// Helper to convert ResultOutputItem stream to AG-UI events with custom message ID.
///
/// # Arguments
/// * `stream` - Stream of ResultOutputItem
/// * `message_id` - Custom message ID to use
///
/// # Returns
/// Stream of AG-UI TEXT_MESSAGE_* events
pub fn convert_result_stream_to_events(
    stream: BoxStream<'static, ResultOutputItem>,
    message_id: MessageId,
) -> impl futures::Stream<Item = AgUiEvent> + Send + 'static {
    result_output_stream_to_ag_ui_events_with_end_guarantee(stream, message_id)
}

/// Convert a JSON stream to AG-UI TEXT_MESSAGE_* events.
///
/// This function transforms a stream of decoded JSON values (from `listen_job_result_as_json_stream`)
/// into the AG-UI event sequence:
/// - TEXT_MESSAGE_START (once, at the beginning)
/// - TEXT_MESSAGE_CONTENT (for each JSON chunk, converted to text)
/// - TEXT_MESSAGE_END (once, at the end)
///
/// # Arguments
/// * `stream` - Stream of JSON values from FunctionCallHelper::listen_job_result_as_json_stream
/// * `message_id` - MessageId to use for all events in this sequence
///
/// # Returns
/// A stream of AgUiEvent representing the response
pub fn convert_json_stream_to_events(
    stream: std::pin::Pin<
        Box<dyn futures::Stream<Item = anyhow::Result<serde_json::Value>> + Send>,
    >,
    message_id: MessageId,
) -> impl futures::Stream<Item = AgUiEvent> + Send + 'static {
    use crate::types::message::Role;
    use futures::StreamExt;

    let message_id_for_start = message_id.clone();
    let message_id_for_content = message_id.clone();
    let message_id_for_end = message_id;

    // Emit TEXT_MESSAGE_START first
    let start_stream = futures::stream::once(async move {
        AgUiEvent::text_message_start(message_id_for_start, Role::Assistant)
    });

    // Convert JSON chunks to TEXT_MESSAGE_CONTENT events
    let content_stream = stream.filter_map(move |result| {
        let message_id = message_id_for_content.clone();

        async move {
            match result {
                Ok(json) => {
                    // Extract text from JSON value
                    let text = extract_text_from_json(&json);
                    if text.is_empty() {
                        None
                    } else {
                        Some(AgUiEvent::text_message_content(message_id, text))
                    }
                }
                Err(e) => {
                    // Log error but continue stream
                    tracing::warn!("Error in JSON stream: {:?}", e);
                    None
                }
            }
        }
    });

    // Emit TEXT_MESSAGE_END after stream completes
    let end_stream =
        futures::stream::once(async move { AgUiEvent::text_message_end(message_id_for_end) });

    start_stream.chain(content_stream).chain(end_stream)
}

/// Extract text content from a JSON value.
///
/// The JSON may come from various runner types (LLM, Command, etc.).
/// This function extracts text in a runner-agnostic way:
/// - For objects with "text" or "content" fields, extract those
/// - For strings, return directly
/// - For other types, serialize to string
fn extract_text_from_json(json: &serde_json::Value) -> String {
    match json {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(obj) => {
            // Try common text field names
            if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                return text.to_string();
            }
            if let Some(content) = obj.get("content").and_then(|v| v.as_str()) {
                return content.to_string();
            }
            // For LlmChatResult JSON format: content.text
            if let Some(content_obj) = obj.get("content").and_then(|v| v.as_object()) {
                if let Some(text) = content_obj.get("text").and_then(|v| v.as_str()) {
                    return text.to_string();
                }
            }
            // Fallback: serialize the entire object
            serde_json::to_string(json).unwrap_or_default()
        }
        serde_json::Value::Null => String::new(),
        _ => serde_json::to_string(json).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[tokio::test]
    async fn test_merge_streams_without_llm() {
        let workflow_events = vec![
            AgUiEvent::run_started("run_1", "thread_1"),
            AgUiEvent::run_finished("run_1", None),
        ];
        let workflow_stream = futures::stream::iter(workflow_events);

        let merged = merge_workflow_and_llm_streams::<_, futures::stream::Empty<AgUiEvent>>(
            workflow_stream,
            None,
        );
        let events: Vec<_> = merged.collect().await;

        assert_eq!(events.len(), 2);
    }

    #[tokio::test]
    async fn test_merge_streams_with_llm() {
        let workflow_events = vec![AgUiEvent::run_started("run_1", "thread_1")];
        let workflow_stream = futures::stream::iter(workflow_events);

        let llm_events = vec![
            AgUiEvent::text_message_start("msg_1", crate::types::message::Role::Assistant),
            AgUiEvent::text_message_content("msg_1", "Hello"),
            AgUiEvent::text_message_end("msg_1"),
        ];
        let llm_stream = futures::stream::iter(llm_events);

        let merged = merge_workflow_and_llm_streams(workflow_stream, Some(llm_stream));
        let events: Vec<_> = merged.collect().await;

        // Should have all 4 events (1 workflow + 3 LLM)
        assert_eq!(events.len(), 4);
    }
}
