//! LLM streaming event conversion for AG-UI protocol.
//!
//! This module provides utilities to convert jobworkerp-rs streaming results
//! (ResultOutputItem) to AG-UI TEXT_MESSAGE_* events.
//!
//! # Design
//!
//! Uses existing `STREAMING_TYPE_INTERNAL` + `listen_result` infrastructure:
//! - Worker executes `run_stream()` internally
//! - Chunks are published via Pub/Sub (`broadcast_results: true` required)
//! - `listen_result(streaming=true)` returns both collected result and chunk stream
//! - This module converts the chunk stream to AG-UI events

use crate::events::types::AgUiEvent;
use crate::types::ids::MessageId;
use crate::types::message::Role;
use futures::stream::{BoxStream, StreamExt};
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content;
use jobworkerp_runner::jobworkerp::runner::llm::LlmChatResult;
use prost::Message;
use proto::jobworkerp::data::{result_output_item, ResultOutputItem};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Extracted tool call information from LlmChatResult.
#[derive(Debug, Clone)]
pub struct ExtractedToolCall {
    pub call_id: String,
    pub fn_name: String,
    pub fn_arguments: String,
}

/// Result of extracting tool calls from LlmChatResult.
#[derive(Debug, Clone)]
pub struct ExtractedToolCalls {
    /// List of tool calls
    pub tool_calls: Vec<ExtractedToolCall>,
    /// Whether the client needs to execute these tools (HITL mode)
    /// - true: requires_tool_execution=true, client must approve and send results
    /// - false: auto-calling mode, tools were already executed by server
    pub requires_execution: bool,
}

/// Extract tool calls from LlmChatResult bytes (supports both protobuf and JSON formats).
///
/// This function extracts tool call information from both:
/// - `content.tool_calls` (auto-calling mode, for display only)
/// - `pending_tool_calls` (HITL mode, requires client execution)
///
/// The bytes can be either:
/// - Protobuf-encoded LlmChatResult (from streaming data)
/// - JSON-serialized serde_json::Value (from StreamingJobCompleted context.output)
///
/// Returns `Some(ExtractedToolCalls)` if tool calls are found.
pub fn extract_tool_calls_from_llm_result(bytes: &[u8]) -> Option<ExtractedToolCalls> {
    // First try protobuf decoding
    if let Ok(result) = LlmChatResult::decode(bytes) {
        if let Some(extracted) = extract_tool_calls_from_protobuf_result(&result) {
            return Some(extracted);
        }
    }

    // Fallback: try JSON decoding (for StreamingJobCompleted context.output)
    if let Ok(json_value) = serde_json::from_slice::<serde_json::Value>(bytes) {
        if let Some(extracted) = extract_tool_calls_from_json(&json_value) {
            return Some(extracted);
        }
    }

    tracing::trace!("No tool calls found in bytes (tried both protobuf and JSON)");
    None
}

/// Extract tool calls from a protobuf LlmChatResult.
fn extract_tool_calls_from_protobuf_result(result: &LlmChatResult) -> Option<ExtractedToolCalls> {
    // Check for pending_tool_calls first (HITL mode)
    if let Some(pending) = &result.pending_tool_calls {
        if !pending.calls.is_empty() {
            let tool_calls = pending
                .calls
                .iter()
                .map(|call| ExtractedToolCall {
                    call_id: call.call_id.clone(),
                    fn_name: call.fn_name.clone(),
                    fn_arguments: call.fn_arguments.clone(),
                })
                .collect();
            let requires_execution = result.requires_tool_execution.unwrap_or(true);
            tracing::debug!(
                "Extracted {} pending tool calls from protobuf, requires_execution={}",
                pending.calls.len(),
                requires_execution
            );
            return Some(ExtractedToolCalls {
                tool_calls,
                requires_execution,
            });
        }
    }

    // Check for content.tool_calls (auto-calling mode, for display)
    if let Some(content) = &result.content {
        if let Some(message_content::Content::ToolCalls(tool_calls)) = &content.content {
            if !tool_calls.calls.is_empty() {
                let extracted = tool_calls
                    .calls
                    .iter()
                    .map(|call| ExtractedToolCall {
                        call_id: call.call_id.clone(),
                        fn_name: call.fn_name.clone(),
                        fn_arguments: call.fn_arguments.clone(),
                    })
                    .collect();
                tracing::debug!(
                    "Extracted {} tool calls from protobuf content (auto-calling mode)",
                    tool_calls.calls.len()
                );
                return Some(ExtractedToolCalls {
                    tool_calls: extracted,
                    requires_execution: false,
                });
            }
        }
    }

    None
}

