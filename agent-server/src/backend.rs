//! Production adapter between the HTTP contract and the model-independent orchestrator.

use crate::chat::{
    CapabilityIssuer, ChatMessage as DomainMessage, ChatOrchestrator, ChatProgress,
    ChatRequest as DomainRequest, ChatStatus as DomainStatus, ModelOptions, OsCapabilityIssuer,
    ResumeRequest as DomainResumeRequest, Role, ToolCall as DomainToolCall,
    ToolResult as DomainToolResult,
};
use crate::http::{
    self, ApprovalDecision, BackendChatStream, BackendError, CancelRequest, CancelResponse,
    ChatContent, ChatMessage, ChatOptions, ChatRequest, ChatResponse, ChatStatus, ChatStreamEvent,
    HttpBackend, MessageRole, PendingCall, ResumeRequest, SkillDetail, SkillDiagnostic,
    SkillDiagnosticScope, SkillListResponse, SkillReloadResponse, SkillSummary, ToolDeleteResponse,
    ToolListResponse, ToolRecord, ToolResult, ToolUpsertRequest,
};
use crate::skills::SkillCatalog;
use crate::tool_registry::{ToolRegistration, ToolRegistry};
use async_trait::async_trait;
use futures_util::stream;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{
    Arc, Mutex, MutexGuard,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::{sync::mpsc, time::Instant};

const DEFAULT_APPROVAL_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_SESSION_SWEEP_INTERVAL: Duration = Duration::from_secs(1);
const MIN_SESSION_SWEEP_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Clone, Default)]
struct StreamedTextTurn {
    delivered_prefix: String,
    stopped: bool,
}

/// Builds chat orchestrators. Implementations must not invoke the model while constructing them;
/// model selection is provided to each domain request and persisted with pending approvals.
#[async_trait]
pub trait ChatOrchestratorFactory: Send + Sync + 'static {
    async fn create(
        &self,
        llm_worker_id: i64,
        options: Option<ChatOptions>,
    ) -> Result<Arc<ChatOrchestrator>, String>;

    /// Create an unbound orchestrator for restoring a pending approval after process restart.
    /// Model selection must come only from the orchestrator's stored continuation, never from the
    /// resume request.
    async fn create_for_resume(&self) -> Result<Arc<ChatOrchestrator>, String>;
}

#[derive(Clone)]
struct ChatSession {
    orchestrator: Arc<ChatOrchestrator>,
    cancel_capability: String,
    awaiting_cleanup: Arc<AtomicBool>,
    active_uses: Arc<AtomicUsize>,
    orphaned: Arc<AtomicBool>,
    cleanup_started: Arc<AtomicBool>,
    approval_expires_at: Option<Instant>,
}

impl ChatSession {
    fn acquire_use(&self) -> ActiveSessionUse {
        self.active_uses.fetch_add(1, Ordering::AcqRel);
        ActiveSessionUse(self.active_uses.clone())
    }
}

struct ActiveSessionUse(Arc<AtomicUsize>);

impl Drop for ActiveSessionUse {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

struct SessionExecutionGuard {
    sessions: Arc<Mutex<HashMap<String, ChatSession>>>,
    chat_id: String,
    session: ChatSession,
    active_use: Option<ActiveSessionUse>,
    armed: bool,
}

impl SessionExecutionGuard {
    fn new(
        sessions: Arc<Mutex<HashMap<String, ChatSession>>>,
        chat_id: String,
        session: ChatSession,
        active_use: ActiveSessionUse,
    ) -> Self {
        Self {
            sessions,
            chat_id,
            session,
            active_use: Some(active_use),
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
        // Releasing the lease under the session lock makes re-entry observe the fully processed
        // result (including a refreshed approval deadline or a retained owned job).
        let _sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.active_use.take();
    }

    fn start_orphan_cleanup_if_idle(&self) {
        if self.session.active_uses.load(Ordering::Acquire) != 0
            || !self.session.orphaned.load(Ordering::Acquire)
            || self
                .session
                .cleanup_started
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return;
        }

        self.session.awaiting_cleanup.store(true, Ordering::Release);
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.session.cleanup_started.store(false, Ordering::Release);
            return;
        };
        let sessions = self.sessions.clone();
        let chat_id = self.chat_id.clone();
        let session = self.session.clone();
        runtime.spawn(async move {
            let mut retry_delay = Duration::from_millis(100);
            loop {
                if !AgentBackend::session_is_stored(&sessions, &chat_id, &session) {
                    return;
                }
                match session
                    .orchestrator
                    .cancel_all_jobs(&chat_id, &session.cancel_capability)
                    .await
                {
                    Ok(true) => {
                        AgentBackend::remove_session_if_same(&sessions, &chat_id, &session);
                        return;
                    }
                    Ok(false) if !session.orchestrator.has_pending_jobs(&chat_id) => {
                        AgentBackend::remove_session_if_same(&sessions, &chat_id, &session);
                        return;
                    }
                    Ok(false) | Err(_) => {
                        tokio::time::sleep(retry_delay).await;
                        retry_delay = retry_delay.saturating_mul(2).min(Duration::from_secs(30));
                    }
                }
            }
        });
    }
}

impl Drop for SessionExecutionGuard {
    fn drop(&mut self) {
        if self.active_use.is_none() {
            return;
        }
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.armed {
            let stored = sessions.get(&self.chat_id).is_some_and(|stored| {
                Arc::ptr_eq(&stored.orchestrator, &self.session.orchestrator)
            });
            if stored {
                if self.session.orchestrator.has_owned_jobs(&self.chat_id) {
                    // No successor can acquire this session until cleanup completes.
                    self.session.orphaned.store(true, Ordering::Release);
                } else {
                    sessions.remove(&self.chat_id);
                }
            }
        }
        self.active_use.take();
        drop(sessions);
        self.start_orphan_cleanup_if_idle();
    }
}

