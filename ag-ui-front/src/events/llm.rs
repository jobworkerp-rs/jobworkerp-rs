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
use proto::jobworkerp::data::{result_output_item, ResultOutputItem};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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
/// let (job_result, stream_opt) = job_result_app
///     .listen_result(&job_id, None, Some(&worker_name), timeout, true)
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
                    let content = String::from_utf8_lossy(&bytes).to_string();
                    if content.is_empty() {
                        None
                    } else {
                        Some(AgUiEvent::text_message_content(message_id, content))
                    }
                }
                Some(result_output_item::Item::End(_)) => {
                    // Emit END if START was emitted (always true after start_stream runs)
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
                    let content = String::from_utf8_lossy(&bytes).to_string();
                    if content.is_empty() {
                        None
                    } else {
                        Some(AgUiEvent::text_message_content(message_id, content))
                    }
                }
                Some(result_output_item::Item::End(_)) => {
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
    use proto::jobworkerp::data::Trailer;

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
}
