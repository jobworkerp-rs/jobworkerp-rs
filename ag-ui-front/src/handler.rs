//! AG-UI HTTP handler implementation.
//!
//! AgUiHandler orchestrates workflow execution and AG-UI event streaming.

use crate::config::AgUiServerConfig;
use crate::error::{AgUiError, Result};
use crate::events::{
    AgUiEvent, EventEncoder, ExtractedToolResult, ExtractedToolStarted,
    InterruptInfo as AgUiInterruptInfo, InterruptPayload, PendingToolCall,
    SharedWorkflowEventAdapter, extract_text_from_completed_output,
    extract_text_from_llm_chat_result, extract_tool_calls_from_llm_result,
    extract_tool_execution_results, extract_tool_execution_started, shared_adapter,
    tool_calls_to_ag_ui_events,
};
use crate::session::{
    EventStore, HitlWaitingInfo, PendingToolCallInfo, ResumeBatchEntry, ResumeBatchResult, Session,
    SessionManager, SessionState,
};
use crate::types::ids::{MessageId, StepId};
use crate::types::{RunAgentInput, RunId, ThreadId};
use app::module::AppModule;
use app_wrapper::modules::AppWrapperModule;
use app_wrapper::workflow::definition::workflow::WorkflowSchema;
use app_wrapper::workflow::execute::checkpoint::CheckPointContext;
use app_wrapper::workflow::execute::context::WorkflowStreamEvent;
use app_wrapper::workflow::execute::task::ExecutionId;
use app_wrapper::workflow::execute::workflow::WorkflowExecutor;
use async_stream::stream;
use futures::{Stream, StreamExt};
use indexmap::IndexMap;
use jobworkerp_runner::jobworkerp::runner::WorkflowRunnerSettings;
use prost::Message as ProstMessage;
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

/// Unregisters a run's executor however its event stream ends, including early error
/// returns and the stream being dropped when the client disconnects.
struct ExecutorRegistration {
    registry: ExecutorRegistry,
    run_id: String,
}

impl ExecutorRegistration {
    /// Unregister ahead of the stream's end, e.g. before announcing a pause.
    async fn release(&self) {
        self.registry.write().await.remove(&self.run_id);
    }
}

impl Drop for ExecutorRegistration {
    fn drop(&mut self) {
        let run_id = std::mem::take(&mut self.run_id);
        if let Ok(mut registry) = self.registry.try_write() {
            registry.remove(&run_id);
            return;
        }
        let registry = self.registry.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    registry.write().await.remove(&run_id);
                });
            }
            Err(_) => tracing::warn!(run_id = %run_id, "Executor left registered: no runtime"),
        }
    }
}

pub(crate) struct PreparedCheckpointResume {
    pub(crate) execution_id: ExecutionId,
    pub(crate) checkpoint: CheckPointContext,
    pub(crate) schema: Arc<WorkflowSchema>,
    pub(crate) workflow_context: serde_json::Value,
}

/// The session a DELETE still has to cancel, or None when the run is already settled.
/// A registered direct run is cancelled here through its own producer; a run in a terminal
/// state already has its terminal event, and another one would follow it.
pub(crate) async fn session_to_cancel<SM: SessionManager>(
    direct_runs: &crate::direct::DirectRuns,
    job_app: &dyn app::app::job::JobApp,
    session_manager: &SM,
    run_id: &str,
) -> Result<Option<Session>> {
    if direct_runs.cancel(job_app, run_id).await? {
        return Ok(None);
    }
    // Read after the direct-run check: a direct run settles its state before it
    // deregisters, so a run that is no longer registered shows its final state here.
    let session = session_manager
        .get_session_by_run_id(&RunId::new(run_id))
        .await
        .ok_or_else(|| AgUiError::SessionNotFound(run_id.to_string()))?;
    Ok((!session.state.is_terminal()).then_some(session))
}