/// Extract tool calls from JSON value (for StreamingJobCompleted context.output).
///
/// This function handles multiple JSON structures because the protobuf `LlmChatResult`
/// uses `oneof` for content types, and different serialization paths produce different
/// JSON shapes. Additionally, the auto-calling mode vs HITL mode affects where tool
/// calls appear in the response.
///
/// # Supported JSON Structures
///
/// ## 1. HITL Mode: `pending_tool_calls` / `pendingToolCalls` (root level)
///
/// When `isAutoCalling: false`, the LLM returns pending tool calls at the root level:
/// ```json
/// {
///   "pending_tool_calls": {
///     "calls": [{ "call_id": "...", "fn_name": "...", "fn_arguments": "..." }]
///   },
///   "requires_tool_execution": true
/// }
/// ```
/// or camelCase (prost JSON serialization):
/// ```json
/// {
///   "pendingToolCalls": {
///     "calls": [{ "callId": "...", "fnName": "...", "fnArguments": "..." }]
///   },
///   "requiresToolExecution": true
/// }
/// ```
///
/// ## 2. Auto-calling Mode: `content.tool_calls` / `content.toolCalls`
///
/// When `isAutoCalling: true`, tool calls are nested inside the `content` field:
/// ```json
/// {
///   "content": {
///     "tool_calls": {
///       "calls": [{ "call_id": "...", "fn_name": "...", "fn_arguments": "..." }]
///     }
///   }
/// }
/// ```
///
/// ## 3. Nested oneof: `content.content.ToolCalls` / `content.content.tool_calls`
///
/// Due to protobuf `oneof MessageContent { ToolCalls tool_calls = N; ... }` serialization,
/// tool calls may be double-nested when the content wrapper contains another content field:
/// ```json
/// {
///   "content": {
///     "content": {
///       "ToolCalls": { "calls": [...] }
///     }
///   }
/// }
/// ```
/// or snake_case variant:
/// ```json
/// {
///   "content": {
///     "content": {
///       "tool_calls": { "calls": [...] }
///     }
///   }
/// }
/// ```
///
/// # Search Path Summary
///
/// | Path | Mode | Description |
/// |------|------|-------------|
/// | `pending_tool_calls` / `pendingToolCalls` | HITL | Root-level pending calls |
/// | `content.tool_calls` / `content.toolCalls` | Auto | Direct tool calls in content |
/// | `content.content.ToolCalls` | Auto | Protobuf oneof with PascalCase key |
/// | `content.content.tool_calls` | Auto | Protobuf oneof with snake_case key |
///
/// The `requires_tool_execution` flag is always checked at the root level to determine
/// whether the client should wait for user approval (HITL mode) or if tools were
/// auto-executed.
fn extract_tool_calls_from_json(value: &serde_json::Value) -> Option<ExtractedToolCalls> {
    let obj = value.as_object()?;

    // Check for pending_tool_calls (snake_case or camelCase)
    let pending_calls = obj
        .get("pending_tool_calls")
        .or_else(|| obj.get("pendingToolCalls"));

    if let Some(pending) = pending_calls {
        if let Some(calls_arr) = pending.get("calls").and_then(|c| c.as_array()) {
            if !calls_arr.is_empty() {
                let tool_calls: Vec<ExtractedToolCall> = calls_arr
                    .iter()
                    .filter_map(|call| {
                        let call_obj = call.as_object()?;
                        let call_id = call_obj
                            .get("call_id")
                            .or_else(|| call_obj.get("callId"))
                            .and_then(|v| v.as_str())?
                            .to_string();
                        let fn_name = call_obj
                            .get("fn_name")
                            .or_else(|| call_obj.get("fnName"))
                            .and_then(|v| v.as_str())?
                            .to_string();
                        let fn_arguments = call_obj
                            .get("fn_arguments")
                            .or_else(|| call_obj.get("fnArguments"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        Some(ExtractedToolCall {
                            call_id,
                            fn_name,
                            fn_arguments,
                        })
                    })
                    .collect();

                if !tool_calls.is_empty() {
                    let requires_execution = obj
                        .get("requires_tool_execution")
                        .or_else(|| obj.get("requiresToolExecution"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);

                    tracing::debug!(
                        "Extracted {} pending tool calls from JSON, requires_execution={}",
                        tool_calls.len(),
                        requires_execution
                    );
                    return Some(ExtractedToolCalls {
                        tool_calls,
                        requires_execution,
                    });
                }
            }
        }
    }

    // Check for content.tool_calls
    // Note: Even if tool_calls are in content, check requires_tool_execution flag
    // to determine if this is HITL mode
    if let Some(content) = obj.get("content") {
        if let Some(content_obj) = content.as_object() {
            // Check for toolCalls in content object
            let tool_calls_value = content_obj
                .get("tool_calls")
                .or_else(|| content_obj.get("toolCalls"))
                .or_else(|| {
                    content_obj.get("content").and_then(|c| {
                        c.as_object()
                            .and_then(|co| co.get("ToolCalls").or_else(|| co.get("tool_calls")))
                    })
                });

            if let Some(tc) = tool_calls_value {
                let calls_arr = tc
                    .get("calls")
                    .and_then(|c| c.as_array())
                    .or_else(|| tc.as_array());

                if let Some(calls) = calls_arr {
                    if !calls.is_empty() {
                        let extracted: Vec<ExtractedToolCall> = calls
                            .iter()
                            .filter_map(|call| {
                                let call_obj = call.as_object()?;
                                let call_id = call_obj
                                    .get("call_id")
                                    .or_else(|| call_obj.get("callId"))
                                    .and_then(|v| v.as_str())?
                                    .to_string();
                                let fn_name = call_obj
                                    .get("fn_name")
                                    .or_else(|| call_obj.get("fnName"))
                                    .and_then(|v| v.as_str())?
                                    .to_string();
                                let fn_arguments = call_obj
                                    .get("fn_arguments")
                                    .or_else(|| call_obj.get("fnArguments"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                Some(ExtractedToolCall {
                                    call_id,
                                    fn_name,
                                    fn_arguments,
                                })
                            })
                            .collect();

                        if !extracted.is_empty() {
                            // Check requires_tool_execution flag at root level
                            // This determines if it's HITL mode (requires client approval)
                            let requires_execution = obj
                                .get("requires_tool_execution")
                                .or_else(|| obj.get("requiresToolExecution"))
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);

                            tracing::debug!(
                                "Extracted {} tool calls from JSON content, requires_execution={}",
                                extracted.len(),
                                requires_execution
                            );
                            return Some(ExtractedToolCalls {
                                tool_calls: extracted,
                                requires_execution,
                            });
                        }
                    }
                }
            }
        }
    }

    None
}

/// Convert extracted tool calls to AG-UI TOOL_CALL events.
///
/// Generates TOOL_CALL_START and TOOL_CALL_ARGS events for each tool call.
/// Note: TOOL_CALL_END is NOT generated here - it should be emitted:
/// - Immediately after ARGS in auto-calling mode
/// - After client sends result in HITL mode
pub fn tool_calls_to_ag_ui_events(
    tool_calls: &[ExtractedToolCall],
    parent_message_id: Option<&MessageId>,
) -> Vec<AgUiEvent> {
    let mut events = Vec::new();

    for call in tool_calls {
        // TOOL_CALL_START
        events.push(AgUiEvent::tool_call_start(
            call.call_id.clone(),
            call.fn_name.clone(),
            parent_message_id.map(|m| m.to_string()),
        ));

        // TOOL_CALL_ARGS (full arguments as single delta)
        events.push(AgUiEvent::tool_call_args(
            call.call_id.clone(),
            call.fn_arguments.clone(),
        ));
    }

    events
}

/// Extract text content from LlmChatResult protobuf bytes.
///
/// LLM_CHAT runner streams serialized LlmChatResult protobuf messages.
/// This function decodes the bytes and extracts the text content.
///
/// Returns `Some(text)` if the protobuf decodes successfully and contains text.
/// Falls back to UTF-8 string interpretation if protobuf decoding fails.
pub fn extract_text_from_llm_chat_result(bytes: &[u8]) -> Option<String> {
    match LlmChatResult::decode(bytes) {
        Ok(result) => {
            if let Some(content) = result.content {
                match content.content {
                    Some(message_content::Content::Text(text)) => {
                        tracing::debug!("Extracted text content: {}", text);
                        if text.is_empty() {
                            None
                        } else {
                            Some(text)
                        }
                    }
                    Some(message_content::Content::ToolCalls(_)) => {
                        // Tool calls are not rendered as text content in AG-UI
                        tracing::debug!("Tool calls are not rendered as text content in AG-UI");
                        None
                    }
                    Some(message_content::Content::Image(_)) => {
                        // Images are not rendered as text content
                        tracing::debug!("Images are not rendered as text content");
                        None
                    }
                    None => {
                        tracing::info!("No content in LlmChatResult");
                        None
                    }
                }
            } else {
                tracing::info!("No content in LlmChatResult");
                None
            }
        }
        Err(e) => {
            tracing::warn!("Failed to decode LlmChatResult from stream data: {}", e);
            // Fallback: try interpreting as raw UTF-8 text for non-LLM streams
            let content = String::from_utf8_lossy(bytes).to_string();
            if content.is_empty() {
                None
            } else {
                Some(content)
            }
        }
    }
}

/// Convert a ResultOutputItem stream to AG-UI TEXT_MESSAGE_* events.
///
/// This function transforms the streaming output from `listen_result(streaming=true)`
/// into the AG-UI event sequence:
/// - TEXT_MESSAGE_START (once, at the beginning)
/// - TEXT_MESSAGE_CONTENT (for each data chunk)
/// - TEXT_MESSAGE_END (once, at the end)
///
/// # Arguments
/// * `stream` - Stream of ResultOutputItem from JobResultSubscriber
/// * `message_id` - MessageId to use for all events in this sequence
///
/// # Returns
/// A stream of AgUiEvent representing the LLM response
///
/// # Example
/// ```ignore
/// use crate::pubsub::DEFAULT_METHOD_NAME;
///
/// let (job_result, stream_opt) = job_result_app
///     .listen_result(&job_id, None, Some(&worker_name), timeout, true, DEFAULT_METHOD_NAME)
///     .await?;
///
/// if let Some(stream) = stream_opt {
///     let message_id = MessageId::random();
///     let event_stream = result_output_stream_to_ag_ui_events(stream, message_id);
///     // yield events from event_stream
/// }
/// ```
pub fn result_output_stream_to_ag_ui_events(
    stream: BoxStream<'static, ResultOutputItem>,
    message_id: MessageId,
) -> impl futures::Stream<Item = AgUiEvent> + Send + 'static {
    // Track whether START was emitted (always true after start_stream)
    let start_emitted = Arc::new(AtomicBool::new(false));
    let start_emitted_for_start = start_emitted.clone();
    let message_id_for_start = message_id.clone();
    let message_id_for_content = message_id.clone();

    // Create start event as first item and mark start as emitted
    let start_stream = futures::stream::once(async move {
        start_emitted_for_start.store(true, Ordering::SeqCst);
        AgUiEvent::text_message_start(message_id_for_start, Role::Assistant)
    });

    // Process chunk stream
    let content_stream = stream.filter_map(move |item| {
        let message_id = message_id_for_content.clone();
        let start_emitted = start_emitted.clone();

        async move {
            match item.item {
                Some(result_output_item::Item::Data(bytes)) => {
                    // Decode LlmChatResult protobuf and extract text content
                    extract_text_from_llm_chat_result(&bytes)
                        .map(|content| AgUiEvent::text_message_content(message_id, content))
                }
                Some(result_output_item::Item::End(_)) => {
                    // Emit END if START was emitted (always true after start_stream runs)
                    if start_emitted.load(Ordering::SeqCst) {
                        Some(AgUiEvent::text_message_end(message_id))
                    } else {
                        None
                    }
                }
                Some(result_output_item::Item::FinalCollected(_)) => {
                    // FinalCollected is for workflow internal use, emit END event here
                    if start_emitted.load(Ordering::SeqCst) {
                        Some(AgUiEvent::text_message_end(message_id))
                    } else {
                        None
                    }
                }
                None => None,
            }
        }
    });

    start_stream.chain(content_stream)
}

/// Convert a ResultOutputItem stream to AG-UI TEXT_MESSAGE_* events with end guarantee.
///
/// Similar to `result_output_stream_to_ag_ui_events`, but ensures TEXT_MESSAGE_END
/// is always emitted even if the stream doesn't contain an explicit End marker.
///
/// # Arguments
/// * `stream` - Stream of ResultOutputItem from JobResultSubscriber
/// * `message_id` - MessageId to use for all events in this sequence
///
/// # Returns
/// A stream of AgUiEvent with guaranteed START and END events
pub fn result_output_stream_to_ag_ui_events_with_end_guarantee(
    stream: BoxStream<'static, ResultOutputItem>,
    message_id: MessageId,
) -> impl futures::Stream<Item = AgUiEvent> + Send + 'static {
    let message_id_clone = message_id.clone();
    let has_ended = Arc::new(AtomicBool::new(false));
    let has_ended_for_stream = has_ended.clone();

    let main_stream = result_output_stream_to_ag_ui_events_internal(
        stream,
        message_id_clone,
        has_ended_for_stream,
    );

    // Append end event if not already emitted
    let end_stream = futures::stream::once(async move {
        if !has_ended.load(Ordering::SeqCst) {
            Some(AgUiEvent::text_message_end(message_id))
        } else {
            None
        }
    })
    .filter_map(|x| async move { x });

    main_stream.chain(end_stream)
}

/// Internal helper for stream conversion with end tracking.
fn result_output_stream_to_ag_ui_events_internal(
    stream: BoxStream<'static, ResultOutputItem>,
    message_id: MessageId,
    has_ended: Arc<AtomicBool>,
) -> impl futures::Stream<Item = AgUiEvent> + Send + 'static {
    let message_id_for_start = message_id.clone();
    let message_id_for_content = message_id.clone();

    let start_stream = futures::stream::once(async move {
        AgUiEvent::text_message_start(message_id_for_start, Role::Assistant)
    });

    let content_stream = stream.filter_map(move |item| {
        let message_id = message_id_for_content.clone();
        let has_ended = has_ended.clone();

        async move {
            match item.item {
                Some(result_output_item::Item::Data(bytes)) => {
                    // Decode LlmChatResult protobuf and extract text content
                    extract_text_from_llm_chat_result(&bytes)
                        .map(|content| AgUiEvent::text_message_content(message_id, content))
                }
                Some(result_output_item::Item::End(_)) => {
                    has_ended.store(true, Ordering::SeqCst);
                    Some(AgUiEvent::text_message_end(message_id))
                }
                Some(result_output_item::Item::FinalCollected(_)) => {
                    // FinalCollected is for workflow internal use, emit END event here
                    has_ended.store(true, Ordering::SeqCst);
                    Some(AgUiEvent::text_message_end(message_id))
                }
                None => None,
            }
        }
    });

    start_stream.chain(content_stream)
}

/// Container for LLM streaming result with both event stream and collected result.
///
/// This struct is used when you need both the real-time AG-UI event stream
/// and the final collected JobResult for workflow continuation.
pub struct LlmStreamingResult<S>
where
    S: futures::Stream<Item = AgUiEvent> + Send,
{
    /// AG-UI event stream for real-time UI updates
    pub event_stream: S,
    /// Collected JobResult for workflow continuation (available after stream ends)
    pub job_result: proto::jobworkerp::data::JobResult,
}

impl<S> LlmStreamingResult<S>
where
    S: futures::Stream<Item = AgUiEvent> + Send,
{
    /// Create a new LlmStreamingResult.
    pub fn new(event_stream: S, job_result: proto::jobworkerp::data::JobResult) -> Self {
        Self {
            event_stream,
            job_result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::MessageContent;
    use proto::jobworkerp::data::Trailer;

    /// Create a ResultOutputItem with LlmChatResult protobuf-encoded data (real LLM stream format)
    fn create_llm_chat_result_item(text: &str, done: bool) -> ResultOutputItem {
        let result = LlmChatResult {
            content: Some(MessageContent {
                content: Some(message_content::Content::Text(text.to_string())),
            }),
            reasoning_content: None,
            done,
            usage: None,
            pending_tool_calls: None,
            requires_tool_execution: None,
            tool_execution_results: vec![],
        };
        let bytes = result.encode_to_vec();
        ResultOutputItem {
            item: Some(result_output_item::Item::Data(bytes)),
        }
    }

    // === Tests for tool call extraction ===

    #[test]
    fn test_extract_pending_tool_calls() {
        use jobworkerp_runner::jobworkerp::runner::llm::{PendingToolCalls, ToolCallRequest};

        let result = LlmChatResult {
            content: None,
            reasoning_content: None,
            done: false,
            usage: None,
            pending_tool_calls: Some(PendingToolCalls {
                calls: vec![
                    ToolCallRequest {
                        call_id: "call_1".to_string(),
                        fn_name: "http_request".to_string(),
                        fn_arguments: r#"{"url":"https://example.com"}"#.to_string(),
                    },
                    ToolCallRequest {
                        call_id: "call_2".to_string(),
                        fn_name: "command".to_string(),
                        fn_arguments: r#"{"cmd":"ls -la"}"#.to_string(),
                    },
                ],
            }),
            requires_tool_execution: Some(true),
            tool_execution_results: vec![],
        };
        let bytes = result.encode_to_vec();

        let extracted = extract_tool_calls_from_llm_result(&bytes);
        assert!(extracted.is_some());

        let extracted = extracted.unwrap();
        assert_eq!(extracted.tool_calls.len(), 2);
        assert!(extracted.requires_execution);

        assert_eq!(extracted.tool_calls[0].call_id, "call_1");
        assert_eq!(extracted.tool_calls[0].fn_name, "http_request");
        assert_eq!(extracted.tool_calls[1].call_id, "call_2");
        assert_eq!(extracted.tool_calls[1].fn_name, "command");
    }

    #[test]
    fn test_extract_content_tool_calls() {
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::ToolCalls;

        let result = LlmChatResult {
            content: Some(MessageContent {
                content: Some(message_content::Content::ToolCalls(ToolCalls {
                    calls: vec![
                        jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::ToolCall {
                            call_id: "call_auto_1".to_string(),
                            fn_name: "fetch".to_string(),
                            fn_arguments: r#"{"url":"https://api.example.com"}"#.to_string(),
                        },
                    ],
                })),
            }),
            reasoning_content: None,
            done: false,
            usage: None,
            pending_tool_calls: None,
            requires_tool_execution: None,
            tool_execution_results: vec![],
        };
        let bytes = result.encode_to_vec();

        let extracted = extract_tool_calls_from_llm_result(&bytes);
        assert!(extracted.is_some());

        let extracted = extracted.unwrap();
        assert_eq!(extracted.tool_calls.len(), 1);
        assert!(!extracted.requires_execution); // Auto-calling mode

        assert_eq!(extracted.tool_calls[0].call_id, "call_auto_1");
        assert_eq!(extracted.tool_calls[0].fn_name, "fetch");
    }

    #[test]
    fn test_tool_calls_to_events() {
        let tool_calls = vec![
            ExtractedToolCall {
                call_id: "call_1".to_string(),
                fn_name: "http_request".to_string(),
                fn_arguments: r#"{"url":"https://example.com"}"#.to_string(),
            },
            ExtractedToolCall {
                call_id: "call_2".to_string(),
                fn_name: "command".to_string(),
                fn_arguments: r#"{"cmd":"ls"}"#.to_string(),
            },
        ];

        let events = tool_calls_to_ag_ui_events(&tool_calls, None);

        // 2 tool calls * 2 events each (START + ARGS) = 4 events
        assert_eq!(events.len(), 4);

        // First tool call
        assert!(
            matches!(&events[0], AgUiEvent::ToolCallStart { tool_call_id, tool_call_name, .. } if tool_call_id == "call_1" && tool_call_name == "http_request")
        );
        assert!(
            matches!(&events[1], AgUiEvent::ToolCallArgs { tool_call_id, delta, .. } if tool_call_id == "call_1" && delta.contains("url"))
        );

        // Second tool call
        assert!(
            matches!(&events[2], AgUiEvent::ToolCallStart { tool_call_id, tool_call_name, .. } if tool_call_id == "call_2" && tool_call_name == "command")
        );
        assert!(
            matches!(&events[3], AgUiEvent::ToolCallArgs { tool_call_id, delta, .. } if tool_call_id == "call_2" && delta.contains("cmd"))
        );
    }

    #[test]
    fn test_tool_calls_to_events_with_parent() {
        let tool_calls = vec![ExtractedToolCall {
            call_id: "call_1".to_string(),
            fn_name: "test".to_string(),
            fn_arguments: "{}".to_string(),
        }];

        let parent_id = MessageId::new("msg_parent");
        let events = tool_calls_to_ag_ui_events(&tool_calls, Some(&parent_id));

        match &events[0] {
            AgUiEvent::ToolCallStart {
                parent_message_id, ..
            } => {
                assert_eq!(parent_message_id.as_deref(), Some("msg_parent"));
            }
            _ => panic!("Expected ToolCallStart"),
        }
    }

    #[test]
    fn test_extract_no_tool_calls() {
        let result = LlmChatResult {
            content: Some(MessageContent {
                content: Some(message_content::Content::Text("Hello".to_string())),
            }),
            reasoning_content: None,
            done: false,
            usage: None,
            pending_tool_calls: None,
            requires_tool_execution: None,
            tool_execution_results: vec![],
        };
        let bytes = result.encode_to_vec();

        let extracted = extract_tool_calls_from_llm_result(&bytes);
        assert!(extracted.is_none());
    }

    /// Create a ResultOutputItem with raw string data (fallback test format)
    fn create_data_item(data: &str) -> ResultOutputItem {
        ResultOutputItem {
            item: Some(result_output_item::Item::Data(data.as_bytes().to_vec())),
        }
    }

    fn create_end_item() -> ResultOutputItem {
        ResultOutputItem {
            item: Some(result_output_item::Item::End(Trailer {
                metadata: std::collections::HashMap::new(),
            })),
        }
    }

    // === Tests for LlmChatResult protobuf format (real LLM stream) ===

    #[tokio::test]
    async fn test_llm_chat_result_stream_conversion() {
        let items = vec![
            create_llm_chat_result_item("Hello", false),
            create_llm_chat_result_item(" World", false),
            create_llm_chat_result_item("", true), // Final chunk with done=true, empty text
            create_end_item(),
        ];
        let stream: BoxStream<'static, ResultOutputItem> = Box::pin(futures::stream::iter(items));

        let message_id = MessageId::new("msg_llm_test");
        let event_stream = result_output_stream_to_ag_ui_events(stream, message_id.clone());
        let events: Vec<_> = event_stream.collect().await;

        assert_eq!(events.len(), 4); // START + 2 CONTENT + END

        // Check START event
        match &events[0] {
            AgUiEvent::TextMessageStart {
                message_id: mid, ..
            } => {
                assert_eq!(mid, "msg_llm_test");
            }
            _ => panic!("Expected TextMessageStart"),
        }

        // Check CONTENT events
        match &events[1] {
            AgUiEvent::TextMessageContent { delta, .. } => {
                assert_eq!(delta, "Hello");
            }
            _ => panic!("Expected TextMessageContent with 'Hello'"),
        }

        match &events[2] {
            AgUiEvent::TextMessageContent { delta, .. } => {
                assert_eq!(delta, " World");
            }
            _ => panic!("Expected TextMessageContent with ' World'"),
        }

        // Check END event
        match &events[3] {
            AgUiEvent::TextMessageEnd {
                message_id: mid, ..
            } => {
                assert_eq!(mid, "msg_llm_test");
            }
            _ => panic!("Expected TextMessageEnd"),
        }
    }

    #[tokio::test]
    async fn test_llm_chat_result_unicode() {
        let items = vec![
            create_llm_chat_result_item("こんにちは", false),
            create_llm_chat_result_item("🎉", false),
            create_end_item(),
        ];
        let stream: BoxStream<'static, ResultOutputItem> = Box::pin(futures::stream::iter(items));

        let message_id = MessageId::new("msg_llm_unicode");
        let event_stream = result_output_stream_to_ag_ui_events(stream, message_id);
        let events: Vec<_> = event_stream.collect().await;

        match &events[1] {
            AgUiEvent::TextMessageContent { delta, .. } => {
                assert_eq!(delta, "こんにちは");
            }
            _ => panic!("Expected TextMessageContent"),
        }

        match &events[2] {
            AgUiEvent::TextMessageContent { delta, .. } => {
                assert_eq!(delta, "🎉");
            }
            _ => panic!("Expected TextMessageContent"),
        }
    }

    // === Tests for raw string fallback (non-LLM streams) ===

    #[tokio::test]
    async fn test_basic_stream_conversion() {
        let items = vec![
            create_data_item("Hello"),
            create_data_item(" World"),
            create_end_item(),
        ];
        let stream: BoxStream<'static, ResultOutputItem> = Box::pin(futures::stream::iter(items));

        let message_id = MessageId::new("msg_test");
        let event_stream = result_output_stream_to_ag_ui_events(stream, message_id.clone());
        let events: Vec<_> = event_stream.collect().await;

        assert_eq!(events.len(), 4); // START + 2 CONTENT + END

        // Check START event
        match &events[0] {
            AgUiEvent::TextMessageStart {
                message_id: mid, ..
            } => {
                assert_eq!(mid, "msg_test");
            }
            _ => panic!("Expected TextMessageStart"),
        }

        // Check CONTENT events
        match &events[1] {
            AgUiEvent::TextMessageContent { delta, .. } => {
                assert_eq!(delta, "Hello");
            }
            _ => panic!("Expected TextMessageContent"),
        }

        match &events[2] {
            AgUiEvent::TextMessageContent { delta, .. } => {
                assert_eq!(delta, " World");
            }
            _ => panic!("Expected TextMessageContent"),
        }

        // Check END event
        match &events[3] {
            AgUiEvent::TextMessageEnd {
                message_id: mid, ..
            } => {
                assert_eq!(mid, "msg_test");
            }
            _ => panic!("Expected TextMessageEnd"),
        }
    }

    #[tokio::test]
    async fn test_empty_content_filtered() {
        let items = vec![
            create_data_item("Hello"),
            create_data_item(""), // Empty should be filtered
            create_data_item("World"),
            create_end_item(),
        ];
        let stream: BoxStream<'static, ResultOutputItem> = Box::pin(futures::stream::iter(items));

        let message_id = MessageId::new("msg_test");
        let event_stream = result_output_stream_to_ag_ui_events(stream, message_id);
        let events: Vec<_> = event_stream.collect().await;

        // START + 2 CONTENT (empty filtered) + END
        assert_eq!(events.len(), 4);
    }

    #[tokio::test]
    async fn test_end_guarantee_without_explicit_end() {
        let items = vec![
            create_data_item("Hello"),
            create_data_item("World"),
            // No end item
        ];
        let stream: BoxStream<'static, ResultOutputItem> = Box::pin(futures::stream::iter(items));

        let message_id = MessageId::new("msg_test");
        let event_stream =
            result_output_stream_to_ag_ui_events_with_end_guarantee(stream, message_id);
        let events: Vec<_> = event_stream.collect().await;

        // START + 2 CONTENT + guaranteed END
        assert_eq!(events.len(), 4);

        // Verify last event is END
        match events.last() {
            Some(AgUiEvent::TextMessageEnd { .. }) => {}
            _ => panic!("Expected TextMessageEnd as last event"),
        }
    }

    #[tokio::test]
    async fn test_end_guarantee_with_explicit_end() {
        let items = vec![
            create_data_item("Hello"),
            create_end_item(), // Explicit end
        ];
        let stream: BoxStream<'static, ResultOutputItem> = Box::pin(futures::stream::iter(items));

        let message_id = MessageId::new("msg_test");
        let event_stream =
            result_output_stream_to_ag_ui_events_with_end_guarantee(stream, message_id);
        let events: Vec<_> = event_stream.collect().await;

        // START + 1 CONTENT + END (no duplicate)
        assert_eq!(events.len(), 3);
    }

    #[tokio::test]
    async fn test_message_id_consistency() {
        let items = vec![create_data_item("Test"), create_end_item()];
        let stream: BoxStream<'static, ResultOutputItem> = Box::pin(futures::stream::iter(items));

        let message_id = MessageId::new("consistent_id");
        let event_stream = result_output_stream_to_ag_ui_events(stream, message_id);
        let events: Vec<_> = event_stream.collect().await;

        // All events should have the same message_id
        for event in &events {
            match event {
                AgUiEvent::TextMessageStart { message_id, .. }
                | AgUiEvent::TextMessageContent { message_id, .. }
                | AgUiEvent::TextMessageEnd { message_id, .. } => {
                    assert_eq!(message_id, "consistent_id");
                }
                _ => {}
            }
        }
    }

    #[tokio::test]
    async fn test_unicode_content() {
        let items = vec![
            create_data_item("こんにちは"),
            create_data_item("🎉"),
            create_end_item(),
        ];
        let stream: BoxStream<'static, ResultOutputItem> = Box::pin(futures::stream::iter(items));

        let message_id = MessageId::new("msg_unicode");
        let event_stream = result_output_stream_to_ag_ui_events(stream, message_id);
        let events: Vec<_> = event_stream.collect().await;

        match &events[1] {
            AgUiEvent::TextMessageContent { delta, .. } => {
                assert_eq!(delta, "こんにちは");
            }
            _ => panic!("Expected TextMessageContent"),
        }

        match &events[2] {
            AgUiEvent::TextMessageContent { delta, .. } => {
                assert_eq!(delta, "🎉");
            }
            _ => panic!("Expected TextMessageContent"),
        }
    }

    #[tokio::test]
    async fn test_end_only_stream() {
        // Stream with only End item, no Data
        let items = vec![create_end_item()];
        let stream: BoxStream<'static, ResultOutputItem> = Box::pin(futures::stream::iter(items));

        let message_id = MessageId::new("msg_end_only");
        let event_stream = result_output_stream_to_ag_ui_events(stream, message_id);
        let events: Vec<_> = event_stream.collect().await;

        // Should have START and END (even without content)
        assert_eq!(events.len(), 2);

        // Verify START event
        assert!(matches!(&events[0], AgUiEvent::TextMessageStart { .. }));

        // Verify END event
        assert!(matches!(&events[1], AgUiEvent::TextMessageEnd { .. }));
    }

    #[tokio::test]
    async fn test_empty_stream() {
        // Completely empty stream (no items at all)
        let items: Vec<ResultOutputItem> = vec![];
        let stream: BoxStream<'static, ResultOutputItem> = Box::pin(futures::stream::iter(items));

        let message_id = MessageId::new("msg_empty");
        let event_stream = result_output_stream_to_ag_ui_events(stream, message_id);
        let events: Vec<_> = event_stream.collect().await;

        // Should have START only (no END because no End item in stream)
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], AgUiEvent::TextMessageStart { .. }));
    }

    fn create_final_collected_item(data: &[u8]) -> ResultOutputItem {
        ResultOutputItem {
            item: Some(result_output_item::Item::FinalCollected(data.to_vec())),
        }
    }

    #[tokio::test]
    async fn test_final_collected_emits_end() {
        // Stream with FinalCollected instead of End
        let items = vec![
            create_data_item("Hello"),
            create_final_collected_item(b"collected data"),
        ];
        let stream: BoxStream<'static, ResultOutputItem> = Box::pin(futures::stream::iter(items));

        let message_id = MessageId::new("msg_final_collected");
        let event_stream = result_output_stream_to_ag_ui_events(stream, message_id.clone());
        let events: Vec<_> = event_stream.collect().await;

        // Should have START, CONTENT, END (from FinalCollected)
        assert_eq!(events.len(), 3);

        assert!(matches!(&events[0], AgUiEvent::TextMessageStart { .. }));
        assert!(matches!(&events[1], AgUiEvent::TextMessageContent { .. }));
        assert!(matches!(&events[2], AgUiEvent::TextMessageEnd { .. }));
    }

    #[tokio::test]
    async fn test_final_collected_with_end_guarantee() {
        // Stream with FinalCollected using end guarantee function
        let items = vec![
            create_data_item("World"),
            create_final_collected_item(b"workflow result"),
        ];
        let stream: BoxStream<'static, ResultOutputItem> = Box::pin(futures::stream::iter(items));

        let message_id = MessageId::new("msg_final_collected_guarantee");
        let event_stream =
            result_output_stream_to_ag_ui_events_with_end_guarantee(stream, message_id);
        let events: Vec<_> = event_stream.collect().await;

        // Should have START, CONTENT, END (from FinalCollected, no duplicate)
        assert_eq!(events.len(), 3);

        assert!(matches!(&events[0], AgUiEvent::TextMessageStart { .. }));
        assert!(matches!(&events[1], AgUiEvent::TextMessageContent { .. }));
        assert!(matches!(&events[2], AgUiEvent::TextMessageEnd { .. }));
    }

    // === Tests for JSON format tool call extraction ===

    #[test]
    fn test_extract_tool_calls_from_json_snake_case() {
        let json = serde_json::json!({
            "pending_tool_calls": {
                "calls": [
                    {
                        "call_id": "call_json_1",
                        "fn_name": "http_request",
                        "fn_arguments": r#"{"url":"https://example.com"}"#
                    },
                    {
                        "call_id": "call_json_2",
                        "fn_name": "command",
                        "fn_arguments": r#"{"cmd":"ls"}"#
                    }
                ]
            },
            "requires_tool_execution": true
        });
        let bytes = serde_json::to_vec(&json).unwrap();

        let extracted = extract_tool_calls_from_llm_result(&bytes);
        assert!(extracted.is_some());

        let extracted = extracted.unwrap();
        assert_eq!(extracted.tool_calls.len(), 2);
        assert!(extracted.requires_execution);

        assert_eq!(extracted.tool_calls[0].call_id, "call_json_1");
        assert_eq!(extracted.tool_calls[0].fn_name, "http_request");
        assert_eq!(extracted.tool_calls[1].call_id, "call_json_2");
        assert_eq!(extracted.tool_calls[1].fn_name, "command");
    }

    #[test]
    fn test_extract_tool_calls_from_json_camel_case() {
        let json = serde_json::json!({
            "pendingToolCalls": {
                "calls": [
                    {
                        "callId": "call_camel_1",
                        "fnName": "fetch",
                        "fnArguments": r#"{"url":"https://api.example.com"}"#
                    }
                ]
            },
            "requiresToolExecution": true
        });
        let bytes = serde_json::to_vec(&json).unwrap();

        let extracted = extract_tool_calls_from_llm_result(&bytes);
        assert!(extracted.is_some());

        let extracted = extracted.unwrap();
        assert_eq!(extracted.tool_calls.len(), 1);
        assert!(extracted.requires_execution);

        assert_eq!(extracted.tool_calls[0].call_id, "call_camel_1");
        assert_eq!(extracted.tool_calls[0].fn_name, "fetch");
    }

    #[test]
    fn test_extract_tool_calls_from_json_no_tool_calls() {
        let json = serde_json::json!({
            "content": {
                "content": {
                    "Text": "Hello, world!"
                }
            },
            "done": true
        });
        let bytes = serde_json::to_vec(&json).unwrap();

        let extracted = extract_tool_calls_from_llm_result(&bytes);
        assert!(extracted.is_none());
    }

    #[test]
    fn test_extract_tool_calls_from_json_empty_calls() {
        let json = serde_json::json!({
            "pending_tool_calls": {
                "calls": []
            },
            "requires_tool_execution": true
        });
        let bytes = serde_json::to_vec(&json).unwrap();

        let extracted = extract_tool_calls_from_llm_result(&bytes);
        assert!(extracted.is_none());
    }
}
