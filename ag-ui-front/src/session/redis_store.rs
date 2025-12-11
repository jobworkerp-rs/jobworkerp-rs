//! Redis-backed event store for scalable deployments.
//!
//! This module provides a Redis-based implementation of the EventStore trait,
//! enabling event persistence and replay across multiple AG-UI server instances.

use super::store::EventStore;
use crate::events::AgUiEvent;
use async_trait::async_trait;
use deadpool_redis::redis::{AsyncCommands, Script};
use deadpool_redis::Pool;
use serde::{Deserialize, Serialize};

/// Redis key prefix for event lists
const EVENT_KEY_PREFIX: &str = "ag_ui:events:";

/// Lua script for atomic event storage with trimming.
///
/// Performs ZADD, EXPIRE, and conditional ZREMRANGEBYRANK atomically to prevent
/// race conditions between concurrent writes that could cause unintended event deletion.
const STORE_EVENT_SCRIPT: &str = r#"
local events_key = KEYS[1]
local max_events = tonumber(ARGV[1])
local ttl_sec = tonumber(ARGV[2])
local event_json = ARGV[3]
local event_id = tonumber(ARGV[4])

-- Add event to sorted set with event_id as score
redis.call('ZADD', events_key, event_id, event_json)

-- Set TTL on the key
redis.call('EXPIRE', events_key, ttl_sec)

-- Trim to max events (remove oldest events beyond limit)
local count = redis.call('ZCARD', events_key)
if count > max_events then
    local to_remove = count - max_events
    redis.call('ZREMRANGEBYRANK', events_key, 0, to_remove - 1)
end

return 'OK'
"#;

/// Serializable event wrapper for Redis storage
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RedisEventData {
    event_id: u64,
    event: AgUiEvent,
}

/// Redis-backed event store for scalable deployments.
///
/// Stores events in Redis lists with automatic TTL-based expiration.
/// Uses sorted sets for efficient range queries by event ID.
#[derive(Clone)]
pub struct RedisEventStore {
    pool: Pool,
    max_events_per_run: usize,
    ttl_sec: u64,
}

impl RedisEventStore {
    /// Create a new Redis event store.
    ///
    /// # Arguments
    /// * `pool` - Redis connection pool
    /// * `max_events_per_run` - Maximum events to store per run
    /// * `ttl_sec` - Event TTL in seconds
    pub fn new(pool: Pool, max_events_per_run: usize, ttl_sec: u64) -> Self {
        Self {
            pool,
            max_events_per_run,
            ttl_sec,
        }
    }

    /// Generate Redis key for event sorted set
    fn events_key(run_id: &str) -> String {
        format!("{}{}", EVENT_KEY_PREFIX, run_id)
    }
}

#[async_trait]
impl EventStore for RedisEventStore {
    async fn store_event(&self, run_id: &str, event_id: u64, event: AgUiEvent) {
        let mut conn = match self.pool.get().await {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(
                    run_id = %run_id,
                    event_id = event_id,
                    error = %e,
                    "Failed to acquire Redis connection for store_event"
                );
                return;
            }
        };

        let events_key = Self::events_key(run_id);
        let data = RedisEventData {
            event_id,
            event: event.clone(),
        };

        let json = match serde_json::to_string(&data) {
            Ok(j) => j,
            Err(e) => {
                tracing::debug!(
                    run_id = %run_id,
                    event_id = event_id,
                    error = %e,
                    "Failed to serialize event data"
                );
                return;
            }
        };

        // Execute atomic Lua script for ZADD + EXPIRE + ZREMRANGEBYRANK
        let script = Script::new(STORE_EVENT_SCRIPT);
        let result: Result<String, _> = script
            .key(&events_key)
            .arg(self.max_events_per_run)
            .arg(self.ttl_sec)
            .arg(&json)
            .arg(event_id)
            .invoke_async(&mut *conn)
            .await;

