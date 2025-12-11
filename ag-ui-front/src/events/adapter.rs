//! Event adapter for converting jobworkerp-rs workflow events to AG-UI events.

use crate::events::AgUiEvent;
use crate::types::{
    ids::{MessageId, RunId, StepId, ThreadId, ToolCallId},
    message::Role,
    state::WorkflowState,
};

/// Adapter for converting jobworkerp-rs workflow execution events to AG-UI events.
/// This adapter maintains state across multiple event conversions.
pub struct WorkflowEventAdapter {
    run_id: RunId,
    thread_id: ThreadId,
    current_message_id: Option<MessageId>,
    current_step_id: Option<StepId>,
    current_tool_call_id: Option<ToolCallId>,
}

impl WorkflowEventAdapter {
    /// Create a new adapter for a workflow execution
    pub fn new(run_id: RunId, thread_id: ThreadId) -> Self {
        Self {
            run_id,
            thread_id,
            current_message_id: None,
            current_step_id: None,
            current_tool_call_id: None,
        }
    }

    /// Get the run ID
    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    /// Get the thread ID
    pub fn thread_id(&self) -> &ThreadId {
        &self.thread_id
    }

    // === Lifecycle Events ===

    /// Create a RUN_STARTED event
    pub fn workflow_started(&self, workflow_name: &str, job_id: Option<i64>) -> AgUiEvent {
        let metadata = job_id.map(|id| {
            serde_json::json!({
                "workflowName": workflow_name,
                "jobId": id.to_string()
            })
        });

        AgUiEvent::RunStarted {
            run_id: self.run_id.to_string(),
            thread_id: self.thread_id.to_string(),
            timestamp: Some(AgUiEvent::now_timestamp()),
            metadata,
        }
    }

    /// Create a RUN_FINISHED event
    pub fn workflow_completed(&self, output: Option<serde_json::Value>) -> AgUiEvent {
        AgUiEvent::RunFinished {
            run_id: self.run_id.to_string(),
            timestamp: Some(AgUiEvent::now_timestamp()),
            result: output,
        }
    }

    /// Create a RUN_ERROR event
    pub fn workflow_error(
        &self,
        message: impl Into<String>,
        code: impl Into<String>,
        details: Option<serde_json::Value>,
    ) -> AgUiEvent {
        AgUiEvent::RunError {
            run_id: self.run_id.to_string(),
            message: message.into(),
            code: Some(code.into()),
            timestamp: Some(AgUiEvent::now_timestamp()),
            details,
        }
    }

    // === Step Events ===

    /// Create a STEP_STARTED event and track the current step
    pub fn task_started(
        &mut self,
        task_name: &str,
        task_type: Option<&str>,
        runner_type: Option<&str>,
    ) -> AgUiEvent {
        let step_id = StepId::random();
        self.current_step_id = Some(step_id.clone());

        let metadata = if task_type.is_some() || runner_type.is_some() {
            Some(serde_json::json!({
                "taskType": task_type,
                "runnerType": runner_type
            }))
        } else {
            None
        };

        AgUiEvent::StepStarted {
            step_id: step_id.to_string(),
            step_name: Some(task_name.to_string()),
            parent_step_id: None,
            timestamp: Some(AgUiEvent::now_timestamp()),
            metadata,
        }
    }

    /// Create a STEP_FINISHED event
    pub fn task_finished(&mut self, output: Option<serde_json::Value>) -> Option<AgUiEvent> {
        let step_id = self.current_step_id.take()?;

        Some(AgUiEvent::StepFinished {
            step_id: step_id.to_string(),
            timestamp: Some(AgUiEvent::now_timestamp()),
            result: output,
        })
    }

    // === LLM Message Events ===

    /// Create a TEXT_MESSAGE_START event and track the message
    pub fn llm_stream_start(&mut self) -> AgUiEvent {
        let message_id = MessageId::random();
        self.current_message_id = Some(message_id.clone());

        AgUiEvent::TextMessageStart {
            message_id: message_id.to_string(),
            role: Role::Assistant,
            timestamp: Some(AgUiEvent::now_timestamp()),
        }
    }

    /// Create a TEXT_MESSAGE_CONTENT event
    pub fn llm_stream_chunk(&self, content: &str) -> Option<AgUiEvent> {
        let message_id = self.current_message_id.as_ref()?;

        Some(AgUiEvent::TextMessageContent {
            message_id: message_id.to_string(),
            delta: content.to_string(),
            timestamp: Some(AgUiEvent::now_timestamp()),
        })
    }

    /// Create a TEXT_MESSAGE_END event
    pub fn llm_stream_end(&mut self) -> Option<AgUiEvent> {
        let message_id = self.current_message_id.take()?;

        Some(AgUiEvent::TextMessageEnd {
            message_id: message_id.to_string(),
            timestamp: Some(AgUiEvent::now_timestamp()),
        })
    }