/// Concrete HTTP backend for the independent Agent Server.
///
/// Model selection is carried by each request and persisted in the orchestrator's continuation,
/// allowing pending approvals to be restored by an unbound orchestrator after process restart.
pub struct AgentBackend {
    factory: Arc<dyn ChatOrchestratorFactory>,
    skills: Arc<SkillCatalog>,
    tools: Arc<ToolRegistry>,
    capability_issuer: Arc<dyn CapabilityIssuer>,
    approval_ttl: Duration,
    sessions: Arc<Mutex<HashMap<String, ChatSession>>>,
    sweeper_started: AtomicBool,
}

impl AgentBackend {
    pub fn new(
        factory: Arc<dyn ChatOrchestratorFactory>,
        skills: Arc<SkillCatalog>,
        tools: Arc<ToolRegistry>,
    ) -> Self {
        Self::with_approval_ttl(factory, skills, tools, DEFAULT_APPROVAL_TTL)
    }

    pub fn with_capability_issuer(
        factory: Arc<dyn ChatOrchestratorFactory>,
        skills: Arc<SkillCatalog>,
        tools: Arc<ToolRegistry>,
        capability_issuer: Arc<dyn CapabilityIssuer>,
    ) -> Self {
        Self::with_capability_issuer_and_approval_ttl(
            factory,
            skills,
            tools,
            capability_issuer,
            DEFAULT_APPROVAL_TTL,
        )
    }

    /// Construct a backend whose in-memory approval-session lifetime matches its approval store.
    pub fn with_approval_ttl(
        factory: Arc<dyn ChatOrchestratorFactory>,
        skills: Arc<SkillCatalog>,
        tools: Arc<ToolRegistry>,
        approval_ttl: Duration,
    ) -> Self {
        Self::with_capability_issuer_and_approval_ttl(
            factory,
            skills,
            tools,
            Arc::new(OsCapabilityIssuer),
            approval_ttl,
        )
    }

    /// Construct a backend with explicit capability generation and approval-session lifetimes.
    pub fn with_capability_issuer_and_approval_ttl(
        factory: Arc<dyn ChatOrchestratorFactory>,
        skills: Arc<SkillCatalog>,
        tools: Arc<ToolRegistry>,
        capability_issuer: Arc<dyn CapabilityIssuer>,
        approval_ttl: Duration,
    ) -> Self {
        Self {
            factory,
            skills,
            tools,
            capability_issuer,
            approval_ttl,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            sweeper_started: AtomicBool::new(false),
        }
    }

