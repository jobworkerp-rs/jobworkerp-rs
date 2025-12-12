//! Async AG-UI HTTP handler implementation (decoupled architecture).
//!
//! AsyncAgUiHandler orchestrates workflow execution via job queue and AG-UI event streaming.
//! Unlike AgUiHandler which executes workflows directly, this handler uses FunctionApp
//! to enqueue jobs for distributed execution across worker processes.

use crate::config::AgUiServerConfig;
use crate::error::{AgUiError, Result};
use crate::events::{shared_adapter, AgUiEvent, EventEncoder, SharedWorkflowEventAdapter};
use crate::session::{EventStore, HitlWaitingInfo, Session, SessionManager, SessionState};
use crate::types::{RunAgentInput, RunId, ThreadId, WorkflowState, WorkflowStatus};
use app::app::function::FunctionApp;
use app::module::AppModule;
use app_wrapper::workflow::definition::workflow::WorkflowSchema;
use async_stream::stream;
use futures::{Stream, StreamExt};
use jobworkerp_runner::jobworkerp::runner::workflow_result::WorkflowStatus as ProtoWorkflowStatus;
use jobworkerp_runner::jobworkerp::runner::{InlineWorkflowArgs, WorkflowResult};
use prost::Message;
use proto::jobworkerp::data::JobId;
use proto::jobworkerp::function::data::FunctionResult;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Fallback counter for event IDs when encoding fails.
static FALLBACK_EVENT_ID_COUNTER: AtomicU64 = AtomicU64::new(0x8000_0000_0000_0000);

/// Registry of active jobs for cancellation support.
/// Maps run_id -> JobId
type JobRegistry = Arc<RwLock<HashMap<String, JobId>>>;

/// Async handler for AG-UI HTTP requests (decoupled architecture).
///
/// Uses FunctionApp to enqueue workflows to job queue instead of direct execution.
/// This enables:
/// - Fault tolerance (jobs persist in queue)
/// - Scalability (multiple workers can process jobs)
/// - Checkpoint-based recovery
pub struct AsyncAgUiHandler<SM, ES>
where
    SM: SessionManager + 'static,
    ES: EventStore + 'static,
{
    app_module: Arc<AppModule>,
    session_manager: Arc<SM>,
    event_store: Arc<ES>,
    config: AgUiServerConfig,
    encoder: Arc<EventEncoder>,
    /// run_id -> JobId mapping for cancellation
    /// Lifecycle:
    ///   - Registered: immediately after job enqueue
    ///   - Removed: on stream completion (success/error/HITL pause/cancel)
    job_registry: JobRegistry,
}

