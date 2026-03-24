//! AG-UI HTTP handler implementation.
//!
//! AgUiHandler orchestrates workflow execution and AG-UI event streaming.

use crate::config::AgUiServerConfig;
use crate::error::{AgUiError, Result};
use crate::events::{
    AgUiEvent, EventEncoder, ExtractedToolResult, ExtractedToolStarted, InterruptInfo,
    InterruptPayload, PendingToolCall, SharedWorkflowEventAdapter,
    extract_text_from_completed_output, extract_text_from_llm_chat_result,
    extract_tool_calls_from_llm_result, extract_tool_execution_results,
    extract_tool_execution_started, shared_adapter, tool_calls_to_ag_ui_events,
};
use crate::session::{
    EventStore, HitlWaitingInfo, PendingToolCallInfo, Session, SessionManager, SessionState,
};
use crate::types::ids::{MessageId, StepId};
use crate::types::input::ResumePayload;
use crate::types::{RunAgentInput, RunId, ThreadId, WorkflowState, WorkflowStatus};
use app::module::AppModule;
use app_wrapper::modules::AppWrapperModule;
use app_wrapper::workflow::definition::workflow::WorkflowSchema;
use app_wrapper::workflow::execute::checkpoint::CheckPointContext;
use app_wrapper::workflow::execute::context::WorkflowStreamEvent;
use app_wrapper::workflow::execute::workflow::WorkflowExecutor;
use async_stream::stream;
use futures::{Stream, StreamExt};
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::RwLock;

/// Fallback counter for event IDs when encoding fails.
/// Starts from a high value (MSB set) to distinguish from normal event IDs.
/// This ensures unique, non-zero fallback IDs even when multiple encoding failures occur.
static FALLBACK_EVENT_ID_COUNTER: AtomicU64 = AtomicU64::new(0x8000_0000_0000_0000);

/// Timeout for nested workflow progress subscriptions (milliseconds).
// TODO: Make configurable or derive from job timeout
const NESTED_WF_PROGRESS_TIMEOUT_MS: u64 = 300_000; // 5 min

/// Timeout for draining nested events and joining spawned tasks after the main loop ends.
const NESTED_DRAIN_TIMEOUT_SECS: u64 = 5;

/// Maximum number of concurrent nested workflow progress subscriptions.
const MAX_NESTED_SUBSCRIPTIONS: usize = 32;

/// Channel buffer size for nested workflow progress events.
const NESTED_EVENT_CHANNEL_SIZE: usize = 64;

/// Registry of active workflow executors for cancellation support.
type ExecutorRegistry = Arc<RwLock<HashMap<String, Arc<WorkflowExecutor>>>>;

/// Handler for AG-UI HTTP requests.
///
/// Manages workflow execution, session tracking, and event streaming.
pub struct AgUiHandler<SM, ES>
where
    SM: SessionManager + 'static,
    ES: EventStore + 'static,
{
    app_wrapper_module: Arc<AppWrapperModule>,
    app_module: Arc<AppModule>,
    session_manager: Arc<SM>,
    event_store: Arc<ES>,
    config: AgUiServerConfig,
    encoder: Arc<EventEncoder>,
    /// Registry of active workflow executors for cancellation support.
    executor_registry: ExecutorRegistry,
}

