//! Redis-backed session manager for scalable deployments.
//!
//! This module provides a Redis-based implementation of the SessionManager trait,
//! enabling session persistence across multiple AG-UI server instances.

use super::manager::{HitlWaitingInfo, PendingToolCallInfo, Session, SessionManager, SessionState};
use crate::types::ids::{RunId, ThreadId};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use deadpool_redis::redis::AsyncCommands;
use deadpool_redis::Pool;
use serde::{Deserialize, Serialize};

/// Redis key prefix for sessions
const SESSION_KEY_PREFIX: &str = "ag_ui:session:";
/// Redis key prefix for run_id index
const RUN_ID_INDEX_PREFIX: &str = "ag_ui:run_index:";

/// Serializable pending tool call info for Redis storage
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RedisPendingToolCallInfo {
    call_id: String,
    fn_name: String,
    fn_arguments: String,
}

impl From<&PendingToolCallInfo> for RedisPendingToolCallInfo {
    fn from(info: &PendingToolCallInfo) -> Self {
        Self {
            call_id: info.call_id.clone(),
            fn_name: info.fn_name.clone(),
            fn_arguments: info.fn_arguments.clone(),
        }
    }
}

impl From<RedisPendingToolCallInfo> for PendingToolCallInfo {
    fn from(data: RedisPendingToolCallInfo) -> Self {
        Self {
            call_id: data.call_id,
            fn_name: data.fn_name,
            fn_arguments: data.fn_arguments,
        }
    }
}

/// Serializable HITL waiting info for Redis storage
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RedisHitlWaitingInfo {
    tool_call_id: String,
    checkpoint_position: String,
    workflow_name: String,
    #[serde(default)]
    pending_tool_calls: Vec<RedisPendingToolCallInfo>,
}

impl From<&HitlWaitingInfo> for RedisHitlWaitingInfo {
    fn from(info: &HitlWaitingInfo) -> Self {
        Self {
            tool_call_id: info.tool_call_id.clone(),
            checkpoint_position: info.checkpoint_position.clone(),
            workflow_name: info.workflow_name.clone(),
            pending_tool_calls: info
                .pending_tool_calls
                .iter()
                .map(RedisPendingToolCallInfo::from)
                .collect(),
        }
    }
}

impl From<RedisHitlWaitingInfo> for HitlWaitingInfo {
    fn from(data: RedisHitlWaitingInfo) -> Self {
        Self {
            tool_call_id: data.tool_call_id,
            checkpoint_position: data.checkpoint_position,
            workflow_name: data.workflow_name,
            pending_tool_calls: data
                .pending_tool_calls
                .into_iter()
                .map(PendingToolCallInfo::from)
                .collect(),
        }
    }
}

/// Serializable session data for Redis storage
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RedisSessionData {
    session_id: String,
    run_id: String,
    thread_id: String,
    created_at: DateTime<Utc>,
    last_event_id: u64,
    state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    hitl_waiting_info: Option<RedisHitlWaitingInfo>,
}

impl From<&Session> for RedisSessionData {
    fn from(session: &Session) -> Self {
        Self {
            session_id: session.session_id.clone(),
            run_id: session.run_id.to_string(),
            thread_id: session.thread_id.to_string(),
            created_at: session.created_at,
            last_event_id: session.last_event_id,
            state: match session.state {
                SessionState::Active => "active".to_string(),
                SessionState::Paused => "paused".to_string(),
                SessionState::Completed => "completed".to_string(),
                SessionState::Cancelled => "cancelled".to_string(),
                SessionState::Error => "error".to_string(),
            },
            hitl_waiting_info: session
                .hitl_waiting_info
                .as_ref()
                .map(RedisHitlWaitingInfo::from),
        }
    }
}

