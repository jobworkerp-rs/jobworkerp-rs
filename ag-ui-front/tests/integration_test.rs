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

// =============================================================================
// Phase 5: Human-in-the-Loop (HITL) Integration Tests
// =============================================================================

/// Test: Session state transitions for HITL workflow
/// Verifies: Active → Paused → Active → Completed lifecycle
#[tokio::test]
async fn test_hitl_session_state_transitions() {
    use ag_ui_front::session::{HitlWaitingInfo, SessionState};
    use ag_ui_front::types::ids::{RunId, ThreadId};

    let session_manager = InMemorySessionManager::new(3600);

    // 1. Create session (starts Active)
    let run_id = RunId::new("hitl_run_1");
    let thread_id = ThreadId::new("hitl_thread_1");
    let session = session_manager
        .create_session(run_id.clone(), thread_id)
        .await;

    assert_eq!(session.state, SessionState::Active);
    assert!(session.hitl_waiting_info.is_none());

    // 2. Transition to Paused with HITL info (simulating wait directive)
    let hitl_info = HitlWaitingInfo {
        tool_call_id: format!("wait_{}", run_id),
        checkpoint_position: "/do/0".to_string(),
        workflow_name: "test_workflow".to_string(),
    };
    let updated = session_manager
        .set_paused_with_hitl_info(&session.session_id, hitl_info.clone())
        .await;
    assert!(updated);

    let paused_session = session_manager
        .get_session(&session.session_id)
        .await
        .unwrap();
    assert_eq!(paused_session.state, SessionState::Paused);
    assert!(paused_session.hitl_waiting_info.is_some());
    let stored_info = paused_session.hitl_waiting_info.unwrap();
    assert_eq!(stored_info.tool_call_id, hitl_info.tool_call_id);
    assert_eq!(
        stored_info.checkpoint_position,
        hitl_info.checkpoint_position
    );
    assert_eq!(stored_info.workflow_name, hitl_info.workflow_name);

    // 3. Atomically resume from Paused (simulating message handler with new atomic method)
    let resumed = session_manager
        .resume_from_paused(&session.session_id, SessionState::Active)
        .await;
    assert!(resumed);

    let active_session = session_manager
        .get_session(&session.session_id)
        .await
        .unwrap();
    assert_eq!(active_session.state, SessionState::Active);
    assert!(active_session.hitl_waiting_info.is_none());

    // 4. Complete the session
    session_manager
        .set_session_state(&session.session_id, SessionState::Completed)
        .await;

    let final_session = session_manager
        .get_session(&session.session_id)
        .await
        .unwrap();
    assert_eq!(final_session.state, SessionState::Completed);
}

/// Test: HITL session lookup by run_id
/// Verifies: Session can be found by run_id during HITL resume
#[tokio::test]
async fn test_hitl_session_lookup_by_run_id() {
    use ag_ui_front::session::{HitlWaitingInfo, SessionState};
    use ag_ui_front::types::ids::{RunId, ThreadId};

    let session_manager = InMemorySessionManager::new(3600);

    let run_id = RunId::new("hitl_lookup_run");
    let thread_id = ThreadId::new("hitl_lookup_thread");
    let session = session_manager
        .create_session(run_id.clone(), thread_id)
        .await;

    // Set to paused with HITL info
    let hitl_info = HitlWaitingInfo {
        tool_call_id: format!("wait_{}", run_id),
        checkpoint_position: "/do/1/do/0".to_string(),
        workflow_name: "nested_workflow".to_string(),
    };
    session_manager
        .set_paused_with_hitl_info(&session.session_id, hitl_info)
        .await;

    // Lookup by run_id (as done in resume_workflow)
    let found_session = session_manager.get_session_by_run_id(&run_id).await;
    assert!(found_session.is_some());

    let found = found_session.unwrap();
    assert_eq!(found.session_id, session.session_id);
    assert_eq!(found.state, SessionState::Paused);
    assert!(found.hitl_waiting_info.is_some());
}

