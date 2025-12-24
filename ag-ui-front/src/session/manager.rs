//! Session management for AG-UI connections.

use crate::types::ids::{RunId, ThreadId};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{watch, RwLock};

/// Session state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Session is active and streaming events
    Active,
    /// Session is paused (Human-in-the-Loop waiting)
    Paused,
    /// Session completed successfully
    Completed,
    /// Session was cancelled by user
    Cancelled,
    /// Session ended with error
    Error,
}

/// Information about a pending tool call for HITL
#[derive(Debug, Clone)]
pub struct PendingToolCallInfo {
    /// Tool call ID
    pub call_id: String,
    /// Function/runner name
    pub fn_name: String,
    /// Function arguments (JSON string)
    pub fn_arguments: String,
}

/// Information about HITL waiting state
#[derive(Debug, Clone)]
pub struct HitlWaitingInfo {
    /// Unique interrupt ID for AG-UI Interrupts protocol
    pub interrupt_id: String,
    /// Primary tool call ID for the waiting state (format: wait_{run_id} or call_id from LLM)
    pub tool_call_id: String,
    /// Checkpoint position (JSON Pointer format)
    pub checkpoint_position: String,
    /// Workflow name for checkpoint lookup
    pub workflow_name: String,
    /// List of pending tool calls that need client approval
    /// Empty for traditional HITL (HUMAN_INPUT), populated for LLM tool calls
    pub pending_tool_calls: Vec<PendingToolCallInfo>,
}

impl HitlWaitingInfo {
    /// Generate a new unique interrupt ID
    pub fn new_interrupt_id() -> String {
        format!("int_{}", uuid::Uuid::new_v4())
    }

    /// Create a new HitlWaitingInfo with auto-generated interrupt_id
    pub fn new(
        tool_call_id: String,
        checkpoint_position: String,
        workflow_name: String,
        pending_tool_calls: Vec<PendingToolCallInfo>,
    ) -> Self {
        Self {
            interrupt_id: Self::new_interrupt_id(),
            tool_call_id,
            checkpoint_position,
            workflow_name,
            pending_tool_calls,
        }
    }
}

/// Session information
#[derive(Debug, Clone)]
pub struct Session {
    /// Unique session identifier
    pub session_id: String,
    /// Associated run ID
    pub run_id: RunId,
    /// Associated thread ID
    pub thread_id: ThreadId,
    /// Session creation time
    pub created_at: DateTime<Utc>,
    /// Last event ID sent to this session
    pub last_event_id: u64,
    /// Current session state
    pub state: SessionState,
    /// HITL waiting information (set when state is Paused)
    pub hitl_waiting_info: Option<HitlWaitingInfo>,
}

impl Session {
    /// Create a new session
    pub fn new(run_id: RunId, thread_id: ThreadId) -> Self {
        Self {
            session_id: uuid::Uuid::new_v4().to_string(),
            run_id,
            thread_id,
            created_at: Utc::now(),
            last_event_id: 0,
            state: SessionState::Active,
            hitl_waiting_info: None,
        }
    }
}

/// Session manager trait for abstracting storage backends
#[async_trait]
pub trait SessionManager: Send + Sync {
    /// Create a new session for a workflow run
    async fn create_session(&self, run_id: RunId, thread_id: ThreadId) -> Session;

    /// Get a session by its ID
    async fn get_session(&self, session_id: &str) -> Option<Session>;

    /// Get a session by run ID
    async fn get_session_by_run_id(&self, run_id: &RunId) -> Option<Session>;

    /// Get a session by interrupt ID (for AG-UI Interrupts resume flow)
    async fn get_session_by_interrupt_id(&self, interrupt_id: &str) -> Option<Session>;

    /// Update the last event ID for a session
    async fn update_last_event_id(&self, session_id: &str, event_id: u64) -> bool;

    /// Set the session state
    async fn set_session_state(&self, session_id: &str, state: SessionState) -> bool;

    /// Set session state to Paused with HITL waiting info
    async fn set_paused_with_hitl_info(&self, session_id: &str, hitl_info: HitlWaitingInfo)
        -> bool;

    /// Clear HITL waiting info (when resuming)
    async fn clear_hitl_info(&self, session_id: &str) -> bool;

    /// Atomically resume from Paused state: clears HITL info and sets new state.
    ///
    /// This method ensures both operations happen under the same lock to prevent
    /// transient invalid states (e.g., Paused with no hitl_info).
    ///
    /// Returns false if session not found.
    async fn resume_from_paused(&self, session_id: &str, new_state: SessionState) -> bool;