    fn ensure_session_sweeper(&self) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            // Constructors and test adapters can be used without a Tokio runtime. The first
            // asynchronous chat request will start the sweeper once a runtime is available.
            return;
        };
        if self
            .sweeper_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        // The detached task owns only a weak reference. It periodically upgrades that reference
        // for one sweep, then drops it before waiting again, so it cannot extend backend lifetime.
        let sessions = Arc::downgrade(&self.sessions);
        let sweep_interval = self
            .approval_ttl
            .min(MAX_SESSION_SWEEP_INTERVAL)
            .max(MIN_SESSION_SWEEP_INTERVAL);
        runtime.spawn(async move {
            let mut interval = tokio::time::interval(sweep_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                let Some(sessions) = sessions.upgrade() else {
                    break;
                };
                Self::sweep_expired_sessions(&sessions, Instant::now());
                drop(sessions);
            }
        });
    }

    async fn create_session(
        &self,
        request: &ChatRequest,
        streaming: bool,
    ) -> Result<(String, ChatSession, i64, ModelOptions, Vec<DomainMessage>), BackendError> {
        self.ensure_session_sweeper();
        let (llm_worker_id, options, history) = request_to_domain(request, streaming)?;
        let chat_id = self.issue_capability()?;
        let orchestrator = self
            .factory
            .create(llm_worker_id, request.options.clone())
            .await
            .map_err(BackendError::new)?;
        let cancel_capability = orchestrator
            .prepare_chat_execution(&chat_id)
            .map_err(|error| BackendError::new(error.to_string()))?;
        let session = ChatSession {
            orchestrator,
            cancel_capability,
            awaiting_cleanup: Arc::new(AtomicBool::new(false)),
            active_uses: Arc::new(AtomicUsize::new(0)),
            orphaned: Arc::new(AtomicBool::new(false)),
            cleanup_started: Arc::new(AtomicBool::new(false)),
            approval_expires_at: None,
        };
        if chat_id == session.cancel_capability {
            return Err(BackendError::new(
                "the issuer returned the same chat identifier and cancel capability",
            ));
        }
        let mut sessions = self.lock_sessions();
        if sessions.contains_key(&chat_id) {
            return Err(BackendError::new(
                "the capability issuer produced a duplicate chat identifier",
            ));
        }
        sessions.insert(chat_id.clone(), session.clone());
        Ok((chat_id, session, llm_worker_id, options, history))
    }

    fn issue_capability(&self) -> Result<String, BackendError> {
        let capability = self.capability_issuer.issue().map_err(BackendError::new)?;
        if capability.is_empty()
            || capability.len() > 128
            || !capability
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(BackendError::new(
                "the capability issuer returned an invalid value",
            ));
        }
        Ok(capability)
    }

    async fn create_resume_session(&self, chat_id: &str) -> Result<ChatSession, BackendError> {
        let orchestrator = self
            .factory
            .create_for_resume()
            .await
            .map_err(BackendError::new)?;
        let cancel_capability = orchestrator
            .prepare_chat_execution(chat_id)
            .map_err(|error| BackendError::new(error.to_string()))?;
        if chat_id == cancel_capability {
            return Err(BackendError::new(
                "the issuer returned the same chat identifier and cancel capability",
            ));
        }
        Ok(ChatSession {
            orchestrator,
            cancel_capability,
            awaiting_cleanup: Arc::new(AtomicBool::new(false)),
            active_uses: Arc::new(AtomicUsize::new(0)),
            orphaned: Arc::new(AtomicBool::new(false)),
            cleanup_started: Arc::new(AtomicBool::new(false)),
            approval_expires_at: None,
        })
    }

    fn session_for_resume(
        &self,
        chat_id: &str,
    ) -> Result<Option<(ChatSession, ActiveSessionUse)>, BackendError> {
        let mut sessions = self.lock_sessions();
        let expired_and_idle = sessions.get(chat_id).is_some_and(|session| {
            session
                .approval_expires_at
                .is_some_and(|expires_at| expires_at <= Instant::now())
                && session.active_uses.load(Ordering::Acquire) == 0
                && !session.orchestrator.has_pending_jobs(chat_id)
        });
        if expired_and_idle {
            sessions.remove(chat_id);
        }
        let Some(session) = sessions.get(chat_id).cloned() else {
            return Ok(None);
        };
        if session.active_uses.load(Ordering::Acquire) != 0
            || session.orphaned.load(Ordering::Acquire)
        {
            return Err(BackendError::new(
                "a chat execution or cleanup is already active",
            ));
        }
        let active_use = session.acquire_use();
        Ok(Some((session, active_use)))
    }

    fn remember_session_for_resume(
        &self,
        chat_id: &str,
        session: ChatSession,
    ) -> Result<(ChatSession, ActiveSessionUse), BackendError> {
        let mut sessions = self.lock_sessions();
        if sessions.get(chat_id).is_some_and(|stored| {
            stored.active_uses.load(Ordering::Acquire) != 0
                || stored.orphaned.load(Ordering::Acquire)
        }) {
            return Err(BackendError::new(
                "a chat execution or cleanup is already active",
            ));
        }
        let session = sessions
            .entry(chat_id.to_owned())
            .or_insert(session)
            .clone();
        let active_use = session.acquire_use();
        Ok((session, active_use))
    }

    fn lock_sessions(&self) -> MutexGuard<'_, HashMap<String, ChatSession>> {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn remove_session(sessions: &Mutex<HashMap<String, ChatSession>>, chat_id: &str) {
        sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(chat_id);
    }

    fn session_is_stored(
        sessions: &Mutex<HashMap<String, ChatSession>>,
        chat_id: &str,
        session: &ChatSession,
    ) -> bool {
        sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(chat_id)
            .is_some_and(|stored| Arc::ptr_eq(&stored.orchestrator, &session.orchestrator))
    }

    fn remove_session_if_same(
        sessions: &Mutex<HashMap<String, ChatSession>>,
        chat_id: &str,
        session: &ChatSession,
    ) {
        let mut sessions = sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if sessions
            .get(chat_id)
            .is_some_and(|stored| Arc::ptr_eq(&stored.orchestrator, &session.orchestrator))
        {
            sessions.remove(chat_id);
        }
    }

    fn mark_approval_expiry(
        sessions: &Mutex<HashMap<String, ChatSession>>,
        chat_id: &str,
        session: &ChatSession,
        approval_ttl: Duration,
    ) {
        let expires_at = Instant::now() + approval_ttl;
        let mut sessions = sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(stored) = sessions.get_mut(chat_id)
            && Arc::ptr_eq(&stored.orchestrator, &session.orchestrator)
        {
            // A newly issued approval owns a fresh store deadline, so its in-memory session gets
            // a matching fresh deadline as well.
            stored.approval_expires_at = Some(expires_at);
        }
    }

    fn update_session_after_successful_response(
        sessions: &Mutex<HashMap<String, ChatSession>>,
        chat_id: &str,
        session: &ChatSession,
        status: DomainStatus,
        approval_ttl: Duration,
    ) {
        match status {
            DomainStatus::Completed => Self::finish_session(sessions, chat_id, session),
            DomainStatus::ApprovalRequired => {
                Self::mark_approval_expiry(sessions, chat_id, session, approval_ttl);
            }
        }
    }

    fn sweep_expired_sessions(sessions: &Mutex<HashMap<String, ChatSession>>, now: Instant) {
        let mut sessions = sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sessions.retain(|chat_id, session| {
            let expired = session
                .approval_expires_at
                .is_some_and(|expires_at| expires_at <= now);
            let in_use = session.active_uses.load(Ordering::Acquire) != 0;
            !expired || in_use || session.orchestrator.has_pending_jobs(chat_id)
        });
    }

    fn finish_session(
        sessions: &Mutex<HashMap<String, ChatSession>>,
        chat_id: &str,
        session: &ChatSession,
    ) {
        // A result wait can be uncertain even when the chat completes. Retain the executor,
        // job ID, and capability until the caller can retry Delete; do not strand a live job.
        // Set the marker before checking the tracker: a concurrent successful Delete must see
        // it and remove the session, even if it races with this error-handling path.
        session.awaiting_cleanup.store(true, Ordering::Release);
        if !session.orchestrator.has_pending_jobs(chat_id) {
            Self::remove_session(sessions, chat_id);
        }
    }
}