impl<SM, ES> AsyncAgUiHandler<SM, ES>
where
    SM: SessionManager + Clone + Send + Sync + 'static,
    ES: EventStore + Clone + Send + Sync + 'static,
{
    /// Create a new AsyncAgUiHandler.
    pub fn new(
        app_module: Arc<AppModule>,
        session_manager: Arc<SM>,
        event_store: Arc<ES>,
        config: AgUiServerConfig,
    ) -> Self {
        Self {
            app_module,
            session_manager,
            event_store,
            config,
            encoder: Arc::new(EventEncoder::new()),
            job_registry: Arc::new(RwLock::new(HashMap::new())),
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

    /// Start a new workflow run via job queue.
    ///
    /// Creates a session, enqueues the workflow job, and returns an event stream.
    pub async fn run_workflow(
        self: Arc<Self>,
        input: RunAgentInput,
    ) -> Result<(
        Session,
        Pin<Box<dyn Stream<Item = (u64, AgUiEvent)> + Send + 'static>>,
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
        let workflow_name = workflow.document.name.to_string();

        // Build InlineWorkflowArgs
        let args =
            self.build_inline_workflow_args(&input, &workflow, run_id.as_str(), workflow_context)?;
        let args_bytes = args.encode_to_vec();

        // Create adapter for event conversion
        let adapter = shared_adapter(run_id.clone(), thread_id.clone());

        // Capture all necessary data for the stream closure
        let timeout_sec = self.config.timeout_sec;
        let run_id_str = run_id.to_string();
        let session_id = session.session_id.clone();
        let handler = self.clone();

        // Create the stream that processes results lazily
        let stream = stream! {
            // Enqueue job via FunctionApp inside the stream
            let metadata = Arc::new(HashMap::new());
            let result_stream = handler.app_module.function_app.handle_runner_for_front(
                metadata,
                "INLINE_WORKFLOW",
                None, // runner_settings
                None, // worker_options
                serde_json::json!({ "args": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &args_bytes) }),
                Some(run_id_str.clone()), // uniq_key
                timeout_sec,
                true, // streaming
            );

            // Process the stream
            for await event in Self::create_event_stream_inner(
                result_stream,
                adapter,
                run_id_str,
                session_id,
                workflow_name,
                true, // emit_run_started
                handler.event_store.clone(),
                handler.encoder.clone(),
                handler.session_manager.clone(),
                handler.job_registry.clone(),
            ) {
                yield event;
            }
        };

        Ok((session, Box::pin(stream)))
    }

    /// Resume an existing workflow run from Last-Event-ID.
    pub async fn resume_stream(
        self: Arc<Self>,
        run_id: &str,
        last_event_id: Option<u64>,
    ) -> Result<Pin<Box<dyn Stream<Item = (u64, AgUiEvent)> + Send + 'static>>> {
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

        // Check if workflow is still running
        let is_running = {
            let registry = self.job_registry.read().await;
            registry.contains_key(run_id)
        };

        if let Some(last_id) = last_event_id {
            self.session_manager
                .update_last_event_id(&session.session_id, last_id)
                .await;
        }

        let run_id_for_poll = run_id.to_string();
        let event_store = self.event_store.clone();
        let session_state = session.state;

        let stream = stream! {
            let mut last_yielded_id: Option<u64> = last_event_id;

            // Yield stored events
            for (id, event) in stored_events {
                last_yielded_id = Some(id);
                yield (id, event);
            }

            // Poll for new events if workflow is still running
            if is_running
                && session_state != SessionState::Completed
                && session_state != SessionState::Cancelled
                && session_state != SessionState::Error
            {
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

                    let new_events = if let Some(last_id) = last_yielded_id {
                        event_store.get_events_since(&run_id_for_poll, last_id).await
                    } else {
                        event_store.get_all_events(&run_id_for_poll).await
                    };

                    if new_events.is_empty() {
                        let all_events = event_store.get_all_events(&run_id_for_poll).await;
                        let workflow_completed = all_events.iter().any(|(_, event)| {
                            matches!(
                                event,
                                AgUiEvent::RunFinished { .. } | AgUiEvent::RunError { .. }
                            )
                        });
                        if workflow_completed {
                            break;
                        }
                        continue;
                    }

                    for (id, event) in new_events {
                        last_yielded_id = Some(id);
                        let is_terminal = matches!(
                            event,
                            AgUiEvent::RunFinished { .. } | AgUiEvent::RunError { .. }
                        );
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
    pub async fn cancel_workflow(&self, run_id: &str) -> Result<()> {
        let run_id_typed = RunId::new(run_id);

        let session = self
            .session_manager
            .get_session_by_run_id(&run_id_typed)
            .await
            .ok_or_else(|| AgUiError::SessionNotFound(run_id.to_string()))?;

        // Get JobId from registry and call delete_job
        let job_id_opt = {
            let registry = self.job_registry.read().await;
            registry.get(run_id).cloned()
        };

        if let Some(job_id) = job_id_opt {
            tracing::info!(
                "Cancelling job for run_id: {}, job_id: {}",
                run_id,
                job_id.value
            );
            match self.app_module.job_app.delete_job(&job_id).await {
                Ok(cancelled) => {
                    tracing::info!("Job cancellation result: {}", cancelled);
                }
                Err(e) => {
                    tracing::warn!("Failed to cancel job: {:?}", e);
                }
            }
        } else {
            tracing::warn!(
                "No active job found for run_id: {}, session may have already completed",
                run_id
            );
        }

        // Update session state
        self.session_manager
            .set_session_state(&session.session_id, SessionState::Cancelled)
            .await;

        // Store RUN_ERROR event
        let event = AgUiEvent::RunError {
            run_id: run_id.to_string(),
            message: "Workflow cancelled by user".to_string(),
            code: Some("CANCELLED".to_string()),
            details: None,
            timestamp: Some(AgUiEvent::now_timestamp()),
        };
        let event_id = Self::encode_event_with_logging(&self.encoder, &event);
        self.event_store.store_event(run_id, event_id, event).await;

        // Remove from registry
        {
            let mut registry = self.job_registry.write().await;
            registry.remove(run_id);
        }

        Ok(())
    }

    /// Get current workflow state.
    pub async fn get_state(&self, run_id: &str) -> Result<WorkflowState> {
        let run_id_typed = RunId::new(run_id);

        let _session = self
            .session_manager
            .get_session_by_run_id(&run_id_typed)
            .await
            .ok_or_else(|| AgUiError::SessionNotFound(run_id.to_string()))?;

        let events = self.event_store.get_all_events(run_id).await;

        for (_id, event) in events.into_iter().rev() {
            if let AgUiEvent::StateSnapshot { snapshot, .. } = event {
                return Ok(snapshot);
            }
        }

        Ok(WorkflowState::new("unknown").with_status(WorkflowStatus::Pending))
    }

    /// Resume a paused HITL workflow with user input.
    pub async fn resume_workflow(
        self: Arc<Self>,
        run_id: &str,
        tool_call_id: &str,
        user_input: serde_json::Value,
        workflow_data: Option<&str>,
    ) -> Result<Pin<Box<dyn Stream<Item = (u64, AgUiEvent)> + Send + 'static>>> {
        let run_id_typed = RunId::new(run_id);

        // Validate session state
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

        // Build resume args with checkpoint
        let args = self.build_resume_workflow_args(hitl_info, user_input.clone(), workflow_data)?;
        let args_bytes = args.encode_to_vec();

        // Create adapter
        let adapter = shared_adapter(run_id_typed.clone(), session.thread_id.clone());

        // Atomically clear HITL info and set session state to Active
        if !self
            .session_manager
            .resume_from_paused(&session.session_id, SessionState::Active)
            .await
        {
            return Err(AgUiError::Internal(anyhow::anyhow!(
                "Failed to update session state from Paused to Active"
            )));
        }

        // Emit TOOL_CALL_RESULT and TOOL_CALL_END events
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

        // Capture all necessary data for the stream closure
        let timeout_sec = self.config.timeout_sec;
        let run_id_str = run_id.to_string();
        let session_id = session.session_id.clone();
        let workflow_name = hitl_info.workflow_name.clone();
        let handler = self.clone();

        let stream = stream! {
            // Yield tool call events first
            yield (result_event_id, tool_call_result);
            yield (end_event_id, tool_call_end);

            // Enqueue resume job
            let metadata = Arc::new(HashMap::new());
            let result_stream = handler.app_module.function_app.handle_runner_for_front(
                metadata,
                "INLINE_WORKFLOW",
                None,
                None,
                serde_json::json!({ "args": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &args_bytes) }),
                Some(run_id_str.clone()),
                timeout_sec,
                true,
            );

            // Process the stream
            for await event in Self::create_event_stream_inner(
                result_stream,
                adapter,
                run_id_str,
                session_id,
                workflow_name,
                false, // emit_run_started: false for HITL resume
                handler.event_store.clone(),
                handler.encoder.clone(),
                handler.session_manager.clone(),
                handler.job_registry.clone(),
            ) {
                yield event;
            }
        };

        Ok(Box::pin(stream))
    }

    /// Parse workflow schema from input context.
    /// Returns (workflow_schema, workflow_context) tuple.
    async fn parse_workflow_from_input(
        &self,
        input: &RunAgentInput,
    ) -> Result<(Arc<WorkflowSchema>, Option<serde_json::Value>)> {
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
            "No workflow definition in context".to_string(),
        ))
    }

    /// Build InlineWorkflowArgs for initial execution.
    fn build_inline_workflow_args(
        &self,
        input: &RunAgentInput,
        workflow: &WorkflowSchema,
        run_id: &str,
        workflow_context: Option<serde_json::Value>,
    ) -> Result<InlineWorkflowArgs> {
        use jobworkerp_runner::jobworkerp::runner::inline_workflow_args::WorkflowSource;

        let workflow_json = serde_json::to_string(workflow)
            .map_err(|e| AgUiError::InvalidInput(format!("Failed to serialize workflow: {}", e)))?;

        let input_json = serde_json::json!({
            "messages": input.messages,
            "tools": input.tools,
        });

        // Parse workflow_context: can be a JSON string or an object
        // Convert to JSON string for protobuf field
        let workflow_context_str = match workflow_context {
            Some(serde_json::Value::String(s)) => {
                // Already a string - use as-is (may be a JSON string that needs to stay as-is)
                // But first try to parse it to validate it's valid JSON
                match serde_json::from_str::<serde_json::Value>(&s) {
                    Ok(_) => Some(s), // Valid JSON string, use as-is
                    Err(e) => {
                        tracing::warn!(
                            "workflowContext string is not valid JSON: {}, wrapping in object",
                            e
                        );
                        Some(serde_json::json!({ "raw": s }).to_string())
                    }
                }
            }
            Some(obj) => {
                // Convert object/array/etc. to JSON string
                Some(serde_json::to_string(&obj).unwrap_or_else(|_| "{}".to_string()))
            }
            None => None,
        };

        Ok(InlineWorkflowArgs {
            workflow_source: Some(WorkflowSource::WorkflowData(workflow_json)),
            input: serde_json::to_string(&input_json).unwrap_or_else(|_| "{}".to_string()),
            workflow_context: workflow_context_str,
            execution_id: Some(run_id.to_string()),
            from_checkpoint: None,
        })
    }

    /// Build InlineWorkflowArgs for HITL resume.
    fn build_resume_workflow_args(
        &self,
        hitl_info: &HitlWaitingInfo,
        user_input: serde_json::Value,
        workflow_data: Option<&str>,
    ) -> Result<InlineWorkflowArgs> {
        use jobworkerp_runner::jobworkerp::runner::inline_workflow_args::{
            checkpoint::{CheckPointContext, TaskCheckPointContext, WorkflowCheckPointContext},
            Checkpoint, WorkflowSource,
        };

        // For inline workflow, workflow_data must be provided by client
        // For workflow_definition, this will be None and handled by the runner
        let workflow_source =
            workflow_data.map(|data| WorkflowSource::WorkflowData(data.to_string()));

        let checkpoint = Checkpoint {
            position: hitl_info.checkpoint_position.clone(),
            data: Some(CheckPointContext {
                workflow: Some(WorkflowCheckPointContext {
                    name: hitl_info.workflow_name.clone(),
                    input: "{}".to_string(),
                    context_variables: serde_json::json!({
                        "user_input": user_input
                    })
                    .to_string(),
                }),
                task: Some(TaskCheckPointContext {
                    input: "{}".to_string(),
                    output: serde_json::to_string(&user_input).unwrap_or_else(|_| "{}".to_string()),
                    context_variables: "{}".to_string(),
                    flow_directive: "continue".to_string(),
                }),
            }),
        };

        Ok(InlineWorkflowArgs {
            workflow_source,
            input: "{}".to_string(),
            workflow_context: None,
            execution_id: Some(hitl_info.workflow_name.clone()), // Use workflow_name as execution_id for checkpoint lookup
            from_checkpoint: Some(checkpoint),
        })
    }

    /// Encode event with error logging.
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

    /// Create event stream from FunctionResult stream (inner implementation).
    ///
    /// Converts FunctionResult.output (WorkflowResult JSON) to AG-UI events.
    #[allow(clippy::too_many_arguments)]
    fn create_event_stream_inner<'a>(
        result_stream: Pin<Box<dyn Stream<Item = anyhow::Result<FunctionResult>> + Send + 'a>>,
        adapter: SharedWorkflowEventAdapter,
        run_id: String,
        session_id: String,
        workflow_name: String,
        emit_run_started: bool,
        event_store: Arc<ES>,
        encoder: Arc<EventEncoder>,
        session_manager: Arc<SM>,
        job_registry: JobRegistry,
    ) -> impl Stream<Item = (u64, AgUiEvent)> + Send + 'a {
        stream! {
            // Emit RUN_STARTED
            if emit_run_started {
                let adapter_lock = adapter.lock().await;
                let event = adapter_lock.workflow_started("workflow", None);
                let event_id = Self::encode_event_with_logging(&encoder, &event);
                event_store.store_event(&run_id, event_id, event.clone()).await;
                yield (event_id, event);
            }

            let mut has_error = false;
            let mut prev_position: Option<String> = None;

            tokio::pin!(result_stream);

            while let Some(result) = result_stream.next().await {
                match result {
                    Ok(function_result) => {
                        // Empty output with last_info indicates stream end
                        if function_result.output.is_empty() {
                            if function_result.last_info.is_some() {
                                // Stream completed normally
                                if prev_position.is_some() {
                                    let mut adapter_lock = adapter.lock().await;
                                    if let Some(event) = adapter_lock.task_finished(None) {
                                        let event_id =
                                            Self::encode_event_with_logging(&encoder, &event);
                                        event_store
                                            .store_event(&run_id, event_id, event.clone())
                                            .await;
                                        yield (event_id, event);
                                    }
                                }
                                break;
                            }
                            continue;
                        }

                        // Parse WorkflowResult from JSON
                        let workflow_result: WorkflowResult =
                            match serde_json::from_str(&function_result.output) {
                                Ok(wr) => wr,
                                Err(e) => {
                                    tracing::warn!("Failed to parse WorkflowResult JSON: {}", e);
                                    continue;
                                }
                            };

                        // Detect position changes for STEP_STARTED/STEP_FINISHED
                        if prev_position.as_ref() != Some(&workflow_result.position) {
                            if prev_position.is_some() {
                                let mut adapter_lock = adapter.lock().await;
                                if let Some(event) = adapter_lock.task_finished(None) {
                                    let event_id = Self::encode_event_with_logging(&encoder, &event);
                                    event_store
                                        .store_event(&run_id, event_id, event.clone())
                                        .await;
                                    yield (event_id, event);
                                }
                            }

                            let step_name = extract_step_name(&workflow_result.position);
                            if let Some(name) = step_name.as_ref() {
                                let mut adapter_lock = adapter.lock().await;
                                let event = adapter_lock.task_started(name, None, None);
                                let event_id = Self::encode_event_with_logging(&encoder, &event);
                                event_store
                                    .store_event(&run_id, event_id, event.clone())
                                    .await;
                                yield (event_id, event);
                            }

                            prev_position = Some(workflow_result.position.clone());
                        }

                        // Handle workflow status
                        match ProtoWorkflowStatus::try_from(workflow_result.status) {
                            Ok(ProtoWorkflowStatus::Waiting) => {
                                // HITL: emit TOOL_CALL events and pause
                                let tool_call_id = format!("wait_{}", run_id);

                                let tool_call_start = AgUiEvent::tool_call_start(
                                    tool_call_id.clone(),
                                    "HUMAN_INPUT".to_string(),
                                );
                                let start_event_id =
                                    Self::encode_event_with_logging(&encoder, &tool_call_start);
                                event_store
                                    .store_event(&run_id, start_event_id, tool_call_start.clone())
                                    .await;
                                yield (start_event_id, tool_call_start);

                                let args = workflow_result.output.clone();
                                let tool_call_args =
                                    AgUiEvent::tool_call_args(tool_call_id.clone(), args);
                                let args_event_id =
                                    Self::encode_event_with_logging(&encoder, &tool_call_args);
                                event_store
                                    .store_event(&run_id, args_event_id, tool_call_args.clone())
                                    .await;
                                yield (args_event_id, tool_call_args);

                                // Save HITL info
                                let hitl_info = HitlWaitingInfo {
                                    tool_call_id,
                                    checkpoint_position: workflow_result.position.clone(),
                                    workflow_name: workflow_name.clone(),
                                };
                                if !session_manager
                                    .set_paused_with_hitl_info(&session_id, hitl_info)
                                    .await
                                {
                                    tracing::error!("Failed to persist HITL waiting info");
                                    let adapter_lock = adapter.lock().await;
                                    let error_event = adapter_lock.workflow_error(
                                        "Failed to persist HITL waiting state".to_string(),
                                        "HITL_PERSISTENCE_ERROR",
                                        None,
                                    );
                                    drop(adapter_lock);
                                    let error_event_id =
                                        Self::encode_event_with_logging(&encoder, &error_event);
                                    event_store
                                        .store_event(&run_id, error_event_id, error_event.clone())
                                        .await;
                                    yield (error_event_id, error_event);
                                    session_manager
                                        .set_session_state(&session_id, SessionState::Error)
                                        .await;
                                    return;
                                }

                                // Remove from job registry
                                {
                                    let mut registry = job_registry.write().await;
                                    registry.remove(&run_id);
                                }

                                tracing::info!(
                                    "Workflow paused for HITL, session {} is now Paused",
                                    session_id
                                );
                                return;
                            }
                            Ok(ProtoWorkflowStatus::Completed) => {
                                let state = build_workflow_state(&workflow_result);
                                let adapter_lock = adapter.lock().await;
                                let event = adapter_lock.state_snapshot(state);
                                drop(adapter_lock);
                                let event_id = Self::encode_event_with_logging(&encoder, &event);
                                event_store
                                    .store_event(&run_id, event_id, event.clone())
                                    .await;
                                yield (event_id, event);
                            }
                            Ok(ProtoWorkflowStatus::Faulted) => {
                                has_error = true;
                                let adapter_lock = adapter.lock().await;
                                let event = adapter_lock.workflow_error(
                                    workflow_result.error_message.unwrap_or_default(),
                                    "WORKFLOW_ERROR",
                                    None,
                                );
                                drop(adapter_lock);
                                let event_id = Self::encode_event_with_logging(&encoder, &event);
                                event_store
                                    .store_event(&run_id, event_id, event.clone())
                                    .await;
                                yield (event_id, event);
                                session_manager
                                    .set_session_state(&session_id, SessionState::Error)
                                    .await;
                                break;
                            }
                            Ok(ProtoWorkflowStatus::Cancelled) => {
                                has_error = true;
                                let adapter_lock = adapter.lock().await;
                                let event = adapter_lock
                                    .workflow_error("Workflow cancelled".to_string(), "CANCELLED", None);
                                drop(adapter_lock);
                                let event_id = Self::encode_event_with_logging(&encoder, &event);
                                event_store
                                    .store_event(&run_id, event_id, event.clone())
                                    .await;
                                yield (event_id, event);
                                session_manager
                                    .set_session_state(&session_id, SessionState::Cancelled)
                                    .await;
                                break;
                            }
                            _ => {
                                // Running or other status - emit state snapshot
                                let state = build_workflow_state(&workflow_result);
                                let adapter_lock = adapter.lock().await;
                                let event = adapter_lock.state_snapshot(state);
                                drop(adapter_lock);
                                let event_id = Self::encode_event_with_logging(&encoder, &event);
                                event_store
                                    .store_event(&run_id, event_id, event.clone())
                                    .await;
                                yield (event_id, event);
                            }
                        }
                    }
                    Err(e) => {
                        // Error from FunctionApp
                        has_error = true;
                        if prev_position.is_some() {
                            let mut adapter_lock = adapter.lock().await;
                            if let Some(event) = adapter_lock.task_finished(None) {
                                let event_id = Self::encode_event_with_logging(&encoder, &event);
                                event_store
                                    .store_event(&run_id, event_id, event.clone())
                                    .await;
                                yield (event_id, event);
                            }
                        }

                        let adapter_lock = adapter.lock().await;
                        let event =
                            adapter_lock.workflow_error(format!("{:?}", e), "FUNCTION_ERROR", None);
                        drop(adapter_lock);
                        let event_id = Self::encode_event_with_logging(&encoder, &event);
                        event_store
                            .store_event(&run_id, event_id, event.clone())
                            .await;
                        yield (event_id, event);
                        session_manager
                            .set_session_state(&session_id, SessionState::Error)
                            .await;
                        break;
                    }
                }
            }

            // Emit STEP_FINISHED for final step
            if prev_position.is_some() && !has_error {
                let mut adapter_lock = adapter.lock().await;
                if let Some(event) = adapter_lock.task_finished(None) {
                    let event_id = Self::encode_event_with_logging(&encoder, &event);
                    event_store
                        .store_event(&run_id, event_id, event.clone())
                        .await;
                    yield (event_id, event);
                }
            }

            // Emit RUN_FINISHED if no error
            if !has_error {
                let adapter_lock = adapter.lock().await;
                let event = adapter_lock.workflow_completed(None);
                let event_id = Self::encode_event_with_logging(&encoder, &event);
                event_store
                    .store_event(&run_id, event_id, event.clone())
                    .await;
                yield (event_id, event);
                session_manager
                    .set_session_state(&session_id, SessionState::Completed)
                    .await;
            }

            // Cleanup: remove from job registry
            {
                let mut registry = job_registry.write().await;
                registry.remove(&run_id);
                tracing::debug!("Removed job from registry for run_id: {}", run_id);
            }
        }
    }
}

