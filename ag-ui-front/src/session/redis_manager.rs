//! Redis-backed session manager for scalable deployments.
//!
//! This module provides a Redis-based implementation of the SessionManager trait,
//! enabling session persistence across multiple AG-UI server instances.

use super::manager::{HitlWaitingInfo, PendingToolCallInfo, Session, SessionManager, SessionState};
use crate::types::ids::{RunId, ThreadId};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use infra_utils::infra::redis::RedisPool;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};

/// Redis key prefix for sessions
const SESSION_KEY_PREFIX: &str = "ag_ui:session:";
/// Redis key prefix for run_id index
const RUN_ID_INDEX_PREFIX: &str = "ag_ui:run_index:";
/// Redis key prefix for thread_id -> session_ids reverse index (Set)
const THREAD_SESSIONS_PREFIX: &str = "ag_ui:thread_sessions:";
/// Thread index lives longer than individual sessions because the set
/// references multiple sessions. Factor of 2 ensures the index survives
/// even when the newest session within the thread is refreshed near its TTL boundary.
const THREAD_INDEX_TTL_MULTIPLIER: u64 = 2;

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
    interrupt_id: String,
    tool_call_id: String,
    checkpoint_position: String,
    workflow_name: String,
    #[serde(default)]
    pending_tool_calls: Vec<RedisPendingToolCallInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workflow_context: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    registered_worker_name: Option<String>,
}

impl From<&HitlWaitingInfo> for RedisHitlWaitingInfo {
    fn from(info: &HitlWaitingInfo) -> Self {
        Self {
            interrupt_id: info.interrupt_id.clone(),
            tool_call_id: info.tool_call_id.clone(),
            checkpoint_position: info.checkpoint_position.clone(),
            workflow_name: info.workflow_name.clone(),
            pending_tool_calls: info
                .pending_tool_calls
                .iter()
                .map(RedisPendingToolCallInfo::from)
                .collect(),
            workflow_context: info.workflow_context.clone(),
            registered_worker_name: info.registered_worker_name.clone(),
        }
    }
}