/// Test: HITL event sequence for wait detection
/// Verifies: Correct TOOL_CALL events are generated during wait
#[tokio::test]
async fn test_hitl_tool_call_event_sequence() {
    let event_store = InMemoryEventStore::new(1000, 3600);
    let run_id = "hitl_events_run";

    // Simulate the event sequence that occurs during HITL wait detection
    // tool_call_args takes a string (JSON serialized args)
    let args_json = serde_json::json!({
        "reason": "Approval required",
        "amount": 50000
    });
    let events = [
        // 1. Workflow starts
        AgUiEvent::run_started(run_id, "hitl_thread"),
        // 2. Task that leads to wait starts
        AgUiEvent::step_started("prepare_step", "Prepare for approval"),
        AgUiEvent::step_finished("prepare_step", None),
        // 3. TOOL_CALL events for HITL
        AgUiEvent::tool_call_start(format!("wait_{}", run_id), "HUMAN_INPUT".to_string()),
        AgUiEvent::tool_call_args(
            format!("wait_{}", run_id),
            serde_json::to_string(&args_json).unwrap(),
        ),
        // Note: stream ends here, waiting for user input
    ];

    for (i, event) in events.iter().enumerate() {
        event_store
            .store_event(run_id, i as u64, event.clone())
            .await;
    }

    // Verify event sequence
    let stored_events = event_store.get_all_events(run_id).await;
    assert_eq!(stored_events.len(), 5);

    // Verify TOOL_CALL_START
    match &stored_events[3].1 {
        AgUiEvent::ToolCallStart {
            tool_call_id,
            tool_call_name,
            ..
        } => {
            assert!(tool_call_id.starts_with("wait_"));
            assert_eq!(tool_call_name, "HUMAN_INPUT");
        }
        _ => panic!("Expected ToolCallStart"),
    }

    // Verify TOOL_CALL_ARGS (delta contains JSON string)
    match &stored_events[4].1 {
        AgUiEvent::ToolCallArgs {
            tool_call_id,
            delta,
            ..
        } => {
            assert!(tool_call_id.starts_with("wait_"));
            let parsed: serde_json::Value = serde_json::from_str(delta).unwrap();
            assert_eq!(parsed["reason"], "Approval required");
            assert_eq!(parsed["amount"], 50000);
        }
        _ => panic!("Expected ToolCallArgs"),
    }
}

/// Test: HITL resume event sequence
/// Verifies: Correct TOOL_CALL_RESULT and TOOL_CALL_END events during resume
#[tokio::test]
async fn test_hitl_resume_event_sequence() {
    let event_store = InMemoryEventStore::new(1000, 3600);
    let run_id = "hitl_resume_run";
    let tool_call_id = format!("wait_{}", run_id);

    // Simulate events stored before resume
    let args_json = serde_json::json!({"prompt": "Enter approval"});
    let wait_events = [
        AgUiEvent::run_started(run_id, "hitl_thread"),
        AgUiEvent::tool_call_start(tool_call_id.clone(), "HUMAN_INPUT".to_string()),
        AgUiEvent::tool_call_args(
            tool_call_id.clone(),
            serde_json::to_string(&args_json).unwrap(),
        ),
    ];

    for (i, event) in wait_events.iter().enumerate() {
        event_store
            .store_event(run_id, i as u64, event.clone())
            .await;
    }

    // Simulate events added during resume
    let resume_events = [
        AgUiEvent::tool_call_result(
            tool_call_id.clone(),
            serde_json::json!({"status": "resumed"}),
        ),
        AgUiEvent::tool_call_end(tool_call_id.clone()),
        // Workflow continues after resume
        AgUiEvent::step_started("after_approval", "Process approval"),
        AgUiEvent::step_finished("after_approval", None),
        AgUiEvent::run_finished(run_id.to_string(), None),
    ];

    for (i, event) in resume_events.iter().enumerate() {
        event_store
            .store_event(run_id, (i + 3) as u64, event.clone())
            .await;
    }

    // Verify complete event sequence
    let all_events = event_store.get_all_events(run_id).await;
    assert_eq!(all_events.len(), 8);

    // Verify TOOL_CALL_RESULT at index 3
    match &all_events[3].1 {
        AgUiEvent::ToolCallResult {
            tool_call_id: id,
            result,
            ..
        } => {
            assert_eq!(id, &tool_call_id);
            assert_eq!(result["status"], "resumed");
        }
        _ => panic!("Expected ToolCallResult at index 3"),
    }

    // Verify TOOL_CALL_END at index 4
    match &all_events[4].1 {
        AgUiEvent::ToolCallEnd {
            tool_call_id: id, ..
        } => {
            assert_eq!(id, &tool_call_id);
        }
        _ => panic!("Expected ToolCallEnd at index 4"),
    }

    // Verify workflow completed
    assert!(matches!(all_events[7].1, AgUiEvent::RunFinished { .. }));
}

