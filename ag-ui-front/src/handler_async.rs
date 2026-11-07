//! Async AG-UI HTTP handler implementation (decoupled architecture).
//!
//! AsyncAgUiHandler orchestrates workflow execution via job queue and AG-UI event streaming.
//! Unlike AgUiHandler which executes workflows directly, this handler uses FunctionApp
//! to enqueue jobs for distributed execution across worker processes.

use crate::config::AgUiServerConfig;
use crate::error::{AgUiError, Result};
use crate::events::{AgUiEvent, EventEncoder, SharedWorkflowEventAdapter, shared_adapter};
use crate::session::{
    EventStore, HitlWaitingInfo, ResumeBatchEntry, ResumeBatchResult, Session, SessionManager,
    SessionState,
};
use crate::types::{RunAgentInput, RunId, ThreadId};
use app::app::job::JobEnqueueOutcomeUnknown;
use app::module::AppModule;
use app_wrapper::modules::AppWrapperModule;
use app_wrapper::workflow::definition::workflow::WorkflowSchema;
use app_wrapper::workflow::execute::checkpoint::CheckPointContext;
use async_stream::stream;
use futures::{Stream, StreamExt};
use jobworkerp_runner::jobworkerp::runner::workflow_result::WorkflowStatus as ProtoWorkflowStatus;
use jobworkerp_runner::jobworkerp::runner::{WorkflowResult, WorkflowRunArgs};
use jobworkerp_runner::runner::workflow_unified::METHOD_RUN;
use prost::Message;
use proto::jobworkerp::data::JobId;
use proto::jobworkerp::function::data::FunctionResult;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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
    app_wrapper_module: Arc<AppWrapperModule>,
    session_manager: Arc<SM>,
    event_store: Arc<ES>,
    config: AgUiServerConfig,
    encoder: Arc<EventEncoder>,
    /// run_id -> JobId mapping for cancellation
    /// Lifecycle:
    ///   - Registered: immediately after job enqueue
    ///   - Removed: on stream completion (success/error/HITL pause/cancel)
    job_registry: JobRegistry,
    direct_runs: crate::direct::DirectRuns,
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
        let redis_pool = app_module
            .repositories
            .redis_module
            .as_ref()
            .map(|repository| repository.redis_pool);
        Self {
            app_wrapper_module: Arc::new(AppWrapperModule::new_by_env(redis_pool)),
            app_module,
            session_manager,
            event_store,
            config,
            encoder: Arc::new(EventEncoder::new()),
            job_registry: Arc::new(RwLock::new(HashMap::new())),
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

    /// Start a new workflow run via job queue.
    ///
    /// Creates a session, enqueues the workflow job, and returns an event stream.
    pub async fn run_workflow(
        self: Arc<Self>,
        input: RunAgentInput,
        worker_name: &str,
        profile_path: &str,
    ) -> Result<(
        Session,
        Pin<Box<dyn Stream<Item = (u64, AgUiEvent)> + Send + 'static>>,
    )> {
        if input.is_resume() {
            return self.handle_resume_batch(input, profile_path).await;
        }
        // Use provided run_id or generate a new one
        let run_id = RunId::new(input.run_id.clone());
        let thread_id = ThreadId::new(input.thread_id.clone());

        // Resolve the server-owned worker before creating session state.
        let (workflow, workflow_context, live_worker) =
            self.parse_workflow_from_input(&input, worker_name).await?;
        let workflow_name = workflow.document.name.to_string();

        // Build WorkflowRunArgs
        let hitl_workflow_context = workflow_context.clone();
        let args =
            self.build_workflow_run_args(&input, &workflow, run_id.as_str(), workflow_context)?;
        let args_bytes = args.encode_to_vec();

        let (job_id, result_handle) = crate::job_queue::enqueue_live_stream(
            &self.app_module,
            live_worker,
            METHOD_RUN,
            args_bytes,
            self.config.timeout_sec,
        )
        .await
        .map_err(|error| {
            AgUiError::Internal(anyhow::anyhow!("Failed to enqueue workflow run: {error}"))
        })?;
        let (result_future, result_stream, result_wait_abort) =
            result_handle.into_parts_with_result_wait_abort_handle();
        let result_stream = result_stream.ok_or_else(|| {
            AgUiError::Internal(anyhow::anyhow!(
                "Workflow worker returned no response stream"
            ))
        })?;
        let result_stream = workflow_function_result_stream(result_stream, result_future);

        // Create the session only after the job has been accepted and its result stream subscribed.
        let session = self
            .session_manager
            .create_session(run_id.clone(), thread_id.clone())
            .await;
        self.session_manager
            .set_profile_path(&session.session_id, profile_path)
            .await;
        self.job_registry
            .write()
            .await
            .insert(run_id.to_string(), job_id);

        // Capture HITL context before consuming workflow_context.
        let hitl_ctx = crate::handler::HitlStreamContext {
            workflow_context: hitl_workflow_context,
            registered_worker_name: Some(worker_name.to_string()),
            execution_id: Some(run_id.to_string()),
            canonical_session_id: Some(session.session_id.clone()),
        };

        // Create adapter for event conversion
        let adapter = shared_adapter(run_id.clone(), thread_id.clone());

        // Capture all necessary data for the stream closure
        let run_id_str = run_id.to_string();
        let session_id = session.session_id.clone();
        let event_stream = Self::create_event_stream_inner(
            result_stream,
            adapter,
            run_id_str,
            session_id,
            workflow_name,
            true,
            self.event_store.clone(),
            self.encoder.clone(),
            self.session_manager.clone(),
            self.job_registry.clone(),
            hitl_ctx,
        );

        // Keep job result consumption and event persistence alive when a POST subscriber disconnects.
        let (sender, receiver) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            let _result_wait_abort = result_wait_abort;
            futures::pin_mut!(event_stream);
            while let Some(event) = event_stream.next().await {
                let _ = sender.send(event).await;
            }
        });

        Ok((
            session,
            Box::pin(tokio_stream::wrappers::ReceiverStream::new(receiver)),
        ))
    }

    pub async fn run_direct_profile(
        self: Arc<Self>,
        input: RunAgentInput,
        worker_name: &str,
        profile_path: &str,
    ) -> Result<(
        Session,
        Pin<Box<dyn Stream<Item = (u64, AgUiEvent)> + Send + 'static>>,
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
        let has_job = self.job_registry.read().await.contains_key(run_id);
        let is_running = has_job || self.direct_runs.is_running(run_id).await;

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
            if is_running && !session_state.is_terminal() {
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
                            event.is_terminal()
                        });
                        if workflow_completed {
                            break;
                        }
                        continue;
                    }

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

    /// Cancel a running workflow or direct run.
    pub async fn cancel_workflow(&self, run_id: &str) -> Result<()> {
        let Some(session) = crate::handler::session_to_cancel(
            &self.direct_runs,
            self.app_module.job_app.as_ref(),
            self.session_manager.as_ref(),
            run_id,
        )
        .await?
        else {
            return Ok(());
        };

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
        let event = AgUiEvent::run_cancelled(run_id);
        let event_id = Self::encode_event_with_logging(&self.encoder, &event);
        self.event_store.store_event(run_id, event_id, event).await;

        // Remove from registry
        {
            let mut registry = self.job_registry.write().await;
            registry.remove(run_id);
        }

        Ok(())
    }

    pub async fn handle_resume_batch(
        self: Arc<Self>,
        input: RunAgentInput,
        profile_path: &str,
    ) -> Result<(
        Session,
        Pin<Box<dyn Stream<Item = (u64, AgUiEvent)> + Send + 'static>>,
    )> {
        let wire_entries = input.resume.as_deref().unwrap_or_default();
        let first_id = wire_entries
            .first()
            .ok_or_else(|| AgUiError::InvalidInput("Resume entries are empty".into()))?
            .interrupt_id
            .clone();
        let previous = self
            .session_manager
            .get_session_by_interrupt_id(&first_id)
            .await
            .ok_or_else(|| AgUiError::InvalidInterruptId {
                interrupt_id: first_id.clone(),
                message: "No pending checkpoint matches this interrupt".into(),
            })?;
        if previous.thread_id.as_str() != input.thread_id {
            return Err(AgUiError::InvalidInput(
                "Resume threadId does not match the checkpoint".into(),
            ));
        }
        if previous.profile_path.as_deref() != Some(profile_path) {
            return Err(AgUiError::InvalidInput(
                "Resume endpoint does not match the checkpoint profile".into(),
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
        let (worker, checkpoint) = self.preflight_resume(&previous, checkpoint_info).await?;
        let entries: Vec<_> = wire_entries
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
        let session = self
            .session_manager
            .create_session(run_id.clone(), ThreadId::new(input.thread_id.clone()))
            .await;
        self.session_manager
            .set_profile_path(&session.session_id, profile_path)
            .await;
        match batch {
            ResumeBatchResult::Partial { remaining } => {
                let encoder = self.encoder.clone();
                let event_store = self.event_store.clone();
                let session_manager = self.session_manager.clone();
                let response_session_id = session.session_id.clone();
                let rid = run_id.to_string();
                let tid = input.thread_id.clone();
                let stream = stream! {
                    let started = AgUiEvent::run_started(rid.clone(), tid.clone());
                    let id = Self::encode_event_with_logging(&encoder, &started);
                    event_store.store_event(&rid, id, started.clone()).await; yield (id, started);
                    let finished = AgUiEvent::run_finished_with_interrupts(
                        rid.clone(), tid.clone(),
                        remaining.iter().map(|entry| crate::events::types::AgUiInterrupt { id: entry.interrupt_id.clone(), reason: "interrupt_pending".into() }).collect(),
                    );
                    let id = Self::encode_event_with_logging(&encoder, &finished);
                    session_manager.set_session_state(&response_session_id, SessionState::Completed).await;
                    event_store.store_event(&rid, id, finished.clone()).await; yield (id, finished);
                };
                Ok((session, Box::pin(stream)))
            }
            ResumeBatchResult::Complete {
                resolved_info,
                pending_info,
            } => {
                let continuation = crate::handler::ResumeContinuation::new(
                    &previous,
                    *resolved_info,
                    *pending_info,
                    session.session_id.clone(),
                );
                self.clone()
                    .resume_workflow(session.clone(), continuation, worker, checkpoint)
                    .await
                    .map(|stream| (session, stream))
            }
        }
    }

    /// Validate everything the continuation enqueue needs before the batch consumes
    /// the checkpoint, so configuration errors leave it resumable.
    async fn preflight_resume(
        &self,
        session: &Session,
        hitl_info: &HitlWaitingInfo,
    ) -> Result<(crate::job_queue::LiveStreamWorker, CheckPointContext)> {
        let worker_name = hitl_info.registered_worker_name.clone().ok_or_else(|| {
            AgUiError::WorkflowDefinitionNotFound {
                name: hitl_info.workflow_name.clone(),
            }
        })?;
        let worker = crate::job_queue::resolve_live_stream_worker(&self.app_module, &worker_name)
            .await
            .map_err(|error| {
                AgUiError::Internal(anyhow::anyhow!(
                    "Workflow worker '{worker_name}' cannot resume: {error}"
                ))
            })?;
        crate::handler::decode_workflow_from_runner_settings(
            &worker.data().runner_settings,
            &hitl_info.workflow_name,
            &self.app_module,
        )
        .await?;
        let (_, checkpoint) =
            crate::handler::load_resume_checkpoint(&self.app_wrapper_module, session, hitl_info)
                .await?;
        Ok((worker, checkpoint))
    }

    /// Get current workflow state.
    pub async fn get_state(&self, run_id: &str) -> Result<serde_json::Value> {
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

        Ok(serde_json::Value::Null)
    }

    /// Continue a checkpoint after the canonical resume batch has been consumed.
    async fn resume_workflow(
        self: Arc<Self>,
        session: Session,
        continuation: crate::handler::ResumeContinuation,
        worker: crate::job_queue::LiveStreamWorker,
        checkpoint: CheckPointContext,
    ) -> Result<Pin<Box<dyn Stream<Item = (u64, AgUiEvent)> + Send + 'static>>> {
        let crate::handler::ResumeContinuation {
            hitl_info,
            user_input,
            consumed,
        } = continuation;
        let canonical_session_id = consumed.canonical_session_id.clone();
        let run_id_typed = session.run_id.clone();
        let run_id = session.run_id.as_str();

        let args = self.build_resume_workflow_args(&hitl_info, checkpoint, user_input, run_id);
        let args_bytes = args.encode_to_vec();

        let adapter = shared_adapter(run_id_typed.clone(), session.thread_id.clone());

        let (job_id, result_handle) = match crate::job_queue::enqueue_live_stream(
            &self.app_module,
            worker,
            METHOD_RUN,
            args_bytes,
            self.config.timeout_sec,
        )
        .await
        {
            Ok(accepted) => accepted,
            Err(error) => {
                if let Some(unknown) = error.downcast_ref::<JobEnqueueOutcomeUnknown>() {
                    tracing::error!(
                        run_id = %run_id,
                        job_id = unknown.job_id.value,
                        %error,
                        "resume enqueue outcome is unknown"
                    );
                    return Ok(self
                        .create_unknown_enqueue_error_stream(
                            &session,
                            &consumed.canonical_session_id,
                        )
                        .await);
                }
                consumed.reopen(&*self.session_manager).await;
                return Err(AgUiError::Internal(anyhow::anyhow!(
                    "Failed to enqueue workflow resume: {error}"
                )));
            }
        };

        let (result_future, result_stream, result_wait_abort) =
            result_handle.into_parts_with_result_wait_abort_handle();
        let Some(result_stream) = result_stream else {
            // The job was accepted, so reopening the interrupts could run the
            // checkpoint twice. Stop the unobservable job and fail this run instead.
            if let Err(error) = self.app_module.job_app.delete_job(&job_id).await {
                tracing::error!(
                    job_id = job_id.value,
                    %error,
                    "Failed to cancel a resume job that has no response stream"
                );
            }
            self.session_manager
                .set_session_state(&session.session_id, SessionState::Error)
                .await;
            return Err(AgUiError::Internal(anyhow::anyhow!(
                "Workflow resume worker returned no response stream"
            )));
        };
        let result_stream = workflow_function_result_stream(result_stream, result_future);

        // Stored only after the continuation job is accepted, so a rejected resume
        // leaves no events behind for a retry under the same runId.
        let started_event =
            AgUiEvent::run_started(run_id.to_string(), session.thread_id.to_string());
        let started_event_id = Self::encode_event_with_logging(&self.encoder, &started_event);
        self.event_store
            .store_event(run_id, started_event_id, started_event.clone())
            .await;

        let session_id = session.session_id.clone();
        let workflow_name = hitl_info.workflow_name.clone();
        self.job_registry
            .write()
            .await
            .insert(run_id.to_string(), job_id);

        let hitl_ctx = crate::handler::HitlStreamContext {
            workflow_context: hitl_info.workflow_context.clone(),
            registered_worker_name: hitl_info.registered_worker_name.clone(),
            execution_id: hitl_info.execution_id.clone(),
            canonical_session_id: Some(canonical_session_id),
        };

        let event_stream = Self::create_event_stream_inner(
            result_stream,
            adapter,
            run_id.to_string(),
            session_id,
            workflow_name,
            false,
            self.event_store.clone(),
            self.encoder.clone(),
            self.session_manager.clone(),
            self.job_registry.clone(),
            hitl_ctx,
        );

        let (sender, receiver) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            let _result_wait_abort = result_wait_abort;
            let _ = sender.send((started_event_id, started_event)).await;
            futures::pin_mut!(event_stream);
            while let Some(event) = event_stream.next().await {
                let _ = sender.send(event).await;
            }
        });

        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(
            receiver,
        )))
    }

    async fn create_unknown_enqueue_error_stream(
        &self,
        session: &Session,
        canonical_session_id: &str,
    ) -> Pin<Box<dyn Stream<Item = (u64, AgUiEvent)> + Send + 'static>> {
        let run_id = session.run_id.to_string();
        let thread_id = session.thread_id.to_string();
        let started = AgUiEvent::run_started(run_id.clone(), thread_id);
        let started_id = Self::encode_event_with_logging(&self.encoder, &started);
        let error = AgUiEvent::run_error(
            run_id.clone(),
            "Workflow resume enqueue outcome is unknown; do not automatically retry this interrupt",
            Some("ENQUEUE_OUTCOME_UNKNOWN".to_string()),
            None,
        );
        let error_id = Self::encode_event_with_logging(&self.encoder, &error);

        self.session_manager
            .set_session_state(&session.session_id, SessionState::Error)
            .await;
        self.session_manager
            .set_session_state(canonical_session_id, SessionState::Error)
            .await;
        self.event_store
            .store_event(&run_id, started_id, started.clone())
            .await;
        self.event_store
            .store_event(&run_id, error_id, error.clone())
            .await;

        Box::pin(tokio_stream::iter(vec![
            (started_id, started),
            (error_id, error),
        ]))
    }

    /// Parse workflow schema from input context.
    /// Returns (workflow_schema, workflow_context, registered_worker_name) tuple.
    /// `registered_worker_name` is `Some` when using `WorkflowDefinition` (registered worker).
    async fn parse_workflow_from_input(
        &self,
        _input: &RunAgentInput,
        workflow_name: &str,
    ) -> Result<(
        Arc<WorkflowSchema>,
        Option<serde_json::Value>,
        crate::job_queue::LiveStreamWorker,
    )> {
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

        let worker = crate::job_queue::LiveStreamWorker::try_new(workflow_name, worker).map_err(
            |error| {
                AgUiError::Internal(anyhow::anyhow!(
                    "Workflow worker '{workflow_name}' cannot run: {error}"
                ))
            },
        )?;

        let (schema, settings_ctx) = crate::handler::decode_workflow_from_runner_settings(
            &worker.data().runner_settings,
            workflow_name,
            &self.app_module,
        )
        .await?;
        Ok((schema, settings_ctx, worker))
    }

    /// Build WorkflowRunArgs for initial execution.
    fn build_workflow_run_args(
        &self,
        input: &RunAgentInput,
        workflow: &WorkflowSchema,
        run_id: &str,
        workflow_context: Option<serde_json::Value>,
    ) -> Result<WorkflowRunArgs> {
        use jobworkerp_runner::jobworkerp::runner::workflow_run_args::WorkflowSource;

        let workflow_json = serde_json::to_string(workflow)
            .map_err(|e| AgUiError::InvalidInput(format!("Failed to serialize workflow: {}", e)))?;

        let input_json = serde_json::json!({
            "threadId": input.thread_id,
            "runId": input.run_id,
            "parentRunId": input.parent_run_id,
            "protocolVersion": input.protocol_version,
            "messages": input.messages,
            "tools": input.tools,
            "context": input.context,
            "state": input.state,
            "forwardedProps": input.forwarded_props,
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

        Ok(WorkflowRunArgs {
            workflow_source: Some(WorkflowSource::WorkflowData(workflow_json)),
            input: serde_json::to_string(&input_json).unwrap_or_else(|_| "{}".to_string()),
            workflow_context: workflow_context_str,
            execution_id: Some(run_id.to_string()),
            from_checkpoint: None,
        })
    }

    /// Build WorkflowRunArgs for HITL resume.
    fn build_resume_workflow_args(
        &self,
        hitl_info: &HitlWaitingInfo,
        checkpoint: CheckPointContext,
        user_input: serde_json::Value,
        fallback_execution_id: &str,
    ) -> WorkflowRunArgs {
        use jobworkerp_runner::jobworkerp::runner::workflow_run_args::{
            Checkpoint,
            checkpoint::{CheckPointContext, TaskCheckPointContext, WorkflowCheckPointContext},
        };

        let mut workflow_context_variables = checkpoint.workflow.context_variables.as_ref().clone();
        workflow_context_variables.insert("user_input".to_string(), user_input.clone());
        let checkpoint_input = checkpoint.workflow.input.as_ref();
        let checkpoint_task_input = checkpoint.task.input.as_ref();
        let checkpoint_task_context = checkpoint.task.context_variables.as_ref();

        let checkpoint = Checkpoint {
            position: checkpoint.position.as_json_pointer(),
            data: Some(CheckPointContext {
                workflow: Some(WorkflowCheckPointContext {
                    name: checkpoint.workflow.name.clone(),
                    input: serde_json::to_string(checkpoint_input)
                        .unwrap_or_else(|_| "{}".to_string()),
                    context_variables: serde_json::to_string(&workflow_context_variables)
                        .unwrap_or_else(|_| "{}".to_string()),
                }),
                task: Some(TaskCheckPointContext {
                    input: serde_json::to_string(checkpoint_task_input)
                        .unwrap_or_else(|_| "{}".to_string()),
                    output: serde_json::to_string(&user_input).unwrap_or_else(|_| "{}".to_string()),
                    context_variables: serde_json::to_string(checkpoint_task_context)
                        .unwrap_or_else(|_| "{}".to_string()),
                    flow_directive: "continue".to_string(),
                }),
            }),
        };

        let workflow_context = hitl_info
            .workflow_context
            .as_ref()
            .map(|value| serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string()));

        WorkflowRunArgs {
            workflow_source: None,
            input: serde_json::to_string(checkpoint_input).unwrap_or_else(|_| "{}".to_string()),
            workflow_context,
            execution_id: Some(
                hitl_info
                    .execution_id
                    .clone()
                    .unwrap_or_else(|| fallback_execution_id.to_string()),
            ),
            from_checkpoint: Some(checkpoint),
        }
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
        hitl_ctx: crate::handler::HitlStreamContext,
    ) -> impl Stream<Item = (u64, AgUiEvent)> + Send + 'a {
        let state_session_id = hitl_ctx
            .canonical_session_id
            .clone()
            .unwrap_or_else(|| session_id.clone());
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
            let mut has_interrupt = false;
            let mut prev_position: Option<String> = None;
            let mut public_result: Option<serde_json::Value> = None;

            tokio::pin!(result_stream);

            while let Some(result) = result_stream.next().await {
                if has_interrupt {
                    continue;
                }
                match result {
                    Ok(function_result) => {
                        // Empty output with last_info indicates stream end
                        if function_result.output.is_empty() {
                            if function_result.last_info.is_some() {
                                // Stream completed normally
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

                        // Keep workflow position private until the public output is validated.
                        if prev_position.as_ref() != Some(&workflow_result.position) {
                            prev_position = Some(workflow_result.position.clone());
                        }

                        // Handle workflow status
                        match ProtoWorkflowStatus::try_from(workflow_result.status) {
                            Ok(ProtoWorkflowStatus::Waiting) => {
                                // Do not expose internal HUMAN_INPUT arguments as a client tool call.
                                let tool_call_id = format!("wait_{}", run_id);

                                // Persist on the session so a resumed stream does not re-emit the
                                // bounded START–ARGS pair for this HUMAN_INPUT wait.
                                session_manager
                                    .record_emitted_tool_call_id(&state_session_id, &tool_call_id)
                                    .await;

                                // Save HITL info
                                let mut hitl_info = HitlWaitingInfo::new(
                                    tool_call_id.clone(),
                                    workflow_result.position.clone(),
                                    workflow_name.clone(),
                                    vec![], // Traditional HITL (HUMAN_INPUT)
                                    hitl_ctx.workflow_context.clone(),
                                    hitl_ctx.registered_worker_name.clone(),
                                );
                                hitl_ctx.attach_execution_id(&mut hitl_info);
                                let interrupt_id = hitl_info.interrupt_id.clone();
                                if !session_manager
                                    .set_paused_with_hitl_info(&state_session_id, hitl_info)
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
                                        .set_session_state(&state_session_id, SessionState::Error)
                                        .await;
                                    crate::handler::finish_request_session(
                                        &*session_manager,
                                        &session_id,
                                        &state_session_id,
                                        SessionState::Error,
                                    )
                                    .await;
                                    return;
                                }

                                let event = AgUiEvent::run_finished_with_interrupts(
                                    run_id.clone(),
                                    session_manager.get_session(&state_session_id).await.map(|s| s.thread_id.to_string()).unwrap_or_default(),
                                    vec![crate::events::types::AgUiInterrupt {
                                        id: interrupt_id,
                                        reason: "workflow_waiting".to_string(),
                                    }],
                                );
                                let event_id = Self::encode_event_with_logging(&encoder, &event);
                                event_store.store_event(&run_id, event_id, event.clone()).await;
                                yield (event_id, event);
                                has_interrupt = true;

                                tracing::info!(
                                    "Workflow paused for HITL, session {} is now Paused",
                                    session_id
                                );
                            }
                            Ok(ProtoWorkflowStatus::Completed) => {
                                let output = serde_json::from_str(&workflow_result.output)
                                    .unwrap_or_else(|_| serde_json::Value::String(workflow_result.output.clone()));
                                match crate::projection::project_workflow_output(&output) {
                                    Ok(projection) => {
                                        public_result = projection.result;
                                        for event in projection.events {
                                            let event_id = Self::encode_event_with_logging(&encoder, &event);
                                            event_store.store_event(&run_id, event_id, event.clone()).await;
                                            yield (event_id, event);
                                        }
                                    }
                                    Err(_) => {
                                        has_error = true;
                                        let adapter_lock = adapter.lock().await;
                                        let event = adapter_lock.workflow_error("Workflow output could not be published", "INVALID_PUBLIC_OUTPUT", None);
                                        drop(adapter_lock);
                                        let event_id = Self::encode_event_with_logging(&encoder, &event);
                                        event_store.store_event(&run_id, event_id, event.clone()).await;
                                        yield (event_id, event);
                                        session_manager.set_session_state(&state_session_id, SessionState::Error).await;
                                        crate::handler::finish_request_session(&*session_manager, &session_id, &state_session_id, SessionState::Error).await;
                                    }
                                }
                            }
                            Ok(ProtoWorkflowStatus::Faulted) => {
                                has_error = true;
                                let adapter_lock = adapter.lock().await;
                                let event = adapter_lock.workflow_error(
                                    "Workflow execution failed",
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
                                    .set_session_state(&state_session_id, SessionState::Error)
                                    .await;
                                crate::handler::finish_request_session(&*session_manager, &session_id, &state_session_id, SessionState::Error).await;
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
                                    .set_session_state(&state_session_id, SessionState::Cancelled)
                                    .await;
                                crate::handler::finish_request_session(&*session_manager, &session_id, &state_session_id, SessionState::Cancelled).await;
                                break;
                            }
                            _ => {
                                // Intermediate workflow status remains server-side.
                            }
                        }
                    }
                    Err(_error) => {
                        // Error from FunctionApp
                        has_error = true;

                        let adapter_lock = adapter.lock().await;
                        let event = adapter_lock.workflow_error(
                            "Workflow execution failed",
                            "FUNCTION_ERROR",
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
                        crate::handler::finish_request_session(
                            &*session_manager,
                            &session_id,
                            &state_session_id,
                            SessionState::Error,
                        )
                        .await;
                        break;
                    }
                }
            }

            // Emit RUN_FINISHED if no error
            if !has_error && !has_interrupt {
                let adapter_lock = adapter.lock().await;
                let event = adapter_lock.workflow_completed(public_result);
                let event_id = Self::encode_event_with_logging(&encoder, &event);
                event_store
                    .store_event(&run_id, event_id, event.clone())
                    .await;
                yield (event_id, event);
                session_manager
                    .set_session_state(&state_session_id, SessionState::Completed)
                    .await;
                crate::handler::finish_request_session(
                    &*session_manager,
                    &session_id,
                    &state_session_id,
                    SessionState::Completed,
                )
                .await;
            } else if has_interrupt {
                crate::handler::finish_request_session(
                    &*session_manager,
                    &session_id,
                    &state_session_id,
                    SessionState::Completed,
                )
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
            app_wrapper_module: self.app_wrapper_module.clone(),
            session_manager: self.session_manager.clone(),
            event_store: self.event_store.clone(),
            config: self.config.clone(),
            encoder: self.encoder.clone(),
            job_registry: self.job_registry.clone(),
            direct_runs: self.direct_runs.clone(),
        }
    }
}

fn workflow_function_result_stream(
    mut stream: futures::stream::BoxStream<'static, proto::jobworkerp::data::ResultOutputItem>,
    result_future: futures::future::BoxFuture<
        'static,
        anyhow::Result<Option<proto::jobworkerp::data::JobResult>>,
    >,
) -> Pin<Box<dyn Stream<Item = anyhow::Result<FunctionResult>> + Send>> {
    Box::pin(stream! {
        let mut saw_data = false;
        let mut saw_end = false;

        while let Some(item) = stream.next().await {
            match item.item {
                Some(proto::jobworkerp::data::result_output_item::Item::Data(bytes)) => {
                    let workflow_result = match WorkflowResult::decode(bytes.as_slice()) {
                        Ok(result) => result,
                        Err(error) => {
                            yield Err(anyhow::anyhow!("Invalid workflow result stream item: {error}"));
                            return;
                        }
                    };
                    let output = match serde_json::to_string(&workflow_result) {
                        Ok(output) => output,
                        Err(error) => {
                            yield Err(anyhow::anyhow!("Failed to serialize workflow result: {error}"));
                            return;
                        }
                    };
                    saw_data = true;
                    yield Ok(FunctionResult {
                        output,
                        status: Some(proto::jobworkerp::data::ResultStatus::Success as i32),
                        ..Default::default()
                    });
                }
                Some(proto::jobworkerp::data::result_output_item::Item::End(trailer)) => {
                    if let Err(error) = proto::stream_error::ensure_end_success(&trailer) {
                        yield Err(error);
                        return;
                    }
                    saw_end = true;
                    break;
                }
                Some(proto::jobworkerp::data::result_output_item::Item::FinalCollected(_))
                | None => {}
            }
        }

        if !saw_end || !saw_data {
            yield Err(anyhow::anyhow!("Workflow stream ended without a result"));
            return;
        }

        match result_future.await {
            Ok(Some(result)) => {
                if let Err(error) = app::app::job_result::ensure_successful_job_result(&result) {
                    yield Err(error);
                }
            }
            Ok(None) => yield Err(anyhow::anyhow!("Workflow stream completed without a job result")),
            Err(error) => yield Err(error),
        }
    })
}
