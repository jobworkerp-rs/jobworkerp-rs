//! Integration tests for AG-UI Front Phase 4 features.
//!
//! These tests verify the integration of LLM streaming, Pub/Sub, STATE_DELTA,
//! and session management components working together.

use ag_ui_front::events::state_diff::{
    calculate_state_diff, create_state_delta_event, StateTracker,
};
use ag_ui_front::events::{
    result_output_stream_to_ag_ui_events, result_output_stream_to_ag_ui_events_with_end_guarantee,
    AgUiEvent,
};
use ag_ui_front::pubsub::merge_workflow_and_llm_streams;
use ag_ui_front::types::ids::MessageId;
use ag_ui_front::types::message::Role;
use ag_ui_front::types::state::{TaskState, WorkflowState, WorkflowStatus};
use ag_ui_front::{EventStore, InMemoryEventStore, InMemorySessionManager, SessionManager};
use futures::stream::BoxStream;
use futures::StreamExt;
use proto::jobworkerp::data::{result_output_item, ResultOutputItem, Trailer};
use std::collections::HashMap;

// Helper functions for creating test data
fn create_data_item(data: &str) -> ResultOutputItem {
    ResultOutputItem {
        item: Some(result_output_item::Item::Data(data.as_bytes().to_vec())),
    }
}

fn create_end_item() -> ResultOutputItem {
    ResultOutputItem {
        item: Some(result_output_item::Item::End(Trailer {
            metadata: HashMap::new(),
        })),
    }
}

/// Test: Complete LLM streaming flow with TEXT_MESSAGE_* events
#[tokio::test]
async fn test_complete_llm_streaming_flow() {
    // Simulate LLM streaming response
    let items = vec![
        create_data_item("Hello, "),
        create_data_item("how can "),
        create_data_item("I help "),
        create_data_item("you today?"),
        create_end_item(),
    ];
    let stream: BoxStream<'static, ResultOutputItem> = Box::pin(futures::stream::iter(items));

    let message_id = MessageId::new("msg_llm_flow");
    let event_stream = result_output_stream_to_ag_ui_events(stream, message_id);
    let events: Vec<_> = event_stream.collect().await;

    // Verify event sequence: START, CONTENT x4, END
    assert_eq!(events.len(), 6);

    // Verify START event
    assert!(
        matches!(&events[0], AgUiEvent::TextMessageStart { role, .. } if *role == Role::Assistant)
    );

    // Verify CONTENT events contain correct text
    let mut full_text = String::new();
    for event in &events[1..5] {
        if let AgUiEvent::TextMessageContent { delta, .. } = event {
            full_text.push_str(delta);
        }
    }
    assert_eq!(full_text, "Hello, how can I help you today?");

    // Verify END event
    assert!(matches!(&events[5], AgUiEvent::TextMessageEnd { .. }));
}

/// Test: Stream merging workflow events with LLM streaming events
#[tokio::test]
async fn test_merged_workflow_and_llm_streams() {
    // Create workflow events
    let workflow_events = vec![
        AgUiEvent::run_started("run_merge", "thread_merge"),
        AgUiEvent::step_started("step_llm", "LLM Call"),
    ];
    let workflow_stream = futures::stream::iter(workflow_events);

    // Create LLM streaming events
    let llm_events = vec![
        AgUiEvent::text_message_start(MessageId::new("msg_1"), Role::Assistant),
        AgUiEvent::text_message_content(MessageId::new("msg_1"), "Response"),
        AgUiEvent::text_message_end(MessageId::new("msg_1")),
    ];
    let llm_stream = futures::stream::iter(llm_events);

    // Merge streams
    let merged = merge_workflow_and_llm_streams(workflow_stream, Some(llm_stream));
    let events: Vec<_> = merged.collect().await;

    // Should have all 5 events (2 workflow + 3 LLM)
    assert_eq!(events.len(), 5);

    // Verify we have both workflow and LLM events
    let has_run_started = events
        .iter()
        .any(|e| matches!(e, AgUiEvent::RunStarted { .. }));
    let has_step_started = events
        .iter()
        .any(|e| matches!(e, AgUiEvent::StepStarted { .. }));
    let has_text_start = events
        .iter()
        .any(|e| matches!(e, AgUiEvent::TextMessageStart { .. }));
    let has_text_content = events
        .iter()
        .any(|e| matches!(e, AgUiEvent::TextMessageContent { .. }));
    let has_text_end = events
        .iter()
        .any(|e| matches!(e, AgUiEvent::TextMessageEnd { .. }));

    assert!(has_run_started);
    assert!(has_step_started);
    assert!(has_text_start);
    assert!(has_text_content);
    assert!(has_text_end);
}