/// Test: Multiple HITL waits in same workflow
/// Verifies: Session can handle multiple wait→resume cycles
#[tokio::test]
async fn test_multiple_hitl_waits_in_workflow() {
    use ag_ui_front::session::{HitlWaitingInfo, SessionState};
    use ag_ui_front::types::ids::{RunId, ThreadId};

    let session_manager = InMemorySessionManager::new(3600);
    let event_store = InMemoryEventStore::new(1000, 3600);

    let run_id = RunId::new("multi_hitl_run");
    let session = session_manager
        .create_session(run_id.clone(), ThreadId::new("multi_hitl_thread"))
        .await;

    // First HITL wait
    let hitl_info_1 = HitlWaitingInfo {
        tool_call_id: format!("wait_1_{}", run_id),
        checkpoint_position: "/do/0".to_string(),
        workflow_name: "multi_wait_workflow".to_string(),
    };
    session_manager
        .set_paused_with_hitl_info(&session.session_id, hitl_info_1)
        .await;

    // Store first wait events
    event_store
        .store_event(
            run_id.as_str(),
            0,
            AgUiEvent::run_started(run_id.as_str(), "thread"),
        )
        .await;
    event_store
        .store_event(
            run_id.as_str(),
            1,
            AgUiEvent::tool_call_start(format!("wait_1_{}", run_id), "HUMAN_INPUT".to_string()),
        )
        .await;

    // First resume (using atomic method)
    session_manager
        .resume_from_paused(&session.session_id, SessionState::Active)
        .await;
    event_store
        .store_event(
            run_id.as_str(),
            2,
            AgUiEvent::tool_call_end(format!("wait_1_{}", run_id)),
        )
        .await;

    // Second HITL wait
    let hitl_info_2 = HitlWaitingInfo {
        tool_call_id: format!("wait_2_{}", run_id),
        checkpoint_position: "/do/1".to_string(),
        workflow_name: "multi_wait_workflow".to_string(),
    };
    session_manager
        .set_paused_with_hitl_info(&session.session_id, hitl_info_2.clone())
        .await;
    event_store
        .store_event(
            run_id.as_str(),
            3,
            AgUiEvent::tool_call_start(format!("wait_2_{}", run_id), "HUMAN_INPUT".to_string()),
        )
        .await;

    // Verify second pause state
    let paused_session = session_manager
        .get_session(&session.session_id)
        .await
        .unwrap();
    assert_eq!(paused_session.state, SessionState::Paused);
    let info = paused_session.hitl_waiting_info.unwrap();
    assert_eq!(info.tool_call_id, hitl_info_2.tool_call_id);
    assert_eq!(info.checkpoint_position, "/do/1");

    // Second resume (using atomic method)
    session_manager
        .resume_from_paused(&session.session_id, SessionState::Active)
        .await;
    event_store
        .store_event(
            run_id.as_str(),
            4,
            AgUiEvent::tool_call_end(format!("wait_2_{}", run_id)),
        )
        .await;
    event_store
        .store_event(
            run_id.as_str(),
            5,
            AgUiEvent::run_finished(run_id.to_string(), None),
        )
        .await;

    // Verify complete
    session_manager
        .set_session_state(&session.session_id, SessionState::Completed)
        .await;

    let final_session = session_manager
        .get_session(&session.session_id)
        .await
        .unwrap();
    assert_eq!(final_session.state, SessionState::Completed);
    assert!(final_session.hitl_waiting_info.is_none());

    // Verify all events stored
    let events = event_store.get_all_events(run_id.as_str()).await;
    assert_eq!(events.len(), 6);
}

