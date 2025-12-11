//! State difference calculation for AG-UI STATE_DELTA events.
//!
//! This module provides utilities to calculate JSON Patch (RFC 6902) differences
//! between workflow states and generate STATE_DELTA events.

use crate::events::AgUiEvent;
use crate::types::state::WorkflowState;
use json_patch::{diff, Patch};

/// Calculate the difference between two workflow states as JSON Patch operations.
///
/// Returns a vector of JSON Patch operations (RFC 6902) representing the changes
/// from `old_state` to `new_state`. Returns an empty vector if states are identical.
///
/// # Arguments
/// * `old_state` - The previous workflow state
/// * `new_state` - The current workflow state
///
/// # Returns
/// A vector of JSON values representing patch operations
///
/// # Example
/// ```
/// use ag_ui_front::events::state_diff::calculate_state_diff;
/// use ag_ui_front::types::state::{WorkflowState, WorkflowStatus};
///
/// let old = WorkflowState::new("test").with_status(WorkflowStatus::Running);
/// let new = WorkflowState::new("test").with_status(WorkflowStatus::Completed);
///
/// let diff = calculate_state_diff(&old, &new);
/// assert!(!diff.is_empty());
/// ```
pub fn calculate_state_diff(
    old_state: &WorkflowState,
    new_state: &WorkflowState,
) -> Vec<serde_json::Value> {
    // Serialize states to JSON values
    let old_json = match serde_json::to_value(old_state) {
        Ok(v) => v,
        Err(_) => return vec![],
    };

    let new_json = match serde_json::to_value(new_state) {
        Ok(v) => v,
        Err(_) => return vec![],
    };

    // Calculate diff
    let patch: Patch = diff(&old_json, &new_json);

    // Convert patch operations to JSON values
    patch
        .0
        .into_iter()
        .map(|op| serde_json::to_value(op).unwrap_or(serde_json::Value::Null))
        .filter(|v| !v.is_null())
        .collect()
}

/// Create a STATE_DELTA event from the difference between two states.
///
/// Returns `Some(event)` if there are differences, `None` if states are identical.
///
/// # Arguments
/// * `old_state` - The previous workflow state
/// * `new_state` - The current workflow state
///
/// # Returns
/// An optional STATE_DELTA event
pub fn create_state_delta_event(
    old_state: &WorkflowState,
    new_state: &WorkflowState,
) -> Option<AgUiEvent> {
    let diff = calculate_state_diff(old_state, new_state);

    if diff.is_empty() {
        None
    } else {
        Some(AgUiEvent::StateDelta {
            delta: diff,
            timestamp: Some(AgUiEvent::now_timestamp()),
        })
    }
}

/// State tracker that maintains the previous state and generates delta events.
///
/// This struct can be used to track state changes over time and automatically
/// generate STATE_DELTA events when changes occur.
#[derive(Debug, Clone)]
pub struct StateTracker {
    current_state: Option<WorkflowState>,
    emit_snapshots: bool,
    emit_deltas: bool,
}

impl Default for StateTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl StateTracker {
    /// Create a new state tracker.
    pub fn new() -> Self {
        Self {
            current_state: None,
            emit_snapshots: true,
            emit_deltas: true,
        }
    }

    /// Configure whether to emit STATE_SNAPSHOT events.
    pub fn with_snapshots(mut self, emit: bool) -> Self {
        self.emit_snapshots = emit;
        self
    }

    /// Configure whether to emit STATE_DELTA events.
    pub fn with_deltas(mut self, emit: bool) -> Self {
        self.emit_deltas = emit;
        self
    }

    /// Update the state and generate appropriate events.
    ///
    /// - First call: emits STATE_SNAPSHOT
    /// - Subsequent calls: emits STATE_DELTA if there are changes
    ///
    /// # Returns
    /// An optional event (STATE_SNAPSHOT or STATE_DELTA)
    pub fn update(&mut self, new_state: WorkflowState) -> Option<AgUiEvent> {
        match &self.current_state {
            None => {
                // First state - emit snapshot
                let event = if self.emit_snapshots {
                    Some(AgUiEvent::StateSnapshot {
                        snapshot: new_state.clone(),
                        timestamp: Some(AgUiEvent::now_timestamp()),
                    })
                } else {
                    None
                };
                self.current_state = Some(new_state);
                event
            }
            Some(old_state) => {
                // Subsequent state - emit delta if changed
                let event = if self.emit_deltas {
                    create_state_delta_event(old_state, &new_state)
                } else {
                    None
                };
                self.current_state = Some(new_state);
                event
            }
        }
    }