    // === Tool Call Events ===

    /// Create a TOOL_CALL_START event
    pub fn job_started(&mut self, job_id: i64, runner_name: &str) -> AgUiEvent {
        let tool_call_id = ToolCallId::from_job_id(job_id);
        self.current_tool_call_id = Some(tool_call_id.clone());

        AgUiEvent::ToolCallStart {
            tool_call_id: tool_call_id.to_string(),
            tool_call_name: runner_name.to_string(),
            parent_message_id: self.current_message_id.as_ref().map(|id| id.to_string()),
            timestamp: Some(AgUiEvent::now_timestamp()),
        }
    }

    /// Create a TOOL_CALL_ARGS event
    pub fn job_args(&self, args_json: &str) -> Option<AgUiEvent> {
        let tool_call_id = self.current_tool_call_id.as_ref()?;

        Some(AgUiEvent::ToolCallArgs {
            tool_call_id: tool_call_id.to_string(),
            delta: args_json.to_string(),
            timestamp: Some(AgUiEvent::now_timestamp()),
        })
    }

    /// Create a TOOL_CALL_END event
    pub fn job_end(&self) -> Option<AgUiEvent> {
        let tool_call_id = self.current_tool_call_id.as_ref()?;

        Some(AgUiEvent::ToolCallEnd {
            tool_call_id: tool_call_id.to_string(),
            timestamp: Some(AgUiEvent::now_timestamp()),
        })
    }

    /// Create a TOOL_CALL_RESULT event
    pub fn job_completed(&mut self, result: serde_json::Value) -> Option<AgUiEvent> {
        let tool_call_id = self.current_tool_call_id.take()?;

        Some(AgUiEvent::ToolCallResult {
            tool_call_id: tool_call_id.to_string(),
            result,
            timestamp: Some(AgUiEvent::now_timestamp()),
        })
    }

    /// Create a TOOL_CALL_RESULT event with a specific job ID
    pub fn job_completed_with_id(&self, job_id: i64, result: serde_json::Value) -> AgUiEvent {
        let tool_call_id = ToolCallId::from_job_id(job_id);

        AgUiEvent::ToolCallResult {
            tool_call_id: tool_call_id.to_string(),
            result,
            timestamp: Some(AgUiEvent::now_timestamp()),
        }
    }

    // === State Events ===

    /// Create a STATE_SNAPSHOT event
    pub fn state_snapshot(&self, state: WorkflowState) -> AgUiEvent {
        AgUiEvent::StateSnapshot {
            snapshot: state,
            timestamp: Some(AgUiEvent::now_timestamp()),
        }
    }

    /// Create a STATE_DELTA event with JSON Patch operations
    pub fn state_delta(&self, delta: Vec<serde_json::Value>) -> AgUiEvent {
        AgUiEvent::StateDelta {
            delta,
            timestamp: Some(AgUiEvent::now_timestamp()),
        }
    }
}

/// Type alias for shared adapter wrapped in Arc<Mutex<>>
pub type SharedWorkflowEventAdapter = std::sync::Arc<tokio::sync::Mutex<WorkflowEventAdapter>>;