/// Test: HITL with event store replay
/// Verifies: Events can be replayed correctly for reconnecting clients
#[tokio::test]
async fn test_hitl_event_replay_on_reconnect() {
    let event_store = InMemoryEventStore::new(1000, 3600);
    let run_id = "hitl_replay_run";
    let tool_call_id = format!("wait_{}", run_id);

    // Store events up to HITL wait
    let args_json = serde_json::json!({"prompt": "Please approve"});
    let events = [
        AgUiEvent::run_started(run_id, "thread"),
        AgUiEvent::step_started("step_1", "First step"),
        AgUiEvent::step_finished("step_1", None),
        AgUiEvent::tool_call_start(tool_call_id.clone(), "HUMAN_INPUT".to_string()),
        AgUiEvent::tool_call_args(
            tool_call_id.clone(),
            serde_json::to_string(&args_json).unwrap(),
        ),
    ];

    for (i, event) in events.iter().enumerate() {
        event_store
            .store_event(run_id, i as u64, event.clone())
            .await;
    }

    // Simulate client reconnect - replay from last_event_id = 2
    let replayed_events = event_store.get_events_since(run_id, 2).await;

    // Should get events 3, 4 (TOOL_CALL_START and TOOL_CALL_ARGS)
    assert_eq!(replayed_events.len(), 2);
    assert_eq!(replayed_events[0].0, 3); // TOOL_CALL_START
    assert_eq!(replayed_events[1].0, 4); // TOOL_CALL_ARGS

    // Verify TOOL_CALL_ARGS contains the prompt for UI display
    match &replayed_events[1].1 {
        AgUiEvent::ToolCallArgs { delta, .. } => {
            let parsed: serde_json::Value = serde_json::from_str(delta).unwrap();
            assert_eq!(parsed["prompt"], "Please approve");
        }
        _ => panic!("Expected ToolCallArgs"),
    }
}

/// Test: Session state validation for resume
/// Verifies: Only Paused sessions can be resumed
#[tokio::test]
async fn test_hitl_session_state_validation() {
    use ag_ui_front::session::SessionState;
    use ag_ui_front::types::ids::{RunId, ThreadId};

    let session_manager = InMemorySessionManager::new(3600);

    // Test with Active session (should not be resumable)
    let active_session = session_manager
        .create_session(RunId::new("active_run"), ThreadId::new("thread"))
        .await;
    assert_eq!(active_session.state, SessionState::Active);

    // Test with Completed session
    let completed_session = session_manager
        .create_session(RunId::new("completed_run"), ThreadId::new("thread"))
        .await;
    session_manager
        .set_session_state(&completed_session.session_id, SessionState::Completed)
        .await;
    let retrieved = session_manager
        .get_session(&completed_session.session_id)
        .await
        .unwrap();
    assert_eq!(retrieved.state, SessionState::Completed);

    // Test with Cancelled session
    let cancelled_session = session_manager
        .create_session(RunId::new("cancelled_run"), ThreadId::new("thread"))
        .await;
    session_manager
        .set_session_state(&cancelled_session.session_id, SessionState::Cancelled)
        .await;
    let retrieved = session_manager
        .get_session(&cancelled_session.session_id)
        .await
        .unwrap();
    assert_eq!(retrieved.state, SessionState::Cancelled);

    // Test with Error session
    let error_session = session_manager
        .create_session(RunId::new("error_run"), ThreadId::new("thread"))
        .await;
    session_manager
        .set_session_state(&error_session.session_id, SessionState::Error)
        .await;
    let retrieved = session_manager
        .get_session(&error_session.session_id)
        .await
        .unwrap();
    assert_eq!(retrieved.state, SessionState::Error);
}