    /// Delete a session
    async fn delete_session(&self, session_id: &str) -> bool;
}

/// Internal shared state for InMemorySessionManager
struct SessionManagerInner {
    sessions: RwLock<HashMap<String, Session>>,
    run_id_index: RwLock<HashMap<String, String>>,
    ttl: Duration,
    cleanup_interval_sec: u64,
    shutdown_tx: watch::Sender<bool>,
    cleanup_started: AtomicBool,
}

impl Drop for SessionManagerInner {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
    }
}

impl SessionManagerInner {
    /// Common cleanup logic for expired sessions
    async fn cleanup_expired_sessions(&self) {
        let now = Utc::now();
        let mut sessions = self.sessions.write().await;
        let mut index = self.run_id_index.write().await;

        let expired: Vec<String> = sessions
            .iter()
            .filter(|(_, session)| now.signed_duration_since(session.created_at) > self.ttl)
            .map(|(id, _)| id.clone())
            .collect();

        for session_id in expired {
            if let Some(session) = sessions.remove(&session_id) {
                index.remove(session.run_id.as_str());
            }
        }
    }

    /// Check if a session is expired
    fn is_expired(&self, session: &Session) -> bool {
        Utc::now().signed_duration_since(session.created_at) > self.ttl
    }
}

/// In-memory session manager for standalone deployments
#[derive(Clone)]
pub struct InMemorySessionManager {
    inner: Arc<SessionManagerInner>,
}

impl InMemorySessionManager {
    /// Maximum TTL value in seconds to prevent i64 overflow
    const MAX_TTL_SEC: u64 = i64::MAX as u64;

    /// Create a new in-memory session manager
    pub fn new(ttl_sec: u64) -> Self {
        let clamped_ttl = ttl_sec.min(Self::MAX_TTL_SEC);
        let cleanup_interval_sec = Self::calculate_cleanup_interval(clamped_ttl);

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let inner = Arc::new(SessionManagerInner {
            sessions: RwLock::new(HashMap::new()),
            run_id_index: RwLock::new(HashMap::new()),
            ttl: Duration::seconds(clamped_ttl as i64),
            cleanup_interval_sec,
            shutdown_tx,
            cleanup_started: AtomicBool::new(false),
        });

        let manager = Self { inner };
        manager.ensure_cleanup_task_started(shutdown_rx);
        manager
    }

    /// Calculate adaptive cleanup interval based on TTL
    fn calculate_cleanup_interval(ttl_sec: u64) -> u64 {
        (ttl_sec / 2).clamp(10, 60)
    }

    /// Ensure cleanup task is started exactly once
    fn ensure_cleanup_task_started(&self, mut shutdown_rx: watch::Receiver<bool>) {
        if self
            .inner
            .cleanup_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let inner = self.inner.clone();

            tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(
                    inner.cleanup_interval_sec,
                ));
                loop {
                    tokio::select! {
                        _ = interval.tick() => {
                            inner.cleanup_expired_sessions().await;
                        }
                        result = shutdown_rx.changed() => {
                            if result.is_err() || *shutdown_rx.borrow() {
                                tracing::debug!("Session manager cleanup task shutting down");
                                break;
                            }
                        }
                    }
                }
            });
        }
    }
}

impl Default for InMemorySessionManager {
    fn default() -> Self {
        Self::new(3600) // 1 hour default TTL
    }
}

#[async_trait]
impl SessionManager for InMemorySessionManager {
    async fn create_session(&self, run_id: RunId, thread_id: ThreadId) -> Session {
        let session = Session::new(run_id.clone(), thread_id);
        let session_id = session.session_id.clone();

        let mut sessions = self.inner.sessions.write().await;
        let mut index = self.inner.run_id_index.write().await;

        index.insert(run_id.to_string(), session_id.clone());
        sessions.insert(session_id, session.clone());

        session
    }

    async fn get_session(&self, session_id: &str) -> Option<Session> {
        let mut sessions = self.inner.sessions.write().await;
        let session = sessions.get(session_id)?;

        if self.inner.is_expired(session) {
            let run_id = session.run_id.clone();
            sessions.remove(session_id);
            let mut index = self.inner.run_id_index.write().await;
            index.remove(run_id.as_str());
            return None;
        }

        Some(session.clone())
    }