pub(crate) async fn finish_request_session<SM: SessionManager>(
    manager: &SM,
    request_session_id: &str,
    canonical_session_id: &str,
    state: SessionState,
) {
    if request_session_id != canonical_session_id {
        manager.set_session_state(request_session_id, state).await;
    }
}

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
    direct_runs: crate::direct::DirectRuns,
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
            direct_runs: Default::default(),
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
        self.run_workflow_for_worker(input, None, None).await
    }

    pub async fn run_workflow_for_worker(
        &self,
        input: RunAgentInput,
        worker_name: Option<&str>,
        profile_path: Option<&str>,
    ) -> Result<(
        Session,
        Pin<Box<dyn Stream<Item = (u64, AgUiEvent)> + Send>>,
    )> {
        // Check if this is a resume request (AG-UI Interrupts protocol)
        if input.is_resume() {
            if let Some(profile_path) = profile_path {
                return self.handle_resume_batch(input, profile_path).await;
            }
            return Err(AgUiError::InvalidInput(
                "Resume requires the registered endpoint profile".to_string(),
            ));
        }

        // Use provided run_id or generate a new one
        let run_id = RunId::new(input.run_id.clone());
        let thread_id = ThreadId::new(input.thread_id.clone());

        // Resolve the server-owned worker before creating session state.
        let (workflow, workflow_context, registered_worker_name) =
            self.parse_workflow_from_input(&input, worker_name).await?;
        let workflow_name = workflow.document.name.to_string();

        // Create session
        let session = self
            .session_manager
            .create_session(run_id.clone(), thread_id.clone())
            .await;
        if let Some(profile_path) = profile_path {
            self.session_manager
                .set_profile_path(&session.session_id, profile_path)
                .await;
        }

        // Capture HITL context before passing workflow_context to init
        let hitl_ctx = HitlStreamContext {
            workflow_context: workflow_context.clone(),
            registered_worker_name: registered_worker_name.clone(),
            execution_id: Some(run_id.to_string()),
            canonical_session_id: Some(session.session_id.clone()),
        };

        // Create adapter for event conversion
        let adapter = shared_adapter(run_id.clone(), thread_id.clone());

        // Initialize workflow executor with run_id for checkpoint tracking
        let executor = self
            .init_workflow_executor(workflow, input, &run_id, adapter.clone(), workflow_context)
            .await?;

        let executor = Arc::new(executor);
        let registration = self
            .register_executor(run_id.as_str(), executor.clone())
            .await;
        let stream = self.create_event_stream(
            executor,
            adapter,
            run_id.to_string(),
            session.session_id.clone(),
            registration,
            workflow_name,
            true, // emit_run_started: true for new workflow runs
            hitl_ctx,
        );

        Ok((session, Box::pin(stream)))
    }

    async fn prepare_checkpoint_resume(
        &self,
        checkpoint_session: &Session,
        hitl_info: &HitlWaitingInfo,
    ) -> Result<PreparedCheckpointResume> {
        let (execution_id, checkpoint) =
            load_resume_checkpoint(&self.app_wrapper_module, checkpoint_session, hitl_info).await?;

        let worker_name = resume_worker_name(hitl_info);
        let worker = self
            .app_module
            .worker_app
            .find_by_name(worker_name)
            .await
            .map_err(|error| {
                AgUiError::Internal(anyhow::anyhow!(
                    "Failed to find workflow worker '{}': {}",
                    worker_name,
                    error
                ))
            })?
            .ok_or_else(|| AgUiError::WorkflowDefinitionNotFound {
                name: worker_name.to_string(),
            })?;
        let worker_data = worker.data.ok_or_else(|| {
            AgUiError::InvalidInput(format!("Worker '{}' has no data", worker_name))
        })?;
        let (schema, settings_workflow_context) = decode_workflow_from_runner_settings(
            &worker_data.runner_settings,
            &hitl_info.workflow_name,
            &self.app_module,
        )
        .await?;
        let workflow_context = hitl_info
            .workflow_context
            .clone()
            .or(settings_workflow_context)
            .unwrap_or_else(|| serde_json::json!({}));

        Ok(PreparedCheckpointResume {
            execution_id,
            checkpoint,
            schema,
            workflow_context,
        })
    }

    pub async fn handle_resume_batch(
        &self,
        input: RunAgentInput,
        profile_path: &str,
    ) -> Result<(
        Session,
        Pin<Box<dyn Stream<Item = (u64, AgUiEvent)> + Send>>,
    )> {
        let entries = input.resume.as_deref().unwrap_or_default();
        if entries.is_empty() {
            return Err(AgUiError::InvalidInput(
                "Resume entry list is empty".to_string(),
            ));
        }
        let previous = self
            .session_manager
            .get_session_by_interrupt_id(&entries[0].interrupt_id)
            .await
            .ok_or_else(|| AgUiError::InvalidInterruptId {
                interrupt_id: entries[0].interrupt_id.clone(),
                message: "No pending checkpoint matches this interrupt".into(),
            })?;
        if previous.thread_id.as_str() != input.thread_id {
            return Err(AgUiError::InvalidInput(
                "Resume threadId does not match the paused checkpoint".into(),
            ));
        }
        if previous.profile_path.as_deref() != Some(profile_path) {
            return Err(AgUiError::InvalidInput(
                "Resume endpoint does not match the paused checkpoint profile".into(),
            ));
        }
        if self
            .session_manager
            .get_session_by_run_id(&RunId::new(input.run_id.clone()))
            .await
            .is_some()
        {
            return Err(AgUiError::InvalidInput(
                "Resume runId must identify a new run".into(),
            ));
        }
        let checkpoint_info = previous.hitl_waiting_info.as_ref().ok_or_else(|| {
            AgUiError::InvalidInput("Paused session has no checkpoint data".into())
        })?;
        let prepared_resume = self
            .prepare_checkpoint_resume(&previous, checkpoint_info)
            .await?;
        let entries: Vec<_> = entries
            .iter()
            .map(|entry| ResumeBatchEntry {
                interrupt_id: entry.interrupt_id.clone(),
                status: entry.status,
                payload: entry.payload.clone(),
            })
            .collect();
        let batch = self
            .session_manager
            .apply_resume_batch(
                &previous.session_id,
                &previous.thread_id,
                profile_path,
                &entries,
            )
            .await
            .map_err(|error| AgUiError::InvalidInput(format!("Invalid resume batch: {error:?}")))?;

        let run_id = RunId::new(input.run_id.clone());
        let thread_id = ThreadId::new(input.thread_id.clone());
        let response_session = self
            .session_manager
            .create_session(run_id.clone(), thread_id)
            .await;
        self.session_manager
            .set_profile_path(&response_session.session_id, profile_path)
            .await;

        match batch {
            ResumeBatchResult::Partial { remaining } => {
                let encoder = self.encoder.clone();
                let event_store = self.event_store.clone();
                let session_manager = self.session_manager.clone();
                let response_session_id = response_session.session_id.clone();
                let rid = run_id.to_string();
                let tid = input.thread_id.clone();
                let stream = stream! {
                    let started = AgUiEvent::run_started(rid.clone(), tid.clone());
                    let id = Self::encode_event_with_logging(&encoder, &started);
                    event_store.store_event(&rid, id, started.clone()).await;
                    yield (id, started);
                    let finished = AgUiEvent::run_finished_with_interrupts(
                        rid.clone(), tid.clone(),
                        remaining.iter().map(|entry| crate::events::types::AgUiInterrupt { id: entry.interrupt_id.clone(), reason: "interrupt_pending".into() }).collect(),
                    );
                    let id = Self::encode_event_with_logging(&encoder, &finished);
                    event_store.store_event(&rid, id, finished.clone()).await;
                    session_manager.set_session_state(&response_session_id, SessionState::Completed).await;
                    yield (id, finished);
                };
                Ok((response_session, Box::pin(stream)))
            }
            ResumeBatchResult::Complete {
                resolved_info,
                pending_info,
            } => {
                let continuation = ResumeContinuation::new(
                    &previous,
                    *resolved_info,
                    *pending_info,
                    response_session.session_id.clone(),
                );
                self.resume_workflow(response_session.clone(), continuation, prepared_resume)
                    .await
                    .map(|stream| (response_session, stream))
            }
        }
    }

    pub async fn run_direct_profile(
        &self,
        input: RunAgentInput,
        worker_name: &str,
        profile_path: &str,
    ) -> Result<(
        Session,
        Pin<Box<dyn Stream<Item = (u64, AgUiEvent)> + Send>>,
    )> {
        let run_id = RunId::new(input.run_id.clone());
        let thread_id = ThreadId::new(input.thread_id.clone());
        let (job_id, events) = crate::direct::enqueue_direct_run(
            &self.app_module,
            worker_name,
            &input,
            self.config.timeout_sec,
        )
        .await
        .map_err(|e| {
            AgUiError::Internal(anyhow::anyhow!("Failed to enqueue direct LLM run: {e}"))
        })?;
        // Created only once the job is accepted so a rejected run never appears active.
        let session = self
            .session_manager
            .create_session(run_id.clone(), thread_id.clone())
            .await;
        self.session_manager
            .set_profile_path(&session.session_id, profile_path)
            .await;
        let output = self
            .direct_runs
            .start(
                job_id,
                &session,
                events,
                self.event_store.clone(),
                self.encoder.clone(),
                self.session_manager.clone(),
            )
            .await;
        Ok((session, output))
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
        let has_executor = self.executor_registry.read().await.contains_key(run_id);
        let is_running = has_executor || self.direct_runs.is_running(run_id).await;

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
            if is_running && !session_state.is_terminal() {
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
                            event.is_terminal()
                        });

                        if workflow_completed {
                            break;
                        }
                        continue;
                    }

                    // Yield new events
                    for (id, event) in new_events {
                        last_yielded_id = Some(id);
                        let is_terminal = event.is_terminal();
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

    #[cfg(test)]
    pub(crate) async fn has_registered_executor(&self, run_id: &str) -> bool {
        self.executor_registry.read().await.contains_key(run_id)
    }

    /// Cancel a running workflow or direct run.
    ///
    /// A direct run is settled by its own event producer. A workflow run triggers the
    /// WorkflowExecutor's cancel method and is marked cancelled here.
    pub async fn cancel_workflow(&self, run_id: &str) -> Result<()> {
        let Some(session) = session_to_cancel(
            &self.direct_runs,
            self.app_module.job_app.as_ref(),
            self.session_manager.as_ref(),
            run_id,
        )
        .await?
        else {
            return Ok(());
        };

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
        let event = AgUiEvent::run_cancelled(run_id);
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
    pub async fn get_state(&self, run_id: &str) -> Result<serde_json::Value> {
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
        Ok(serde_json::Value::Null)
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

    /// Continue a checkpoint after the canonical resume batch has been consumed.
    async fn resume_workflow(
        &self,
        session: Session,
        continuation: ResumeContinuation,
        prepared: PreparedCheckpointResume,
    ) -> Result<Pin<Box<dyn Stream<Item = (u64, AgUiEvent)> + Send>>> {
        let ResumeContinuation {
            hitl_info,
            user_input,
            consumed,
        } = continuation;
        let canonical_session_id = consumed.canonical_session_id.clone();
        let run_id_typed = session.run_id.clone();
        let run_id = session.run_id.as_str();

        let execution_id = prepared.execution_id;
        let checkpoint = prepared.checkpoint;

        // Inject all submitted resume entries into the canonical checkpoint.
        let mut ctx_vars = (*checkpoint.workflow.context_variables).clone();
        ctx_vars.insert("user_input".to_string(), user_input.clone());
        let modified_checkpoint = CheckPointContext {
            task: app_wrapper::workflow::execute::checkpoint::TaskCheckPointContext {
                output: Arc::new(user_input.clone()),
                flow_directive: "continue".to_string(),
                ..checkpoint.task.clone()
            },
            workflow: app_wrapper::workflow::execute::checkpoint::WorkflowCheckPointContext {
                context_variables: Arc::new(ctx_vars),
                ..checkpoint.workflow.clone()
            },
            ..checkpoint
        };
        let schema = prepared.schema;
        let context_value = prepared.workflow_context;

        // 6. Create adapter for event conversion
        let adapter = shared_adapter(run_id_typed.clone(), session.thread_id.clone());

        // 7. Initialize executor from checkpoint (before updating session state)
        let executor = match WorkflowExecutor::init(
            self.app_wrapper_module.clone(),
            self.app_module.clone(),
            schema,
            modified_checkpoint.workflow.input.clone(),
            Some(execution_id.clone()),
            Arc::new(context_value.clone()),
            Arc::new(HashMap::new()),
            Some(modified_checkpoint),
        )
        .await
        {
            Ok(executor) => executor,
            Err(e) => {
                consumed.reopen(&*self.session_manager).await;
                return Err(AgUiError::WorkflowInitFailed(format!("{:?}", e)));
            }
        };

        let run_started = AgUiEvent::run_started(run_id.to_string(), session.thread_id.to_string());
        let run_started_id = Self::encode_event_with_logging(&self.encoder, &run_started);
        self.event_store
            .store_event(run_id, run_started_id, run_started.clone())
            .await;

        // 8. Register executor for cancellation support
        let executor = Arc::new(executor);
        let registration = self.register_executor(run_id, executor.clone()).await;

        // 9. Create event stream after the standard RUN_STARTED event.
        let run_id_for_stream = run_id.to_string();
        let workflow_name_for_stream = hitl_info.workflow_name.clone();

        let hitl_ctx = HitlStreamContext {
            workflow_context: Some(context_value),
            registered_worker_name: hitl_info.registered_worker_name.clone(),
            execution_id: hitl_info.execution_id.clone(),
            canonical_session_id: Some(canonical_session_id),
        };

        let workflow_stream = self.create_event_stream(
            executor,
            adapter,
            run_id_for_stream,
            session.session_id.clone(),
            registration,
            workflow_name_for_stream,
            false,
            hitl_ctx,
        );

        // Resume payloads are checkpoint input and do not expose internal workflow calls.
        let started = futures::stream::iter(vec![(run_started_id, run_started)]);
        let combined_stream = started.chain(workflow_stream);

        Ok(Box::pin(combined_stream))
    }

    /// Parse workflow schema from input context.
    /// Returns (workflow_schema, workflow_context, registered_worker_name) tuple.
    /// `registered_worker_name` is `Some` for `WorkflowDefinition`, `None` for `WorkflowInline`.
    async fn parse_workflow_from_input(
        &self,
        _input: &RunAgentInput,
        worker_name: Option<&str>,
    ) -> Result<(
        Arc<WorkflowSchema>,
        Option<serde_json::Value>,
        Option<String>,
    )> {
        let workflow_name = worker_name.ok_or_else(|| {
            AgUiError::InvalidInput("No server workflow profile selected".to_string())
        })?;
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
                name: workflow_name.to_string(),
            })?;
        let worker_data = worker.data.ok_or_else(|| {
            AgUiError::InvalidInput(format!("Worker '{}' has no data", workflow_name))
        })?;
        let (schema, settings_ctx) = decode_workflow_from_runner_settings(
            &worker_data.runner_settings,
            workflow_name,
            &self.app_module,
        )
        .await?;
        Ok((schema, settings_ctx, Some(workflow_name.to_string())))
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
        let checkpoint: Option<CheckPointContext> = None;

        // Build input value from messages
        let input_value = Arc::new(serde_json::json!({
            "threadId": input.thread_id,
            "runId": input.run_id,
            "parentRunId": input.parent_run_id,
            "protocolVersion": input.protocol_version,
            "messages": input.messages,
            "tools": input.tools,
            "context": input.context,
            "state": input.state,
            "forwardedProps": input.forwarded_props,
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
            Arc::new(HashMap::new()),
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

    /// Register a run's executor for cancellation; it stays registered while the
    /// returned guard lives.
    async fn register_executor(
        &self,
        run_id: &str,
        executor: Arc<WorkflowExecutor>,
    ) -> ExecutorRegistration {
        self.executor_registry
            .write()
            .await
            .insert(run_id.to_string(), executor);
        ExecutorRegistration {
            registry: self.executor_registry.clone(),
            run_id: run_id.to_string(),
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
        registration: ExecutorRegistration,
        workflow_name: String,
        emit_run_started: bool,
        hitl_ctx: HitlStreamContext,
    ) -> impl Stream<Item = (u64, AgUiEvent)> + Send + 'static {
        let event_store = self.event_store.clone();
        let encoder = self.encoder.clone();
        let session_manager = self.session_manager.clone();
        let app_module = self.app_module.clone();
        let state_session_id = hitl_ctx
            .canonical_session_id
            .clone()
            .unwrap_or_else(|| session_id.clone());

        stream! {
            let registration = registration;
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
            // Buffer tool results from StreamingData until StreamingJobCompleted emits START/ARGS/END first.
            // IndexMap preserves insertion order so the drain() fallback flushes in a stable, predictable
            // order — required for AG-UI clients that render events strictly as they arrive.
            let mut pending_tool_results = IndexMap::<String, ExtractedToolResult>::new();
            // Track whether text content was sent via StreamingData for each job_id
            // Used to decide whether to emit compensating TEXT_MESSAGE_CONTENT at StreamingJobCompleted
            let mut text_content_sent = std::collections::HashSet::<i64>::new();
            // Track whether TEXT_MESSAGE_START has been emitted for each job_id
            // TEXT_MESSAGE_START is deferred until text content actually arrives
            let mut text_message_started = std::collections::HashSet::<i64>::new();
            // Buffer ToolExecutionStarted info for real-time TOOL_CALL emission.
            // IndexMap preserves insertion order to match pending_tool_results ordering.
            let mut pending_tool_starts = IndexMap::<String, ExtractedToolStarted>::new();
            // Track tool calls already emitted via START/ARGS or END in this stream.
            // Both sets are seeded from the session's persisted snapshot so HITL pause→resume
            // re-runs do not re-emit START/ARGS or a second END for call_ids whose bounded
            // START–ARGS–END unit was already sent before the pause (per AG-UI spec:
            // "Each START–ARGS–END forms a bounded unit"). A single get_session call is used
            // to avoid two consecutive Redis round trips.
            let session_snapshot = session_manager.get_session(&state_session_id).await;
            let mut sent_tool_call_ids: std::collections::HashSet<String> = session_snapshot
                .as_ref()
                .map(|s| s.emitted_tool_call_ids.clone())
                .unwrap_or_default();
            let mut sent_tool_end_ids: std::collections::HashSet<String> = session_snapshot
                .map(|s| s.emitted_tool_call_end_ids)
                .unwrap_or_default();

            // Channel for nested workflow progress events (from parallel subscriptions)
            // Wrapped in Option so early-return paths can drop the sender to unblock receivers.
            let (nested_event_tx_raw, mut nested_event_rx) =
                tokio::sync::mpsc::channel::<(u64, AgUiEvent)>(NESTED_EVENT_CHANNEL_SIZE);
            let mut nested_event_tx = Some(nested_event_tx_raw);
            // Track spawned nested workflow subscription tasks for cleanup
            let mut nested_task_handles: std::collections::HashMap<i64, tokio::task::JoinHandle<()>> = std::collections::HashMap::new();

            // Main event loop - process workflow events and nested progress events
            // Uses tokio::select! to merge both event sources
            let strict_output_only = true;
            loop {
                let event_result = tokio::select! {
                    result = workflow_stream.next() => {
                        match result {
                            Some(r) => r,
                            None => break, // workflow stream ended
                        }
                    }
                    Some(nested_event) = nested_event_rx.recv() => {
                        if strict_output_only { continue; }
                        yield nested_event;
                        continue;
                    }
                };
                match event_result {
                    Ok(event) => {
                        // Handle StreamingJobStarted: prepare message_id but defer TEXT_MESSAGE_START
                        // until text content actually arrives (avoids empty START/END for tool-only responses)
                        if !strict_output_only && let WorkflowStreamEvent::StreamingJobStarted { event: ev } = &event {
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
                        if !strict_output_only && let WorkflowStreamEvent::StreamingData { event: ev } = &event {
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
                                    // Skip START/ARGS if this call_id was already announced earlier in
                                    // this run (e.g. pre-HITL initial emission). Emitting a second START
                                    // for the same toolCallId would violate the AG-UI bounded START–ARGS–END
                                    // unit. END/RESULT correlation below still runs so the unit can be
                                    // closed when the result arrives.
                                    let already_emitted = sent_tool_call_ids.contains(&started.call_id);

                                    if !already_emitted {
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
                                        // Persist on the session so subsequent streams (e.g. resumes after
                                        // a transient disconnect) do not re-emit the same bounded unit.
                                        session_manager
                                            .record_emitted_tool_call_id(&state_session_id, &started.call_id)
                                            .await;
                                    }

                                    // Check if a result was already buffered (arrived before Started)
                                    if let Some(extracted) = pending_tool_results.shift_remove(&started.call_id) {
                                        // Emit END only if it has not been emitted earlier in this
                                        // session (e.g. eagerly during HITL approval). RESULT is
                                        // always allowed exactly once per call_id.
                                        if sent_tool_end_ids.insert(started.call_id.clone()) {
                                            let end_event = AgUiEvent::tool_call_end(started.call_id.clone());
                                            let end_eid = Self::encode_event_with_logging(&encoder, &end_event);
                                            event_store.store_event(&run_id, end_eid, end_event.clone()).await;
                                            yield (end_eid, end_event);
                                            session_manager
                                                .record_emitted_tool_call_end_id(&state_session_id, &started.call_id)
                                                .await;
                                        }

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
                                    if let Some(_tool_start) = pending_tool_starts.shift_remove(&call_id) {
                                        // START/ARGS already emitted; emit END only if it has not
                                        // been sent earlier in this run's event stream.
                                        if sent_tool_end_ids.insert(call_id.clone()) {
                                            let end_event = AgUiEvent::tool_call_end(call_id.clone());
                                            let end_eid = Self::encode_event_with_logging(&encoder, &end_event);
                                            event_store.store_event(&run_id, end_eid, end_event.clone()).await;
                                            yield (end_eid, end_event);
                                            session_manager
                                                .record_emitted_tool_call_end_id(&state_session_id, &call_id)
                                                .await;
                                        }

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
                                                                        step_name: "workflow-step".to_string(),
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
                                                                    step_name: step_name.to_string(),
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
                                                        step_name: "workflow-step".to_string(),
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
                        if !strict_output_only && let WorkflowStreamEvent::StreamingJobCompleted { event: ev, context: tc } = &event {
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
                                        // Persist on the session so a subsequent stream (resume after
                                        // disconnect, or HITL re-run) does not re-emit the bounded
                                        // START–ARGS–END unit for these call_ids.
                                        for call in &unsent_tool_calls {
                                            sent_tool_call_ids.insert(call.call_id.clone());
                                            session_manager
                                                .record_emitted_tool_call_id(&state_session_id, &call.call_id)
                                                .await;
                                        }
                                    }

                                    // Auto-calling mode: emit TOOL_CALL_END + TOOL_CALL_RESULT for unsent tools
                                    // Order: START → ARGS → END → RESULT (AG-UI protocol compliant)
                                    if !tool_calls.requires_execution {
                                        let tool_results_from_output = extract_tool_execution_results(&output_bytes);
                                        for call in &tool_calls.tool_calls {
                                            // START/ARGS were already emitted above (either via the
                                            // real-time StreamingData path or this iteration's
                                            // unsent_tool_calls block, which also seeded
                                            // sent_tool_call_ids), so only END/RESULT remains.
                                            if !sent_tool_result_ids.contains(call.call_id.as_str()) {
                                                // TOOL_CALL_END — guard against a second END for
                                                // call_ids whose END was already emitted earlier.
                                                // RESULT is still required and emitted once below.
                                                if sent_tool_end_ids.insert(call.call_id.clone()) {
                                                    let end_event = AgUiEvent::tool_call_end(call.call_id.clone());
                                                    let end_event_id = Self::encode_event_with_logging(&encoder, &end_event);
                                                    event_store.store_event(&run_id, end_event_id, end_event.clone()).await;
                                                    yield (end_event_id, end_event);
                                                    session_manager
                                                        .record_emitted_tool_call_end_id(&state_session_id, &call.call_id)
                                                        .await;
                                                }

                                                // TOOL_CALL_RESULT: buffered result (from StreamingData) takes priority,
                                                // then fall back to output bytes, then Null
                                                sent_tool_result_ids.insert(call.call_id.clone());
                                                let result = pending_tool_results.shift_remove(&call.call_id)
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
                            for (call_id, extracted) in pending_tool_results.drain(..) {
                                if !sent_tool_result_ids.contains(call_id.as_str()) {
                                    sent_tool_result_ids.insert(call_id.clone());

                                    // Ensure START/ARGS were emitted before END/RESULT
                                    if !sent_tool_call_ids.contains(call_id.as_str()) {
                                        if let Some(tool_start) = pending_tool_starts.shift_remove(&call_id) {
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
                                        session_manager
                                            .record_emitted_tool_call_id(&state_session_id, &call_id)
                                            .await;
                                    }

                                    // Same guard as the StreamingData paths above: skip END if it
                                    // was already emitted earlier in this session.
                                    if sent_tool_end_ids.insert(call_id.clone()) {
                                        let end_event = AgUiEvent::tool_call_end(call_id.clone());
                                        let end_event_id = Self::encode_event_with_logging(&encoder, &end_event);
                                        event_store.store_event(&run_id, end_event_id, end_event.clone()).await;
                                        yield (end_event_id, end_event);
                                        session_manager
                                            .record_emitted_tool_call_end_id(&state_session_id, &call_id)
                                            .await;
                                    }

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
                                    let mut hitl_info = HitlWaitingInfo::new(
                                        primary_tool_call_id.clone(),
                                        current_position.clone(),
                                        workflow_name.clone(),
                                        pending_tool_calls.clone(),
                                        hitl_ctx.workflow_context.clone(),
                                        hitl_ctx.registered_worker_name.clone(),
                                    );
                                    hitl_ctx.attach_execution_id(&mut hitl_info);
                                    let interrupt_id = hitl_info.interrupt_id.clone();
                                    if !session_manager.set_paused_with_hitl_info(&state_session_id, hitl_info).await {
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

                                        session_manager.set_session_state(&state_session_id, SessionState::Error).await;
                        finish_request_session(&*session_manager, &session_id, &state_session_id, SessionState::Error).await;
                                        // Drop sender so nested subscription tasks can terminate
                                        nested_event_tx.take();
                                        return;
                                    }

                                    registration.release().await;

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
                                    let interrupt_info = AgUiInterruptInfo {
                                        id: interrupt_id.clone(),
                                        reason: "tool_approval_required".to_string(),
                                        payload: interrupt_payload,
                                    };
                                    let run_finished_event = AgUiEvent::run_finished_with_interrupt(
                                        run_id.clone(),
                                        session_manager.get_session(&state_session_id).await
                                            .map(|s| s.thread_id.to_string()).unwrap_or_default(),
                                        interrupt_info,
                                    );
                                    let event_id = Self::encode_event_with_logging(&encoder, &run_finished_event);
                                    event_store.store_event(&run_id, event_id, run_finished_event.clone()).await;
                                    yield (event_id, run_finished_event);
                                    finish_request_session(&*session_manager, &session_id, &state_session_id, SessionState::Completed).await;

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
                        if !strict_output_only && let WorkflowStreamEvent::TaskStarted { event: ev } = &event {
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
                        if !strict_output_only && let WorkflowStreamEvent::TaskCompleted { event: ev, .. } = &event {
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
                                // Clear prev_position so the context fallback below
                                // does not redundantly attempt task_finished() again.
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

                        // ForItemFailed: per-iteration failure surfaced by a
                        // for-task running with onError=continue. The for-task
                        // itself will still emit a single TaskCompleted at the
                        // end, so we surface this as a self-contained
                        // STEP_STARTED + STEP_FINISHED pair carrying the error
                        // payload as the step result, so the UI can render
                        // "iteration N failed" without losing details.
                        if let WorkflowStreamEvent::ForItemFailed {
                            task_name,
                            position,
                            index,
                            error_payload,
                        } = &event
                        {
                            if strict_output_only {
                                continue;
                            }
                            tracing::warn!(
                                task_name = %task_name,
                                position = %position,
                                index = %index,
                                error_payload = %error_payload,
                                "for-task iteration failed under onError=continue"
                            );
                            // If a step was previously open at the failing
                            // iteration's position (parallel branch span), close
                            // it first carrying the error payload so the UI sees
                            // the failure attached to that step.
                            if prev_position.as_deref() == Some(position) {
                                let finished_event = {
                                    let mut adapter_lock = adapter.lock().await;
                                    adapter_lock.task_finished(Some(error_payload.clone()))
                                };
                                if let Some(finished_event) = finished_event {
                                    let event_id = Self::encode_event_with_logging(
                                        &encoder,
                                        &finished_event,
                                    );
                                    event_store
                                        .store_event(&run_id, event_id, finished_event.clone())
                                        .await;
                                    yield (event_id, finished_event);
                                }
                                prev_position = None;
                            } else {
                                // No matching open step: emit a standalone
                                // STEP_STARTED + STEP_FINISHED pair so the
                                // failure is still visible in the UI timeline.
                                let step_name = format!("{task_name}[{index}]");
                                let mut adapter_lock = adapter.lock().await;
                                let started_event = adapter_lock.task_started(
                                    &step_name,
                                    Some("forItemFailed"),
                                    None,
                                );
                                let finished_event =
                                    adapter_lock.task_finished(Some(error_payload.clone()));
                                drop(adapter_lock);
                                let event_id =
                                    Self::encode_event_with_logging(&encoder, &started_event);
                                event_store
                                    .store_event(&run_id, event_id, started_event.clone())
                                    .await;
                                yield (event_id, started_event);
                                if let Some(finished_event) = finished_event {
                                    let event_id = Self::encode_event_with_logging(
                                        &encoder,
                                        &finished_event,
                                    );
                                    event_store
                                        .store_event(&run_id, event_id, finished_event.clone())
                                        .await;
                                    yield (event_id, finished_event);
                                }
                            }
                            // ForItemFailed exposes no TaskContext, so the
                            // context() fallback below would no-op anyway.
                            continue;
                        }

                        // Extract context from completed events for workflow state updates
                        if let Some(tc) = event.context() {
                            // Read position from TaskContext
                            let current_position = tc.position.read().await.as_json_pointer();
                            let current_step_name = tc.position.read().await.last_name();

                            if prev_position.as_ref() != Some(&current_position) {
                                // Position changed - emit STEP_FINISHED for previous step if exists
                                if !strict_output_only && prev_position.is_some() {
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
                                if !strict_output_only && let Some(step_name) = current_step_name.as_ref() {
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

                        }
                        // Start events (StreamingJobStarted, JobStarted, TaskStarted) are handled above
                    }
                    Err(_) => {
                        // Flush any remaining TEXT_MESSAGE_END for interrupted streams
                        if !strict_output_only {
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
                        }

                        // Emit STEP_FINISHED for current step before error
                        if !strict_output_only && prev_position.is_some() {
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
                            "Workflow execution failed".to_string(),
                            "WORKFLOW_ERROR",
                            None,
                        );
                        drop(adapter_lock);

                        let event_id = Self::encode_event_with_logging(&encoder, &event);
                        event_store.store_event(&run_id, event_id, event.clone()).await;
                        yield (event_id, event);

                        // Update session state to Error
                        session_manager.set_session_state(&state_session_id, SessionState::Error).await;
                        finish_request_session(&*session_manager, &session_id, &state_session_id, SessionState::Error).await;
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
                if !strict_output_only {
                    yield nested_event;
                }
            }

            // Abort remaining nested tasks — channel drop already signals graceful
            // exit, but abort ensures cleanup even if a task is stuck waiting on
            // subscribe_result_stream_for_job (up to NESTED_WF_PROGRESS_TIMEOUT_MS).
            for handle in nested_task_handles.into_values() {
                handle.abort();
            }

            let waiting_checkpoint = {
                let workflow_context = executor.workflow_context.read().await;
                if !has_error
                    && workflow_context.status
                    == app_wrapper::workflow::execute::context::WorkflowStatus::Waiting
                {
                    Some(
                        workflow_context
                            .resume_position
                            .as_ref()
                            .map(|position| position.as_json_pointer()),
                    )
                } else {
                    None
                }
            };

            if let Some(Some(checkpoint_position)) = waiting_checkpoint {
                let tool_call_id = format!("wait_{run_id}");
                session_manager
                    .record_emitted_tool_call_id(&state_session_id, &tool_call_id)
                    .await;
                let mut hitl_info = HitlWaitingInfo::new(
                    tool_call_id,
                    checkpoint_position,
                    workflow_name.clone(),
                    vec![],
                    hitl_ctx.workflow_context.clone(),
                    hitl_ctx.registered_worker_name.clone(),
                );
                hitl_ctx.attach_execution_id(&mut hitl_info);
                let interrupt_id = hitl_info.interrupt_id.clone();
                if !session_manager
                    .set_paused_with_hitl_info(&state_session_id, hitl_info)
                    .await
                {
                    let adapter_lock = adapter.lock().await;
                    let event = adapter_lock.workflow_error(
                        "Failed to persist HITL waiting state",
                        "HITL_PERSISTENCE_ERROR",
                        None,
                    );
                    drop(adapter_lock);
                    let event_id = Self::encode_event_with_logging(&encoder, &event);
                    event_store
                        .store_event(&run_id, event_id, event.clone())
                        .await;
                    yield (event_id, event);
                    session_manager
                        .set_session_state(&state_session_id, SessionState::Error)
                        .await;
                    finish_request_session(
                        &*session_manager,
                        &session_id,
                        &state_session_id,
                        SessionState::Error,
                    )
                    .await;
                    nested_event_tx.take();
                    return;
                }

                registration.release().await;
                let thread_id = session_manager
                    .get_session(&state_session_id)
                    .await
                    .map(|session| session.thread_id.to_string())
                    .unwrap_or_default();
                let event = AgUiEvent::run_finished_with_interrupts(
                    run_id.clone(),
                    thread_id,
                    vec![crate::events::types::AgUiInterrupt {
                        id: interrupt_id,
                        reason: "workflow_waiting".to_string(),
                    }],
                );
                let event_id = Self::encode_event_with_logging(&encoder, &event);
                event_store
                    .store_event(&run_id, event_id, event.clone())
                    .await;
                yield (event_id, event);
                finish_request_session(
                    &*session_manager,
                    &session_id,
                    &state_session_id,
                    SessionState::Completed,
                )
                .await;
                nested_event_tx.take();
                return;
            }

            if waiting_checkpoint.is_some() {
                let adapter_lock = adapter.lock().await;
                let event = adapter_lock.workflow_error(
                    "Waiting workflow has no checkpoint position",
                    "MISSING_CHECKPOINT_POSITION",
                    None,
                );
                drop(adapter_lock);
                let event_id = Self::encode_event_with_logging(&encoder, &event);
                event_store
                    .store_event(&run_id, event_id, event.clone())
                    .await;
                yield (event_id, event);
                session_manager
                    .set_session_state(&state_session_id, SessionState::Error)
                    .await;
                finish_request_session(
                    &*session_manager,
                    &session_id,
                    &state_session_id,
                    SessionState::Error,
                )
                .await;
                return;
            }

            // Flush any remaining TEXT_MESSAGE_END for streams that didn't complete normally
            // (safety net for edge cases where StreamingJobCompleted wasn't received)
            if !strict_output_only {
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
            }

            // Emit STEP_FINISHED for final step if exists
            if !strict_output_only && prev_position.is_some() && !has_error {
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
                let output = executor.workflow_context.read().await.output.clone();
                let result = if let Some(output) = output {
                    match crate::projection::project_workflow_output(&output) {
                        Ok(projection) => {
                            for event in projection.events {
                                let event_id = Self::encode_event_with_logging(&encoder, &event);
                                event_store.store_event(&run_id, event_id, event.clone()).await;
                                yield (event_id, event);
                            }
                            projection.result
                        }
                        Err(_) => {
                            has_error = true;
                            let event = AgUiEvent::run_error(run_id.clone(), "Workflow output could not be published", Some("INVALID_PUBLIC_OUTPUT".into()), None);
                            let event_id = Self::encode_event_with_logging(&encoder, &event);
                            event_store.store_event(&run_id, event_id, event.clone()).await;
                            yield (event_id, event);
                            session_manager.set_session_state(&state_session_id, SessionState::Error).await;
                        finish_request_session(&*session_manager, &session_id, &state_session_id, SessionState::Error).await;
                            None
                        }
                    }
                } else {
                    has_error = true;
                    let event = AgUiEvent::run_error(run_id.clone(), "Workflow produced no public output", Some("MISSING_PUBLIC_OUTPUT".into()), None);
                    let event_id = Self::encode_event_with_logging(&encoder, &event);
                    event_store.store_event(&run_id, event_id, event.clone()).await;
                    yield (event_id, event);
                    session_manager.set_session_state(&state_session_id, SessionState::Error).await;
                        finish_request_session(&*session_manager, &session_id, &state_session_id, SessionState::Error).await;
                    None
                };
                if has_error { return; }
                let thread_id = session_manager.get_session(&state_session_id).await.map(|s| s.thread_id.to_string()).unwrap_or_default();
                let event = AgUiEvent::run_finished_success(run_id.clone(), thread_id, result);
                let event_id = Self::encode_event_with_logging(&encoder, &event);
                event_store.store_event(&run_id, event_id, event.clone()).await;
                yield (event_id, event);

                // Update session state to Completed
                session_manager.set_session_state(&state_session_id, SessionState::Completed).await;
                finish_request_session(&*session_manager, &session_id, &state_session_id, SessionState::Completed).await;
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
            direct_runs: self.direct_runs.clone(),
        }
    }
}

/// Load the checkpoint a paused session waits on, from Redis when configured.
pub(crate) async fn load_resume_checkpoint(
    app_wrapper_module: &AppWrapperModule,
    checkpoint_session: &Session,
    hitl_info: &HitlWaitingInfo,
) -> Result<(ExecutionId, CheckPointContext)> {
    let execution_id = hitl_info
        .execution_id
        .as_deref()
        .map(RunId::new)
        .unwrap_or_else(|| checkpoint_session.run_id.clone())
        .to_execution_id()
        .ok_or_else(|| AgUiError::InvalidInput("Invalid run_id for execution".to_string()))?;
    let repositories = &app_wrapper_module.repositories;
    let checkpoint_repo = repositories
        .redis_checkpoint_repository
        .as_ref()
        .map(|repository| repository.as_ref())
        .unwrap_or(repositories.memory_checkpoint_repository.as_ref());
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
    Ok((execution_id, checkpoint))
}

pub(crate) async fn decode_workflow_from_runner_settings(
    runner_settings: &[u8],
    workflow_name: &str,
    app_module: &AppModule,
) -> Result<(Arc<WorkflowSchema>, Option<serde_json::Value>)> {
    use jobworkerp_runner::jobworkerp::runner::workflow_runner_settings::WorkflowSource;

    let parsed = WorkflowRunnerSettings::decode(runner_settings).map_err(|e| {
        AgUiError::InvalidInput(format!(
            "Failed to decode WorkflowRunnerSettings for worker '{}': {}",
            workflow_name, e
        ))
    })?;

    let source = parsed.workflow_source.ok_or_else(|| {
        AgUiError::InvalidInput(format!(
            "Worker '{}' has no workflow_source in settings",
            workflow_name
        ))
    })?;

    let schema = match source {
        WorkflowSource::WorkflowData(data) => {
            let s: WorkflowSchema = serde_json::from_str(&data).map_err(|e| {
                AgUiError::InvalidInput(format!(
                    "Invalid workflow schema in worker '{}': {}",
                    workflow_name, e
                ))
            })?;
            Arc::new(s)
        }
        WorkflowSource::WorkflowUrl(url) => {
            let s = app_module
                .workflow_loader
                .load_workflow(Some(url.as_str()), None, false)
                .await
                .map_err(|e| {
                    AgUiError::InvalidInput(format!(
                        "Failed to load workflow '{}' from URL '{}': {}",
                        workflow_name, url, e
                    ))
                })?;
            Arc::new(s)
        }
    };

    let workflow_context = parsed
        .workflow_context
        .map(|s| serde_json::from_str(&s))
        .transpose()
        .map_err(|e| {
            AgUiError::InvalidInput(format!(
                "Invalid workflow_context JSON in worker '{}': {}",
                workflow_name, e
            ))
        })?;

    Ok((schema, workflow_context))
}

/// HITL-related context carried through the event stream for pause/resume.
#[derive(Debug, Clone)]
pub(crate) struct HitlStreamContext {
    pub workflow_context: Option<serde_json::Value>,
    pub registered_worker_name: Option<String>,
    pub execution_id: Option<String>,
    pub canonical_session_id: Option<String>,
}

impl HitlStreamContext {
    pub(crate) fn attach_execution_id(&self, hitl_info: &mut HitlWaitingInfo) {
        hitl_info.execution_id = self.execution_id.clone();
    }
}

pub(crate) fn ensure_checkpoint_execution_id(hitl_info: &mut HitlWaitingInfo, run_id: &RunId) {
    hitl_info
        .execution_id
        .get_or_insert_with(|| run_id.to_string());
}

pub(crate) fn resume_worker_name(hitl_info: &HitlWaitingInfo) -> &str {
    hitl_info
        .registered_worker_name
        .as_deref()
        .unwrap_or(&hitl_info.workflow_name)
}

/// Continuation inputs for a checkpoint consumed by a completed resume batch.
pub(crate) struct ResumeContinuation {
    pub hitl_info: HitlWaitingInfo,
    pub user_input: serde_json::Value,
    pub consumed: ConsumedResumeBatch,
}

impl ResumeContinuation {
    pub(crate) fn new(
        canonical: &Session,
        resolved_info: HitlWaitingInfo,
        pending_info: HitlWaitingInfo,
        response_session_id: String,
    ) -> Self {
        let mut hitl_info = resolved_info;
        ensure_checkpoint_execution_id(&mut hitl_info, &canonical.run_id);
        if let Some(first) = hitl_info.interrupts.first() {
            hitl_info.tool_call_id = first.call_id.clone();
        }
        let user_input = resume_entries_to_user_input(&hitl_info.interrupts);
        Self {
            hitl_info,
            user_input,
            consumed: ConsumedResumeBatch {
                canonical_session_id: canonical.session_id.clone(),
                pending_info,
                response_session_id,
            },
        }
    }
}

/// A resume batch whose checkpoint was consumed before its continuation was accepted.
pub(crate) struct ConsumedResumeBatch {
    pub canonical_session_id: String,
    pub pending_info: HitlWaitingInfo,
    pub response_session_id: String,
}

impl ConsumedResumeBatch {
    /// Reopen the consumed interrupts and drop the response session so the client can
    /// retry the same answers. Call only while no continuation job has been accepted,
    /// otherwise the checkpoint could be resumed twice.
    pub(crate) async fn reopen<SM: SessionManager + ?Sized>(self, session_manager: &SM) {
        if !session_manager
            .restore_resume_batch(&self.canonical_session_id, self.pending_info)
            .await
        {
            tracing::error!(
                session_id = %self.canonical_session_id,
                "Failed to reopen interrupts after the resume continuation was rejected"
            );
        }
        session_manager
            .delete_session(&self.response_session_id)
            .await;
    }
}

pub(crate) fn resume_entries_to_user_input(
    entries: &[crate::session::InterruptInfo],
) -> serde_json::Value {
    serde_json::json!({
        "resume": entries
            .iter()
            .map(|entry| serde_json::json!({
                "interruptId": entry.interrupt_id,
                "status": entry.status,
                "payload": entry.payload,
            }))
            .collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        ConsumedResumeBatch, HitlStreamContext, ensure_checkpoint_execution_id,
        resume_entries_to_user_input, resume_worker_name,
    };
    use crate::session::{
        HitlWaitingInfo, InMemorySessionManager, InterruptInfo, ResumeBatchEntry,
        ResumeBatchResult, SessionManager, SessionState,
    };
    use crate::types::{ResumeStatus, RunId, ThreadId};

    #[tokio::test]
    async fn reopening_a_rejected_resume_allows_retrying_the_same_answer() {
        let manager = InMemorySessionManager::new(3600);
        let thread_id = ThreadId::new("rejected_resume_thread");
        let paused = manager
            .create_session(RunId::new("paused_run"), thread_id.clone())
            .await;
        manager
            .set_profile_path(&paused.session_id, "/agent/run")
            .await;
        let info = HitlWaitingInfo::new_simple(
            "call-a".into(),
            "/tasks/approval".into(),
            "workflow".into(),
            vec![],
        );
        let interrupt_id = info.interrupts[0].interrupt_id.clone();
        manager
            .set_paused_with_hitl_info(&paused.session_id, info)
            .await;
        let answer = [ResumeBatchEntry {
            interrupt_id: interrupt_id.clone(),
            status: ResumeStatus::Resolved,
            payload: Some(serde_json::json!({"approved": true})),
        }];
        let ResumeBatchResult::Complete { pending_info, .. } = manager
            .apply_resume_batch(&paused.session_id, &thread_id, "/agent/run", &answer)
            .await
            .unwrap()
        else {
            panic!("a single answer must consume the checkpoint");
        };
        let response = manager
            .create_session(RunId::new("resume_run"), thread_id.clone())
            .await;

        ConsumedResumeBatch {
            canonical_session_id: paused.session_id.clone(),
            pending_info: *pending_info,
            response_session_id: response.session_id.clone(),
        }
        .reopen(&manager)
        .await;

        assert!(manager.get_session(&response.session_id).await.is_none());
        assert!(
            manager
                .get_session_by_run_id(&RunId::new("resume_run"))
                .await
                .is_none()
        );
        let reopened = manager
            .get_session_by_interrupt_id(&interrupt_id)
            .await
            .unwrap();
        assert_eq!(reopened.session_id, paused.session_id);
        assert_eq!(reopened.state, SessionState::Paused);
        assert!(matches!(
            manager
                .apply_resume_batch(&paused.session_id, &thread_id, "/agent/run", &answer)
                .await,
            Ok(ResumeBatchResult::Complete { .. })
        ));
    }
    // Note: Full integration tests require AppWrapperModule setup
    // which is complex. These tests focus on handler construction.

    #[test]
    fn test_handler_clone() {
        // Handler should be clonable for use in axum state
        // This is a compile-time check - actual instantiation requires AppWrapperModule
    }

    #[test]
    fn repeated_waits_keep_the_original_execution_id() {
        let mut first_wait = HitlWaitingInfo::new_simple(
            "call-1".into(),
            "/tasks/first".into(),
            "workflow".into(),
            vec![],
        );
        let first_context = HitlStreamContext {
            workflow_context: None,
            registered_worker_name: Some("registered-worker".into()),
            execution_id: Some("original-execution".into()),
            canonical_session_id: Some("canonical-session".into()),
        };
        first_context.attach_execution_id(&mut first_wait);

        let mut second_wait = HitlWaitingInfo::new_simple(
            "call-2".into(),
            "/tasks/second".into(),
            "workflow".into(),
            vec![],
        );
        let resumed_context = HitlStreamContext {
            workflow_context: first_wait.workflow_context.clone(),
            registered_worker_name: first_wait.registered_worker_name.clone(),
            execution_id: first_wait.execution_id.clone(),
            canonical_session_id: Some("canonical-session".into()),
        };
        resumed_context.attach_execution_id(&mut second_wait);
        ensure_checkpoint_execution_id(&mut second_wait, &RunId::new("intermediate-run"));

        assert_eq!(
            second_wait.execution_id.as_deref(),
            Some("original-execution")
        );
    }

    #[test]
    fn resume_payload_includes_each_interrupt_status_and_payload() {
        let entries = vec![
            InterruptInfo {
                interrupt_id: "interrupt-a".into(),
                call_id: "call-a".into(),
                status: Some(crate::types::ResumeStatus::Resolved),
                payload: Some(serde_json::json!({"result": 1})),
            },
            InterruptInfo {
                interrupt_id: "interrupt-b".into(),
                call_id: "call-b".into(),
                status: Some(crate::types::ResumeStatus::Cancelled),
                payload: None,
            },
        ];

        assert_eq!(
            resume_entries_to_user_input(&entries),
            serde_json::json!({
                "resume": [
                    {"interruptId":"interrupt-a", "status":"resolved", "payload":{"result":1}},
                    {"interruptId":"interrupt-b", "status":"cancelled", "payload":null}
                ]
            })
        );
    }

    #[test]
    fn checkpoint_resume_looks_up_registered_worker_not_document_name() {
        let mut hitl_info = HitlWaitingInfo::new_simple(
            "call-a".into(),
            "/tasks/approval".into(),
            "guardrail-workflow".into(),
            vec![],
        );
        hitl_info.registered_worker_name = Some("chat-guarded".into());

        assert_eq!(resume_worker_name(&hitl_info), "chat-guarded");
    }
}