/// Test: STATE_DELTA calculation and event generation
#[tokio::test]
async fn test_state_delta_workflow() {
    // Initial state
    let state1 = WorkflowState::new("test_workflow")
        .with_status(WorkflowStatus::Running)
        .with_current_task(TaskState::new("step_1"));

    // Updated state with progress
    let mut vars = HashMap::new();
    vars.insert("progress".to_string(), serde_json::json!(50));
    let state2 = WorkflowState::new("test_workflow")
        .with_status(WorkflowStatus::Running)
        .with_current_task(TaskState::new("step_2"))
        .with_context_variables(vars.clone());

    // Calculate diff
    let diff = calculate_state_diff(&state1, &state2);
    assert!(!diff.is_empty());

    // Verify diff contains expected changes
    let diff_str = serde_json::to_string(&diff).unwrap();
    assert!(diff_str.contains("step_2") || diff_str.contains("current_step"));

    // Create delta event
    let event = create_state_delta_event(&state1, &state2);
    assert!(event.is_some());
    assert!(matches!(event.unwrap(), AgUiEvent::StateDelta { .. }));
}

/// Test: StateTracker for continuous state monitoring
#[tokio::test]
async fn test_state_tracker_lifecycle() {
    let mut tracker = StateTracker::new();

    // First update should return StateSnapshot
    let state1 = WorkflowState::new("tracker_test").with_status(WorkflowStatus::Pending);
    let event1 = tracker.update(state1);
    assert!(matches!(event1, Some(AgUiEvent::StateSnapshot { .. })));

    // Second update with change should return StateDelta
    let state2 = WorkflowState::new("tracker_test").with_status(WorkflowStatus::Running);
    let event2 = tracker.update(state2);
    assert!(matches!(event2, Some(AgUiEvent::StateDelta { .. })));

    // Third update with same state should return None
    let state3 = WorkflowState::new("tracker_test").with_status(WorkflowStatus::Running);
    let event3 = tracker.update(state3);
    assert!(event3.is_none());

    // Final update with completion should return StateDelta
    let state4 = WorkflowState::new("tracker_test").with_status(WorkflowStatus::Completed);
    let event4 = tracker.update(state4);
    assert!(matches!(event4, Some(AgUiEvent::StateDelta { .. })));

    // Verify current state
    let current = tracker.current_state().unwrap();
    assert_eq!(current.status, WorkflowStatus::Completed);
}

/// Test: Session and Event Store integration
#[tokio::test]
async fn test_session_and_event_store_integration() {
    use ag_ui_front::session::SessionState;
    use ag_ui_front::types::ids::{RunId, ThreadId};

    let session_manager = InMemorySessionManager::new(3600);
    let event_store = InMemoryEventStore::new(1000, 3600);

    // Create session
    let run_id = RunId::new("integration_run");
    let thread_id = ThreadId::new("integration_thread");
    let session = session_manager
        .create_session(run_id.clone(), thread_id)
        .await;

    // Store events
    let events = [
        AgUiEvent::run_started(run_id.to_string(), "integration_thread"),
        AgUiEvent::step_started("step_1", "Task 1"),
        AgUiEvent::text_message_start(MessageId::new("msg_1"), Role::Assistant),
        AgUiEvent::text_message_content(MessageId::new("msg_1"), "Hello"),
        AgUiEvent::text_message_end(MessageId::new("msg_1")),
        AgUiEvent::step_finished("step_1", None),
        AgUiEvent::run_finished(run_id.to_string(), None),
    ];

    for (i, event) in events.iter().enumerate() {
        event_store
            .store_event(run_id.as_str(), i as u64, event.clone())
            .await;
        session_manager
            .update_last_event_id(&session.session_id, i as u64)
            .await;
    }

    // Verify session state
    let updated_session = session_manager
        .get_session(&session.session_id)
        .await
        .unwrap();
    assert_eq!(updated_session.last_event_id, 6);

    // Verify event retrieval
    let all_events = event_store.get_all_events(run_id.as_str()).await;
    assert_eq!(all_events.len(), 7);

    // Verify replay from specific point
    let events_since_3 = event_store.get_events_since(run_id.as_str(), 3).await;
    assert_eq!(events_since_3.len(), 3); // Events 4, 5, 6

    // Update session state to completed
    session_manager
        .set_session_state(&session.session_id, SessionState::Completed)
        .await;

    let final_session = session_manager
        .get_session(&session.session_id)
        .await
        .unwrap();
    assert_eq!(final_session.state, SessionState::Completed);
}

/// Test: End guarantee for incomplete streams
#[tokio::test]
async fn test_stream_end_guarantee() {
    // Stream that doesn't have explicit end
    let items = vec![
        create_data_item("Partial"),
        create_data_item(" response"),
        // No end item - simulating interrupted stream
    ];
    let stream: BoxStream<'static, ResultOutputItem> = Box::pin(futures::stream::iter(items));

    let message_id = MessageId::new("msg_incomplete");
    let event_stream =
        result_output_stream_to_ag_ui_events_with_end_guarantee(stream, message_id.clone());
    let events: Vec<_> = event_stream.collect().await;

    // Should still have START, 2 CONTENT, and guaranteed END
    assert_eq!(events.len(), 4);

    // Verify END event is present
    assert!(matches!(
        events.last().unwrap(),
        AgUiEvent::TextMessageEnd { .. }
    ));
}