impl From<RedisSessionData> for Session {
    fn from(data: RedisSessionData) -> Self {
        Self {
            session_id: data.session_id,
            run_id: RunId::new(&data.run_id),
            thread_id: ThreadId::new(&data.thread_id),
            created_at: data.created_at,
            last_event_id: data.last_event_id,
            state: match data.state.as_str() {
                "active" => SessionState::Active,
                "paused" => SessionState::Paused,
                "completed" => SessionState::Completed,
                "cancelled" => SessionState::Cancelled,
                "error" => SessionState::Error,
                _ => SessionState::Active,
            },
            hitl_waiting_info: data.hitl_waiting_info.map(HitlWaitingInfo::from),
        }
    }
}

/// Redis-backed session manager for scalable deployments.
///
/// Stores session data in Redis with automatic TTL-based expiration.
/// Supports multiple AG-UI server instances sharing session state.
#[derive(Clone)]
pub struct RedisSessionManager {
    pool: Pool,
    ttl_sec: u64,
}

impl RedisSessionManager {
    /// Create a new Redis session manager.
    ///
    /// # Arguments
    /// * `pool` - Redis connection pool
    /// * `ttl_sec` - Session TTL in seconds
    pub fn new(pool: Pool, ttl_sec: u64) -> Self {
        Self { pool, ttl_sec }
    }

    /// Generate Redis key for session
    fn session_key(session_id: &str) -> String {
        format!("{}{}", SESSION_KEY_PREFIX, session_id)
    }

    /// Generate Redis key for run_id index
    fn run_id_key(run_id: &str) -> String {
        format!("{}{}", RUN_ID_INDEX_PREFIX, run_id)
    }
}

#[async_trait]
impl SessionManager for RedisSessionManager {
    async fn create_session(&self, run_id: RunId, thread_id: ThreadId) -> Session {
        let session = Session::new(run_id.clone(), thread_id);
        let session_id = session.session_id.clone();
        let data = RedisSessionData::from(&session);

        match self.pool.get().await {
            Ok(mut conn) => {
                let session_key = Self::session_key(&session_id);
                let run_id_key = Self::run_id_key(run_id.as_str());

                // Store session data with TTL
                match serde_json::to_string(&data) {
                    Ok(json) => {
                        if let Err(e) = conn
                            .set_ex::<_, _, ()>(&session_key, &json, self.ttl_sec)
                            .await
                        {
                            tracing::error!(
                                session_id = %session_id,
                                run_id = %run_id,
                                ttl_sec = self.ttl_sec,
                                error = %e,
                                "Failed to store session in Redis"
                            );
                        }

                        // Store run_id -> session_id mapping
                        if let Err(e) = conn
                            .set_ex::<_, _, ()>(&run_id_key, &session_id, self.ttl_sec)
                            .await
                        {
                            tracing::error!(
                                session_id = %session_id,
                                run_id = %run_id,
                                ttl_sec = self.ttl_sec,
                                error = %e,
                                "Failed to store run_id mapping in Redis"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            session_id = %session_id,
                            run_id = %run_id,
                            error = %e,
                            "Failed to serialize session data"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::error!(
                    session_id = %session_id,
                    run_id = %run_id,
                    error = %e,
                    "Failed to acquire Redis connection for create_session"
                );
            }
        }

        session
    }

    async fn get_session(&self, session_id: &str) -> Option<Session> {
        let mut conn = match self.pool.get().await {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(
                    session_id = %session_id,
                    error = %e,
                    "Failed to acquire Redis connection for get_session"
                );
                return None;
            }
        };
        let session_key = Self::session_key(session_id);

        let json: Option<String> = match conn.get(&session_key).await {
            Ok(j) => j,
            Err(e) => {
                tracing::debug!(
                    session_id = %session_id,
                    error = %e,
                    "Failed to get session from Redis"
                );
                return None;
            }
        };
        let json = json?;

        let data: RedisSessionData = match serde_json::from_str(&json) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    "Failed to deserialize session data from Redis"
                );
                return None;
            }
        };

        // Redis handles TTL expiry automatically via set_ex, no manual check needed
        Some(data.into())
    }