#[async_trait]
impl HttpBackend for AgentBackend {
    async fn start_chat(&self, request: ChatRequest) -> Result<ChatResponse, BackendError> {
        let (chat_id, session, llm_worker_id, options, history) =
            self.create_session(&request, false).await?;
        let mut execution_guard = SessionExecutionGuard::new(
            self.sessions.clone(),
            chat_id.clone(),
            session.clone(),
            session.acquire_use(),
        );
        let result = session
            .orchestrator
            .chat(DomainRequest {
                chat_id: chat_id.clone(),
                llm_worker_id,
                options,
                history,
            })
            .await;
        let output = match result {
            Ok(response) => {
                let status = response.status;
                match domain_response_to_http(response, &chat_id, &session.cancel_capability) {
                    Ok(mapped) => {
                        Self::update_session_after_successful_response(
                            &self.sessions,
                            &chat_id,
                            &session,
                            status,
                            self.approval_ttl,
                        );
                        Ok(mapped)
                    }
                    Err(error) => {
                        let recoverable = session.orchestrator.has_pending_jobs(&chat_id);
                        Self::finish_session(&self.sessions, &chat_id, &session);
                        if recoverable {
                            Err(BackendError::with_chat_cancel(
                                error.to_string(),
                                chat_id,
                                session.cancel_capability.clone(),
                            ))
                        } else {
                            Err(error)
                        }
                    }
                }
            }
            Err(error) => {
                let recoverable = session.orchestrator.has_pending_jobs(&chat_id);
                Self::finish_session(&self.sessions, &chat_id, &session);
                if recoverable {
                    Err(BackendError::with_chat_cancel(
                        error.to_string(),
                        chat_id,
                        session.cancel_capability.clone(),
                    ))
                } else {
                    Err(BackendError::new(error.to_string()))
                }
            }
        };
        execution_guard.disarm();
        output
    }

    async fn stream_chat(&self, request: ChatRequest) -> Result<BackendChatStream, BackendError> {
        let (chat_id, session, llm_worker_id, options, history) =
            self.create_session(&request, true).await?;
        let input_message_count = history.len();
        let (sender, receiver) = mpsc::channel(16);
        let progress_sender = sender.clone();
        let streamed_text_turns = Arc::new(Mutex::new(HashMap::<usize, StreamedTextTurn>::new()));
        let progress_streamed_text_turns = streamed_text_turns.clone();
        if let Err(error) = session.orchestrator.set_progress_callback(
            &chat_id,
            Arc::new(move |progress| {
                let event = match progress {
                    ChatProgress::TextDelta { turn, text } => {
                        let mut streamed = progress_streamed_text_turns
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let delivery = streamed.entry(turn).or_default();
                        if !delivery.stopped {
                            if progress_sender
                                .try_send(Ok(ChatStreamEvent::TextDelta { text: text.clone() }))
                                .is_ok()
                            {
                                delivery.delivered_prefix.push_str(&text);
                            } else {
                                // Once any delta is missed, later deltas cannot be delivered
                                // without creating a gap. Final response reconciliation resumes
                                // from the last successfully queued prefix instead.
                                delivery.stopped = true;
                            }
                        }
                        return;
                    }
                    ChatProgress::ToolCall(call) => ChatStreamEvent::ToolCall {
                        call_id: call.call_id,
                        name: call.name,
                        arguments: call.arguments,
                    },
                    ChatProgress::ToolResult(result) => ChatStreamEvent::ToolResult {
                        call_id: result.call_id,
                        result: result.content,
                    },
                };
                let _ = progress_sender.try_send(Ok(event));
            }),
        ) {
            Self::remove_session(&self.sessions, &chat_id);
            return Err(BackendError::new(error.to_string()));
        }
        let execution = StreamExecution {
            orchestrator: session.orchestrator.clone(),
            sessions: self.sessions.clone(),
            chat_id: chat_id.clone(),
            session: session.clone(),
            approval_ttl: self.approval_ttl,
            llm_worker_id,
            options,
            history,
            input_message_count,
            streamed_text_turns,
            sender,
        };

        // Keep orchestration independent of the response body's lifetime: closing an SSE
        // connection is not a cancellation request.
        tokio::spawn(run_stream_execution(execution));
        let events = stream::unfold(receiver, |mut receiver| async move {
            receiver.recv().await.map(|event| (event, receiver))
        });
        let backend_stream =
            BackendChatStream::new(chat_id, session.cancel_capability, Box::pin(events));
        Ok(backend_stream)
    }