/// Test: Multiple concurrent sessions
#[tokio::test]
async fn test_multiple_concurrent_sessions() {
    use ag_ui_front::types::ids::{RunId, ThreadId};

    let session_manager = InMemorySessionManager::new(3600);
    let event_store = InMemoryEventStore::new(1000, 3600);

    // Create multiple sessions
    let sessions: Vec<_> = futures::future::join_all((0..5).map(|i| {
        let sm = session_manager.clone();
        async move {
            sm.create_session(
                RunId::new(format!("run_{}", i)),
                ThreadId::new(format!("thread_{}", i)),
            )
            .await
        }
    }))
    .await;

    // Store events for each session
    for (i, _session) in sessions.iter().enumerate() {
        let run_id = format!("run_{}", i);
        for j in 0..10 {
            let event = AgUiEvent::text_message_content(
                MessageId::new(format!("msg_{}_{}", i, j)),
                format!("Content {} for run {}", j, i),
            );
            event_store.store_event(&run_id, j as u64, event).await;
        }
    }

    // Verify each session's events are isolated
    for i in 0..5 {
        let run_id = format!("run_{}", i);
        let events = event_store.get_all_events(&run_id).await;
        assert_eq!(events.len(), 10);

        // Verify content is specific to this run
        if let AgUiEvent::TextMessageContent { delta, .. } = &events[0].1 {
            assert!(delta.contains(&format!("run {}", i)));
        }
    }

    // Verify session isolation
    for (i, session) in sessions.iter().enumerate() {
        let retrieved = session_manager
            .get_session(&session.session_id)
            .await
            .unwrap();
        assert_eq!(retrieved.run_id.as_str(), &format!("run_{}", i));
    }
}

/// Test: Event store max capacity
#[tokio::test]
async fn test_event_store_capacity_limit() {
    let event_store = InMemoryEventStore::new(5, 3600); // Small limit for testing

    // Store more events than capacity
    for i in 0..10 {
        let event = AgUiEvent::text_message_content(
            MessageId::new(format!("msg_{}", i)),
            format!("Content {}", i),
        );
        event_store
            .store_event("capacity_test", i as u64, event)
            .await;
    }

    // Should only have last 5 events
    let events = event_store.get_all_events("capacity_test").await;
    assert_eq!(events.len(), 5);

    // Verify oldest events were removed (should have events 5-9)
    assert_eq!(events[0].0, 5);
    assert_eq!(events[4].0, 9);
}

/// Test: StateTracker configuration options
#[tokio::test]
async fn test_state_tracker_configuration() {
    // Test with snapshots disabled
    let mut tracker = StateTracker::new().with_snapshots(false);

    let state1 = WorkflowState::new("config_test").with_status(WorkflowStatus::Running);
    let event1 = tracker.update(state1);
    // First update should return None when snapshots disabled
    assert!(event1.is_none());

    // Second update should still produce delta
    let state2 = WorkflowState::new("config_test").with_status(WorkflowStatus::Completed);
    let event2 = tracker.update(state2);
    assert!(matches!(event2, Some(AgUiEvent::StateDelta { .. })));

    // Test with deltas disabled
    let mut tracker2 = StateTracker::new().with_deltas(false);

    let state3 = WorkflowState::new("config_test2").with_status(WorkflowStatus::Running);
    let event3 = tracker2.update(state3);
    // First update should still produce snapshot
    assert!(matches!(event3, Some(AgUiEvent::StateSnapshot { .. })));

    // Second update should return None when deltas disabled
    let state4 = WorkflowState::new("config_test2").with_status(WorkflowStatus::Completed);
    let event4 = tracker2.update(state4);
    assert!(event4.is_none());
}

/// Test: Forced snapshot emission for reconnection
#[tokio::test]
async fn test_state_tracker_forced_snapshot() {
    let mut tracker = StateTracker::new();

    // Setup initial state
    let state = WorkflowState::new("reconnect_test")
        .with_status(WorkflowStatus::Running)
        .with_current_task(TaskState::new("step_5"));
    let _ = tracker.update(state);

    // Simulate reconnection - client requests current state
    let snapshot = tracker.emit_current_snapshot();
    assert!(snapshot.is_some());

    match snapshot.unwrap() {
        AgUiEvent::StateSnapshot { snapshot, .. } => {
            assert_eq!(snapshot.workflow_name, "reconnect_test");
            assert_eq!(snapshot.status, WorkflowStatus::Running);
            assert_eq!(
                snapshot.current_task.as_ref().map(|t| t.name.as_str()),
                Some("step_5")
            );
        }
        _ => panic!("Expected StateSnapshot"),
    }
}