/// Create a shared adapter
pub fn shared_adapter(run_id: RunId, thread_id: ThreadId) -> SharedWorkflowEventAdapter {
    std::sync::Arc::new(tokio::sync::Mutex::new(WorkflowEventAdapter::new(
        run_id, thread_id,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::state::WorkflowStatus;

    #[test]
    fn test_workflow_started() {
        let adapter = WorkflowEventAdapter::new(RunId::new("run_1"), ThreadId::new("thread_1"));
        let event = adapter.workflow_started("my_workflow", Some(12345));

        match event {
            AgUiEvent::RunStarted {
                run_id,
                thread_id,
                metadata,
                ..
            } => {
                assert_eq!(run_id, "run_1");
                assert_eq!(thread_id, "thread_1");
                assert!(metadata.is_some());
                let meta = metadata.unwrap();
                assert_eq!(meta["workflowName"], "my_workflow");
            }
            _ => panic!("Expected RunStarted"),
        }
    }

    #[test]
    fn test_workflow_completed() {
        let adapter = WorkflowEventAdapter::new(RunId::new("run_1"), ThreadId::new("thread_1"));
        let output = serde_json::json!({ "result": "success" });
        let event = adapter.workflow_completed(Some(output.clone()));

        match event {
            AgUiEvent::RunFinished { run_id, result, .. } => {
                assert_eq!(run_id, "run_1");
                assert_eq!(result, Some(output));
            }
            _ => panic!("Expected RunFinished"),
        }
    }

    #[test]
    fn test_workflow_error() {
        let adapter = WorkflowEventAdapter::new(RunId::new("run_1"), ThreadId::new("thread_1"));
        let event = adapter.workflow_error("Task failed", "TASK_FAILED", None);

        match event {
            AgUiEvent::RunError {
                run_id,
                message,
                code,
                ..
            } => {
                assert_eq!(run_id, "run_1");
                assert_eq!(message, "Task failed");
                assert_eq!(code, Some("TASK_FAILED".to_string()));
            }
            _ => panic!("Expected RunError"),
        }
    }

    #[test]
    fn test_task_lifecycle() {
        let mut adapter = WorkflowEventAdapter::new(RunId::new("run_1"), ThreadId::new("thread_1"));

        // Start task
        let start_event = adapter.task_started("http_request", Some("run"), Some("HTTP_REQUEST"));
        match &start_event {
            AgUiEvent::StepStarted {
                step_name,
                metadata,
                ..
            } => {
                assert_eq!(step_name, &Some("http_request".to_string()));
                assert!(metadata.is_some());
            }
            _ => panic!("Expected StepStarted"),
        }

        // Finish task
        let finish_event = adapter.task_finished(Some(serde_json::json!({ "status": 200 })));
        assert!(finish_event.is_some());
        match finish_event.unwrap() {
            AgUiEvent::StepFinished { result, .. } => {
                assert!(result.is_some());
            }
            _ => panic!("Expected StepFinished"),
        }

        // Finishing again should return None
        assert!(adapter.task_finished(None).is_none());
    }

    #[test]
    fn test_llm_stream_lifecycle() {
        let mut adapter = WorkflowEventAdapter::new(RunId::new("run_1"), ThreadId::new("thread_1"));

        // Start message
        let start = adapter.llm_stream_start();
        match &start {
            AgUiEvent::TextMessageStart { role, .. } => {
                assert_eq!(*role, Role::Assistant);
            }
            _ => panic!("Expected TextMessageStart"),
        }

        // Stream chunks
        let chunk1 = adapter.llm_stream_chunk("Hello");
        assert!(chunk1.is_some());

        let chunk2 = adapter.llm_stream_chunk(", world!");
        assert!(chunk2.is_some());

        // End message
        let end = adapter.llm_stream_end();
        assert!(end.is_some());

        // Streaming after end should return None
        assert!(adapter.llm_stream_chunk("more").is_none());
    }

    #[test]
    fn test_job_lifecycle() {
        let mut adapter = WorkflowEventAdapter::new(RunId::new("run_1"), ThreadId::new("thread_1"));

        // Start job
        let start = adapter.job_started(12345, "http_request");
        match &start {
            AgUiEvent::ToolCallStart {
                tool_call_id,
                tool_call_name,
                ..
            } => {
                assert!(tool_call_id.starts_with("call_"));
                assert_eq!(tool_call_name, "http_request");
            }
            _ => panic!("Expected ToolCallStart"),
        }

        // Args
        let args = adapter.job_args(r#"{"url":"https://example.com"}"#);
        assert!(args.is_some());

        // End
        let end = adapter.job_end();
        assert!(end.is_some());

        // Result
        let result = adapter.job_completed(serde_json::json!({ "status": 200 }));
        assert!(result.is_some());
    }

    #[test]
    fn test_state_snapshot() {
        let adapter = WorkflowEventAdapter::new(RunId::new("run_1"), ThreadId::new("thread_1"));

        let state = WorkflowState::new("my_workflow").with_status(WorkflowStatus::Running);

        let event = adapter.state_snapshot(state);
        match event {
            AgUiEvent::StateSnapshot { snapshot, .. } => {
                assert_eq!(snapshot.workflow_name, "my_workflow");
                assert_eq!(snapshot.status, WorkflowStatus::Running);
            }
            _ => panic!("Expected StateSnapshot"),
        }
    }

    #[test]
    fn test_state_delta() {
        let adapter = WorkflowEventAdapter::new(RunId::new("run_1"), ThreadId::new("thread_1"));

        let delta =
            vec![serde_json::json!({ "op": "replace", "path": "/status", "value": "completed" })];

        let event = adapter.state_delta(delta.clone());
        match event {
            AgUiEvent::StateDelta {
                delta: event_delta, ..
            } => {
                assert_eq!(event_delta.len(), 1);
                assert_eq!(event_delta[0]["op"], "replace");
            }
            _ => panic!("Expected StateDelta"),
        }
    }

    #[test]
    fn test_job_completed_with_id() {
        let adapter = WorkflowEventAdapter::new(RunId::new("run_1"), ThreadId::new("thread_1"));

        let event = adapter.job_completed_with_id(99999, serde_json::json!({ "data": "result" }));
        match event {
            AgUiEvent::ToolCallResult {
                tool_call_id,
                result,
                ..
            } => {
                assert!(tool_call_id.contains("1869f")); // hex for 99999
                assert_eq!(result["data"], "result");
            }
            _ => panic!("Expected ToolCallResult"),
        }
    }
}