impl From<RedisHitlWaitingInfo> for HitlWaitingInfo {
    fn from(data: RedisHitlWaitingInfo) -> Self {
        Self {
            interrupt_id: data.interrupt_id,
            tool_call_id: data.tool_call_id,
            checkpoint_position: data.checkpoint_position,
            workflow_name: data.workflow_name,
            pending_tool_calls: data
                .pending_tool_calls
                .into_iter()
                .map(PendingToolCallInfo::from)
                .collect(),
            workflow_context: data.workflow_context,
            registered_worker_name: data.registered_worker_name,
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
            state: session.state.to_string(),
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
    pool: &'static RedisPool,
    ttl_sec: u64,
}

impl RedisSessionManager {
    /// Maximum TTL value in seconds to prevent i64 overflow on Redis EXPIRE
    const MAX_TTL_SEC: u64 = i64::MAX as u64;

    /// Create a new Redis session manager.
    ///
    /// # Arguments
    /// * `pool` - Redis connection pool
    /// * `ttl_sec` - Session TTL in seconds
    pub fn new(pool: &'static RedisPool, ttl_sec: u64) -> Self {
        Self {
            pool,
            ttl_sec: ttl_sec.min(Self::MAX_TTL_SEC),
        }
    }

    /// Generate Redis key for session
    fn session_key(session_id: &str) -> String {
        format!("{}{}", SESSION_KEY_PREFIX, session_id)
    }

    /// Generate Redis key for run_id index
    fn run_id_key(run_id: &str) -> String {
        format!("{}{}", RUN_ID_INDEX_PREFIX, run_id)
    }

    /// Generate Redis key for thread_id -> session_ids reverse index
    fn thread_sessions_key(thread_id: &str) -> String {
        format!("{}{}", THREAD_SESSIONS_PREFIX, thread_id)
    }

    /// Add session_id to thread Set index and refresh TTL
    async fn add_to_thread_index(
        &self,
        conn: &mut impl AsyncCommands,
        thread_id: &str,
        session_id: &str,
    ) {
        let key = Self::thread_sessions_key(thread_id);
        let thread_ttl = self
            .ttl_sec
            .saturating_mul(THREAD_INDEX_TTL_MULTIPLIER)
            .min(Self::MAX_TTL_SEC);
        if let Err(e) = conn.sadd::<_, _, ()>(&key, session_id).await {
            tracing::warn!(thread_id = %thread_id, error = %e, "Failed to SADD to thread index");
        }
        if let Err(e) = conn.expire::<_, ()>(&key, thread_ttl as i64).await {
            tracing::warn!(thread_id = %thread_id, error = %e, "Failed to refresh thread index TTL");
        }
    }

    /// Remove session_id from thread Set index
    async fn remove_from_thread_index(
        &self,
        conn: &mut impl AsyncCommands,
        thread_id: &str,
        session_id: &str,
    ) {
        let key = Self::thread_sessions_key(thread_id);
        if let Err(e) = conn.srem::<_, _, ()>(&key, session_id).await {
            tracing::warn!(thread_id = %thread_id, error = %e, "Failed to SREM from thread index");
        }
    }

    /// Refresh thread Set index TTL
    async fn refresh_thread_index_ttl(&self, conn: &mut impl AsyncCommands, thread_id: &str) {
        let key = Self::thread_sessions_key(thread_id);
        let thread_ttl = self
            .ttl_sec
            .saturating_mul(THREAD_INDEX_TTL_MULTIPLIER)
            .min(Self::MAX_TTL_SEC);
        if let Err(e) = conn.expire::<_, ()>(&key, thread_ttl as i64).await {
            tracing::warn!(thread_id = %thread_id, error = %e, "Failed to refresh thread index TTL");
        }
    }
}

#[async_trait]
impl SessionManager for RedisSessionManager {
    async fn create_session(&self, run_id: RunId, thread_id: ThreadId) -> Session {
        let session = Session::new(run_id.clone(), thread_id.clone());
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

                        // Add to thread -> session_ids reverse index
                        self.add_to_thread_index(&mut *conn, thread_id.as_str(), &session_id)
                            .await;
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

        let json: Option<String> = match (*conn).get(&session_key).await {
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

        let session_id: Option<String> = (*conn).get(&run_id_key).await.ok()?;
        let session_id = session_id?;

        self.get_session(&session_id).await
    }

    async fn get_session_by_interrupt_id(&self, interrupt_id: &str) -> Option<Session> {
        // For Redis, we need to scan all sessions to find one with matching interrupt_id.
        // This is less efficient than in-memory, but interrupt lookups are infrequent.
        // A production optimization would be to add a separate interrupt_id -> session_id index.
        let mut conn = self.pool.get().await.ok()?;
        let pattern = format!("{}*", SESSION_KEY_PREFIX);

        let keys: Vec<String> = redis::cmd("KEYS")
            .arg(&pattern)
            .query_async(&mut *conn)
            .await
            .ok()?;

        for key in keys {
            if let Ok(json) = (*conn).get::<_, String>(&key).await
                && let Ok(data) = serde_json::from_str::<RedisSessionData>(&json)
                && let Some(hitl_info) = &data.hitl_waiting_info
                && hitl_info.interrupt_id == interrupt_id
            {
                return Some(data.into());
            }
        }
        None
    }

    async fn update_last_event_id(&self, session_id: &str, event_id: u64) -> bool {
        if let Some(mut session) = self.get_session(session_id).await {
            let run_id = session.run_id.to_string();
            let thread_id = session.thread_id.to_string();
            session.last_event_id = event_id;
            let data = RedisSessionData::from(&session);

            if let Ok(mut conn) = self.pool.get().await
                && let Ok(json) = serde_json::to_string(&data)
            {
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
                self.refresh_thread_index_ttl(&mut *conn, &thread_id).await;
                return true;
            }
        }
        false
    }

    async fn set_session_state(&self, session_id: &str, state: SessionState) -> bool {
        if let Some(mut session) = self.get_session(session_id).await {
            let run_id = session.run_id.to_string();
            let thread_id = session.thread_id.to_string();
            session.state = state;
            let data = RedisSessionData::from(&session);

            if let Ok(mut conn) = self.pool.get().await
                && let Ok(json) = serde_json::to_string(&data)
            {
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
                self.refresh_thread_index_ttl(&mut *conn, &thread_id).await;
                return true;
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
            let thread_id = session.thread_id.to_string();
            session.state = SessionState::Paused;
            session.hitl_waiting_info = Some(hitl_info);
            let data = RedisSessionData::from(&session);

            if let Ok(mut conn) = self.pool.get().await
                && let Ok(json) = serde_json::to_string(&data)
            {
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
                self.refresh_thread_index_ttl(&mut *conn, &thread_id).await;
                return true;
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
            let thread_id = session.thread_id.to_string();
            session.hitl_waiting_info = None;
            let data = RedisSessionData::from(&session);

            if let Ok(mut conn) = self.pool.get().await
                && let Ok(json) = serde_json::to_string(&data)
            {
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
                self.refresh_thread_index_ttl(&mut *conn, &thread_id).await;
                return true;
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
            let thread_id = session.thread_id.to_string();
            session.hitl_waiting_info = None;
            session.state = new_state;
            let data = RedisSessionData::from(&session);

            if let Ok(mut conn) = self.pool.get().await
                && let Ok(json) = serde_json::to_string(&data)
            {
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
                self.refresh_thread_index_ttl(&mut *conn, &thread_id).await;
                return true;
            }
        }
        false
    }

    async fn delete_session(&self, session_id: &str) -> bool {
        if let Ok(mut conn) = self.pool.get().await {
            // Get session to find run_id and thread_id for index cleanup
            let session_key = Self::session_key(session_id);
            let json: Option<String> = (*conn).get(&session_key).await.ok().flatten();

            if let Some(json) = json
                && let Ok(data) = serde_json::from_str::<RedisSessionData>(&json)
            {
                let run_id_key = Self::run_id_key(&data.run_id);
                let _: Result<(), _> = (*conn).del(&run_id_key).await;
                self.remove_from_thread_index(&mut *conn, &data.thread_id, session_id)
                    .await;
            }

            let result: Result<i32, _> = (*conn).del(&session_key).await;
            return result.map(|n| n > 0).unwrap_or(false);
        }
        false
    }

    async fn get_active_session_by_thread_id(&self, thread_id: &ThreadId) -> Option<Session> {
        let mut conn = self.pool.get().await.ok()?;
        let thread_key = Self::thread_sessions_key(thread_id.as_str());
        let session_ids: Vec<String> = match conn.smembers::<_, Vec<String>>(&thread_key).await {
            Ok(ids) => ids,
            Err(e) => {
                tracing::warn!(thread_id = %thread_id, error = %e, "Failed to SMEMBERS for thread sessions");
                return None;
            }
        };

        if session_ids.is_empty() {
            return None;
        }

        // Single MGET instead of N separate GETs
        let keys: Vec<String> = session_ids
            .iter()
            .map(|sid| Self::session_key(sid))
            .collect();
        let values: Vec<Option<String>> = match conn.mget::<_, Vec<Option<String>>>(&keys).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(thread_id = %thread_id, error = %e, "Failed to MGET session data");
                return None;
            }
        };

        let mut best: Option<Session> = None;
        let mut stale_ids: Vec<String> = Vec::new();

        for (sid, json_opt) in session_ids.iter().zip(values) {
            match json_opt {
                None => stale_ids.push(sid.clone()),
                Some(json) => {
                    match serde_json::from_str::<RedisSessionData>(&json).map(Session::from) {
                        Ok(s) if matches!(s.state, SessionState::Active | SessionState::Paused) => {
                            if best.as_ref().is_none_or(|b| s.created_at > b.created_at) {
                                best = Some(s);
                            }
                        }
                        Ok(_) => stale_ids.push(sid.clone()),
                        Err(e) => {
                            tracing::warn!(session_id = %sid, error = %e, "Failed to deserialize session data, removing from thread index");
                            stale_ids.push(sid.clone());
                        }
                    }
                }
            }
        }

        // Lazy cleanup reusing the same connection
        for sid in &stale_ids {
            let _: Result<(), _> = conn.srem(&thread_key, sid).await;
        }

        best
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

    #[tokio::test]
    #[ignore]
    async fn test_redis_get_active_session_by_thread_id() {
        let pool = create_test_pool().await;
        let manager = RedisSessionManager::new(pool, 3600);

        let session = manager
            .create_session(RunId::new("run_active_1"), ThreadId::new("thread_active_1"))
            .await;

        // Active session should be found
        let found = manager
            .get_active_session_by_thread_id(&ThreadId::new("thread_active_1"))
            .await;
        assert!(found.is_some());
        assert_eq!(found.unwrap().session_id, session.session_id);

        // Set to Completed -> should not be found
        manager
            .set_session_state(&session.session_id, SessionState::Completed)
            .await;
        let found = manager
            .get_active_session_by_thread_id(&ThreadId::new("thread_active_1"))
            .await;
        assert!(found.is_none());

        // Cleanup
        manager.delete_session(&session.session_id).await;
    }

    #[tokio::test]
    #[ignore]
    async fn test_redis_get_active_session_by_thread_id_paused() {
        let pool = create_test_pool().await;
        let manager = RedisSessionManager::new(pool, 3600);

        let session = manager
            .create_session(RunId::new("run_paused_1"), ThreadId::new("thread_paused_1"))
            .await;

        let hitl_info = HitlWaitingInfo::new_simple(
            "call_1".to_string(),
            "/tasks/test".to_string(),
            "test_wf".to_string(),
            vec![PendingToolCallInfo {
                call_id: "call_1".to_string(),
                fn_name: "test_fn".to_string(),
                fn_arguments: "{}".to_string(),
            }],
        );
        manager
            .set_paused_with_hitl_info(&session.session_id, hitl_info)
            .await;

        let found = manager
            .get_active_session_by_thread_id(&ThreadId::new("thread_paused_1"))
            .await;
        assert!(found.is_some());
        assert_eq!(found.unwrap().state, SessionState::Paused);

        // Cleanup
        manager.delete_session(&session.session_id).await;
    }

    #[tokio::test]
    #[ignore]
    async fn test_redis_get_active_session_by_thread_id_nonexistent() {
        let pool = create_test_pool().await;
        let manager = RedisSessionManager::new(pool, 3600);

        let found = manager
            .get_active_session_by_thread_id(&ThreadId::new("nonexistent_thread"))
            .await;
        assert!(found.is_none());
    }

    async fn create_test_pool() -> &'static RedisPool {
        infra_utils::infra::test::setup_test_redis_pool().await
    }
}