    async fn resume_chat(
        &self,
        chat_id: String,
        request: ResumeRequest,
    ) -> Result<ChatResponse, BackendError> {
        if chat_id.trim().is_empty()
            || request.resume_capability.trim().is_empty()
            || request.decisions.len() != 1
        {
            return Err(BackendError::new("the resume request is invalid"));
        }
        let decision = request
            .decisions
            .into_iter()
            .next()
            .ok_or_else(|| BackendError::new("the resume request is invalid"))?;
        if decision.call_id.trim().is_empty() {
            return Err(BackendError::new("the resume request is invalid"));
        }
        self.ensure_session_sweeper();
        let (session, active_use, restored) = match self.session_for_resume(&chat_id)? {
            Some((session, active_use)) => (session, active_use, false),
            None => {
                let restored_session = self.create_resume_session(&chat_id).await?;
                let (session, active_use) =
                    self.remember_session_for_resume(&chat_id, restored_session)?;
                (session, active_use, true)
            }
        };
        let mut execution_guard = SessionExecutionGuard::new(
            self.sessions.clone(),
            chat_id.clone(),
            session.clone(),
            active_use,
        );
        let result = session
            .orchestrator
            .resume(DomainResumeRequest {
                chat_id: chat_id.clone(),
                call_id: decision.call_id,
                capability: request.resume_capability,
                approve: decision.decision == ApprovalDecision::Approve,
            })
            .await;
        let output = match result {
            Ok(response) => {
                let status = response.status;
                match domain_response_to_http(response, &chat_id, &session.cancel_capability) {
                    Ok(mapped) => {
                        Self::update_session_after_successful_response(
                            &self.sessions,
                            &chat_id,
                            &session,
                            status,
                            self.approval_ttl,
                        );
                        Ok(mapped)
                    }
                    Err(error) => {
                        Self::finish_session(&self.sessions, &chat_id, &session);
                        Err(error)
                    }
                }
            }
            Err(error) => {
                if session.orchestrator.has_pending_jobs(&chat_id) {
                    Self::finish_session(&self.sessions, &chat_id, &session);
                    if restored {
                        Err(BackendError::with_cancel_capability(
                            error.to_string(),
                            session.cancel_capability.clone(),
                        ))
                    } else {
                        Err(BackendError::new(error.to_string()))
                    }
                } else {
                    if restored {
                        // A restored session is registered before awaiting resume so it can be
                        // cancelled safely if the request future is dropped. An invalid or
                        // otherwise terminal resume has no live job to retain in that cache.
                        Self::remove_session_if_same(&self.sessions, &chat_id, &session);
                    }
                    Err(BackendError::new(error.to_string()))
                }
            }
        };
        execution_guard.disarm();
        output
    }

    async fn cancel_chat(
        &self,
        chat_id: String,
        request: CancelRequest,
    ) -> Result<CancelResponse, BackendError> {
        // Clone the session before awaiting so cancellation cannot hold the registry lock while
        // worker RPCs run or prevent concurrent session cleanup.
        let session = { self.lock_sessions().get(&chat_id).cloned() };
        let cancelled = if let Some(session) = session {
            let cancelled = session
                .orchestrator
                .cancel_all_jobs(&chat_id, &request.cancel_capability)
                .await
                .map_err(|error| BackendError::new(error.to_string()))?;
            if cancelled
                && session.awaiting_cleanup.load(Ordering::Acquire)
                && !session.orchestrator.has_pending_jobs(&chat_id)
            {
                Self::remove_session(&self.sessions, &chat_id);
            }
            cancelled
        } else {
            false
        };
        Ok(CancelResponse { chat_id, cancelled })
    }

    async fn list_skills(&self) -> Result<SkillListResponse, BackendError> {
        Ok(SkillListResponse {
            skills: self
                .skills
                .snapshot()
                .list()
                .into_iter()
                .map(|skill| SkillSummary {
                    name: skill.name,
                    description: skill.description,
                })
                .collect(),
        })
    }

    async fn skill_detail(&self, name: String) -> Result<SkillDetail, BackendError> {
        let snapshot = self.skills.snapshot();
        let activation = snapshot
            .activate(&name)
            .map_err(|error| BackendError::new(error.to_string()))?;
        let summary = snapshot
            .list()
            .into_iter()
            .find(|skill| skill.name == activation.name)
            .ok_or_else(|| BackendError::new("the skill disappeared from its snapshot"))?;
        Ok(SkillDetail {
            name: activation.name,
            description: summary.description,
            body: activation.content,
        })
    }

    async fn reload_skills(&self) -> Result<SkillReloadResponse, BackendError> {
        let report = self.skills.reload();
        Ok(SkillReloadResponse {
            loaded_count: report.skill_count,
            diagnostics: report
                .diagnostics
                .into_iter()
                .map(|diagnostic| SkillDiagnostic {
                    scope: if diagnostic.skill_name.is_some() {
                        SkillDiagnosticScope::Skill
                    } else {
                        SkillDiagnosticScope::Root
                    },
                    // Do not expose administrator-configured filesystem paths in the HTTP API.
                    name: diagnostic
                        .skill_name
                        .unwrap_or_else(|| "configured-root".to_owned()),
                    reason: diagnostic.message,
                })
                .collect(),
        })
    }