    async fn get_session_by_run_id(&self, run_id: &RunId) -> Option<Session> {
        let mut conn = self.pool.get().await.ok()?;
        let run_id_key = Self::run_id_key(run_id.as_str());

        let session_id: Option<String> = conn.get(&run_id_key).await.ok()?;
        let session_id = session_id?;

        self.get_session(&session_id).await
    }

    async fn update_last_event_id(&self, session_id: &str, event_id: u64) -> bool {
        if let Some(mut session) = self.get_session(session_id).await {
            let run_id = session.run_id.to_string();
            session.last_event_id = event_id;
            let data = RedisSessionData::from(&session);

            if let Ok(mut conn) = self.pool.get().await {
                if let Ok(json) = serde_json::to_string(&data) {
                    let session_key = Self::session_key(session_id);
                    let run_id_key = Self::run_id_key(&run_id);
                    if let Err(e) = conn
                        .set_ex::<_, _, ()>(&session_key, &json, self.ttl_sec)
                        .await
                    {
                        tracing::warn!(
                            session_id = %session_id,
                            error = %e,
                            "Failed to update session in Redis"
                        );
                        return false;
                    }
                    // Refresh run_id index TTL to keep it in sync with session TTL
                    if let Err(e) = conn
                        .set_ex::<_, _, ()>(&run_id_key, session_id, self.ttl_sec)
                        .await
                    {
                        tracing::warn!(
                            session_id = %session_id,
                            run_id = %run_id,
                            ttl_sec = self.ttl_sec,
                            error = %e,
                            "Failed to refresh run_id index TTL"
                        );
                        return false;
                    }
                    return true;
                }
            }
        }
        false
    }

    async fn set_session_state(&self, session_id: &str, state: SessionState) -> bool {
        if let Some(mut session) = self.get_session(session_id).await {
            let run_id = session.run_id.to_string();
            session.state = state;
            let data = RedisSessionData::from(&session);

            if let Ok(mut conn) = self.pool.get().await {
                if let Ok(json) = serde_json::to_string(&data) {
                    let session_key = Self::session_key(session_id);
                    let run_id_key = Self::run_id_key(&run_id);
                    if let Err(e) = conn
                        .set_ex::<_, _, ()>(&session_key, &json, self.ttl_sec)
                        .await
                    {
                        tracing::warn!(
                            session_id = %session_id,
                            error = %e,
                            "Failed to set session state in Redis"
                        );
                        return false;
                    }
                    // Refresh run_id index TTL to keep it in sync with session TTL
                    if let Err(e) = conn
                        .set_ex::<_, _, ()>(&run_id_key, session_id, self.ttl_sec)
                        .await
                    {
                        tracing::warn!(
                            session_id = %session_id,
                            run_id = %run_id,
                            ttl_sec = self.ttl_sec,
                            error = %e,
                            "Failed to refresh run_id index TTL"
                        );
                        return false;
                    }
                    return true;
                }
            }
        }
        false
    }

    async fn set_paused_with_hitl_info(
        &self,
        session_id: &str,
        hitl_info: HitlWaitingInfo,
    ) -> bool {
        if let Some(mut session) = self.get_session(session_id).await {
            // Only allow transition to Paused from Active state
            if session.state != SessionState::Active {
                return false;
            }
            let run_id = session.run_id.to_string();
            session.state = SessionState::Paused;
            session.hitl_waiting_info = Some(hitl_info);
            let data = RedisSessionData::from(&session);

            if let Ok(mut conn) = self.pool.get().await {
                if let Ok(json) = serde_json::to_string(&data) {
                    let session_key = Self::session_key(session_id);
                    let run_id_key = Self::run_id_key(&run_id);
                    if let Err(e) = conn
                        .set_ex::<_, _, ()>(&session_key, &json, self.ttl_sec)
                        .await
                    {
                        tracing::warn!(
                            session_id = %session_id,
                            error = %e,
                            "Failed to set paused state with HITL info in Redis"
                        );
                        return false;
                    }
                    // Refresh run_id index TTL to keep it in sync with session TTL
                    if let Err(e) = conn
                        .set_ex::<_, _, ()>(&run_id_key, session_id, self.ttl_sec)
                        .await
                    {
                        tracing::warn!(
                            session_id = %session_id,
                            run_id = %run_id,
                            ttl_sec = self.ttl_sec,
                            error = %e,
                            "Failed to refresh run_id index TTL"
                        );
                        return false;
                    }
                    return true;
                }
            }
        }
        false
    }