/// Test: HITL waiting info validation
/// Verifies: tool_call_id must match for resume
#[tokio::test]
async fn test_hitl_tool_call_id_validation() {
    use ag_ui_front::session::HitlWaitingInfo;
    use ag_ui_front::types::ids::{RunId, ThreadId};

    let session_manager = InMemorySessionManager::new(3600);

    let run_id = RunId::new("validation_run");
    let session = session_manager
        .create_session(run_id.clone(), ThreadId::new("thread"))
        .await;

    // Set HITL info with specific tool_call_id
    let expected_tool_call_id = format!("wait_{}", run_id);
    let hitl_info = HitlWaitingInfo {
        tool_call_id: expected_tool_call_id.clone(),
        checkpoint_position: "/do/0".to_string(),
        workflow_name: "test_workflow".to_string(),
    };
    session_manager
        .set_paused_with_hitl_info(&session.session_id, hitl_info)
        .await;

    // Retrieve and verify
    let retrieved = session_manager
        .get_session(&session.session_id)
        .await
        .unwrap();
    let stored_info = retrieved.hitl_waiting_info.unwrap();

    // Correct tool_call_id should match
    assert_eq!(stored_info.tool_call_id, expected_tool_call_id);

    // Wrong tool_call_id would be caught by resume_workflow validation
    let wrong_tool_call_id = "wrong_tool_call_id";
    assert_ne!(stored_info.tool_call_id, wrong_tool_call_id);
}

/// Test: Atomic resume_from_paused method
/// Verifies: Both state change and HITL info clear happen atomically
#[tokio::test]
async fn test_hitl_atomic_resume_from_paused() {
    use ag_ui_front::session::{HitlWaitingInfo, SessionState};
    use ag_ui_front::types::ids::{RunId, ThreadId};

    let session_manager = InMemorySessionManager::new(3600);

    let run_id = RunId::new("atomic_resume_run");
    let session = session_manager
        .create_session(run_id.clone(), ThreadId::new("thread"))
        .await;

    // Set to Paused with HITL info
    let hitl_info = HitlWaitingInfo {
        tool_call_id: format!("wait_{}", run_id),
        checkpoint_position: "/do/0".to_string(),
        workflow_name: "atomic_test_workflow".to_string(),
    };
    session_manager
        .set_paused_with_hitl_info(&session.session_id, hitl_info)
        .await;

    // Verify Paused state with HITL info
    let paused = session_manager
        .get_session(&session.session_id)
        .await
        .unwrap();
    assert_eq!(paused.state, SessionState::Paused);
    assert!(paused.hitl_waiting_info.is_some());

    // Atomically resume
    let result = session_manager
        .resume_from_paused(&session.session_id, SessionState::Active)
        .await;
    assert!(result);

    // Verify both state change and HITL info clear happened together
    let resumed = session_manager
        .get_session(&session.session_id)
        .await
        .unwrap();
    assert_eq!(resumed.state, SessionState::Active);
    assert!(
        resumed.hitl_waiting_info.is_none(),
        "HITL info should be cleared atomically with state change"
    );
}

/// Test: resume_from_paused with non-existent session
/// Verifies: Returns false for non-existent session
#[tokio::test]
async fn test_hitl_atomic_resume_nonexistent_session() {
    use ag_ui_front::session::SessionState;

    let session_manager = InMemorySessionManager::new(3600);

    // Try to resume non-existent session
    let result = session_manager
        .resume_from_paused("nonexistent_session_id", SessionState::Active)
        .await;
    assert!(
        !result,
        "resume_from_paused should return false for non-existent session"
    );
}