impl<SM, ES> AgUiHandler<SM, ES>
where
    SM: SessionManager + 'static,
    ES: EventStore + 'static,
{
    /// Create a new AgUiHandler.
    pub fn new(
        app_wrapper_module: Arc<AppWrapperModule>,
        app_module: Arc<AppModule>,
        session_manager: Arc<SM>,
        event_store: Arc<ES>,
        config: AgUiServerConfig,
    ) -> Self {
        Self {
            app_wrapper_module,
            app_module,
            session_manager,
            event_store,
            config,
            encoder: Arc::new(EventEncoder::new()),
            executor_registry: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get the event encoder.
    pub fn encoder(&self) -> &EventEncoder {
        &self.encoder
    }

    /// Get the session manager.
    pub fn session_manager(&self) -> &Arc<SM> {
        &self.session_manager
    }

    /// Get the event store.
    pub fn event_store(&self) -> &Arc<ES> {
        &self.event_store
    }

    /// Start a new workflow run.
    ///
    /// Creates a session, initializes the workflow executor, and returns an event stream.
    /// If `run_id` is provided in input, it will be used; otherwise a new one is generated.
    ///
    /// Returns a stream of (event_id, AgUiEvent) tuples for consistent SSE ID handling.
    pub async fn run_workflow(
        &self,
        input: RunAgentInput,
    ) -> Result<(
        Session,
        Pin<Box<dyn Stream<Item = (u64, AgUiEvent)> + Send>>,
    )> {
        // Check if this is a resume request (AG-UI Interrupts protocol)
        if let Some(resume) = input.resume.as_ref() {
            return self
                .handle_resume_request(
                    input.clone(),
                    resume.interrupt_id.clone(),
                    resume.payload.clone(),
                )
                .await;
        }

        // Use provided run_id or generate a new one
        let run_id = input
            .run_id
            .as_ref()
            .map(|id| RunId::new(id.clone()))
            .unwrap_or_else(RunId::random);
        let thread_id = input
            .thread_id
            .clone()
            .map(ThreadId::new)
            .unwrap_or_else(ThreadId::random);

        // Create session
        let session = self
            .session_manager
            .create_session(run_id.clone(), thread_id.clone())
            .await;

        // Parse workflow from context (including optional workflow_context)
        let (workflow, workflow_context) = self.parse_workflow_from_input(&input).await?;

        // Extract workflow name for HITL checkpoint tracking
        let workflow_name = workflow.document.name.to_string();

        // Create adapter for event conversion
        let adapter = shared_adapter(run_id.clone(), thread_id.clone());

        // Initialize workflow executor with run_id for checkpoint tracking
        let executor = self
            .init_workflow_executor(workflow, input, &run_id, adapter.clone(), workflow_context)
            .await?;

        // Register executor for cancellation support
        let executor = Arc::new(executor);
        {
            let mut registry = self.executor_registry.write().await;
            registry.insert(run_id.to_string(), executor.clone());
        }

        // Create event stream with cleanup on completion
        let executor_registry = self.executor_registry.clone();
        let run_id_for_cleanup = run_id.to_string();
        let stream = self.create_event_stream(
            executor,
            adapter,
            run_id.to_string(),
            session.session_id.clone(),
            executor_registry,
            run_id_for_cleanup,
            workflow_name,
            true, // emit_run_started: true for new workflow runs
        );

        Ok((session, Box::pin(stream)))
    }

    /// Resume an existing workflow run from Last-Event-ID.
    ///
    /// Returns stored events since the given ID, then continues with live events if the
    /// workflow is still running. This ensures reconnecting clients receive both historical
    /// events and subsequent live updates.
    ///
    /// Returns a stream of (event_id, AgUiEvent) tuples for consistent SSE ID handling.
    pub async fn resume_stream(
        &self,
        run_id: &str,
        last_event_id: Option<u64>,
    ) -> Result<Pin<Box<dyn Stream<Item = (u64, AgUiEvent)> + Send>>> {
        // Find session by run_id
        let run_id_typed = RunId::new(run_id);
        let session = self
            .session_manager
            .get_session_by_run_id(&run_id_typed)
            .await
            .ok_or_else(|| AgUiError::SessionNotFound(run_id.to_string()))?;

        // Get stored events since last_event_id
        let stored_events = if let Some(last_id) = last_event_id {
            self.event_store.get_events_since(run_id, last_id).await
        } else {
            self.event_store.get_all_events(run_id).await
        };

        // Check if workflow is still running (executor exists in registry)
        let is_running = {
            let registry = self.executor_registry.read().await;
            registry.contains_key(run_id)
        };

        // Update session last_event_id
        if let Some(last_id) = last_event_id {
            self.session_manager
                .update_last_event_id(&session.session_id, last_id)
                .await;
        }

        // Create combined stream: stored events first, then live events if running
        let run_id_for_poll = run_id.to_string();
        let event_store = self.event_store.clone();
        let session_state = session.state;

        let stream = stream! {
            // First, yield all stored events with their IDs
            let mut last_yielded_id: Option<u64> = last_event_id;
            for (id, event) in stored_events {
                last_yielded_id = Some(id);
                yield (id, event);
            }

            // If workflow is still running, poll for new events
            if is_running && session_state != SessionState::Completed && session_state != SessionState::Cancelled && session_state != SessionState::Error {
                // Poll for new events until workflow completes
                loop {
                    // Small delay to avoid busy-waiting
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

                    // Get new events since last yielded
                    let new_events = if let Some(last_id) = last_yielded_id {
                        event_store.get_events_since(&run_id_for_poll, last_id).await
                    } else {
                        event_store.get_all_events(&run_id_for_poll).await
                    };

                    if new_events.is_empty() {
                        // Check if workflow completed (by looking for RUN_FINISHED or RUN_ERROR)
                        let all_events = event_store.get_all_events(&run_id_for_poll).await;
                        let workflow_completed = all_events.iter().any(|(_, event)| {
                            matches!(event, AgUiEvent::RunFinished { .. } | AgUiEvent::RunError { .. })
                        });

                        if workflow_completed {
                            break;
                        }
                        continue;
                    }

                    // Yield new events
                    for (id, event) in new_events {
                        last_yielded_id = Some(id);
                        let is_terminal = matches!(event, AgUiEvent::RunFinished { .. } | AgUiEvent::RunError { .. });
                        yield (id, event);

                        if is_terminal {
                            return;
                        }
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }

    /// Cancel a running workflow.
    ///
    /// This method cancels the workflow execution if it's still running.
    /// It updates the session state and triggers the WorkflowExecutor's cancel method.
    pub async fn cancel_workflow(&self, run_id: &str) -> Result<()> {
        let run_id_typed = RunId::new(run_id);

        // Find session
        let session = self
            .session_manager
            .get_session_by_run_id(&run_id_typed)
            .await
            .ok_or_else(|| AgUiError::SessionNotFound(run_id.to_string()))?;

        // Cancel the workflow executor if it exists
        // Clone the Arc outside the lock to avoid holding read lock during cancel().await
        let executor_opt = {
            let registry = self.executor_registry.read().await;
            registry.get(run_id).cloned()
        };

        if let Some(executor) = executor_opt {
            tracing::info!("Cancelling workflow executor for run_id: {}", run_id);
            executor.cancel().await;
        } else {
            tracing::warn!(
                "No active executor found for run_id: {}, session may have already completed",
                run_id
            );
        }

        // Update session state to cancelled
        self.session_manager
            .set_session_state(&session.session_id, SessionState::Cancelled)
            .await;

        // Store RUN_ERROR event for cancelled workflow
        let event = AgUiEvent::RunError {
            run_id: run_id.to_string(),
            message: "Workflow cancelled by user".to_string(),
            code: Some("CANCELLED".to_string()),
            details: None,
            timestamp: Some(AgUiEvent::now_timestamp()),
        };
        let event_id = Self::encode_event_with_logging(&self.encoder, &event);
        self.event_store.store_event(run_id, event_id, event).await;

        // Remove executor from registry
        {
            let mut registry = self.executor_registry.write().await;
            registry.remove(run_id);
        }

        Ok(())
    }

    /// Get current workflow state.
    pub async fn get_state(&self, run_id: &str) -> Result<WorkflowState> {
        let run_id_typed = RunId::new(run_id);

        // Find session
        let _session = self
            .session_manager
            .get_session_by_run_id(&run_id_typed)
            .await
            .ok_or_else(|| AgUiError::SessionNotFound(run_id.to_string()))?;

        // Get latest state from event store
        let events = self.event_store.get_all_events(run_id).await;

        // Find the latest state snapshot or construct from events
        for (_id, event) in events.into_iter().rev() {
            if let AgUiEvent::StateSnapshot { snapshot, .. } = event {
                return Ok(snapshot);
            }
        }

        // Return empty state if no snapshot found
        Ok(WorkflowState::new("unknown").with_status(WorkflowStatus::Pending))
    }

    /// Handle tool call results for LLM HITL.
    ///
    /// This method:
    /// 1. Validates session is in Paused state
    /// 2. Validates tool_call_ids match pending tool calls
    /// 3. Emits TOOL_CALL_RESULT and TOOL_CALL_END events
    /// 4. Updates session state to Completed
    ///
    /// After this, the client should send a new /ag-ui/run request
    /// with the tool results included in the message history.
    pub async fn handle_tool_call_results(
        &self,
        run_id: &str,
        tool_results: Vec<(String, serde_json::Value)>, // (tool_call_id, result)
    ) -> Result<Pin<Box<dyn Stream<Item = (u64, AgUiEvent)> + Send>>> {
        let run_id_typed = RunId::new(run_id);

        // 1. Find session and validate state
        let session = self
            .session_manager
            .get_session_by_run_id(&run_id_typed)
            .await
            .ok_or_else(|| AgUiError::SessionNotFound(run_id.to_string()))?;

        if session.state != SessionState::Paused {
            return Err(AgUiError::SessionNotPaused {
                current_state: format!("{:?}", session.state),
            });
        }

        // 2. Get HITL waiting info and validate tool_call_ids
        let hitl_info =
            session
                .hitl_waiting_info
                .as_ref()
                .ok_or_else(|| AgUiError::HitlInfoNotFound {
                    session_id: session.session_id.clone(),
                })?;

        // Validate all tool_call_ids
        for (tool_call_id, _) in &tool_results {
            let is_valid = hitl_info
                .pending_tool_calls
                .iter()
                .any(|tc| &tc.call_id == tool_call_id);
            if !is_valid {
                let expected = hitl_info
                    .pending_tool_calls
                    .iter()
                    .map(|tc| tc.call_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(AgUiError::InvalidToolCallId {
                    expected,
                    actual: tool_call_id.clone(),
                });
            }
        }

        // 3. Emit TOOL_CALL_END and TOOL_CALL_RESULT events for each result
        // Order: END → RESULT (AG-UI protocol compliant)
        let mut events = Vec::new();
        for (tool_call_id, result) in &tool_results {
            let end_event = AgUiEvent::tool_call_end(tool_call_id.clone());
            let end_event_id = Self::encode_event_with_logging(&self.encoder, &end_event);
            self.event_store
                .store_event(run_id, end_event_id, end_event.clone())
                .await;
            events.push((end_event_id, end_event));

            let result_event = AgUiEvent::tool_call_result(tool_call_id.clone(), result.clone());
            let result_event_id = Self::encode_event_with_logging(&self.encoder, &result_event);
            self.event_store
                .store_event(run_id, result_event_id, result_event.clone())
                .await;
            events.push((result_event_id, result_event));
        }

        // 4. Emit RUN_FINISHED to signal client can proceed
        let adapter = shared_adapter(run_id_typed.clone(), session.thread_id.clone());
        let adapter_lock = adapter.lock().await;
        let run_finished_event = adapter_lock.workflow_completed(None);
        drop(adapter_lock);

        let run_finished_id = Self::encode_event_with_logging(&self.encoder, &run_finished_event);
        self.event_store
            .store_event(run_id, run_finished_id, run_finished_event.clone())
            .await;
        events.push((run_finished_id, run_finished_event));

        // 5. Update session state to Completed
        self.session_manager
            .set_session_state(&session.session_id, SessionState::Completed)
            .await;

        tracing::info!(
            run_id = %run_id,
            tool_count = tool_results.len(),
            "Tool call results processed, session completed"
        );

        Ok(Box::pin(futures::stream::iter(events)))
    }

    /// Resume a paused HITL workflow with user input (checkpoint-based).
    ///
    /// This method:
    /// 1. Validates session is in Paused state
    /// 2. Validates tool_call_id matches
    /// 3. Retrieves checkpoint from storage
    /// 4. Injects user_input into checkpoint
    /// 5. Restarts workflow execution from checkpoint
    /// 6. Returns event stream for resumed execution
    ///
    /// Note: For LLM tool call HITL, use handle_tool_call_results instead.
    pub async fn resume_workflow(
        &self,
        run_id: &str,
        tool_call_id: &str,
        user_input: serde_json::Value,
    ) -> Result<Pin<Box<dyn Stream<Item = (u64, AgUiEvent)> + Send>>> {
        let run_id_typed = RunId::new(run_id);

        // 1. Find session and validate state
        let session = self
            .session_manager
            .get_session_by_run_id(&run_id_typed)
            .await
            .ok_or_else(|| AgUiError::SessionNotFound(run_id.to_string()))?;

        if session.state != SessionState::Paused {
            return Err(AgUiError::SessionNotPaused {
                current_state: format!("{:?}", session.state),
            });
        }

        // 2. Get HITL waiting info and validate tool_call_id
        let hitl_info =
            session
                .hitl_waiting_info
                .as_ref()
                .ok_or_else(|| AgUiError::HitlInfoNotFound {
                    session_id: session.session_id.clone(),
                })?;

        // Validate tool_call_id
        // For LLM tool calls (pending_tool_calls non-empty), check if tool_call_id matches any pending call
        // For traditional HITL (pending_tool_calls empty), check if tool_call_id matches the primary ID
        let is_valid_tool_call_id = if !hitl_info.pending_tool_calls.is_empty() {
            hitl_info
                .pending_tool_calls
                .iter()
                .any(|tc| tc.call_id == tool_call_id)
        } else {
            hitl_info.tool_call_id == tool_call_id
        };

        if !is_valid_tool_call_id {
            let expected = if !hitl_info.pending_tool_calls.is_empty() {
                hitl_info
                    .pending_tool_calls
                    .iter()
                    .map(|tc| tc.call_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            } else {
                hitl_info.tool_call_id.clone()
            };
            return Err(AgUiError::InvalidToolCallId {
                expected,
                actual: tool_call_id.to_string(),
            });
        }

        // 3. Get checkpoint from repository
        let execution_id = run_id_typed
            .to_execution_id()
            .ok_or_else(|| AgUiError::InvalidInput("Invalid run_id for execution".to_string()))?;
        let checkpoint_repo = self
            .app_wrapper_module
            .repositories
            .redis_checkpoint_repository
            .as_ref()
            .map(|r| r.as_ref())
            .unwrap_or(
                self.app_wrapper_module
                    .repositories
                    .memory_checkpoint_repository
                    .as_ref(),
            );
        let checkpoint = checkpoint_repo
            .get_checkpoint_with_id(
                &execution_id,
                &hitl_info.workflow_name,
                &hitl_info.checkpoint_position,
            )
            .await
            .map_err(AgUiError::Internal)?
            .ok_or_else(|| AgUiError::CheckpointNotFound {
                workflow_name: hitl_info.workflow_name.clone(),
                position: hitl_info.checkpoint_position.clone(),
            })?;

        // 4. Modify checkpoint with user_input
        // Set user_input as task.output (becomes next task's input) and in context_variables
        let user_input_for_event = user_input.clone();
        let mut ctx_vars = (*checkpoint.workflow.context_variables).clone();
        ctx_vars.insert("user_input".to_string(), user_input.clone());

        let modified_checkpoint = CheckPointContext {
            task: app_wrapper::workflow::execute::checkpoint::TaskCheckPointContext {
                output: Arc::new(user_input.clone()),
                ..checkpoint.task.clone()
            },
            workflow: app_wrapper::workflow::execute::checkpoint::WorkflowCheckPointContext {
                context_variables: Arc::new(ctx_vars),
                ..checkpoint.workflow.clone()
            },
            ..checkpoint.clone()
        };

        // 5. Get workflow schema for re-execution
        let workflow = self
            .app_module
            .worker_app
            .find_by_name(&hitl_info.workflow_name)
            .await
            .map_err(|e| {
                AgUiError::Internal(anyhow::anyhow!(
                    "Failed to find workflow '{}': {}",
                    hitl_info.workflow_name,
                    e
                ))
            })?
            .ok_or_else(|| AgUiError::WorkflowDefinitionNotFound {
                name: hitl_info.workflow_name.clone(),
            })?;

        let worker_data = workflow.data.ok_or_else(|| {
            AgUiError::InvalidInput(format!("Worker '{}' has no data", hitl_info.workflow_name))
        })?;

        let schema: WorkflowSchema =
            serde_json::from_slice(&worker_data.runner_settings).map_err(|e| {
                AgUiError::InvalidInput(format!(
                    "Invalid workflow schema in worker '{}': {}",
                    hitl_info.workflow_name, e
                ))
            })?;

        // 6. Create adapter for event conversion
        let adapter = shared_adapter(run_id_typed.clone(), session.thread_id.clone());

        // 7. Initialize executor from checkpoint (before updating session state)
        let executor = WorkflowExecutor::init(
            self.app_wrapper_module.clone(),
            self.app_module.clone(),
            Arc::new(schema),
            modified_checkpoint.workflow.input.clone(),
            Some(execution_id.clone()),
            Arc::new(serde_json::json!({})),
            Arc::new(HashMap::new()),
            Some(modified_checkpoint),
        )
        .await
        .map_err(|e| AgUiError::WorkflowInitFailed(format!("{:?}", e)))?;

        // 8. Atomically clear HITL info and set session state to Active
        // This is done after executor init succeeds to avoid leaving session in
        // an inconsistent state if initialization fails.
        if !self
            .session_manager
            .resume_from_paused(&session.session_id, SessionState::Active)
            .await
        {
            return Err(AgUiError::Internal(anyhow::anyhow!(
                "Failed to update session state from Paused to Active for session {}",
                session.session_id
            )));
        }

        // 9. Emit TOOL_CALL_END and TOOL_CALL_RESULT events
        // For LLM tool calls (pending_tool_calls non-empty), emit events for the specific tool
        // For traditional HITL (pending_tool_calls empty), emit events for the primary tool_call_id
        let mut tool_events_vec = Vec::new();

        if !hitl_info.pending_tool_calls.is_empty() {
            // LLM tool call mode: emit TOOL_CALL_END and TOOL_CALL_RESULT for the matched tool
            // Order: END → RESULT (AG-UI protocol compliant)
            let tool_call_end_event = AgUiEvent::tool_call_end(tool_call_id.to_string());
            let end_event_id = Self::encode_event_with_logging(&self.encoder, &tool_call_end_event);
            self.event_store
                .store_event(run_id, end_event_id, tool_call_end_event.clone())
                .await;
            tool_events_vec.push((end_event_id, tool_call_end_event));

            let tool_call_result_event =
                AgUiEvent::tool_call_result(tool_call_id.to_string(), user_input_for_event);
            let result_event_id =
                Self::encode_event_with_logging(&self.encoder, &tool_call_result_event);
            self.event_store
                .store_event(run_id, result_event_id, tool_call_result_event.clone())
                .await;
            tool_events_vec.push((result_event_id, tool_call_result_event));

            tracing::info!(
                tool_call_id = %tool_call_id,
                pending_count = hitl_info.pending_tool_calls.len(),
                "LLM tool call result received"
            );
        } else {
            // Traditional HITL mode: emit TOOL_CALL_END then status-based TOOL_CALL_RESULT
            // Order: END → RESULT (AG-UI protocol compliant)
            let tool_call_end_event = AgUiEvent::tool_call_end(tool_call_id.to_string());
            let end_event_id = Self::encode_event_with_logging(&self.encoder, &tool_call_end_event);
            self.event_store
                .store_event(run_id, end_event_id, tool_call_end_event.clone())
                .await;
            tool_events_vec.push((end_event_id, tool_call_end_event));

            let tool_call_result_event = AgUiEvent::tool_call_result(
                tool_call_id.to_string(),
                serde_json::json!({"status": "resumed"}),
            );
            let result_event_id =
                Self::encode_event_with_logging(&self.encoder, &tool_call_result_event);
            self.event_store
                .store_event(run_id, result_event_id, tool_call_result_event.clone())
                .await;
            tool_events_vec.push((result_event_id, tool_call_result_event));
        }

        // 10. Register executor for cancellation support
        let executor = Arc::new(executor);
        {
            let mut registry = self.executor_registry.write().await;
            registry.insert(run_id.to_string(), executor.clone());
        }

        // 11. Create event stream (prepend TOOL_CALL events, then workflow events)
        let executor_registry = self.executor_registry.clone();
        let run_id_for_stream = run_id.to_string();
        let run_id_for_cleanup = run_id.to_string();
        let workflow_name_for_stream = hitl_info.workflow_name.clone();

        let workflow_stream = self.create_event_stream(
            executor,
            adapter,
            run_id_for_stream,
            session.session_id.clone(),
            executor_registry,
            run_id_for_cleanup,
            workflow_name_for_stream,
            true, // emit_run_started: resume is a new run invocation per AG-UI spec
        );

        // Prepend the tool_call events to the workflow stream
        let tool_events = futures::stream::iter(tool_events_vec);

        let combined_stream = tool_events.chain(workflow_stream);

        Ok(Box::pin(combined_stream))
    }

    /// Handle AG-UI Interrupts resume request.
    ///
    /// This method handles the resume flow for AG-UI Interrupts protocol:
    /// 1. Look up session by interrupt_id
    /// 2. Validate session state is Paused
    /// 3. Handle approve/reject based on ResumePayload
    /// 4. For approve: execute tool calls and continue workflow
    async fn handle_resume_request(
        &self,
        input: RunAgentInput,
        interrupt_id: String,
        payload: ResumePayload,
    ) -> Result<(
        Session,
        Pin<Box<dyn Stream<Item = (u64, AgUiEvent)> + Send>>,
    )> {
        // 1. Look up session by interrupt_id
        let session = self
            .session_manager
            .get_session_by_interrupt_id(&interrupt_id)
            .await
            .ok_or_else(|| AgUiError::InvalidInterruptId {
                interrupt_id: interrupt_id.clone(),
                message: "No session found with this interrupt ID".to_string(),
            })?;

        // 2. Verify session is in Paused state
        if session.state != SessionState::Paused {
            return Err(AgUiError::InvalidInterruptId {
                interrupt_id: interrupt_id.clone(),
                message: format!("Session state is {:?}, expected Paused", session.state),
            });
        }

        // 3. Get HITL info (interrupt_id match is already validated by get_session_by_interrupt_id)
        let hitl_info =
            session
                .hitl_waiting_info
                .clone()
                .ok_or_else(|| AgUiError::InvalidInterruptId {
                    interrupt_id: interrupt_id.clone(),
                    message: "No HITL info found in session".to_string(),
                })?;

        // 4. Handle based on payload type
        match payload {
            ResumePayload::Approve { tool_results } => {
                // Check if this is LLM tool call HITL (pending_tool_calls non-empty)
                if !hitl_info.pending_tool_calls.is_empty() {
                    // LLM tool call HITL: Re-execute workflow with ToolExecutionRequests
                    // This triggers OllamaChatService to execute tools and continue LLM conversation
                    self.handle_llm_tool_approval(input, session, hitl_info, tool_results)
                        .await
                } else {
                    // Traditional HITL (checkpoint-based): Use resume_workflow
                    // Extract text from all messages
                    let tool_result: serde_json::Value = if input.messages.is_empty() {
                        serde_json::json!("")
                    } else {
                        let combined_text: String = input
                            .messages
                            .iter()
                            .map(|m| m.content.extract_text())
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>()
                            .join("\n");
                        serde_json::json!(combined_text)
                    };

                    self.resume_workflow(
                        session.run_id.as_str(),
                        &hitl_info.tool_call_id,
                        tool_result,
                    )
                    .await
                    .map(|stream| (session, stream))
                }
            }
            ResumePayload::Reject { reason } => {
                // Check if this is LLM tool call HITL (pending_tool_calls non-empty)
                if !hitl_info.pending_tool_calls.is_empty() {
                    // LLM tool call HITL: Feed rejection back to LLM so it can respond accordingly
                    self.handle_llm_tool_rejection(input, session, hitl_info, reason)
                        .await
                } else {
                    // Traditional HITL (checkpoint-based): End the run
                    let run_id = session.run_id.to_string();
                    let session_id = session.session_id.clone();
                    let encoder = self.encoder.clone();
                    let event_store = self.event_store.clone();
                    let session_manager = self.session_manager.clone();

                    let rejection_message =
                        reason.unwrap_or_else(|| "User rejected tool calls".to_string());

                    let stream = stream! {
                        if session_manager
                            .resume_from_paused(&session_id, SessionState::Completed)
                            .await
                        {
                            let run_finished = AgUiEvent::run_finished(
                                run_id.clone(),
                                Some(serde_json::json!({
                                    "status": "rejected",
                                    "message": rejection_message
                                })),
                            );
                            let event_id = Self::encode_event_with_logging(&encoder, &run_finished);
                            event_store.store_event(&run_id, event_id, run_finished.clone()).await;
                            yield (event_id, run_finished);
                        } else {
                            tracing::warn!(
                                session_id = %session_id,
                                "Failed to update session state to Completed"
                            );
                            let error_event = AgUiEvent::run_error(
                                run_id.clone(),
                                format!("Failed to complete session {}", session_id),
                                None,
                                None,
                            );
                            let event_id = Self::encode_event_with_logging(&encoder, &error_event);
                            event_store.store_event(&run_id, event_id, error_event.clone()).await;
                            yield (event_id, error_event);
                        }
                    };

                    Ok((session, Box::pin(stream)))
                }
            }
        }
    }

    /// Handle LLM tool call approval by re-executing workflow with ToolExecutionRequests.
    ///
    /// This method:
    /// 1. Builds ToolExecutionRequests from pending_tool_calls
    /// 2. Updates workflowContext with toolExecutionRequests
    /// 3. Starts a new workflow execution
    /// 4. OllamaChatService will execute tools and continue LLM conversation
    async fn handle_llm_tool_approval(
        &self,
        mut input: RunAgentInput,
        session: Session,
        hitl_info: HitlWaitingInfo,
        tool_results: Option<Vec<crate::types::input::ToolCallResult>>,
    ) -> Result<(
        Session,
        Pin<Box<dyn Stream<Item = (u64, AgUiEvent)> + Send>>,
    )> {
        tracing::info!(
            run_id = %session.run_id,
            interrupt_id = %hitl_info.interrupt_id,
            pending_tool_calls = hitl_info.pending_tool_calls.len(),
            "Handling LLM tool approval"
        );

        // Pre-resolve fn_arguments with client overrides
        let resolved_fn_args: HashMap<&str, String> = hitl_info
            .pending_tool_calls
            .iter()
            .map(|tc| {
                let fn_arguments = tool_results
                    .as_ref()
                    .and_then(|results| results.iter().find(|r| r.call_id == tc.call_id))
                    .map(|r| {
                        let overridden = value_to_json_string(&r.result);
                        tracing::info!(
                            call_id = %tc.call_id,
                            original_len = tc.fn_arguments.len(),
                            overridden_len = overridden.len(),
                            "Client overrode tool call arguments"
                        );
                        tracing::debug!(
                            call_id = %tc.call_id,
                            original = %tc.fn_arguments,
                            overridden = %overridden,
                            "Client overrode tool call arguments (detail)"
                        );
                        overridden
                    })
                    .unwrap_or_else(|| tc.fn_arguments.clone());
                (tc.call_id.as_str(), fn_arguments)
            })
            .collect();

        // 1. Build ToolExecutionRequests message for LLM
        let tool_execution_requests: Vec<serde_json::Value> = hitl_info
            .pending_tool_calls
            .iter()
            .map(|tc| {
                let fn_arguments = resolved_fn_args
                    .get(tc.call_id.as_str())
                    .cloned()
                    .unwrap_or_else(|| tc.fn_arguments.clone());
                serde_json::json!({
                    "callId": tc.call_id,
                    "fnName": tc.fn_name,
                    "fnArguments": fn_arguments
                })
            })
            .collect();

        // 3. Update workflowContext to include toolExecutionRequests in runnerMessages
        // The workflow uses ${ $runnerMessages } to pass messages to LLM
        // We need to add a TOOL message with ToolExecutionRequests format
        let mut found_workflow_inline = false;
        for ctx in &mut input.context {
            if let crate::types::Context::WorkflowInline {
                workflow_context, ..
            } = ctx
            {
                found_workflow_inline = true;

                // Parse existing workflowContext or create new one
                let mut ctx_value: serde_json::Value = workflow_context
                    .as_ref()
                    .and_then(|v| {
                        if let serde_json::Value::String(s) = v {
                            serde_json::from_str(s).ok()
                        } else {
                            Some(v.clone())
                        }
                    })
                    .unwrap_or_else(|| serde_json::json!({}));

                // Get existing runnerMessages or create empty array
                let runner_messages = ctx_value
                    .get("runnerMessages")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!([]));

                let mut messages_array = match runner_messages {
                    serde_json::Value::Array(arr) => arr,
                    _ => vec![],
                };

                // Add Assistant's tool call response (required for OllamaChatService)
                let tool_calls: Vec<serde_json::Value> = hitl_info
                    .pending_tool_calls
                    .iter()
                    .map(|tc| {
                        let fn_arguments = resolved_fn_args
                            .get(tc.call_id.as_str())
                            .cloned()
                            .unwrap_or_else(|| tc.fn_arguments.clone());
                        serde_json::json!({
                            "callId": tc.call_id,
                            "fnName": tc.fn_name,
                            "fnArguments": fn_arguments
                        })
                    })
                    .collect();

                // Proto3 JSON format: enum values use uppercase names
                messages_array.push(serde_json::json!({
                    "role": "ASSISTANT",
                    "content": {
                        "toolCalls": {
                            "calls": tool_calls
                        }
                    }
                }));

                // Add TOOL message with ToolExecutionRequests
                // Proto3 JSON format: enum values use uppercase names
                messages_array.push(serde_json::json!({
                    "role": "TOOL",
                    "content": {
                        "toolExecutionRequests": {
                            "requests": tool_execution_requests.clone()
                        }
                    }
                }));

                // Update context
                if let serde_json::Value::Object(ref mut map) = ctx_value {
                    map.insert(
                        "runnerMessages".to_string(),
                        serde_json::Value::Array(messages_array),
                    );
                }

                // Update the context (as JSON string per proto convention)
                *workflow_context = Some(serde_json::Value::String(ctx_value.to_string()));

                tracing::debug!(
                    workflow_context = ?workflow_context,
                    "Updated workflowContext with toolExecutionRequests in runnerMessages"
                );
                break;
            }
        }

        if !found_workflow_inline {
            return Err(AgUiError::InvalidInput(
                "Missing WorkflowInline context; cannot attach ToolExecutionRequests for LLM tool approval".to_string(),
            ));
        }

        // 4. Clear resume info to avoid infinite loop
        input.resume = None;

        // 5. Parse workflow from input
        let (workflow, workflow_context) = self.parse_workflow_from_input(&input).await?;
        let workflow_name = workflow.document.name.to_string();

        // 6. Use existing run_id and thread_id from session
        let run_id = session.run_id.clone();
        let thread_id = session.thread_id.clone();

        // 7. Create adapter for event conversion
        let adapter = shared_adapter(run_id.clone(), thread_id.clone());

        // 8. Initialize workflow executor
        let executor = self
            .init_workflow_executor(workflow, input, &run_id, adapter.clone(), workflow_context)
            .await?;

        // 9. Atomically clear HITL info and set session state to Active
        // This is done after executor init succeeds to avoid leaving session in
        // an inconsistent state if initialization fails (same pattern as resume_workflow).
        if !self
            .session_manager
            .resume_from_paused(&session.session_id, SessionState::Active)
            .await
        {
            return Err(AgUiError::Internal(anyhow::anyhow!(
                "Failed to update session state from Paused to Active for session {}",
                session.session_id
            )));
        }

        // 10. Register executor for cancellation support
        let executor = Arc::new(executor);
        {
            let mut registry = self.executor_registry.write().await;
            registry.insert(run_id.to_string(), executor.clone());
        }

        // 11. Create event stream (emit_run_started: true — resume is a new run per AG-UI spec)
        // TOOL_CALL_END + TOOL_CALL_RESULT are NOT emitted here because tool execution
        // hasn't happened yet. They will be emitted by StreamingJobCompleted's fallback
        // logic when the auto-executed tool results arrive via pending_tool_results.
        let executor_registry = self.executor_registry.clone();
        let run_id_for_cleanup = run_id.to_string();
        let stream = self.create_event_stream(
            executor,
            adapter,
            run_id.to_string(),
            session.session_id.clone(),
            executor_registry,
            run_id_for_cleanup,
            workflow_name,
            true, // Resume is a new run invocation per AG-UI spec
        );

        Ok((session, Box::pin(stream)))
    }

    /// Handle LLM tool call rejection by feeding rejection info back to LLM.
    ///
    /// This method follows the same pattern as handle_llm_tool_approval:
    /// 1. Emits TOOL_CALL_RESULT + TOOL_CALL_END events for each pending tool call (for client/memory)
    /// 2. Updates workflowContext with rejection text as TOOL message
    /// 3. Re-executes workflow so LLM receives the rejection and can respond accordingly
    async fn handle_llm_tool_rejection(
        &self,
        mut input: RunAgentInput,
        session: Session,
        hitl_info: HitlWaitingInfo,
        reason: Option<String>,
    ) -> Result<(
        Session,
        Pin<Box<dyn Stream<Item = (u64, AgUiEvent)> + Send>>,
    )> {
        let rejection_message = reason.unwrap_or_else(|| "User rejected tool calls".to_string());

        tracing::info!(
            run_id = %session.run_id,
            interrupt_id = %hitl_info.interrupt_id,
            pending_tool_calls = hitl_info.pending_tool_calls.len(),
            reason_len = rejection_message.len(),
            "Handling LLM tool rejection"
        );

        // 1. Build TOOL_CALL_END + TOOL_CALL_RESULT events (persist after validation succeeds)
        // Order: END → RESULT (AG-UI protocol compliant)
        let mut rejection_events: Vec<(u64, AgUiEvent)> = Vec::new();
        for tc in &hitl_info.pending_tool_calls {
            let end_event = AgUiEvent::tool_call_end(tc.call_id.clone());
            let end_id = Self::encode_event_with_logging(&self.encoder, &end_event);
            rejection_events.push((end_id, end_event));

            let result_event = AgUiEvent::tool_call_result(
                tc.call_id.clone(),
                serde_json::json!({
                    "status": "rejected",
                    "reason": rejection_message,
                }),
            );
            let event_id = Self::encode_event_with_logging(&self.encoder, &result_event);
            rejection_events.push((event_id, result_event));
        }

        // 2. Update workflowContext with rejection info in runnerMessages
        let mut found_workflow_inline = false;
        for ctx in &mut input.context {
            if let crate::types::Context::WorkflowInline {
                workflow_context, ..
            } = ctx
            {
                found_workflow_inline = true;

                let mut ctx_value: serde_json::Value = workflow_context
                    .as_ref()
                    .and_then(|v| {
                        if let serde_json::Value::String(s) = v {
                            serde_json::from_str(s).ok()
                        } else {
                            Some(v.clone())
                        }
                    })
                    .unwrap_or_else(|| serde_json::json!({}));

                let runner_messages = ctx_value
                    .get("runnerMessages")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!([]));

                let mut messages_array = match runner_messages {
                    serde_json::Value::Array(arr) => arr,
                    _ => vec![],
                };

                // Add Assistant's tool call message (same as approval flow)
                let tool_calls: Vec<serde_json::Value> = hitl_info
                    .pending_tool_calls
                    .iter()
                    .map(|tc| {
                        serde_json::json!({
                            "callId": tc.call_id,
                            "fnName": tc.fn_name,
                            "fnArguments": tc.fn_arguments
                        })
                    })
                    .collect();

                messages_array.push(serde_json::json!({
                    "role": "ASSISTANT",
                    "content": {
                        "toolCalls": {
                            "calls": tool_calls
                        }
                    }
                }));

                // Add TOOL message with rejection text (not toolExecutionRequests)
                // LLM will see this as tool result text and respond accordingly
                let rejection_text = format!(
                    "All tool calls were rejected by the user. Reason: {}. \
                     Please suggest alternative approaches or ask the user what they would like to do instead.",
                    rejection_message
                );
                messages_array.push(serde_json::json!({
                    "role": "TOOL",
                    "content": {
                        "text": rejection_text
                    }
                }));

                if let serde_json::Value::Object(ref mut map) = ctx_value {
                    map.insert(
                        "runnerMessages".to_string(),
                        serde_json::Value::Array(messages_array),
                    );
                }

                *workflow_context = Some(serde_json::Value::String(ctx_value.to_string()));

                tracing::debug!(
                    workflow_context = ?workflow_context,
                    "Updated workflowContext with rejection in runnerMessages"
                );
                break;
            }
        }

        if !found_workflow_inline {
            return Err(AgUiError::InvalidInput(
                "Missing WorkflowInline context; cannot attach rejection for LLM tool rejection"
                    .to_string(),
            ));
        }

        // 3. Clear resume info to avoid infinite loop
        input.resume = None;

        // 4. Parse workflow from input
        let (workflow, workflow_context) = self.parse_workflow_from_input(&input).await?;
        let workflow_name = workflow.document.name.to_string();

        // 5. Use existing run_id and thread_id from session
        let run_id = session.run_id.clone();
        let thread_id = session.thread_id.clone();

        // 6. Create adapter for event conversion
        let adapter = shared_adapter(run_id.clone(), thread_id.clone());

        // 7. Initialize workflow executor
        let executor = self
            .init_workflow_executor(workflow, input, &run_id, adapter.clone(), workflow_context)
            .await?;

        // 8. Atomically clear HITL info and set session state to Active
        if !self
            .session_manager
            .resume_from_paused(&session.session_id, SessionState::Active)
            .await
        {
            return Err(AgUiError::Internal(anyhow::anyhow!(
                "Failed to update session state from Paused to Active for session {}",
                session.session_id
            )));
        }

        // 9. Persist rejection events now that all checks have passed
        for (event_id, event) in &rejection_events {
            self.event_store
                .store_event(session.run_id.as_str(), *event_id, event.clone())
                .await;
        }

        // 10. Register executor for cancellation support
        let executor = Arc::new(executor);
        {
            let mut registry = self.executor_registry.write().await;
            registry.insert(run_id.to_string(), executor.clone());
        }

        // 11. Create event stream, prepending rejection events
        let executor_registry = self.executor_registry.clone();
        let run_id_for_cleanup = run_id.to_string();
        let workflow_stream = self.create_event_stream(
            executor,
            adapter,
            run_id.to_string(),
            session.session_id.clone(),
            executor_registry,
            run_id_for_cleanup,
            workflow_name,
            false, // Don't emit RUN_STARTED again
        );

        let rejection_stream = futures::stream::iter(rejection_events);
        let combined = rejection_stream.chain(workflow_stream);

        Ok((session, Box::pin(combined)))
    }

    /// Parse workflow schema from input context.
    /// Returns (workflow_schema, workflow_context) tuple.
    async fn parse_workflow_from_input(
        &self,
        input: &RunAgentInput,
    ) -> Result<(Arc<WorkflowSchema>, Option<serde_json::Value>)> {
        // Try to extract inline workflow first, then fall back to workflow by name
        for ctx in &input.context {
            match ctx {
                crate::types::Context::WorkflowInline {
                    workflow,
                    workflow_context,
                } => {
                    let schema: WorkflowSchema =
                        serde_json::from_value(workflow.clone()).map_err(|e| {
                            AgUiError::InvalidInput(format!(
                                "Failed to parse inline workflow: {}",
                                e
                            ))
                        })?;
                    return Ok((Arc::new(schema), workflow_context.clone()));
                }
                crate::types::Context::WorkflowDefinition { workflow_name, .. } => {
                    // Search for REUSABLE_WORKFLOW runner Worker by name
                    let worker = self
                        .app_module
                        .worker_app
                        .find_by_name(workflow_name)
                        .await
                        .map_err(|e| {
                            AgUiError::Internal(anyhow::anyhow!(
                                "Failed to search workflow '{}': {}",
                                workflow_name,
                                e
                            ))
                        })?
                        .ok_or_else(|| AgUiError::WorkflowDefinitionNotFound {
                            name: workflow_name.clone(),
                        })?;

                    // Get WorkerData and deserialize runner_settings to WorkflowSchema
                    let worker_data = worker.data.ok_or_else(|| {
                        AgUiError::InvalidInput(format!("Worker '{}' has no data", workflow_name))
                    })?;

                    let schema: WorkflowSchema =
                        serde_json::from_slice(&worker_data.runner_settings).map_err(|e| {
                            AgUiError::InvalidInput(format!(
                                "Invalid workflow schema in worker '{}': {}",
                                workflow_name, e
                            ))
                        })?;
                    // WorkflowDefinition doesn't support workflowContext
                    return Ok((Arc::new(schema), None));
                }
                _ => continue,
            }
        }

        Err(AgUiError::InvalidInput(
            "No workflow definition in context. Provide either 'workflow_inline' or 'workflow_definition'.".to_string()
        ))
    }

    /// Initialize workflow executor.
    ///
    /// If checkpoint resume is requested, validates that execution_id matches run_id.
    async fn init_workflow_executor(
        &self,
        workflow: Arc<WorkflowSchema>,
        input: RunAgentInput,
        run_id: &RunId,
        _adapter: SharedWorkflowEventAdapter,
        workflow_context: Option<serde_json::Value>,
    ) -> Result<WorkflowExecutor> {
        let app_module = self.app_module.clone();

        // Extract checkpoint if resuming, with execution_id validation
        let checkpoint: Option<CheckPointContext> = input
            .context
            .iter()
            .find_map(|ctx| {
                if let crate::types::Context::CheckpointResume {
                    execution_id,
                    checkpoint_data: Some(data),
                    ..
                } = ctx
                {
                    Some((execution_id.clone(), data.clone()))
                } else {
                    None
                }
            })
            .and_then(|(execution_id, data)| {
                // Validate execution_id matches run_id
                if execution_id != run_id.as_str() {
                    tracing::warn!(
                        "CheckpointResume execution_id '{}' does not match run_id '{}', ignoring checkpoint",
                        execution_id,
                        run_id.as_str()
                    );
                    return None;
                }
                serde_json::from_value(data).ok()
            });

        // Build input value from messages
        let input_value = Arc::new(serde_json::json!({
            "messages": input.messages,
            "tools": input.tools,
        }));

        // Parse workflow_context: can be a JSON string or an object
        // If it's a string, try to parse it as JSON
        let context_value = Arc::new(match workflow_context {
            Some(serde_json::Value::String(s)) => {
                // Try to parse string as JSON object
                serde_json::from_str(&s).unwrap_or_else(|e| {
                    tracing::warn!(
                        "Failed to parse workflowContext string as JSON: {}, using as-is",
                        e
                    );
                    serde_json::json!({ "raw": s })
                })
            }
            Some(obj @ serde_json::Value::Object(_)) => obj,
            Some(other) => {
                tracing::warn!(
                    "workflowContext is not a string or object: {:?}, wrapping in 'value' key",
                    other
                );
                serde_json::json!({ "value": other })
            }
            None => serde_json::json!({}),
        });

        // Build metadata
        let metadata = Arc::new(HashMap::new());

        // Convert RunId to ExecutionId for checkpoint tracking
        let execution_id = run_id.to_execution_id();

        // Initialize executor
        let executor = WorkflowExecutor::init(
            self.app_wrapper_module.clone(),
            app_module,
            workflow,
            input_value,
            execution_id,
            context_value,
            metadata,
            checkpoint,
        )
        .await
        .map_err(|e| AgUiError::WorkflowInitFailed(format!("{:?}", e)))?;

        Ok(executor)
    }

    /// Encode event with error logging.
    ///
    /// Logs a warning if encoding fails and returns a unique fallback event ID.
    /// Fallback IDs start from 0x8000_0000_0000_0000 to distinguish from normal IDs.
    fn encode_event_with_logging(encoder: &EventEncoder, event: &AgUiEvent) -> u64 {
        match encoder.encode(event) {
            Ok((event_id, _)) => event_id,
            Err(e) => {
                let fallback_id = FALLBACK_EVENT_ID_COUNTER.fetch_add(1, Ordering::SeqCst);
                tracing::warn!(
                    event_type = ?event.event_type(),
                    fallback_id = fallback_id,
                    error = %e,
                    "Failed to encode AG-UI event, using fallback ID"
                );
                fallback_id
            }
        }
    }

    /// Create the event stream from workflow execution.
    ///
    /// Returns a stream of (event_id, AgUiEvent) tuples for consistent SSE ID handling.
    ///
    /// # Arguments
    /// * `emit_run_started` - Whether to emit RUN_STARTED event. Set to false for HITL resume
    ///   to avoid duplicate RUN_STARTED events for the same run.
    #[allow(clippy::too_many_arguments)]
    fn create_event_stream(
        &self,
        executor: Arc<WorkflowExecutor>,
        adapter: SharedWorkflowEventAdapter,
        run_id: String,
        session_id: String,
        executor_registry: ExecutorRegistry,
        run_id_for_cleanup: String,
        workflow_name: String,
        emit_run_started: bool,
    ) -> impl Stream<Item = (u64, AgUiEvent)> + Send + 'static {
        let event_store = self.event_store.clone();
        let encoder = self.encoder.clone();
        let session_manager = self.session_manager.clone();
        let app_module = self.app_module.clone();

        stream! {
            // Emit RUN_STARTED if requested (resume passes true per AG-UI spec: each resume is a new run)
            if emit_run_started {
                let adapter_lock = adapter.lock().await;
                let event = adapter_lock.workflow_started("workflow", None);
                let event_id = Self::encode_event_with_logging(&encoder, &event);
                event_store.store_event(&run_id, event_id, event.clone()).await;
                yield (event_id, event);
            }

            // Track whether an error occurred (RUN_ERROR and RUN_FINISHED are mutually exclusive)
            let mut has_error = false;

            // Track previous position for STEP_STARTED/STEP_FINISHED detection
            let mut prev_position: Option<String> = None;

            // Execute workflow with events (includes StreamingJobStarted/StreamingData for LLM streaming)
            // ag-ui-front needs streaming data for real-time UI updates
            let cx = Arc::new(opentelemetry::Context::current());
            let workflow_stream = executor.execute_workflow_with_events(cx, true);

            // Pin the stream for iteration
            tokio::pin!(workflow_stream);

            // Track active message IDs for streaming jobs (job_id -> MessageId)
            // Used to correlate StreamingData events with TEXT_MESSAGE_CONTENT
            let mut active_message_ids: HashMap<i64, MessageId> = HashMap::new();
            // Track sent tool result call_ids to prevent duplicate TOOL_CALL_RESULT events
            let mut sent_tool_result_ids = std::collections::HashSet::<String>::new();
            // Buffer tool results from StreamingData until StreamingJobCompleted emits START/ARGS/END first
            let mut pending_tool_results = std::collections::HashMap::<String, ExtractedToolResult>::new();
            // Track whether text content was sent via StreamingData for each job_id
            // Used to decide whether to emit compensating TEXT_MESSAGE_CONTENT at StreamingJobCompleted
            let mut text_content_sent = std::collections::HashSet::<i64>::new();
            // Track whether TEXT_MESSAGE_START has been emitted for each job_id
            // TEXT_MESSAGE_START is deferred until text content actually arrives
            let mut text_message_started = std::collections::HashSet::<i64>::new();
            // Buffer ToolExecutionStarted info for real-time TOOL_CALL emission
            let mut pending_tool_starts = std::collections::HashMap::<String, ExtractedToolStarted>::new();
            // Track tool calls already emitted via StreamingData (to avoid duplicates at StreamingJobCompleted)
            let mut sent_tool_call_ids = std::collections::HashSet::<String>::new();

            // Channel for nested workflow progress events (from parallel subscriptions)
            // Wrapped in Option so early-return paths can drop the sender to unblock receivers.
            let (nested_event_tx_raw, mut nested_event_rx) =
                tokio::sync::mpsc::channel::<(u64, AgUiEvent)>(NESTED_EVENT_CHANNEL_SIZE);
            let mut nested_event_tx = Some(nested_event_tx_raw);
            // Track spawned nested workflow subscription tasks for cleanup
            let mut nested_task_handles: std::collections::HashMap<i64, tokio::task::JoinHandle<()>> = std::collections::HashMap::new();

            // Main event loop - process workflow events and nested progress events
            // Uses tokio::select! to merge both event sources
            loop {
                let event_result = tokio::select! {
                    result = workflow_stream.next() => {
                        match result {
                            Some(r) => r,
                            None => break, // workflow stream ended
                        }
                    }
                    Some(nested_event) = nested_event_rx.recv() => {
                        yield nested_event;
                        continue;
                    }
                };
                match event_result {
                    Ok(event) => {
                        // Handle StreamingJobStarted: prepare message_id but defer TEXT_MESSAGE_START
                        // until text content actually arrives (avoids empty START/END for tool-only responses)
                        if let WorkflowStreamEvent::StreamingJobStarted { event: ev } = &event {
                            let job_id_value = ev.job_id.as_ref().map(|j| j.value).unwrap_or(0);
                            let message_id = MessageId::random();

                            tracing::info!(
                                job_id = job_id_value,
                                worker_name = ?ev.worker_name,
                                position = %ev.position,
                                "StreamingJobStarted: preparing message_id (TEXT_MESSAGE_START deferred)"
                            );

                            // Close previous message if exists for this job_id (prevents dangling UI messages)
                            if let Some(old_message_id) = active_message_ids.remove(&job_id_value)
                                && text_message_started.remove(&job_id_value)
                            {
                                tracing::warn!(
                                    job_id = job_id_value,
                                    "Received duplicate StreamingJobStarted, closing previous message"
                                );
                                let end_event = AgUiEvent::text_message_end(old_message_id);
                                let end_event_id = Self::encode_event_with_logging(&encoder, &end_event);
                                event_store.store_event(&run_id, end_event_id, end_event.clone()).await;
                                yield (end_event_id, end_event);
                            }

                            // Store message_id for correlation with StreamingData
                            // TEXT_MESSAGE_START will be emitted lazily when text content arrives
                            active_message_ids.insert(job_id_value, message_id);
                        }

                        // Handle StreamingData: emit TEXT_MESSAGE_CONTENT or role=tool messages
                        if let WorkflowStreamEvent::StreamingData { event: ev } = &event {
                            let job_id_value = ev.job_id.as_ref().map(|j| j.value).unwrap_or(0);
                            if let Some(message_id) = active_message_ids.get(&job_id_value) {
                                // Decode LlmChatResult protobuf and extract text content
                                if let Some(text) = extract_text_from_llm_chat_result(&ev.data) {
                                    // Lazily emit TEXT_MESSAGE_START on first text content
                                    if !text_message_started.contains(&job_id_value) {
                                        text_message_started.insert(job_id_value);
                                        let start_event = AgUiEvent::text_message_start(
                                            message_id.clone(),
                                            crate::types::Role::Assistant,
                                        );
                                        let start_event_id = Self::encode_event_with_logging(&encoder, &start_event);
                                        event_store.store_event(&run_id, start_event_id, start_event.clone()).await;
                                        yield (start_event_id, start_event);
                                    }
                                    text_content_sent.insert(job_id_value);
                                    let ag_event = AgUiEvent::text_message_content(
                                        message_id.clone(),
                                        text,
                                    );
                                    let event_id = Self::encode_event_with_logging(&encoder, &ag_event);
                                    event_store.store_event(&run_id, event_id, ag_event.clone()).await;
                                    yield (event_id, ag_event);
                                }

                                // Detect ToolExecutionStarted: emit TOOL_CALL_START/ARGS immediately
                                // so the UI sees them up-front, and buffer for correlating the result.
                                let tool_started_opt = extract_tool_execution_started(&ev.data)
                                    .filter(|s| s.job_id > 0);
                                if let Some(ref started) = tool_started_opt {
                                    // Emit START/ARGS immediately for real-time UI feedback
                                    let parent_msg_id = active_message_ids.get(&job_id_value)
                                        .map(|m| m.to_string());
                                    let start_event = AgUiEvent::tool_call_start(
                                        started.call_id.clone(),
                                        started.fn_name.clone(),
                                        parent_msg_id,
                                    );
                                    let start_eid = Self::encode_event_with_logging(&encoder, &start_event);
                                    event_store.store_event(&run_id, start_eid, start_event.clone()).await;
                                    yield (start_eid, start_event);

                                    let args_event = AgUiEvent::tool_call_args(
                                        started.call_id.clone(),
                                        started.fn_arguments.clone(),
                                    );
                                    let args_eid = Self::encode_event_with_logging(&encoder, &args_event);
                                    event_store.store_event(&run_id, args_eid, args_event.clone()).await;
                                    yield (args_eid, args_event);

                                    sent_tool_call_ids.insert(started.call_id.clone());

                                    // Check if a result was already buffered (arrived before Started)
                                    if let Some(extracted) = pending_tool_results.remove(&started.call_id) {
                                        // Emit END → RESULT immediately
                                        let end_event = AgUiEvent::tool_call_end(started.call_id.clone());
                                        let end_eid = Self::encode_event_with_logging(&encoder, &end_event);
                                        event_store.store_event(&run_id, end_eid, end_event.clone()).await;
                                        yield (end_eid, end_event);

                                        let result_ev = AgUiEvent::tool_call_result(
                                            started.call_id.clone(),
                                            extracted.result,
                                        );
                                        let result_eid = Self::encode_event_with_logging(&encoder, &result_ev);
                                        event_store.store_event(&run_id, result_eid, result_ev.clone()).await;
                                        yield (result_eid, result_ev);

                                        sent_tool_result_ids.insert(started.call_id.clone());
                                    } else {
                                        // Keep in pending_tool_starts for correlating the eventual result
                                        pending_tool_starts.insert(started.call_id.clone(), started.clone());
                                    }
                                }

                                // Process tool execution results from streaming chunks.
                                // START/ARGS were already emitted at ToolExecutionStarted time,
                                // so only emit END/RESULT here.
                                let tool_results = extract_tool_execution_results(&ev.data);
                                for (call_id, extracted) in tool_results {
                                    if sent_tool_result_ids.contains(call_id.as_str()) {
                                        continue;
                                    }
                                    if let Some(_tool_start) = pending_tool_starts.remove(&call_id) {
                                        // START/ARGS already emitted; emit only END → RESULT
                                        let end_event = AgUiEvent::tool_call_end(call_id.clone());
                                        let end_eid = Self::encode_event_with_logging(&encoder, &end_event);
                                        event_store.store_event(&run_id, end_eid, end_event.clone()).await;
                                        yield (end_eid, end_event);

                                        let result_ev = AgUiEvent::tool_call_result(
                                            call_id.clone(),
                                            extracted.result,
                                        );
                                        let result_eid = Self::encode_event_with_logging(&encoder, &result_ev);
                                        event_store.store_event(&run_id, result_eid, result_ev.clone()).await;
                                        yield (result_eid, result_ev);

                                        sent_tool_result_ids.insert(call_id);
                                    } else {
                                        // No matching ToolExecutionStarted yet, buffer for later
                                        pending_tool_results.entry(call_id).or_insert(extracted);
                                    }
                                }

                                // Spawn nested workflow progress subscription for ToolExecutionStarted
                                if let Some(ref started) = tool_started_opt {
                                    // Evict finished handles to keep active count accurate
                                    nested_task_handles.retain(|_, h| !h.is_finished());
                                    if nested_task_handles.contains_key(&started.job_id) {
                                        // Already subscribed to this job_id
                                    } else if nested_task_handles.len() >= MAX_NESTED_SUBSCRIPTIONS {
                                        tracing::warn!(
                                            limit = MAX_NESTED_SUBSCRIPTIONS,
                                            tool_job_id = started.job_id,
                                            "Nested workflow subscription limit reached, skipping"
                                        );
                                    } else if let Some(nested_tx) = nested_event_tx.as_ref().cloned() {
                                        let nested_job_id = proto::jobworkerp::data::JobId { value: started.job_id };
                                        let job_result_app = app_module.job_result_app.clone();
                                        let nested_encoder = encoder.clone();
                                        let nested_event_store = event_store.clone();
                                        let nested_run_id = run_id.clone();
                                        let timeout_ms = NESTED_WF_PROGRESS_TIMEOUT_MS;

                                        // Capture parent step ID for nested step hierarchy
                                        let parent_step_id = {
                                            let adapter_lock = adapter.lock().await;
                                            adapter_lock.current_step_id().map(|s| s.to_string())
                                        };

                                        tracing::info!(
                                            tool_job_id = started.job_id,
                                            fn_name = %started.fn_name,
                                            "ToolExecutionStarted: spawning nested workflow progress subscription"
                                        );

                                        // Use local step_id tracking instead of shared adapter
                                        // to avoid current_step_id contention with the main loop.
                                        let handle = tokio::spawn(async move {
                                            use prost::Message as _;
                                            let stream_opt = crate::pubsub::subscribe_result_stream_for_job(
                                                &job_result_app,
                                                &nested_job_id,
                                                timeout_ms,
                                            ).await;

                                            if let Some(mut stream) = stream_opt {
                                                let mut prev_pos: Option<String> = None;
                                                let mut prev_step_id: Option<StepId> = None;
                                                while let Some(item) = stream.next().await {
                                                    match &item.item {
                                                        Some(proto::jobworkerp::data::result_output_item::Item::End(_))
                                                        | Some(proto::jobworkerp::data::result_output_item::Item::FinalCollected(_)) => {
                                                            break;
                                                        }
                                                        Some(proto::jobworkerp::data::result_output_item::Item::Data(data)) => {
                                                            if let Ok(wf_result) = jobworkerp_runner::jobworkerp::runner::WorkflowResult::decode(&data[..]) {
                                                            let current_pos = wf_result.position.clone();
                                                            if prev_pos.as_ref() != Some(&current_pos) && !current_pos.is_empty() {
                                                                // Position changed: emit STEP_FINISHED for previous, STEP_STARTED for current
                                                                if let Some(finished_id) = prev_step_id.take() {
                                                                    let finished_event = AgUiEvent::StepFinished {
                                                                        step_id: finished_id.to_string(),
                                                                        timestamp: Some(AgUiEvent::now_timestamp()),
                                                                        result: None,
                                                                    };
                                                                    let eid = Self::encode_event_with_logging(&nested_encoder, &finished_event);
                                                                    nested_event_store.store_event(&nested_run_id, eid, finished_event.clone()).await;
                                                                    if nested_tx.send((eid, finished_event)).await.is_err() {
                                                                        tracing::debug!("Nested event receiver dropped, stopping subscription");
                                                                        break;
                                                                    }
                                                                }

                                                                // Extract step name from position (last segment)
                                                                let step_name = current_pos.rsplit('/').next().unwrap_or(&current_pos);
                                                                let step_id = StepId::random();
                                                                let started_event = AgUiEvent::StepStarted {
                                                                    step_id: step_id.to_string(),
                                                                    step_name: Some(step_name.to_string()),
                                                                    parent_step_id: parent_step_id.clone(),
                                                                    timestamp: Some(AgUiEvent::now_timestamp()),
                                                                    metadata: None,
                                                                };
                                                                let eid = Self::encode_event_with_logging(&nested_encoder, &started_event);
                                                                nested_event_store.store_event(&nested_run_id, eid, started_event.clone()).await;
                                                                if nested_tx.send((eid, started_event)).await.is_err() {
                                                                    tracing::debug!("Nested event receiver dropped, stopping subscription");
                                                                    break;
                                                                }
                                                                prev_step_id = Some(step_id);

                                                                prev_pos = Some(current_pos);
                                                            }
                                                            } else {
                                                                tracing::trace!(
                                                                    data_len = data.len(),
                                                                    "Failed to decode WorkflowResult from nested streaming data, skipping"
                                                                );
                                                            }
                                                        }
                                                        _ => {}
                                                    }
                                                }

                                                // Emit final STEP_FINISHED for the last step
                                                if let Some(finished_id) = prev_step_id.take() {
                                                    let finished_event = AgUiEvent::StepFinished {
                                                        step_id: finished_id.to_string(),
                                                        timestamp: Some(AgUiEvent::now_timestamp()),
                                                        result: None,
                                                    };
                                                    let eid = Self::encode_event_with_logging(&nested_encoder, &finished_event);
                                                    nested_event_store.store_event(&nested_run_id, eid, finished_event.clone()).await;
                                                    let _ = nested_tx.send((eid, finished_event)).await;
                                                }
                                            } else {
                                                tracing::warn!(
                                                    job_id = nested_job_id.value,
                                                    "subscribe_result_stream_for_job returned None: nested job may have already completed"
                                                );
                                            }
                                        });
                                        nested_task_handles.insert(started.job_id, handle);
                                    }
                                }
                            } else {
                                tracing::warn!(
                                    job_id = job_id_value,
                                    "Received StreamingData for unknown job_id"
                                );
                            }
                            // StreamingData doesn't need further processing
                            continue;
                        }

                        // Handle StreamingJobCompleted: emit TEXT_MESSAGE_END and check for LLM tool calls
                        if let WorkflowStreamEvent::StreamingJobCompleted { event: ev, context: tc } = &event {
                            let job_id_value = ev.job_id.as_ref().map(|j| j.value).unwrap_or(0);
                            let message_id = active_message_ids.remove(&job_id_value);

                            // Serialize tc.output for tool call extraction
                            let output_bytes = serde_json::to_vec(&tc.output).unwrap_or_default();

                            // Debug log the output content for troubleshooting
                            tracing::debug!(
                                job_id = job_id_value,
                                output_len = output_bytes.len(),
                                output_preview = %String::from_utf8_lossy(&output_bytes[..std::cmp::min(500, output_bytes.len())]),
                                "StreamingJobCompleted: checking for tool calls"
                            );

                            // Check for LLM tool calls in the output (both auto-calling and HITL modes)
                            let extracted_tool_calls = extract_tool_calls_from_llm_result(&output_bytes);

                            // Emit TOOL_CALL events for both modes (display purpose)
                            if let Some(ref tool_calls) = extracted_tool_calls
                                && !tool_calls.tool_calls.is_empty() {
                                    tracing::info!(
                                        job_id = job_id_value,
                                        tool_count = tool_calls.tool_calls.len(),
                                        requires_execution = tool_calls.requires_execution,
                                        "StreamingJobCompleted: detected LLM tool calls"
                                    );

                                    // Filter out tool calls already emitted in real-time via StreamingData
                                    let unsent_tool_calls: Vec<_> = tool_calls.tool_calls.iter()
                                        .filter(|tc| !sent_tool_call_ids.contains(&tc.call_id))
                                        .cloned()
                                        .collect();

                                    if !unsent_tool_calls.is_empty() {
                                        // Emit TOOL_CALL_START and TOOL_CALL_ARGS for unsent tool calls
                                        let tool_events = tool_calls_to_ag_ui_events(
                                            &unsent_tool_calls,
                                            message_id.as_ref(),
                                        );
                                        for tool_event in tool_events {
                                            let event_id = Self::encode_event_with_logging(&encoder, &tool_event);
                                            event_store.store_event(&run_id, event_id, tool_event.clone()).await;
                                            yield (event_id, tool_event);
                                        }
                                    }

                                    // Auto-calling mode: emit TOOL_CALL_END + TOOL_CALL_RESULT for unsent tools
                                    // Order: START → ARGS → END → RESULT (AG-UI protocol compliant)
                                    if !tool_calls.requires_execution {
                                        let tool_results_from_output = extract_tool_execution_results(&output_bytes);
                                        for call in &tool_calls.tool_calls {
                                            // Emit START/ARGS only if not already sent via real-time path
                                            if !sent_tool_call_ids.contains(&call.call_id) {
                                                // START/ARGS were already emitted in tool_calls_to_ag_ui_events above
                                                // for unsent_tool_calls, so just mark as sent here
                                                sent_tool_call_ids.insert(call.call_id.clone());
                                            }

                                            // Always try to emit END/RESULT (guarded by sent_tool_result_ids)
                                            if !sent_tool_result_ids.contains(call.call_id.as_str()) {
                                                // TOOL_CALL_END
                                                let end_event = AgUiEvent::tool_call_end(call.call_id.clone());
                                                let end_event_id = Self::encode_event_with_logging(&encoder, &end_event);
                                                event_store.store_event(&run_id, end_event_id, end_event.clone()).await;
                                                yield (end_event_id, end_event);

                                                // TOOL_CALL_RESULT: buffered result (from StreamingData) takes priority,
                                                // then fall back to output bytes, then Null
                                                sent_tool_result_ids.insert(call.call_id.clone());
                                                let result = pending_tool_results.remove(&call.call_id)
                                                    .or_else(|| tool_results_from_output.get(&call.call_id).cloned())
                                                    .map(|e| e.result)
                                                    .unwrap_or(serde_json::Value::Null);
                                                let result_ev = AgUiEvent::tool_call_result(
                                                    call.call_id.clone(),
                                                    result,
                                                );
                                                let result_ev_id = Self::encode_event_with_logging(&encoder, &result_ev);
                                                event_store.store_event(&run_id, result_ev_id, result_ev.clone()).await;
                                                yield (result_ev_id, result_ev);
                                            }
                                        }
                                    }

                                }

                            // Flush any buffered tool results that haven't been emitted yet.
                            // This handles two cases:
                            // 1. auto_calling with requires_execution=true (collect_stream preserves
                            //    the flag from the initial LLM response even after server-side execution)
                            // 2. HITL resume: tools were executed during re-run, results arrived via
                            //    StreamingData, but the final StreamingJobCompleted has no tool_calls
                            //    (it's the LLM's text response after seeing the tool results)
                            for (call_id, extracted) in pending_tool_results.drain() {
                                if !sent_tool_result_ids.contains(call_id.as_str()) {
                                    sent_tool_result_ids.insert(call_id.clone());

                                    // Ensure START/ARGS were emitted before END/RESULT
                                    if !sent_tool_call_ids.contains(call_id.as_str()) {
                                        if let Some(tool_start) = pending_tool_starts.remove(&call_id) {
                                            let start_event = AgUiEvent::tool_call_start(
                                                call_id.clone(),
                                                tool_start.fn_name.clone(),
                                                None,
                                            );
                                            let start_eid = Self::encode_event_with_logging(&encoder, &start_event);
                                            event_store.store_event(&run_id, start_eid, start_event.clone()).await;
                                            yield (start_eid, start_event);

                                            let args_event = AgUiEvent::tool_call_args(
                                                call_id.clone(),
                                                tool_start.fn_arguments,
                                            );
                                            let args_eid = Self::encode_event_with_logging(&encoder, &args_event);
                                            event_store.store_event(&run_id, args_eid, args_event.clone()).await;
                                            yield (args_eid, args_event);
                                        }
                                        sent_tool_call_ids.insert(call_id.clone());
                                    }

                                    let end_event = AgUiEvent::tool_call_end(call_id.clone());
                                    let end_event_id = Self::encode_event_with_logging(&encoder, &end_event);
                                    event_store.store_event(&run_id, end_event_id, end_event.clone()).await;
                                    yield (end_event_id, end_event);

                                    let result_ev = AgUiEvent::tool_call_result(
                                        call_id,
                                        extracted.result,
                                    );
                                    let result_ev_id = Self::encode_event_with_logging(&encoder, &result_ev);
                                    event_store.store_event(&run_id, result_ev_id, result_ev.clone()).await;
                                    yield (result_ev_id, result_ev);
                                }
                            }

                            // Compensate for missing text content when StreamingData didn't deliver it
                            // (e.g., Phase 3 continuation stream started after job completion was published)
                            if let Some(ref msg_id) = message_id
                                && !text_content_sent.contains(&job_id_value)
                                && let Some(text) = extract_text_from_completed_output(&tc.output)
                                && !text.is_empty()
                            {
                                // Lazily emit TEXT_MESSAGE_START for compensating text
                                if !text_message_started.contains(&job_id_value) {
                                    text_message_started.insert(job_id_value);
                                    let start_event = AgUiEvent::text_message_start(
                                        msg_id.clone(),
                                        crate::types::Role::Assistant,
                                    );
                                    let start_event_id = Self::encode_event_with_logging(&encoder, &start_event);
                                    event_store.store_event(&run_id, start_event_id, start_event.clone()).await;
                                    yield (start_event_id, start_event);
                                }
                                tracing::info!(
                                    job_id = job_id_value,
                                    text_len = text.len(),
                                    "StreamingJobCompleted: compensating missing text from output"
                                );
                                let ag_event = AgUiEvent::text_message_content(
                                    msg_id.clone(),
                                    text,
                                );
                                let event_id = Self::encode_event_with_logging(&encoder, &ag_event);
                                event_store.store_event(&run_id, event_id, ag_event.clone()).await;
                                yield (event_id, ag_event);
                            }

                            // Emit TEXT_MESSAGE_END only if TEXT_MESSAGE_START was emitted
                            if let Some(msg_id) = message_id.clone() {
                                if text_message_started.remove(&job_id_value) {
                                    tracing::info!(
                                        job_id = job_id_value,
                                        "StreamingJobCompleted: emitting TEXT_MESSAGE_END"
                                    );

                                    let ag_event = AgUiEvent::text_message_end(msg_id);
                                    let event_id = Self::encode_event_with_logging(&encoder, &ag_event);
                                    event_store.store_event(&run_id, event_id, ag_event.clone()).await;
                                    yield (event_id, ag_event);
                                } else {
                                    tracing::debug!(
                                        job_id = job_id_value,
                                        "StreamingJobCompleted: skipping TEXT_MESSAGE_END (no text content was sent)"
                                    );
                                }
                            }

                            // HITL mode: If requires_execution is true, pause for client approval
                            if let Some(ref tool_calls) = extracted_tool_calls {
                                tracing::info!(
                                    job_id = job_id_value,
                                    requires_execution = tool_calls.requires_execution,
                                    tool_count = tool_calls.tool_calls.len(),
                                    "StreamingJobCompleted: HITL check - requires_execution={}, tool_count={}",
                                    tool_calls.requires_execution,
                                    tool_calls.tool_calls.len()
                                );
                                if tool_calls.requires_execution && !tool_calls.tool_calls.is_empty() {
                                    tracing::info!(
                                        "LLM tool calls require client approval (HITL mode)"
                                    );

                                    // Get checkpoint position for later resume
                                    let current_position = tc.position.read().await.as_json_pointer();
                                    tracing::info!(
                                        checkpoint_position = %current_position,
                                        "HITL: checkpoint position for resume"
                                    );

                                    // Convert extracted tool calls to PendingToolCallInfo
                                    let pending_tool_calls: Vec<PendingToolCallInfo> = tool_calls.tool_calls.iter()
                                        .map(|tc| PendingToolCallInfo {
                                            call_id: tc.call_id.clone(),
                                            fn_name: tc.fn_name.clone(),
                                            fn_arguments: tc.fn_arguments.clone(),
                                        })
                                        .collect();

                                    // Use first tool call ID as the primary identifier
                                    let primary_tool_call_id = tool_calls.tool_calls[0].call_id.clone();

                                    // Update session state to Paused with HITL info for later resume
                                    let hitl_info = HitlWaitingInfo::new(
                                        primary_tool_call_id.clone(),
                                        current_position.clone(),
                                        workflow_name.clone(),
                                        pending_tool_calls.clone(),
                                    );
                                    let interrupt_id = hitl_info.interrupt_id.clone();
                                    if !session_manager.set_paused_with_hitl_info(&session_id, hitl_info).await {
                                        tracing::error!(
                                            session_id = %session_id,
                                            run_id = %run_id,
                                            "Failed to persist LLM HITL waiting info, emitting RUN_ERROR"
                                        );

                                        let adapter_lock = adapter.lock().await;
                                        let error_event = adapter_lock.workflow_error(
                                            "Failed to persist LLM HITL waiting state".to_string(),
                                            "HITL_PERSISTENCE_ERROR",
                                            None,
                                        );
                                        drop(adapter_lock);

                                        let error_event_id = Self::encode_event_with_logging(&encoder, &error_event);
                                        event_store.store_event(&run_id, error_event_id, error_event.clone()).await;
                                        yield (error_event_id, error_event);

                                        session_manager.set_session_state(&session_id, SessionState::Error).await;
                                        // Drop sender so nested subscription tasks can terminate
                                        nested_event_tx.take();
                                        return;
                                    }

                                    // Remove executor from registry
                                    {
                                        let mut registry = executor_registry.write().await;
                                        registry.remove(&run_id_for_cleanup);
                                        tracing::debug!(
                                            run_id = %run_id_for_cleanup,
                                            "Removed executor from registry for LLM HITL pause"
                                        );
                                    }

                                    // AG-UI Interrupts: Emit RUN_FINISHED with outcome="interrupt"
                                    let interrupt_payload = InterruptPayload {
                                        pending_tool_calls: pending_tool_calls.iter()
                                            .map(|tc| PendingToolCall {
                                                call_id: tc.call_id.clone(),
                                                fn_name: tc.fn_name.clone(),
                                                fn_arguments: tc.fn_arguments.clone(),
                                            })
                                            .collect(),
                                        checkpoint_position: current_position,
                                        workflow_name: workflow_name.clone(),
                                    };
                                    let interrupt_info = InterruptInfo {
                                        id: interrupt_id.clone(),
                                        reason: "tool_approval_required".to_string(),
                                        payload: interrupt_payload,
                                    };
                                    let run_finished_event = AgUiEvent::run_finished_with_interrupt(
                                        run_id.clone(),
                                        interrupt_info,
                                    );
                                    let event_id = Self::encode_event_with_logging(&encoder, &run_finished_event);
                                    event_store.store_event(&run_id, event_id, run_finished_event.clone()).await;
                                    yield (event_id, run_finished_event);

                                    tracing::info!(
                                        interrupt_id = %interrupt_id,
                                        "Workflow paused for LLM tool call approval, session {} is now Paused",
                                        session_id
                                    );
                                    // Drop sender so nested subscription tasks can terminate
                                    nested_event_tx.take();
                                    return;
                                }
                            }

                            // Continue to process context for state snapshot
                        }

                        // Handle TaskStarted: emit STEP_STARTED at task begin time
                        if let WorkflowStreamEvent::TaskStarted { event: ev } = &event {
                            let task_position = &ev.position;
                            if !task_position.is_empty() && prev_position.as_deref() != Some(task_position) {
                                // Close previous step if position changed
                                if prev_position.is_some() {
                                    let finished_event = {
                                        let mut adapter_lock = adapter.lock().await;
                                        adapter_lock.task_finished(None)
                                    };
                                    if let Some(finished_event) = finished_event {
                                        let event_id = Self::encode_event_with_logging(&encoder, &finished_event);
                                        event_store.store_event(&run_id, event_id, finished_event.clone()).await;
                                        yield (event_id, finished_event);
                                    }
                                }

                                let step_name = ev.task_name.rsplit('/').next().unwrap_or(&ev.task_name);
                                let mut adapter_lock = adapter.lock().await;
                                let started_event = adapter_lock.task_started(
                                    step_name,
                                    Some(&ev.task_type),
                                    None,
                                );
                                drop(adapter_lock);
                                let event_id = Self::encode_event_with_logging(&encoder, &started_event);
                                event_store.store_event(&run_id, event_id, started_event.clone()).await;
                                yield (event_id, started_event);

                                prev_position = Some(task_position.clone());
                                tracing::debug!(
                                    task_name = %ev.task_name,
                                    task_type = %ev.task_type,
                                    position = %task_position,
                                    "TaskStarted: emitted STEP_STARTED"
                                );
                            }
                        }

                        // Handle TaskCompleted: emit STEP_FINISHED and skip context() fallback
                        // to avoid duplicate STEP_STARTED for the same position.
                        if let WorkflowStreamEvent::TaskCompleted { event: ev, .. } = &event {
                            let task_position = &ev.position;
                            if !task_position.is_empty() && prev_position.as_deref() == Some(task_position) {
                                let finished_event = {
                                    let mut adapter_lock = adapter.lock().await;
                                    adapter_lock.task_finished(None)
                                };
                                if let Some(finished_event) = finished_event {
                                    let event_id = Self::encode_event_with_logging(&encoder, &finished_event);
                                    event_store.store_event(&run_id, event_id, finished_event.clone()).await;
                                    yield (event_id, finished_event);
                                }
                                prev_position = None;
                                tracing::debug!(
                                    task_name = %ev.task_name,
                                    position = %task_position,
                                    "TaskCompleted: emitted STEP_FINISHED"
                                );
                                // Skip context() fallback — prev_position is now None but
                                // the completed position's STEP_FINISHED was already emitted;
                                // falling through would emit a spurious STEP_STARTED.
                                continue;
                            }
                        }

                        // Extract context from completed events for workflow state updates
                        if let Some(tc) = event.context() {
                            // Read position from TaskContext
                            let current_position = tc.position.read().await.as_json_pointer();
                            let current_step_name = tc.position.read().await.last_name();

                            if prev_position.as_ref() != Some(&current_position) {
                                // Position changed - emit STEP_FINISHED for previous step if exists
                                if prev_position.is_some() {
                                    let finished_event = {
                                        let mut adapter_lock = adapter.lock().await;
                                        adapter_lock.task_finished(None)
                                    };
                                    if let Some(event) = finished_event {
                                        let event_id = Self::encode_event_with_logging(&encoder, &event);
                                        event_store.store_event(&run_id, event_id, event.clone()).await;
                                        yield (event_id, event);
                                    }
                                }

                                // Emit STEP_STARTED for new step if we have a step name
                                if let Some(step_name) = current_step_name.as_ref() {
                                    let event = {
                                        let mut adapter_lock = adapter.lock().await;
                                        adapter_lock.task_started(step_name, None, None)
                                    };
                                    let event_id = Self::encode_event_with_logging(&encoder, &event);
                                    event_store.store_event(&run_id, event_id, event.clone()).await;
                                    yield (event_id, event);
                                }

                                prev_position = Some(current_position.clone());
                            }

                            // Get workflow context for state snapshot
                            let wfc = executor.workflow_context.read().await;

                            // Get context variables from tokio::sync::Mutex
                            let context_vars = {
                                let guard = wfc.context_variables.lock().await;
                                guard
                                    .iter()
                                    .map(|(k, v)| (k.clone(), v.clone()))
                                    .collect::<HashMap<String, serde_json::Value>>()
                            };

                            // Check for HITL Waiting status
                            let is_waiting = wfc.status == app_wrapper::workflow::execute::context::WorkflowStatus::Waiting;

                            // Convert context to state snapshot event
                            let state = WorkflowState::new(&wfc.name)
                                .with_status(match wfc.status {
                                    app_wrapper::workflow::execute::context::WorkflowStatus::Pending => WorkflowStatus::Pending,
                                    app_wrapper::workflow::execute::context::WorkflowStatus::Running => WorkflowStatus::Running,
                                    app_wrapper::workflow::execute::context::WorkflowStatus::Completed => WorkflowStatus::Completed,
                                    app_wrapper::workflow::execute::context::WorkflowStatus::Faulted => WorkflowStatus::Faulted,
                                    app_wrapper::workflow::execute::context::WorkflowStatus::Cancelled => WorkflowStatus::Cancelled,
                                    app_wrapper::workflow::execute::context::WorkflowStatus::Waiting => WorkflowStatus::Running, // Waiting maps to Running for AG-UI protocol
                                })
                                .with_context_variables(context_vars);

                            let adapter_lock = adapter.lock().await;
                            let event = adapter_lock.state_snapshot(state);
                            drop(adapter_lock);
                            drop(wfc);

                            let event_id = Self::encode_event_with_logging(&encoder, &event);
                            event_store.store_event(&run_id, event_id, event.clone()).await;
                            yield (event_id, event);

                            // HITL: If workflow is waiting for user input, emit TOOL_CALL events
                            if is_waiting {
                                tracing::info!("Workflow waiting for user input, emitting TOOL_CALL events");

                                // Generate tool_call_id from run_id for consistency
                                let tool_call_id = format!("wait_{}", run_id);

                                // Get checkpoint position for later resume
                                let checkpoint_position = current_position;

                                // Emit TOOL_CALL_START event with canonical HUMAN_INPUT tool name
                                let tool_call_start = AgUiEvent::tool_call_start(
                                    tool_call_id.clone(),
                                    "HUMAN_INPUT".to_string(),
                                    None,
                                );
                                let start_event_id = Self::encode_event_with_logging(&encoder, &tool_call_start);
                                event_store.store_event(&run_id, start_event_id, tool_call_start.clone()).await;
                                yield (start_event_id, tool_call_start);

                                // Emit TOOL_CALL_ARGS event with current output
                                let args = tc.output
                                    .as_ref()
                                    .to_string();
                                let tool_call_args = AgUiEvent::tool_call_args(
                                    tool_call_id.clone(),
                                    args,
                                );
                                let args_event_id = Self::encode_event_with_logging(&encoder, &tool_call_args);
                                event_store.store_event(&run_id, args_event_id, tool_call_args.clone()).await;
                                yield (args_event_id, tool_call_args);

                                // Update session state to Paused with HITL info for later resume
                                let hitl_info = HitlWaitingInfo::new(
                                    tool_call_id.clone(),
                                    checkpoint_position,
                                    workflow_name.clone(),
                                    vec![], // Traditional HITL (HUMAN_INPUT)
                                );
                                if !session_manager.set_paused_with_hitl_info(&session_id, hitl_info).await {
                                    // Failed to persist HITL state - emit RUN_ERROR and set session to Error
                                    tracing::error!(
                                        session_id = %session_id,
                                        run_id = %run_id,
                                        "Failed to persist HITL waiting info, emitting RUN_ERROR"
                                    );

                                    let adapter_lock = adapter.lock().await;
                                    let error_event = adapter_lock.workflow_error(
                                        "Failed to persist HITL waiting state".to_string(),
                                        "HITL_PERSISTENCE_ERROR",
                                        None,
                                    );
                                    drop(adapter_lock);

                                    let error_event_id = Self::encode_event_with_logging(&encoder, &error_event);
                                    event_store.store_event(&run_id, error_event_id, error_event.clone()).await;
                                    yield (error_event_id, error_event);

                                    session_manager.set_session_state(&session_id, SessionState::Error).await;
                                    // Drop sender so nested subscription tasks can terminate
                                    nested_event_tx.take();
                                    return;
                                }

                                // Remove executor from registry so /stream does not treat the run as active
                                {
                                    let mut registry = executor_registry.write().await;
                                    registry.remove(&run_id_for_cleanup);
                                    tracing::debug!(
                                        run_id = %run_id_for_cleanup,
                                        "Removed executor from registry for paused HITL run"
                                    );
                                }

                                // Flush TEXT_MESSAGE_END for any active streaming messages before pausing
                                for (job_id, message_id) in active_message_ids.drain() {
                                    if !text_message_started.remove(&job_id) {
                                        continue;
                                    }
                                    tracing::debug!(
                                        job_id = job_id,
                                        "HITL pause: flushing TEXT_MESSAGE_END for interrupted stream"
                                    );
                                    let ag_event = AgUiEvent::text_message_end(message_id);
                                    let event_id = Self::encode_event_with_logging(&encoder, &ag_event);
                                    event_store.store_event(&run_id, event_id, ag_event.clone()).await;
                                    yield (event_id, ag_event);
                                }

                                // Exit stream without emitting RUN_FINISHED
                                tracing::info!("Workflow paused for HITL, session {} is now Paused", session_id);
                                // Drop sender so nested subscription tasks can terminate
                                nested_event_tx.take();
                                return;
                            }
                        }
                        // Start events (StreamingJobStarted, JobStarted, TaskStarted) are handled above
                    }
                    Err(e) => {
                        // Flush any remaining TEXT_MESSAGE_END for interrupted streams
                        for (job_id, message_id) in active_message_ids.drain() {
                            if !text_message_started.remove(&job_id) {
                                continue;
                            }
                            tracing::debug!(
                                job_id = job_id,
                                "Error path: flushing TEXT_MESSAGE_END for interrupted stream"
                            );
                            let ag_event = AgUiEvent::text_message_end(message_id);
                            let event_id = Self::encode_event_with_logging(&encoder, &ag_event);
                            event_store.store_event(&run_id, event_id, ag_event.clone()).await;
                            yield (event_id, ag_event);
                        }

                        // Emit STEP_FINISHED for current step before error
                        if prev_position.is_some() {
                            let finished_event = {
                                let mut adapter_lock = adapter.lock().await;
                                adapter_lock.task_finished(None)
                            };
                            if let Some(event) = finished_event {
                                let event_id = Self::encode_event_with_logging(&encoder, &event);
                                event_store.store_event(&run_id, event_id, event.clone()).await;
                                yield (event_id, event);
                            }
                        }

                        // Emit RUN_ERROR event (mutually exclusive with RUN_FINISHED)
                        // Set flag before break so post-loop code skips STEP_FINISHED/RUN_FINISHED
                        // NOTE: `stream!` macro's control flow analysis cannot see that `has_error`
                        // is read after the loop, so it warns "unused_assignments".
                        // The block `{}` is Rust syntax to apply `#[allow]` to a statement.
                        // TODO: Remove workaround when stream! macro improves
                        #[allow(unused_assignments)]
                        { has_error = true; }
                        let adapter_lock = adapter.lock().await;
                        let event = adapter_lock.workflow_error(
                            format!("{:?}", e),
                            "WORKFLOW_ERROR",
                            None,
                        );
                        drop(adapter_lock);

                        let event_id = Self::encode_event_with_logging(&encoder, &event);
                        event_store.store_event(&run_id, event_id, event.clone()).await;
                        yield (event_id, event);

                        // Update session state to Error
                        session_manager.set_session_state(&session_id, SessionState::Error).await;
                        break;
                    }
                }
            } // end of main event loop

            // Drop the sender so spawned tasks' channels close when they finish,
            // allowing the drain loop to terminate.
            nested_event_tx.take();

            // Drain remaining nested events with timeout
            // (spawned tasks may still be sending final STEP_FINISHED events)
            let drain_deadline = tokio::time::Instant::now()
                + tokio::time::Duration::from_secs(NESTED_DRAIN_TIMEOUT_SECS);
            while let Ok(Some(nested_event)) = tokio::time::timeout_at(drain_deadline, nested_event_rx.recv()).await {
                yield nested_event;
            }

            // Abort remaining nested tasks — channel drop already signals graceful
            // exit, but abort ensures cleanup even if a task is stuck waiting on
            // subscribe_result_stream_for_job (up to NESTED_WF_PROGRESS_TIMEOUT_MS).
            for handle in nested_task_handles.into_values() {
                handle.abort();
            }

            // Flush any remaining TEXT_MESSAGE_END for streams that didn't complete normally
            // (safety net for edge cases where StreamingJobCompleted wasn't received)
            for (job_id, message_id) in active_message_ids.drain() {
                if !text_message_started.remove(&job_id) {
                    continue;
                }
                tracing::debug!(
                    job_id = job_id,
                    "Normal completion: flushing TEXT_MESSAGE_END for unclosed stream"
                );
                let ag_event = AgUiEvent::text_message_end(message_id);
                let event_id = Self::encode_event_with_logging(&encoder, &ag_event);
                event_store.store_event(&run_id, event_id, ag_event.clone()).await;
                yield (event_id, ag_event);
            }

            // Emit STEP_FINISHED for final step if exists
            if prev_position.is_some() && !has_error {
                let finished_event = {
                    let mut adapter_lock = adapter.lock().await;
                    adapter_lock.task_finished(None)
                };
                if let Some(event) = finished_event {
                    let event_id = Self::encode_event_with_logging(&encoder, &event);
                    event_store.store_event(&run_id, event_id, event.clone()).await;
                    yield (event_id, event);
                }
            }

            // Emit RUN_FINISHED only if no error occurred (AG-UI spec: RUN_FINISHED and RUN_ERROR are mutually exclusive)
            if !has_error {
                let event = {
                    let adapter_lock = adapter.lock().await;
                    adapter_lock.workflow_completed(None)
                };
                let event_id = Self::encode_event_with_logging(&encoder, &event);
                event_store.store_event(&run_id, event_id, event.clone()).await;
                yield (event_id, event);

                // Update session state to Completed
                session_manager.set_session_state(&session_id, SessionState::Completed).await;
            }

            // Cleanup: remove executor from registry after stream completes
            {
                let mut registry = executor_registry.write().await;
                registry.remove(&run_id_for_cleanup);
                tracing::debug!("Removed executor from registry for run_id: {}", run_id_for_cleanup);
            }
        }
    }
}

impl<SM, ES> Clone for AgUiHandler<SM, ES>
where
    SM: SessionManager + 'static,
    ES: EventStore + 'static,
{
    fn clone(&self) -> Self {
        Self {
            app_wrapper_module: self.app_wrapper_module.clone(),
            app_module: self.app_module.clone(),
            session_manager: self.session_manager.clone(),
            event_store: self.event_store.clone(),
            config: self.config.clone(),
            encoder: self.encoder.clone(),
            executor_registry: self.executor_registry.clone(),
        }
    }
}

/// Convert a serde_json::Value to a JSON string suitable for fn_arguments.
/// - Value::String is unwrapped directly (avoids double-quoting). Callers are expected
///   to send JSON-encoded strings (e.g. `"{\"key\": \"value\"}"`) in String variants,
///   or JSON objects/arrays directly.
/// - Other variants are serialized to JSON.
fn value_to_json_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_else(|e| {
            tracing::warn!("Failed to serialize serde_json::Value to string: {}", e);
            String::new()
        }),
    }
}

#[cfg(test)]
mod tests {
    // Note: Full integration tests require AppWrapperModule setup
    // which is complex. These tests focus on handler construction.

    #[test]
    fn test_handler_clone() {
        // Handler should be clonable for use in axum state
        // This is a compile-time check - actual instantiation requires AppWrapperModule
    }
}