    async fn clear_hitl_info(&self, session_id: &str) -> bool {
        if let Some(mut session) = self.get_session(session_id).await {
            // Only clear HITL info if session is in Paused state
            if session.state != SessionState::Paused {
                return false;
            }
            let run_id = session.run_id.to_string();
            session.hitl_waiting_info = None;
            let data = RedisSessionData::from(&session);

            if let Ok(mut conn) = self.pool.get().await {
                if let Ok(json) = serde_json::to_string(&data) {
                    let session_key = Self::session_key(session_id);
                    let run_id_key = Self::run_id_key(&run_id);
                    if let Err(e) = conn
                        .set_ex::<_, _, ()>(&session_key, &json, self.ttl_sec)
                        .await
                    {
                        tracing::warn!(
                            session_id = %session_id,
                            error = %e,
                            "Failed to clear HITL info in Redis"
                        );
                        return false;
                    }
                    // Refresh run_id index TTL to keep it in sync with session TTL
                    if let Err(e) = conn
                        .set_ex::<_, _, ()>(&run_id_key, session_id, self.ttl_sec)
                        .await
                    {
                        tracing::warn!(
                            session_id = %session_id,
                            run_id = %run_id,
                            ttl_sec = self.ttl_sec,
                            error = %e,
                            "Failed to refresh run_id index TTL"
                        );
                        return false;
                    }
                    return true;
                }
            }
        }
        false
    }

    async fn resume_from_paused(&self, session_id: &str, new_state: SessionState) -> bool {
        if let Some(mut session) = self.get_session(session_id).await {
            // Only resume if session is in Paused state
            if session.state != SessionState::Paused {
                return false;
            }
            let run_id = session.run_id.to_string();
            session.hitl_waiting_info = None;
            session.state = new_state;
            let data = RedisSessionData::from(&session);

            if let Ok(mut conn) = self.pool.get().await {
                if let Ok(json) = serde_json::to_string(&data) {
                    let session_key = Self::session_key(session_id);
                    let run_id_key = Self::run_id_key(&run_id);
                    if let Err(e) = conn
                        .set_ex::<_, _, ()>(&session_key, &json, self.ttl_sec)
                        .await
                    {
                        tracing::warn!(
                            session_id = %session_id,
                            error = %e,
                            "Failed to resume from paused state in Redis"
                        );
                        return false;
                    }
                    // Refresh run_id index TTL to keep it in sync with session TTL
                    if let Err(e) = conn
                        .set_ex::<_, _, ()>(&run_id_key, session_id, self.ttl_sec)
                        .await
                    {
                        tracing::warn!(
                            session_id = %session_id,
                            run_id = %run_id,
                            ttl_sec = self.ttl_sec,
                            error = %e,
                            "Failed to refresh run_id index TTL"
                        );
                        return false;
                    }
                    return true;
                }
            }
        }
        false
    }

