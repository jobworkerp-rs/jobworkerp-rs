//! AG-UI HTTP handler implementation.
//!
//! AgUiHandler orchestrates workflow execution and AG-UI event streaming.

use crate::config::AgUiServerConfig;
use crate::error::{AgUiError, Result};
use crate::events::{shared_adapter, AgUiEvent, EventEncoder, SharedWorkflowEventAdapter};
use crate::session::{EventStore, HitlWaitingInfo, Session, SessionManager, SessionState};
use crate::types::{RunAgentInput, RunId, ThreadId, WorkflowState, WorkflowStatus};
use app::module::AppModule;
use app_wrapper::modules::AppWrapperModule;
use app_wrapper::workflow::definition::workflow::WorkflowSchema;
use app_wrapper::workflow::execute::checkpoint::CheckPointContext;
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

    /// Resume a paused HITL workflow with user input.
    ///
    /// This method:
    /// 1. Validates session is in Paused state
    /// 2. Validates tool_call_id matches
    /// 3. Retrieves checkpoint from storage
    /// 4. Injects user_input into checkpoint
    /// 5. Restarts workflow execution from checkpoint
    /// 6. Returns event stream for resumed execution
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

        if hitl_info.tool_call_id != tool_call_id {
            return Err(AgUiError::InvalidToolCallId {
                expected: hitl_info.tool_call_id.clone(),
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
        let mut ctx_vars = (*checkpoint.workflow.context_variables).clone();
        ctx_vars.insert("user_input".to_string(), user_input.clone());

        let modified_checkpoint = CheckPointContext {
            task: app_wrapper::workflow::execute::checkpoint::TaskCheckPointContext {
                output: Arc::new(user_input),
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
        let tool_call_result = AgUiEvent::tool_call_result(
            tool_call_id.to_string(),
            serde_json::json!({"status": "resumed"}),
        );
        let result_event_id = Self::encode_event_with_logging(&self.encoder, &tool_call_result);
        self.event_store
            .store_event(run_id, result_event_id, tool_call_result.clone())
            .await;

        let tool_call_end = AgUiEvent::tool_call_end(tool_call_id.to_string());
        let end_event_id = Self::encode_event_with_logging(&self.encoder, &tool_call_end);
        self.event_store
            .store_event(run_id, end_event_id, tool_call_end.clone())
            .await;

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
        let tool_events = futures::stream::iter(vec![
            (result_event_id, tool_call_result),
            (end_event_id, tool_call_end),
        ]);

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

            // Execute workflow and stream context updates
            let cx = Arc::new(opentelemetry::Context::current());
            let workflow_stream = executor.execute_workflow(cx);

            // Pin the stream for iteration
            tokio::pin!(workflow_stream);

            while let Some(result) = workflow_stream.next().await {
                match result {
                    Ok(context) => {
                        // Detect position changes and emit STEP_STARTED/STEP_FINISHED events
                        let current_position = context.position.as_json_pointer();
                        let current_step_name = context.position.last_name();

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

                            prev_position = Some(current_position);
                        }

                        // Get context variables from tokio::sync::Mutex
                        let context_vars = {
                            let guard = context.context_variables.lock().await;
                            guard
                                .iter()
                                .map(|(k, v)| (k.clone(), v.clone()))
                                .collect::<HashMap<String, serde_json::Value>>()
                        };

                        // Check for HITL Waiting status
                        let is_waiting = context.status == app_wrapper::workflow::execute::context::WorkflowStatus::Waiting;

                        // Convert context to state snapshot event
                        let state = WorkflowState::new(&context.name)
                            .with_status(match context.status {
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

                        let event_id = Self::encode_event_with_logging(&encoder, &event);
                        event_store.store_event(&run_id, event_id, event.clone()).await;
                        yield (event_id, event);

                        // HITL: If workflow is waiting for user input, emit TOOL_CALL events
                        if is_waiting {
                            tracing::info!("Workflow waiting for user input, emitting TOOL_CALL events");

                            // Generate tool_call_id from run_id for consistency
                            let tool_call_id = format!("wait_{}", run_id);

                            // Get checkpoint position for later resume
                            let checkpoint_position = context.position.as_json_pointer();

                            // Emit TOOL_CALL_START event with canonical HUMAN_INPUT tool name
                            let tool_call_start = AgUiEvent::tool_call_start(
                                tool_call_id.clone(),
                                "HUMAN_INPUT".to_string(),
                            );
                            let start_event_id = Self::encode_event_with_logging(&encoder, &tool_call_start);
                            event_store.store_event(&run_id, start_event_id, tool_call_start.clone()).await;
                            yield (start_event_id, tool_call_start);

                            // Emit TOOL_CALL_ARGS event with current output
                            let args = context.output
                                .as_ref()
                                .map(|o| serde_json::to_string(o.as_ref()).unwrap_or_default())
                                .unwrap_or_else(|| "{}".to_string());
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

                            // Exit stream without emitting RUN_FINISHED
                            tracing::info!("Workflow paused for HITL, session {} is now Paused", session_id);
                            return;
                        }
                    }
                    Err(e) => {
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
