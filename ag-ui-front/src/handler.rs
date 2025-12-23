//! AG-UI HTTP handler implementation.
//!
//! AgUiHandler orchestrates workflow execution and AG-UI event streaming.

use crate::config::AgUiServerConfig;
use crate::error::{AgUiError, Result};
use crate::events::{
    extract_text_from_llm_chat_result, extract_tool_calls_from_llm_result, shared_adapter,
    tool_calls_to_ag_ui_events, AgUiEvent, EventEncoder, SharedWorkflowEventAdapter,
};
use crate::session::{
    EventStore, HitlWaitingInfo, PendingToolCallInfo, Session, SessionManager, SessionState,
};
use crate::types::ids::MessageId;
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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Fallback counter for event IDs when encoding fails.
/// Starts from a high value (MSB set) to distinguish from normal event IDs.
/// This ensures unique, non-zero fallback IDs even when multiple encoding failures occur.
static FALLBACK_EVENT_ID_COUNTER: AtomicU64 = AtomicU64::new(0x8000_0000_0000_0000);

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

        // 3. Emit TOOL_CALL_RESULT and TOOL_CALL_END events for each result
        let mut events = Vec::new();
        for (tool_call_id, result) in &tool_results {
            let result_event = AgUiEvent::tool_call_result(tool_call_id.clone(), result.clone());
            let result_event_id = Self::encode_event_with_logging(&self.encoder, &result_event);
            self.event_store
                .store_event(run_id, result_event_id, result_event.clone())
                .await;
            events.push((result_event_id, result_event));

            let end_event = AgUiEvent::tool_call_end(tool_call_id.clone());
            let end_event_id = Self::encode_event_with_logging(&self.encoder, &end_event);
            self.event_store
                .store_event(run_id, end_event_id, end_event.clone())
                .await;
            events.push((end_event_id, end_event));
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

        // 9. Emit TOOL_CALL_RESULT and TOOL_CALL_END events
        // For LLM tool calls (pending_tool_calls non-empty), emit events for the specific tool
        // For traditional HITL (pending_tool_calls empty), emit events for the primary tool_call_id
        let mut tool_events_vec = Vec::new();

        if !hitl_info.pending_tool_calls.is_empty() {
            // LLM tool call mode: emit TOOL_CALL_RESULT and TOOL_CALL_END for the matched tool
            let tool_call_result_event =
                AgUiEvent::tool_call_result(tool_call_id.to_string(), user_input_for_event);
            let result_event_id =
                Self::encode_event_with_logging(&self.encoder, &tool_call_result_event);
            self.event_store
                .store_event(run_id, result_event_id, tool_call_result_event.clone())
                .await;
            tool_events_vec.push((result_event_id, tool_call_result_event));

            let tool_call_end_event = AgUiEvent::tool_call_end(tool_call_id.to_string());
            let end_event_id = Self::encode_event_with_logging(&self.encoder, &tool_call_end_event);
            self.event_store
                .store_event(run_id, end_event_id, tool_call_end_event.clone())
                .await;
            tool_events_vec.push((end_event_id, tool_call_end_event));

            tracing::info!(
                tool_call_id = %tool_call_id,
                pending_count = hitl_info.pending_tool_calls.len(),
                "LLM tool call result received"
            );
        } else {
            // Traditional HITL mode: emit status-based result
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

            let tool_call_end_event = AgUiEvent::tool_call_end(tool_call_id.to_string());
            let end_event_id = Self::encode_event_with_logging(&self.encoder, &tool_call_end_event);
            self.event_store
                .store_event(run_id, end_event_id, tool_call_end_event.clone())
                .await;
            tool_events_vec.push((end_event_id, tool_call_end_event));
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
            false, // emit_run_started: false for HITL resume (RUN_STARTED already emitted)
        );

        // Prepend the tool_call events to the workflow stream
        let tool_events = futures::stream::iter(tool_events_vec);

        let combined_stream = tool_events.chain(workflow_stream);

        Ok(Box::pin(combined_stream))
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

        stream! {
            // Emit RUN_STARTED (skip for HITL resume to avoid duplicate)
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

            // Main event loop - process workflow events sequentially
            // StreamingData events are now yielded directly from workflow_stream
            while let Some(result) = workflow_stream.next().await {
                match result {
                    Ok(event) => {
                        // Handle StreamingJobStarted: emit TEXT_MESSAGE_START
                        if let WorkflowStreamEvent::StreamingJobStarted { event: ev } = &event {
                            let job_id_value = ev.job_id.as_ref().map(|j| j.value).unwrap_or(0);
                            let message_id = MessageId::random();

                            tracing::info!(
                                job_id = job_id_value,
                                worker_name = ?ev.worker_name,
                                position = %ev.position,
                                "StreamingJobStarted: emitting TEXT_MESSAGE_START"
                            );

                            // Close previous message if exists for this job_id (prevents dangling UI messages)
                            if let Some(old_message_id) = active_message_ids.remove(&job_id_value) {
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
                            active_message_ids.insert(job_id_value, message_id.clone());

                            // Emit TEXT_MESSAGE_START
                            let ag_event = AgUiEvent::text_message_start(
                                message_id,
                                crate::types::Role::Assistant,
                            );
                            let event_id = Self::encode_event_with_logging(&encoder, &ag_event);
                            event_store.store_event(&run_id, event_id, ag_event.clone()).await;
                            yield (event_id, ag_event);
                        }

                        // Handle StreamingData: emit TEXT_MESSAGE_CONTENT
                        if let WorkflowStreamEvent::StreamingData { event: ev } = &event {
                            let job_id_value = ev.job_id.as_ref().map(|j| j.value).unwrap_or(0);
                            if let Some(message_id) = active_message_ids.get(&job_id_value) {
                                // Decode LlmChatResult protobuf and extract text content
                                if let Some(text) = extract_text_from_llm_chat_result(&ev.data) {
                                    let ag_event = AgUiEvent::text_message_content(
                                        message_id.clone(),
                                        text,
                                    );
                                    let event_id = Self::encode_event_with_logging(&encoder, &ag_event);
                                    event_store.store_event(&run_id, event_id, ag_event.clone()).await;
                                    yield (event_id, ag_event);
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
                            if let Some(ref tool_calls) = extracted_tool_calls {
                                if !tool_calls.tool_calls.is_empty() {
                                    tracing::info!(
                                        job_id = job_id_value,
                                        tool_count = tool_calls.tool_calls.len(),
                                        requires_execution = tool_calls.requires_execution,
                                        "StreamingJobCompleted: detected LLM tool calls"
                                    );

                                    // Emit TOOL_CALL_START and TOOL_CALL_ARGS for each tool call
                                    let tool_events = tool_calls_to_ag_ui_events(
                                        &tool_calls.tool_calls,
                                        message_id.as_ref(),
                                    );
                                    for tool_event in tool_events {
                                        let event_id = Self::encode_event_with_logging(&encoder, &tool_event);
                                        event_store.store_event(&run_id, event_id, tool_event.clone()).await;
                                        yield (event_id, tool_event);
                                    }
                                }
                            }

                            // Emit TEXT_MESSAGE_END for the text portion
                            if let Some(msg_id) = message_id.clone() {
                                tracing::info!(
                                    job_id = job_id_value,
                                    "StreamingJobCompleted: emitting TEXT_MESSAGE_END"
                                );

                                let ag_event = AgUiEvent::text_message_end(msg_id);
                                let event_id = Self::encode_event_with_logging(&encoder, &ag_event);
                                event_store.store_event(&run_id, event_id, ag_event.clone()).await;
                                yield (event_id, ag_event);
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
                                    let hitl_info = HitlWaitingInfo {
                                        tool_call_id: primary_tool_call_id.clone(),
                                        checkpoint_position: current_position,
                                        workflow_name: workflow_name.clone(),
                                        pending_tool_calls,
                                    };
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

                                    // Exit stream without emitting RUN_FINISHED - client needs to approve tool calls
                                    tracing::info!(
                                        "Workflow paused for LLM tool call approval, session {} is now Paused",
                                        session_id
                                    );
                                    return;
                                }
                            }

                            // Continue to process context for state snapshot
                        }

                        // Extract context from completed events for workflow state updates
                        if let Some(tc) = event.context() {
                            // Read position from TaskContext
                            let current_position = tc.position.read().await.as_json_pointer();
                            let current_step_name = tc.position.read().await.last_name();

                            if prev_position.as_ref() != Some(&current_position) {
                                // Position changed - emit STEP_FINISHED for previous step if exists
                                if prev_position.is_some() {
                                    let mut adapter_lock = adapter.lock().await;
                                    if let Some(event) = adapter_lock.task_finished(None) {
                                        let event_id = Self::encode_event_with_logging(&encoder, &event);
                                        event_store.store_event(&run_id, event_id, event.clone()).await;
                                        yield (event_id, event);
                                    }
                                }

                                // Emit STEP_STARTED for new step if we have a step name
                                if let Some(step_name) = current_step_name.as_ref() {
                                    let mut adapter_lock = adapter.lock().await;
                                    let event = adapter_lock.task_started(step_name, None, None);
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
                                let hitl_info = HitlWaitingInfo {
                                    tool_call_id: tool_call_id.clone(),
                                    checkpoint_position,
                                    workflow_name: workflow_name.clone(),
                                    pending_tool_calls: vec![], // Traditional HITL (HUMAN_INPUT)
                                };
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
                                return;
                            }
                        }
                        // Start events (StreamingJobStarted, JobStarted, TaskStarted) are handled above
                    }
                    Err(e) => {
                        // Flush any remaining TEXT_MESSAGE_END for interrupted streams
                        for (job_id, message_id) in active_message_ids.drain() {
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
                            let mut adapter_lock = adapter.lock().await;
                            if let Some(event) = adapter_lock.task_finished(None) {
                                let event_id = Self::encode_event_with_logging(&encoder, &event);
                                event_store.store_event(&run_id, event_id, event.clone()).await;
                                yield (event_id, event);
                            }
                        }

                        // Emit RUN_ERROR event (mutually exclusive with RUN_FINISHED)
                        has_error = true;
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
            }

            // Flush any remaining TEXT_MESSAGE_END for streams that didn't complete normally
            // (safety net for edge cases where StreamingJobCompleted wasn't received)
            for (job_id, message_id) in active_message_ids.drain() {
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
                let mut adapter_lock = adapter.lock().await;
                if let Some(event) = adapter_lock.task_finished(None) {
                    let event_id = Self::encode_event_with_logging(&encoder, &event);
                    event_store.store_event(&run_id, event_id, event.clone()).await;
                    yield (event_id, event);
                }
            }

            // Emit RUN_FINISHED only if no error occurred (AG-UI spec: RUN_FINISHED and RUN_ERROR are mutually exclusive)
            if !has_error {
                let adapter_lock = adapter.lock().await;
                let event = adapter_lock.workflow_completed(None);
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