    async fn delete_session(&self, session_id: &str) -> bool {
        if let Ok(mut conn) = self.pool.get().await {
            // Get session to find run_id for index cleanup
            let session_key = Self::session_key(session_id);
            let json: Option<String> = conn.get(&session_key).await.ok().flatten();

            if let Some(json) = json {
                if let Ok(data) = serde_json::from_str::<RedisSessionData>(&json) {
                    let run_id_key = Self::run_id_key(&data.run_id);
                    let _: Result<(), _> = conn.del(&run_id_key).await;
                }
            }

            let result: Result<i32, _> = conn.del(&session_key).await;
            return result.map(|n| n > 0).unwrap_or(false);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Integration tests require running Redis instance
    // These tests are marked as ignored by default

    #[tokio::test]
    #[ignore]
    async fn test_redis_create_session() {
        let pool = create_test_pool().await;
        let manager = RedisSessionManager::new(pool, 3600);

        let session = manager
            .create_session(RunId::new("run_1"), ThreadId::new("thread_1"))
            .await;

        assert!(!session.session_id.is_empty());
        assert_eq!(session.run_id.as_str(), "run_1");
        assert_eq!(session.thread_id.as_str(), "thread_1");
        assert_eq!(session.state, SessionState::Active);

        // Cleanup
        manager.delete_session(&session.session_id).await;
    }

    #[tokio::test]
    #[ignore]
    async fn test_redis_get_session() {
        let pool = create_test_pool().await;
        let manager = RedisSessionManager::new(pool, 3600);

        let created = manager
            .create_session(RunId::new("run_2"), ThreadId::new("thread_2"))
            .await;

        let retrieved = manager.get_session(&created.session_id).await;
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().session_id, created.session_id);

        // Cleanup
        manager.delete_session(&created.session_id).await;
    }

    #[tokio::test]
    #[ignore]
    async fn test_redis_get_session_by_run_id() {
        let pool = create_test_pool().await;
        let manager = RedisSessionManager::new(pool, 3600);

        let run_id = RunId::new("run_unique_123");
        let created = manager
            .create_session(run_id.clone(), ThreadId::new("thread_3"))
            .await;

        let retrieved = manager.get_session_by_run_id(&run_id).await;
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().session_id, created.session_id);

        // Cleanup
        manager.delete_session(&created.session_id).await;
    }

    #[tokio::test]
    #[ignore]
    async fn test_redis_update_last_event_id() {
        let pool = create_test_pool().await;
        let manager = RedisSessionManager::new(pool, 3600);

        let session = manager
            .create_session(RunId::new("run_4"), ThreadId::new("thread_4"))
            .await;

        let updated = manager.update_last_event_id(&session.session_id, 42).await;
        assert!(updated);

        let retrieved = manager.get_session(&session.session_id).await.unwrap();
        assert_eq!(retrieved.last_event_id, 42);

        // Cleanup
        manager.delete_session(&session.session_id).await;
    }

    #[tokio::test]
    #[ignore]
    async fn test_redis_set_session_state() {
        let pool = create_test_pool().await;
        let manager = RedisSessionManager::new(pool, 3600);

        let session = manager
            .create_session(RunId::new("run_5"), ThreadId::new("thread_5"))
            .await;

        let updated = manager
            .set_session_state(&session.session_id, SessionState::Completed)
            .await;
        assert!(updated);

        let retrieved = manager.get_session(&session.session_id).await.unwrap();
        assert_eq!(retrieved.state, SessionState::Completed);

        // Cleanup
        manager.delete_session(&session.session_id).await;
    }

    #[tokio::test]
    #[ignore]
    async fn test_redis_delete_session() {
        let pool = create_test_pool().await;
        let manager = RedisSessionManager::new(pool, 3600);

        let session = manager
            .create_session(RunId::new("run_6"), ThreadId::new("thread_6"))
            .await;

        let deleted = manager.delete_session(&session.session_id).await;
        assert!(deleted);

        let retrieved = manager.get_session(&session.session_id).await;
        assert!(retrieved.is_none());

        // Also verify run_id index is cleaned up
        let by_run = manager.get_session_by_run_id(&RunId::new("run_6")).await;
        assert!(by_run.is_none());
    }

    async fn create_test_pool() -> Pool {
        let config = deadpool_redis::Config::from_url("redis://127.0.0.1:6379");
        config
            .builder()
            .map(|b| b.max_size(4).build().unwrap())
            .expect("Failed to create Redis pool")
    }
}