impl<SM, ES> Clone for AsyncAgUiHandler<SM, ES>
where
    SM: SessionManager + 'static,
    ES: EventStore + 'static,
{
    fn clone(&self) -> Self {
        Self {
            app_module: self.app_module.clone(),
            session_manager: self.session_manager.clone(),
            event_store: self.event_store.clone(),
            config: self.config.clone(),
            encoder: self.encoder.clone(),
            job_registry: self.job_registry.clone(),
        }
    }
}

/// Extract step name from JSON pointer position.
fn extract_step_name(position: &str) -> Option<String> {
    position.split('/').next_back().map(|s| s.to_string())
}

/// Build WorkflowState from WorkflowResult.
fn build_workflow_state(result: &WorkflowResult) -> WorkflowState {
    let status = match ProtoWorkflowStatus::try_from(result.status) {
        Ok(ProtoWorkflowStatus::Completed) => WorkflowStatus::Completed,
        Ok(ProtoWorkflowStatus::Faulted) => WorkflowStatus::Faulted,
        Ok(ProtoWorkflowStatus::Cancelled) => WorkflowStatus::Cancelled,
        Ok(ProtoWorkflowStatus::Pending) => WorkflowStatus::Pending,
        Ok(ProtoWorkflowStatus::Running) | Ok(ProtoWorkflowStatus::Waiting) => {
            WorkflowStatus::Running
        }
        Err(_) => WorkflowStatus::Pending,
    };

    WorkflowState::new(&result.id).with_status(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_step_name() {
        assert_eq!(extract_step_name("/do/task1"), Some("task1".to_string()));
        assert_eq!(
            extract_step_name("/do/nested/subtask"),
            Some("subtask".to_string())
        );
        assert_eq!(extract_step_name(""), Some("".to_string()));
    }

    #[test]
    fn test_build_workflow_state() {
        let result = WorkflowResult {
            id: "test-id".to_string(),
            output: "{}".to_string(),
            position: "/do/task1".to_string(),
            status: ProtoWorkflowStatus::Completed as i32,
            error_message: None,
        };

        let state = build_workflow_state(&result);
        assert_eq!(state.status, WorkflowStatus::Completed);
    }
}