    async fn list_tools(&self) -> Result<ToolListResponse, BackendError> {
        let snapshot = self
            .tools
            .snapshot_for_request()
            .await
            .map_err(|error| BackendError::new(error.to_string()))?;
        let tools = snapshot
            .tools()
            .iter()
            .map(|(name, tool)| {
                registration_to_record(name, tool.registration(), tool.input_schema().clone())
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ToolListResponse { tools })
    }

    async fn upsert_tool(
        &self,
        name: String,
        request: ToolUpsertRequest,
    ) -> Result<ToolRecord, BackendError> {
        let worker_id = i64::try_from(request.worker_id)
            .map_err(|_| BackendError::new("the Worker ID is outside the supported range"))?;
        let registration = ToolRegistration {
            name: name.clone(),
            description: request.description,
            worker_id,
            using: request.method,
            requires_approval: request.requires_approval,
        };
        let exists = self
            .tools
            .list()
            .map_err(|error| BackendError::new(error.to_string()))?
            .iter()
            .any(|current| current.name == name);
        if exists {
            self.tools
                .update(registration)
                .await
                .map_err(|error| BackendError::new(error.to_string()))?;
        } else {
            self.tools
                .register(registration)
                .await
                .map_err(|error| BackendError::new(error.to_string()))?;
        }
        let snapshot = self
            .tools
            .snapshot_for_request()
            .await
            .map_err(|error| BackendError::new(error.to_string()))?;
        let tool = snapshot
            .get(&name)
            .ok_or_else(|| BackendError::new("the registered tool is unavailable"))?;
        registration_to_record(&name, tool.registration(), tool.input_schema().clone())
    }

    async fn delete_tool(&self, name: String) -> Result<ToolDeleteResponse, BackendError> {
        self.tools
            .remove(&name)
            .map_err(|error| BackendError::new(error.to_string()))?;
        Ok(ToolDeleteResponse {
            name,
            deleted: true,
        })
    }
}

struct StreamExecution {
    orchestrator: Arc<ChatOrchestrator>,
    sessions: Arc<Mutex<HashMap<String, ChatSession>>>,
    chat_id: String,
    session: ChatSession,
    approval_ttl: Duration,
    llm_worker_id: i64,
    options: ModelOptions,
    history: Vec<DomainMessage>,
    input_message_count: usize,
    streamed_text_turns: Arc<Mutex<HashMap<usize, StreamedTextTurn>>>,
    sender: mpsc::Sender<Result<ChatStreamEvent, BackendError>>,
}

async fn run_stream_execution(execution: StreamExecution) {
    let result = execution
        .orchestrator
        .chat(DomainRequest {
            chat_id: execution.chat_id.clone(),
            llm_worker_id: execution.llm_worker_id,
            options: execution.options,
            history: execution.history,
        })
        .await;
    match result {
        Ok(response) => {
            let status = response.status;
            let mapped = domain_response_to_http(
                response,
                &execution.chat_id,
                &execution.session.cancel_capability,
            );
            match mapped {
                Ok(response) => {
                    AgentBackend::update_session_after_successful_response(
                        &execution.sessions,
                        &execution.chat_id,
                        &execution.session,
                        status,
                        execution.approval_ttl,
                    );
                    let streamed_text_turns = execution
                        .streamed_text_turns
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    match response_events(
                        response,
                        execution.input_message_count,
                        &streamed_text_turns,
                    ) {
                        Ok(events) => send_events(&execution.sender, events).await,
                        Err(_) => {
                            let _ = execution.sender.send(Ok(ChatStreamEvent::Error {
                                code: "unsupported_chat_output".to_owned(),
                                message: "the orchestration result could not be represented by the HTTP contract".to_owned(),
                            })).await;
                            AgentBackend::finish_session(
                                &execution.sessions,
                                &execution.chat_id,
                                &execution.session,
                            );
                        }
                    }
                }
                Err(_) => {
                    AgentBackend::finish_session(
                        &execution.sessions,
                        &execution.chat_id,
                        &execution.session,
                    );
                    let _ = execution.sender.send(Ok(ChatStreamEvent::Error {
                        code: "unsupported_chat_output".to_owned(),
                        message: "the orchestration result could not be represented by the HTTP contract".to_owned(),
                    })).await;
                }
            }
        }
        Err(_) => {
            AgentBackend::finish_session(
                &execution.sessions,
                &execution.chat_id,
                &execution.session,
            );
            let _ = execution
                .sender
                .send(Ok(ChatStreamEvent::Error {
                    code: "orchestration_failed".to_owned(),
                    message: "the Agent Server could not complete the chat".to_owned(),
                }))
                .await;
        }
    }
}

async fn send_events(
    sender: &mpsc::Sender<Result<ChatStreamEvent, BackendError>>,
    events: Vec<ChatStreamEvent>,
) {
    for event in events {
        if sender.send(Ok(event)).await.is_err() {
            break;
        }
    }
}

fn request_to_domain(
    request: &ChatRequest,
    streaming: bool,
) -> Result<(i64, ModelOptions, Vec<DomainMessage>), BackendError> {
    if request.llm_worker_id == 0 || request.messages.is_empty() {
        return Err(BackendError::new(
            "a positive LLM Worker ID and at least one message are required",
        ));
    }
    if request
        .stream
        .is_some_and(|requested| requested != streaming)
    {
        return Err(BackendError::new(
            "the stream option conflicts with the selected route",
        ));
    }
    let llm_worker_id = i64::try_from(request.llm_worker_id)
        .map_err(|_| BackendError::new("the LLM Worker ID is outside the supported range"))?;
    let mut history = Vec::with_capacity(request.messages.len());
    let mut pending_calls: Option<HashMap<String, String>> = None;

    for message in &request.messages {
        if message.role == MessageRole::System {
            continue;
        }
        if message.tool_call_id.is_some() {
            return Err(BackendError::new(
                "message-level toolCallId cannot be represented by the chat domain",
            ));
        }
        let domain_message = match (&message.role, &message.content) {
            (MessageRole::User, ChatContent::Text(text)) => {
                if pending_calls.is_some() {
                    return Err(invalid_tool_history());
                }
                DomainMessage::new(Role::User, Value::String(text.text.clone()))
            }
            (MessageRole::User, ChatContent::Image(image)) => {
                if pending_calls.is_some() {
                    return Err(invalid_tool_history());
                }
                let content = serde_json::to_value(image)
                    .map_err(|_| BackendError::new("image content cannot be encoded"))?;
                DomainMessage::new(Role::User, content)
            }
            (MessageRole::Assistant, ChatContent::Text(text)) => {
                if pending_calls.is_some() {
                    return Err(invalid_tool_history());
                }
                DomainMessage::new(Role::Assistant, Value::String(text.text.clone()))
            }
            (MessageRole::Assistant, ChatContent::ToolCalls(calls)) => {
                if pending_calls.is_some() || calls.tool_calls.is_empty() {
                    return Err(invalid_tool_history());
                }
                let mut names = HashMap::new();
                let mut domain_calls = Vec::with_capacity(calls.tool_calls.len());
                for call in &calls.tool_calls {
                    if call.call_id.trim().is_empty()
                        || call.name.trim().is_empty()
                        || names
                            .insert(call.call_id.clone(), call.name.clone())
                            .is_some()
                    {
                        return Err(invalid_tool_history());
                    }
                    domain_calls.push(DomainToolCall {
                        call_id: call.call_id.clone(),
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    });
                }
                pending_calls = Some(names);
                let mut result = DomainMessage::new(Role::Assistant, Value::Null);
                result.tool_calls = domain_calls;
                result
            }
            (MessageRole::Tool, ChatContent::ToolResults(results)) => {
                let expected = pending_calls.take().ok_or_else(invalid_tool_history)?;
                if results.tool_results.is_empty() || expected.len() != results.tool_results.len() {
                    return Err(invalid_tool_history());
                }
                let mut seen = HashSet::new();
                let mut domain_results = Vec::with_capacity(results.tool_results.len());
                for result in &results.tool_results {
                    let Some(expected_name) = expected.get(&result.call_id) else {
                        return Err(invalid_tool_history());
                    };
                    if !seen.insert(result.call_id.as_str())
                        || result
                            .name
                            .as_ref()
                            .is_some_and(|name| name != expected_name)
                    {
                        return Err(invalid_tool_history());
                    }
                    domain_results.push(DomainToolResult {
                        call_id: result.call_id.clone(),
                        name: result.name.clone().unwrap_or_else(|| expected_name.clone()),
                        content: result.result.clone(),
                        is_error: result.is_error,
                    });
                }
                if seen.len() != expected.len() {
                    return Err(invalid_tool_history());
                }
                let mut result = DomainMessage::new(Role::Tool, Value::Null);
                result.tool_results = domain_results;
                result
            }
            _ => {
                return Err(BackendError::new(
                    "the message role and content cannot be represented by the chat domain",
                ));
            }
        };
        history.push(domain_message);
    }
    if pending_calls.is_some() || history.is_empty() {
        return Err(invalid_tool_history());
    }
    let options = request
        .options
        .as_ref()
        .map(|options| ModelOptions {
            temperature: options.temperature,
            top_p: options.top_p,
            max_tokens: options.max_tokens,
        })
        .unwrap_or_default();
    Ok((llm_worker_id, options, history))
}

fn invalid_tool_history() -> BackendError {
    BackendError::new("assistant tool calls and TOOL results are not a matching sequence")
}

fn domain_response_to_http(
    response: crate::chat::ChatResponse,
    expected_chat_id: &str,
    cancel_capability: &str,
) -> Result<ChatResponse, BackendError> {
    if response.chat_id != expected_chat_id {
        return Err(BackendError::new(
            "the orchestrator returned a response for a different chat",
        ));
    }
    let messages = domain_history_to_http(&response.history)?;
    let (status, resume_capability, pending_calls) = match response.status {
        DomainStatus::Completed if response.pending_approval.is_none() => {
            (ChatStatus::Completed, None, Vec::new())
        }
        DomainStatus::ApprovalRequired => {
            let pending = response
                .pending_approval
                .ok_or_else(|| BackendError::new("approval response has no pending call"))?;
            (
                ChatStatus::ApprovalRequired,
                Some(pending.resume_capability),
                vec![PendingCall {
                    call_id: pending.call_id,
                    name: pending.name,
                    arguments: pending.arguments,
                }],
            )
        }
        _ => {
            return Err(BackendError::new(
                "the orchestrator returned an inconsistent status",
            ));
        }
    };
    Ok(ChatResponse {
        chat_id: response.chat_id,
        status,
        messages,
        cancel_capability: Some(cancel_capability.to_owned()),
        resume_capability,
        pending_calls,
    })
}

fn domain_history_to_http(history: &[DomainMessage]) -> Result<Vec<ChatMessage>, BackendError> {
    let mut messages = Vec::with_capacity(history.len());
    let mut index = 0;
    while index < history.len() {
        if history[index].role != Role::Tool {
            if let Some(message) = domain_message_to_http(&history[index])? {
                messages.push(message);
            }
            index += 1;
            continue;
        }

        let mut results = Vec::new();
        while index < history.len() && history[index].role == Role::Tool {
            let Some(message) = domain_message_to_http(&history[index])? else {
                return Err(BackendError::new("TOOL results cannot be hidden"));
            };
            let ChatContent::ToolResults(content) = message.content else {
                return Err(BackendError::new("TOOL message projection is inconsistent"));
            };
            results.extend(content.tool_results);
            index += 1;
        }
        messages.push(ChatMessage {
            role: MessageRole::Tool,
            content: ChatContent::ToolResults(http::ToolResultsContent {
                tool_results: results,
            }),
            tool_call_id: None,
        });
    }
    Ok(messages)
}

fn domain_message_to_http(message: &DomainMessage) -> Result<Option<ChatMessage>, BackendError> {
    if message.tool_execution_requests.is_some()
        || value_contains_execution_request(&message.content)
        || message
            .tool_calls
            .iter()
            .any(|call| value_contains_execution_request(&call.arguments))
        || message
            .tool_results
            .iter()
            .any(|result| value_contains_execution_request(&result.content))
    {
        return Err(BackendError::new(
            "the chat history contains unsupported execution directives",
        ));
    }

    let (role, content) = match message.role {
        Role::System => {
            if !message.tool_calls.is_empty() || !message.tool_results.is_empty() {
                return Err(BackendError::new(
                    "system messages cannot contain tool calls or results",
                ));
            }
            return Ok(None);
        }
        Role::User => {
            if !message.tool_calls.is_empty() || !message.tool_results.is_empty() {
                return Err(BackendError::new("user messages cannot contain tool data"));
            }
            let content = if let Some(text) = message.content.as_str() {
                ChatContent::Text(http::TextContent {
                    text: text.to_owned(),
                })
            } else {
                ChatContent::Image(
                    serde_json::from_value::<http::ImageContent>(message.content.clone()).map_err(
                        |_| BackendError::new("user image cannot be represented as HTTP content"),
                    )?,
                )
            };
            (MessageRole::User, content)
        }
        Role::Assistant => {
            if !message.tool_results.is_empty() {
                return Err(BackendError::new(
                    "assistant messages cannot contain tool results",
                ));
            }
            if !message.tool_calls.is_empty() {
                if !message.content.is_null() {
                    return Err(BackendError::new(
                        "assistant text and tool calls cannot be represented in one HTTP message",
                    ));
                }
                let calls = message
                    .tool_calls
                    .iter()
                    .map(|call| http::AssistantToolCall {
                        call_id: call.call_id.clone(),
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    })
                    .collect();
                (
                    MessageRole::Assistant,
                    ChatContent::ToolCalls(http::ToolCallsContent { tool_calls: calls }),
                )
            } else {
                let text = message.content.as_str().ok_or_else(|| {
                    BackendError::new("assistant content cannot be represented as HTTP text")
                })?;
                (
                    MessageRole::Assistant,
                    ChatContent::Text(http::TextContent {
                        text: text.to_owned(),
                    }),
                )
            }
        }
        Role::Tool => {
            if !message.tool_calls.is_empty() || message.tool_results.is_empty() {
                return Err(BackendError::new(
                    "TOOL messages must contain tool results only",
                ));
            }
            let results = message
                .tool_results
                .iter()
                .map(|result| ToolResult {
                    call_id: result.call_id.clone(),
                    name: Some(result.name.clone()),
                    result: result.content.clone(),
                    is_error: result.is_error,
                })
                .collect();
            (
                MessageRole::Tool,
                ChatContent::ToolResults(http::ToolResultsContent {
                    tool_results: results,
                }),
            )
        }
    };
    Ok(Some(ChatMessage {
        role,
        content,
        tool_call_id: None,
    }))
}

fn response_events(
    response: ChatResponse,
    input_message_count: usize,
    streamed_text_turns: &HashMap<usize, StreamedTextTurn>,
) -> Result<Vec<ChatStreamEvent>, BackendError> {
    if input_message_count > response.messages.len() {
        return Err(BackendError::new(
            "the orchestrator removed messages from the conversation history",
        ));
    }
    let mut events = Vec::new();
    let mut turn = 0_usize;
    for message in response.messages.iter().skip(input_message_count) {
        let message_turn = if message.role == MessageRole::Assistant {
            let message_turn = turn;
            turn = turn.saturating_add(1);
            Some(message_turn)
        } else {
            None
        };
        match &message.content {
            ChatContent::Text(text) if message.role == MessageRole::Assistant => {
                let streamed_prefix = message_turn
                    .and_then(|message_turn| streamed_text_turns.get(&message_turn))
                    .map(|delivery| delivery.delivered_prefix.as_str())
                    .unwrap_or_default();
                if !text.text.starts_with(streamed_prefix) {
                    return Err(BackendError::new(
                        "streamed assistant text does not match the final response",
                    ));
                }
                let unsent_text = &text.text[streamed_prefix.len()..];
                if !unsent_text.is_empty() {
                    events.push(ChatStreamEvent::TextDelta {
                        text: unsent_text.to_owned(),
                    });
                }
            }
            // The model can stream transient commentary before selecting tools. Tool calls are
            // authoritative for this turn; delivered commentary is not part of final history.
            ChatContent::ToolCalls(_) if message.role == MessageRole::Assistant => {}
            ChatContent::ToolResults(_) if message.role == MessageRole::Tool => {}
            _ => {
                return Err(BackendError::new(
                    "generated chat content cannot be represented as SSE events",
                ));
            }
        }
    }
    if streamed_text_turns
        .keys()
        .any(|streamed_turn| *streamed_turn >= turn)
    {
        return Err(BackendError::new(
            "streamed assistant text has no matching final response turn",
        ));
    }
    match response.status {
        ChatStatus::Completed => events.push(ChatStreamEvent::Completed {
            response: Some(response),
        }),
        ChatStatus::ApprovalRequired => events.push(ChatStreamEvent::ApprovalRequired {
            pending_calls: response.pending_calls,
            resume_capability: response
                .resume_capability
                .ok_or_else(|| BackendError::new("approval response has no capability"))?,
        }),
    }
    Ok(events)
}

fn registration_to_record(
    name: &str,
    registration: &ToolRegistration,
    input_schema: Value,
) -> Result<ToolRecord, BackendError> {
    let worker_id = u64::try_from(registration.worker_id)
        .map_err(|_| BackendError::new("the registered Worker ID is invalid"))?;
    Ok(ToolRecord {
        name: name.to_owned(),
        description: registration.description.clone(),
        worker_id,
        method: registration.using.clone(),
        requires_approval: registration.requires_approval,
        input_schema,
    })
}

fn value_contains_execution_request(value: &Value) -> bool {
    match value {
        Value::Object(fields) => {
            fields.keys().any(|key| {
                matches!(
                    key.as_str(),
                    "tool_execution_requests" | "toolExecutionRequests"
                )
            }) || fields.values().any(value_contains_execution_request)
        }
        Value::Array(values) => values.iter().any(value_contains_execution_request),
        _ => false,
    }
}