    /// Force emit a STATE_SNAPSHOT for the current state.
    ///
    /// Useful for reconnection scenarios where the client needs
    /// the full state.
    pub fn emit_current_snapshot(&self) -> Option<AgUiEvent> {
        self.current_state
            .as_ref()
            .map(|state| AgUiEvent::StateSnapshot {
                snapshot: state.clone(),
                timestamp: Some(AgUiEvent::now_timestamp()),
            })
    }

    /// Get the current state.
    pub fn current_state(&self) -> Option<&WorkflowState> {
        self.current_state.as_ref()
    }

    /// Reset the tracker, clearing the current state.
    pub fn reset(&mut self) {
        self.current_state = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::state::WorkflowStatus;
    use std::collections::HashMap;

    #[test]
    fn test_calculate_state_diff_status_change() {
        let old = WorkflowState::new("test").with_status(WorkflowStatus::Running);
        let new = WorkflowState::new("test").with_status(WorkflowStatus::Completed);

        let diff = calculate_state_diff(&old, &new);

        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0]["op"], "replace");
        assert_eq!(diff[0]["path"], "/status");
        assert_eq!(diff[0]["value"], "completed");
    }

    #[test]
    fn test_calculate_state_diff_no_change() {
        let state = WorkflowState::new("test").with_status(WorkflowStatus::Running);

        let diff = calculate_state_diff(&state, &state);

        assert!(diff.is_empty());
    }

    #[test]
    fn test_calculate_state_diff_context_variables() {
        let old = WorkflowState::new("test").with_status(WorkflowStatus::Running);

        let mut vars = HashMap::new();
        vars.insert("key".to_string(), serde_json::json!("value"));
        let new = WorkflowState::new("test")
            .with_status(WorkflowStatus::Running)
            .with_context_variables(vars);

        let diff = calculate_state_diff(&old, &new);

        assert!(!diff.is_empty());
    }

    #[test]
    fn test_create_state_delta_event() {
        let old = WorkflowState::new("test").with_status(WorkflowStatus::Running);
        let new = WorkflowState::new("test").with_status(WorkflowStatus::Completed);

        let event = create_state_delta_event(&old, &new);

        assert!(event.is_some());
        match event.unwrap() {
            AgUiEvent::StateDelta { delta, .. } => {
                assert_eq!(delta.len(), 1);
            }
            _ => panic!("Expected StateDelta"),
        }
    }

    #[test]
    fn test_create_state_delta_event_no_change() {
        let state = WorkflowState::new("test");

        let event = create_state_delta_event(&state, &state);

        assert!(event.is_none());
    }

    #[test]
    fn test_state_tracker_first_update() {
        let mut tracker = StateTracker::new();
        let state = WorkflowState::new("test").with_status(WorkflowStatus::Running);

        let event = tracker.update(state);

        assert!(event.is_some());
        match event.unwrap() {
            AgUiEvent::StateSnapshot { snapshot, .. } => {
                assert_eq!(snapshot.workflow_name, "test");
            }
            _ => panic!("Expected StateSnapshot"),
        }
    }

    #[test]
    fn test_state_tracker_subsequent_update() {
        let mut tracker = StateTracker::new();

        // First update - snapshot
        let state1 = WorkflowState::new("test").with_status(WorkflowStatus::Running);
        let _ = tracker.update(state1);

        // Second update with change - delta
        let state2 = WorkflowState::new("test").with_status(WorkflowStatus::Completed);
        let event = tracker.update(state2);

        assert!(event.is_some());
        match event.unwrap() {
            AgUiEvent::StateDelta { delta, .. } => {
                assert!(!delta.is_empty());
            }
            _ => panic!("Expected StateDelta"),
        }
    }

    #[test]
    fn test_state_tracker_no_change() {
        let mut tracker = StateTracker::new();
        let state = WorkflowState::new("test").with_status(WorkflowStatus::Running);

        // First update
        let _ = tracker.update(state.clone());

        // Second update with same state
        let event = tracker.update(state);

        assert!(event.is_none());
    }

    #[test]
    fn test_state_tracker_emit_current_snapshot() {
        let mut tracker = StateTracker::new();
        let state = WorkflowState::new("test");

        tracker.update(state);

        let event = tracker.emit_current_snapshot();
        assert!(event.is_some());
    }

    #[test]
    fn test_state_tracker_config() {
        let mut tracker = StateTracker::new().with_snapshots(false).with_deltas(true);

        let state = WorkflowState::new("test");
        let event = tracker.update(state);

        // Snapshots disabled, so first update returns None
        assert!(event.is_none());
    }
}
