//! Temporary, single-use state for external-tool approvals.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use redis::Script;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use thiserror::Error;

const CLAIM_PENDING_APPROVAL_SCRIPT: &str = r#"
local value = redis.call('GET', KEYS[1])
if not value then
    return nil
end
redis.call('DEL', KEYS[1])
return value
"#;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingApproval {
    pub chat_id: String,
    pub call_id: String,
    pub tool_name: String,
    pub worker_id: i64,
    pub method: String,
    pub arguments: Value,
    pub continuation_history: Vec<Value>,
    pub deadline: DateTime<Utc>,
}

#[derive(Debug, Error)]
pub enum PendingApprovalStoreError {
    #[error("pending approval deadline has elapsed")]
    Expired,
    #[error("pending approval already exists")]
    AlreadyExists,
    #[error("redis approval store operation failed")]
    Redis(#[from] redis::RedisError),
    #[error("pending approval encoding error: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// Save an approval challenge and atomically claim it at most once.
#[async_trait]
pub trait PendingApprovalStore: Send + Sync {
    async fn save(&self, approval: PendingApproval) -> Result<(), PendingApprovalStoreError>;

    /// Returns the pending state only to the caller that wins the single-use claim.
    async fn claim(
        &self,
        chat_id: &str,
        call_id: &str,
    ) -> Result<Option<PendingApproval>, PendingApprovalStoreError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ApprovalKey {
    chat_id: String,
    call_id: String,
}

struct MemoryEntry {
    approval: PendingApproval,
    expires_at: Instant,
}

/// In-process implementation. State is lost on restart, and each claim performs lookup, expiry
/// check, and removal while holding the same mutex.
pub struct MemoryPendingApprovalStore {
    ttl: Duration,
    entries: Mutex<HashMap<ApprovalKey, MemoryEntry>>,
}

impl MemoryPendingApprovalStore {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl PendingApprovalStore for MemoryPendingApprovalStore {
    async fn save(&self, approval: PendingApproval) -> Result<(), PendingApprovalStoreError> {
        if approval.deadline <= Utc::now() || self.ttl.is_zero() {
            return Err(PendingApprovalStoreError::Expired);
        }
        let key = ApprovalKey {
            chat_id: approval.chat_id.clone(),
            call_id: approval.call_id.clone(),
        };
        let now = Instant::now();
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        prune_expired(&mut entries, now);
        if entries.contains_key(&key) {
            return Err(PendingApprovalStoreError::AlreadyExists);
        }
        let Some(expires_at) = now.checked_add(self.ttl) else {
            return Err(PendingApprovalStoreError::Expired);
        };
        entries.insert(
            key,
            MemoryEntry {
                approval,
                expires_at,
            },
        );
        Ok(())
    }

    async fn claim(
        &self,
        chat_id: &str,
        call_id: &str,
    ) -> Result<Option<PendingApproval>, PendingApprovalStoreError> {
        let key = ApprovalKey {
            chat_id: chat_id.to_owned(),
            call_id: call_id.to_owned(),
        };
        let now = Instant::now();
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(entry) = entries.remove(&key) else {
            return Ok(None);
        };
        if entry.expires_at <= now || entry.approval.deadline <= Utc::now() {
            return Ok(None);
        }
        Ok(Some(entry.approval))
    }
}

fn prune_expired(entries: &mut HashMap<ApprovalKey, MemoryEntry>, now: Instant) {
    let utc_now = Utc::now();
    entries.retain(|_, entry| entry.expires_at > now && entry.approval.deadline > utc_now);
}

/// Redis implementation using a private connection without debug-printing secret-bearing URLs.
#[derive(Clone)]
pub struct RedisPendingApprovalStore {
    client: redis::Client,
    ttl: Duration,
}

impl RedisPendingApprovalStore {
    pub fn new(client: redis::Client, ttl: Duration) -> Self {
        Self { client, ttl }
    }

    fn redis_key(chat_id: &str, call_id: &str) -> String {
        // Length-prefix the components to avoid ambiguous keys when IDs contain separators.
        format!(
            "agent_server:pending_approval:{}:{}:{}:{}",
            chat_id.len(),
            chat_id,
            call_id.len(),
            call_id
        )
    }
}

#[async_trait]
impl PendingApprovalStore for RedisPendingApprovalStore {
    async fn save(&self, approval: PendingApproval) -> Result<(), PendingApprovalStoreError> {
        if approval.deadline <= Utc::now() || self.ttl.is_zero() {
            return Err(PendingApprovalStoreError::Expired);
        }
        let redis_key = Self::redis_key(&approval.chat_id, &approval.call_id);
        let value = serde_json::to_string(&approval)?;
        let mut connection = self.client.get_multiplexed_async_connection().await?;
        // A connection can take longer than the remaining approval window. Never publish
        // continuation text with a fresh TTL after the original deadline has elapsed.
        let remaining = approval.deadline.timestamp_millis() - Utc::now().timestamp_millis();
        if remaining <= 0 {
            return Err(PendingApprovalStoreError::Expired);
        }
        let ttl_millis = self
            .ttl
            .as_millis()
            .max(1)
            .min(remaining as u128)
            .min(i64::MAX as u128) as i64;
        let result: Option<String> = redis::cmd("SET")
            .arg(&redis_key)
            .arg(value)
            .arg("NX")
            .arg("PX")
            .arg(ttl_millis)
            .query_async(&mut connection)
            .await?;
        if result.is_none() {
            return Err(PendingApprovalStoreError::AlreadyExists);
        }
        Ok(())
    }

    async fn claim(
        &self,
        chat_id: &str,
        call_id: &str,
    ) -> Result<Option<PendingApproval>, PendingApprovalStoreError> {
        let redis_key = Self::redis_key(chat_id, call_id);
        let mut connection = self.client.get_multiplexed_async_connection().await?;
        let serialized: Option<String> = Script::new(CLAIM_PENDING_APPROVAL_SCRIPT)
            .key(redis_key)
            .invoke_async(&mut connection)
            .await?;
        let Some(serialized) = serialized else {
            return Ok(None);
        };
        let approval: PendingApproval = serde_json::from_str(&serialized)?;
        if approval.deadline <= Utc::now() {
            return Ok(None);
        }
        Ok(Some(approval))
    }
}