    async fn get_session_by_run_id(&self, run_id: &RunId) -> Option<Session> {
        let index = self.inner.run_id_index.read().await;
        let session_id = index.get(run_id.as_str())?.clone();
        drop(index);

        self.get_session(&session_id).await
    }

    async fn get_session_by_interrupt_id(&self, interrupt_id: &str) -> Option<Session> {
        let sessions = self.inner.sessions.read().await;
        for session in sessions.values() {
            if let Some(hitl_info) = &session.hitl_waiting_info {
                if hitl_info.interrupt_id == interrupt_id && !self.inner.is_expired(session) {
                    return Some(session.clone());
                }
            }
        }
        None
    }

    async fn update_last_event_id(&self, session_id: &str, event_id: u64) -> bool {
        let mut sessions = self.inner.sessions.write().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.last_event_id = event_id;
            true
        } else {
            false
        }
    }

    async fn set_session_state(&self, session_id: &str, state: SessionState) -> bool {
        let mut sessions = self.inner.sessions.write().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.state = state;
            true
        } else {
            false
        }
    }

    async fn set_paused_with_hitl_info(
        &self,
        session_id: &str,
        hitl_info: HitlWaitingInfo,
    ) -> bool {
        let mut sessions = self.inner.sessions.write().await;
        if let Some(session) = sessions.get_mut(session_id) {
            // Only allow transition to Paused from Active state
            if session.state != SessionState::Active {
                return false;
            }
            session.state = SessionState::Paused;
            session.hitl_waiting_info = Some(hitl_info);
            true
        } else {
            false
        }
    }

    async fn clear_hitl_info(&self, session_id: &str) -> bool {
        let mut sessions = self.inner.sessions.write().await;
        if let Some(session) = sessions.get_mut(session_id) {
            // Only clear HITL info if session is in Paused state
            if session.state != SessionState::Paused {
                return false;
            }
            session.hitl_waiting_info = None;
            true
        } else {
            false
        }
    }

    async fn resume_from_paused(&self, session_id: &str, new_state: SessionState) -> bool {
        let mut sessions = self.inner.sessions.write().await;
        if let Some(session) = sessions.get_mut(session_id) {
            // Only resume if session is in Paused state
            if session.state != SessionState::Paused {
                return false;
            }
            session.hitl_waiting_info = None;
            session.state = new_state;
            true
        } else {
            false
        }
    }

    async fn delete_session(&self, session_id: &str) -> bool {
        let mut sessions = self.inner.sessions.write().await;
        if let Some(session) = sessions.remove(session_id) {
            let mut index = self.inner.run_id_index.write().await;
            index.remove(session.run_id.as_str());
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_create_session() {
        let manager = InMemorySessionManager::new(3600);
        let session = manager
            .create_session(RunId::new("run_1"), ThreadId::new("thread_1"))
            .await;

        assert!(!session.session_id.is_empty());
        assert_eq!(session.run_id.as_str(), "run_1");
        assert_eq!(session.thread_id.as_str(), "thread_1");
        assert_eq!(session.state, SessionState::Active);
        assert_eq!(session.last_event_id, 0);
    }

    #[tokio::test]
    async fn test_get_session() {
        let manager = InMemorySessionManager::new(3600);
        let created = manager
            .create_session(RunId::new("run_1"), ThreadId::new("thread_1"))
            .await;

        let retrieved = manager.get_session(&created.session_id).await;
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().session_id, created.session_id);
    }

    #[tokio::test]
    async fn test_get_session_by_run_id() {
        let manager = InMemorySessionManager::new(3600);
        let run_id = RunId::new("run_123");
        let created = manager
            .create_session(run_id.clone(), ThreadId::new("thread_1"))
            .await;

        let retrieved = manager.get_session_by_run_id(&run_id).await;
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().session_id, created.session_id);
    }

    #[tokio::test]
    async fn test_get_nonexistent_session() {
        let manager = InMemorySessionManager::new(3600);
        let session = manager.get_session("nonexistent").await;
        assert!(session.is_none());
    }

    #[tokio::test]
    async fn test_update_last_event_id() {
        let manager = InMemorySessionManager::new(3600);
        let session = manager
            .create_session(RunId::new("run_1"), ThreadId::new("thread_1"))
            .await;

        let updated = manager.update_last_event_id(&session.session_id, 42).await;
        assert!(updated);

        let retrieved = manager.get_session(&session.session_id).await.unwrap();
        assert_eq!(retrieved.last_event_id, 42);
    }

    #[tokio::test]
    async fn test_set_session_state() {
        let manager = InMemorySessionManager::new(3600);
        let session = manager
            .create_session(RunId::new("run_1"), ThreadId::new("thread_1"))
            .await;

        let updated = manager
            .set_session_state(&session.session_id, SessionState::Completed)
            .await;
        assert!(updated);

        let retrieved = manager.get_session(&session.session_id).await.unwrap();
        assert_eq!(retrieved.state, SessionState::Completed);
    }

    #[tokio::test]
    async fn test_delete_session() {
        let manager = InMemorySessionManager::new(3600);
        let session = manager
            .create_session(RunId::new("run_1"), ThreadId::new("thread_1"))
            .await;

        let deleted = manager.delete_session(&session.session_id).await;
        assert!(deleted);

        let retrieved = manager.get_session(&session.session_id).await;
        assert!(retrieved.is_none());

        // Also verify run_id index is cleaned up
        let by_run = manager.get_session_by_run_id(&RunId::new("run_1")).await;
        assert!(by_run.is_none());
    }

    #[tokio::test]
    async fn test_delete_nonexistent_session() {
        let manager = InMemorySessionManager::new(3600);
        let deleted = manager.delete_session("nonexistent").await;
        assert!(!deleted);
    }

    #[tokio::test]
    async fn test_session_state_transitions() {
        let manager = InMemorySessionManager::new(3600);
        let session = manager
            .create_session(RunId::new("run_1"), ThreadId::new("thread_1"))
            .await;

        // Active -> Paused
        manager
            .set_session_state(&session.session_id, SessionState::Paused)
            .await;
        let s = manager.get_session(&session.session_id).await.unwrap();
        assert_eq!(s.state, SessionState::Paused);

        // Paused -> Active
        manager
            .set_session_state(&session.session_id, SessionState::Active)
            .await;
        let s = manager.get_session(&session.session_id).await.unwrap();
        assert_eq!(s.state, SessionState::Active);

        // Active -> Completed
        manager
            .set_session_state(&session.session_id, SessionState::Completed)
            .await;
        let s = manager.get_session(&session.session_id).await.unwrap();
        assert_eq!(s.state, SessionState::Completed);
    }

    #[tokio::test]
    async fn test_hitl_waiting_info_with_interrupt_id() {
        let manager = InMemorySessionManager::new(3600);
        let session = manager
            .create_session(RunId::new("run_1"), ThreadId::new("thread_1"))
            .await;

        let hitl_info = HitlWaitingInfo::new(
            "call_123".to_string(),
            "/tasks/llm_chat".to_string(),
            "test_workflow".to_string(),
            vec![PendingToolCallInfo {
                call_id: "call_123".to_string(),
                fn_name: "COMMAND___run".to_string(),
                fn_arguments: r#"{"command":"date"}"#.to_string(),
            }],
        );

        let interrupt_id = hitl_info.interrupt_id.clone();
        assert!(interrupt_id.starts_with("int_"));

        // Set paused with HITL info
        let result = manager
            .set_paused_with_hitl_info(&session.session_id, hitl_info)
            .await;
        assert!(result);

        // Verify session is paused
        let s = manager.get_session(&session.session_id).await.unwrap();
        assert_eq!(s.state, SessionState::Paused);
        assert!(s.hitl_waiting_info.is_some());
        assert_eq!(
            s.hitl_waiting_info.as_ref().unwrap().interrupt_id,
            interrupt_id
        );

        // Get session by interrupt_id
        let by_interrupt = manager.get_session_by_interrupt_id(&interrupt_id).await;
        assert!(by_interrupt.is_some());
        assert_eq!(by_interrupt.unwrap().session_id, session.session_id);

        // Resume from paused
        let resumed = manager
            .resume_from_paused(&session.session_id, SessionState::Active)
            .await;
        assert!(resumed);

        let s = manager.get_session(&session.session_id).await.unwrap();
        assert_eq!(s.state, SessionState::Active);
        assert!(s.hitl_waiting_info.is_none());

        // Can no longer find by interrupt_id
        let by_interrupt = manager.get_session_by_interrupt_id(&interrupt_id).await;
        assert!(by_interrupt.is_none());
    }

    #[tokio::test]
    async fn test_get_session_by_interrupt_id_not_found() {
        let manager = InMemorySessionManager::new(3600);
        let session = manager.get_session_by_interrupt_id("int_nonexistent").await;
        assert!(session.is_none());
    }
}