        if let Err(e) = result {
            tracing::debug!(
                run_id = %run_id,
                event_id = event_id,
                error = %e,
                "Failed to execute store_event Lua script"
            );
        }
    }

    async fn get_events_since(&self, run_id: &str, since_event_id: u64) -> Vec<(u64, AgUiEvent)> {
        let mut conn = match self.pool.get().await {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(
                    run_id = %run_id,
                    since_event_id = since_event_id,
                    error = %e,
                    "Failed to acquire Redis connection for get_events_since"
                );
                return Vec::new();
            }
        };

        let events_key = Self::events_key(run_id);

        // Get events with score > since_event_id (exclusive)
        let results: Vec<String> = match conn
            .zrangebyscore(
                &events_key,
                format!("({}", since_event_id), // Exclusive lower bound
                "+inf",
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(
                    run_id = %run_id,
                    since_event_id = since_event_id,
                    error = %e,
                    "Failed to get events from Redis sorted set"
                );
                return Vec::new();
            }
        };

        results
            .into_iter()
            .filter_map(|json| {
                serde_json::from_str::<RedisEventData>(&json)
                    .map_err(|e| {
                        tracing::debug!(
                            run_id = %run_id,
                            error = %e,
                            "Failed to deserialize event data from Redis"
                        );
                        e
                    })
                    .ok()
                    .map(|data| (data.event_id, data.event))
            })
            .collect()
    }

    async fn get_all_events(&self, run_id: &str) -> Vec<(u64, AgUiEvent)> {
        let mut conn = match self.pool.get().await {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(
                    run_id = %run_id,
                    error = %e,
                    "Failed to acquire Redis connection for get_all_events"
                );
                return Vec::new();
            }
        };

        let events_key = Self::events_key(run_id);

        // Get all events sorted by event_id
        let results: Vec<String> = match conn.zrangebyscore(&events_key, "-inf", "+inf").await {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(
                    run_id = %run_id,
                    error = %e,
                    "Failed to get all events from Redis sorted set"
                );
                return Vec::new();
            }
        };

        results
            .into_iter()
            .filter_map(|json| {
                serde_json::from_str::<RedisEventData>(&json)
                    .map_err(|e| {
                        tracing::debug!(
                            run_id = %run_id,
                            error = %e,
                            "Failed to deserialize event data from Redis"
                        );
                        e
                    })
                    .ok()
                    .map(|data| (data.event_id, data.event))
            })
            .collect()
    }

    async fn clear_events(&self, run_id: &str) {
        let mut conn = match self.pool.get().await {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(
                    run_id = %run_id,
                    error = %e,
                    "Failed to acquire Redis connection for clear_events"
                );
                return;
            }
        };

        let events_key = Self::events_key(run_id);

        if let Err(e) = conn.del::<_, ()>(&events_key).await {
            tracing::debug!(
                run_id = %run_id,
                error = %e,
                "Failed to delete events key from Redis"
            );
        }
    }

    async fn get_latest_event_id(&self, run_id: &str) -> Option<u64> {
        let mut conn = match self.pool.get().await {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(
                    run_id = %run_id,
                    error = %e,
                    "Failed to acquire Redis connection for get_latest_event_id"
                );
                return None;
            }
        };

        let events_key = Self::events_key(run_id);

        // Get the highest scored (most recent) event
        let results: Vec<(String, f64)> = match conn
            .zrevrangebyscore_withscores(&events_key, "+inf", "-inf")
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(
                    run_id = %run_id,
                    error = %e,
                    "Failed to get latest event from Redis"
                );
                return None;
            }
        };

        results.first().map(|(_, score)| *score as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::state::WorkflowState;

    fn create_test_event(n: u64) -> AgUiEvent {
        AgUiEvent::TextMessageContent {
            message_id: format!("msg_{}", n),
            delta: format!("chunk {}", n),
            timestamp: Some(n as f64),
        }
    }

    // Integration tests require running Redis instance

    #[tokio::test]
    async fn test_redis_store_and_retrieve_event() {
        let pool = create_test_pool().await;
        let store = RedisEventStore::new(pool, 100, 3600);
        let run_id = "test_run_store_1";

        // Clean up before test
        store.clear_events(run_id).await;

        let event = create_test_event(1);
        store.store_event(run_id, 0, event.clone()).await;

        let events = store.get_all_events(run_id).await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, 0);

        // Cleanup
        store.clear_events(run_id).await;
    }

    #[tokio::test]
    async fn test_redis_get_events_since() {
        let pool = create_test_pool().await;
        let store = RedisEventStore::new(pool, 100, 3600);
        let run_id = "test_run_since_1";

        // Clean up before test
        store.clear_events(run_id).await;

        for i in 0..5 {
            store.store_event(run_id, i, create_test_event(i)).await;
        }

        let events = store.get_events_since(run_id, 2).await;
        assert_eq!(events.len(), 2); // Events 3 and 4
        assert_eq!(events[0].0, 3);
        assert_eq!(events[1].0, 4);

        // Cleanup
        store.clear_events(run_id).await;
    }

    #[tokio::test]
    async fn test_redis_max_events_limit() {
        let pool = create_test_pool().await;
        let store = RedisEventStore::new(pool, 3, 3600);
        let run_id = "test_run_limit_1";

        // Clean up before test
        store.clear_events(run_id).await;

        for i in 0..5 {
            store.store_event(run_id, i, create_test_event(i)).await;
        }

        let events = store.get_all_events(run_id).await;
        assert_eq!(events.len(), 3);
        // Should keep the last 3 events (2, 3, 4)
        assert_eq!(events[0].0, 2);
        assert_eq!(events[1].0, 3);
        assert_eq!(events[2].0, 4);

        // Cleanup
        store.clear_events(run_id).await;
    }

    #[tokio::test]
    async fn test_redis_clear_events() {
        let pool = create_test_pool().await;
        let store = RedisEventStore::new(pool, 100, 3600);
        let run_id = "test_run_clear_1";

        store.store_event(run_id, 0, create_test_event(0)).await;
        store.store_event(run_id, 1, create_test_event(1)).await;

        store.clear_events(run_id).await;

        let events = store.get_all_events(run_id).await;
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn test_redis_get_latest_event_id() {
        let pool = create_test_pool().await;
        let store = RedisEventStore::new(pool, 100, 3600);
        let run_id = "test_run_latest_1";

        // Clean up before test
        store.clear_events(run_id).await;

        assert!(store.get_latest_event_id(run_id).await.is_none());

        store.store_event(run_id, 5, create_test_event(5)).await;
        store.store_event(run_id, 10, create_test_event(10)).await;
        store.store_event(run_id, 7, create_test_event(7)).await;

        // Should return the highest event ID
        let latest = store.get_latest_event_id(run_id).await;
        assert_eq!(latest, Some(10));

        // Cleanup
        store.clear_events(run_id).await;
    }

    #[tokio::test]
    async fn test_redis_multiple_runs() {
        let pool = create_test_pool().await;
        let store = RedisEventStore::new(pool, 100, 3600);
        let run_id_1 = "test_multi_run_1";
        let run_id_2 = "test_multi_run_2";

        // Clean up before test
        store.clear_events(run_id_1).await;
        store.clear_events(run_id_2).await;

        store.store_event(run_id_1, 0, create_test_event(0)).await;
        store.store_event(run_id_1, 1, create_test_event(1)).await;
        store.store_event(run_id_2, 0, create_test_event(10)).await;

        let events_1 = store.get_all_events(run_id_1).await;
        let events_2 = store.get_all_events(run_id_2).await;

        assert_eq!(events_1.len(), 2);
        assert_eq!(events_2.len(), 1);

        // Cleanup
        store.clear_events(run_id_1).await;
        store.clear_events(run_id_2).await;
    }

    #[tokio::test]
    async fn test_redis_store_different_event_types() {
        let pool = create_test_pool().await;
        let store = RedisEventStore::new(pool, 100, 3600);
        let run_id = "test_run_types_1";

        // Clean up before test
        store.clear_events(run_id).await;

        let run_started = AgUiEvent::RunStarted {
            run_id: "run_1".to_string(),
            thread_id: "thread_1".to_string(),
            timestamp: None,
            metadata: None,
        };

        let step_started = AgUiEvent::StepStarted {
            step_id: "step_1".to_string(),
            step_name: Some("task1".to_string()),
            parent_step_id: None,
            timestamp: None,
            metadata: None,
        };

        let state_snapshot = AgUiEvent::StateSnapshot {
            snapshot: WorkflowState::new("test"),
            timestamp: None,
        };

        store.store_event(run_id, 0, run_started).await;
        store.store_event(run_id, 1, step_started).await;
        store.store_event(run_id, 2, state_snapshot).await;

        let events = store.get_all_events(run_id).await;
        assert_eq!(events.len(), 3);

        // Verify event types
        assert!(matches!(events[0].1, AgUiEvent::RunStarted { .. }));
        assert!(matches!(events[1].1, AgUiEvent::StepStarted { .. }));
        assert!(matches!(events[2].1, AgUiEvent::StateSnapshot { .. }));

        // Cleanup
        store.clear_events(run_id).await;
    }

    /// Test concurrent writes to verify Lua script atomicity.
    ///
    /// This test verifies that concurrent store_event calls do not cause
    /// race conditions that could delete more events than intended.
    #[tokio::test]
    async fn test_redis_concurrent_writes_atomic_trimming() {
        let pool = create_test_pool().await;
        let max_events = 50;
        let store = std::sync::Arc::new(RedisEventStore::new(pool, max_events, 3600));
        let run_id = "test_concurrent_writes_1";

        // Clean up before test
        store.clear_events(run_id).await;

        // Spawn multiple tasks that write events concurrently
        let num_tasks = 10;
        let events_per_task = 20;
        let total_events = num_tasks * events_per_task;

        let mut handles = Vec::new();
        for task_id in 0..num_tasks {
            let store_clone = store.clone();
            let run_id_owned = run_id.to_string();
            handles.push(tokio::spawn(async move {
                for i in 0..events_per_task {
                    let event_id = (task_id * events_per_task + i) as u64;
                    let event = create_test_event(event_id);
                    store_clone
                        .store_event(&run_id_owned, event_id, event)
                        .await;
                }
            }));
        }

        // Wait for all tasks to complete
        for handle in handles {
            handle.await.unwrap();
        }

        // Verify results
        let events = store.get_all_events(run_id).await;

        // Should have exactly max_events (or total_events if less than max)
        let expected_count = std::cmp::min(max_events, total_events);
        assert_eq!(
            events.len(),
            expected_count,
            "Expected {} events but got {}. Race condition may have caused over-deletion.",
            expected_count,
            events.len()
        );

        // Verify that events are sorted by event_id and contain the highest IDs
        let event_ids: Vec<u64> = events.iter().map(|(id, _)| *id).collect();
        let mut sorted_ids = event_ids.clone();
        sorted_ids.sort();
        assert_eq!(event_ids, sorted_ids, "Events should be sorted by event_id");

        // The highest event_ids should be preserved (oldest trimmed first)
        let min_expected_id = (total_events - max_events) as u64;
        for (id, _) in &events {
            assert!(
                *id >= min_expected_id,
                "Event ID {} is lower than expected minimum {}. Old events should be trimmed.",
                id,
                min_expected_id
            );
        }

        // Cleanup
        store.clear_events(run_id).await;
    }

    /// Test that concurrent writes from multiple "runs" don't interfere with each other.
    #[tokio::test]
    async fn test_redis_concurrent_multiple_runs() {
        let pool = create_test_pool().await;
        let store = std::sync::Arc::new(RedisEventStore::new(pool, 10, 3600));

        let num_runs = 5;
        let events_per_run = 15;

        // Clean up before test
        for i in 0..num_runs {
            store
                .clear_events(&format!("test_concurrent_run_{}", i))
                .await;
        }

        let mut handles = Vec::new();
        for run_idx in 0..num_runs {
            let store_clone = store.clone();
            let run_id = format!("test_concurrent_run_{}", run_idx);
            handles.push(tokio::spawn(async move {
                for i in 0..events_per_run {
                    let event = create_test_event(i as u64);
                    store_clone.store_event(&run_id, i as u64, event).await;
                }
            }));
        }

        // Wait for all tasks to complete
        for handle in handles {
            handle.await.unwrap();
        }

        // Verify each run has exactly max_events
        for run_idx in 0..num_runs {
            let run_id = format!("test_concurrent_run_{}", run_idx);
            let events = store.get_all_events(&run_id).await;
            assert_eq!(
                events.len(),
                10,
                "Run {} should have exactly 10 events, got {}",
                run_idx,
                events.len()
            );

            // Cleanup
            store.clear_events(&run_id).await;
        }
    }

    async fn create_test_pool() -> Pool {
        let config = deadpool_redis::Config::from_url("redis://127.0.0.1:6379");
        config
            .builder()
            .map(|b| b.max_size(16).build().unwrap())
            .expect("Failed to create Redis pool")
    }
}
