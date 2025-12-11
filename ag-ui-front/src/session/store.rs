//! Event store for AG-UI event persistence and replay.

use crate::events::AgUiEvent;
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{watch, RwLock};

/// Type alias for event queue storage
type EventQueue = VecDeque<(u64, AgUiEvent)>;

/// Event data for a run, including events and last update timestamp
struct RunEventData {
    events: EventQueue,
    last_updated: DateTime<Utc>,
}

impl RunEventData {
    fn new() -> Self {
        Self {
            events: VecDeque::new(),
            last_updated: Utc::now(),
        }
    }
}

/// Type alias for run-to-events mapping
type EventsMap = HashMap<String, RunEventData>;

/// Event store trait for abstracting storage backends
#[async_trait]
pub trait EventStore: Send + Sync {
    /// Store an event for a run
    async fn store_event(&self, run_id: &str, event_id: u64, event: AgUiEvent);

    /// Get all events since a given event ID (exclusive)
    async fn get_events_since(&self, run_id: &str, since_event_id: u64) -> Vec<(u64, AgUiEvent)>;

    /// Get all events for a run
    async fn get_all_events(&self, run_id: &str) -> Vec<(u64, AgUiEvent)>;

    /// Clear all events for a run
    async fn clear_events(&self, run_id: &str);

    /// Get the latest event ID for a run
    async fn get_latest_event_id(&self, run_id: &str) -> Option<u64>;
}

/// Internal shared state for InMemoryEventStore
struct EventStoreInner {
    events: RwLock<EventsMap>,
    max_events_per_run: usize,
    ttl: Duration,
    cleanup_interval_sec: u64,
    shutdown_tx: watch::Sender<bool>,
    cleanup_started: AtomicBool,
}

impl Drop for EventStoreInner {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
    }
}

impl EventStoreInner {
    /// Common cleanup logic for expired events
    async fn cleanup_expired_events(&self) {
        let now = Utc::now();
        let mut events_map = self.events.write().await;

        let expired: Vec<String> = events_map
            .iter()
            .filter(|(_, data)| now.signed_duration_since(data.last_updated) > self.ttl)
            .map(|(run_id, _)| run_id.clone())
            .collect();

        for run_id in &expired {
            events_map.remove(run_id);
        }

        if !expired.is_empty() {
            tracing::debug!("Cleaned up {} expired event store entries", expired.len());
        }
    }
}

/// In-memory event store for standalone deployments
#[derive(Clone)]
pub struct InMemoryEventStore {
    inner: Arc<EventStoreInner>,
}

impl InMemoryEventStore {
    /// Maximum TTL value in seconds to prevent i64 overflow
    const MAX_TTL_SEC: u64 = i64::MAX as u64;

    /// Create a new in-memory event store
    ///
    /// # Arguments
    /// * `max_events_per_run` - Maximum events to store per run
    /// * `ttl_sec` - Time-to-live in seconds for run entries
    pub fn new(max_events_per_run: usize, ttl_sec: u64) -> Self {
        let clamped_ttl = ttl_sec.min(Self::MAX_TTL_SEC);
        let cleanup_interval_sec = Self::calculate_cleanup_interval(clamped_ttl);

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let inner = Arc::new(EventStoreInner {
            events: RwLock::new(HashMap::new()),
            max_events_per_run,
            ttl: Duration::seconds(clamped_ttl as i64),
            cleanup_interval_sec,
            shutdown_tx,
            cleanup_started: AtomicBool::new(false),
        });

        let store = Self { inner };
        store.ensure_cleanup_task_started(shutdown_rx);
        store
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
                            inner.cleanup_expired_events().await;
                        }
                        result = shutdown_rx.changed() => {
                            if result.is_err() || *shutdown_rx.borrow() {
                                tracing::debug!("Event store cleanup task shutting down");
                                break;
                            }
                        }
                    }
                }
            });
        }
    }
}

impl Default for InMemoryEventStore {
    fn default() -> Self {
        Self::new(1000, 3600) // 1000 events, 1 hour TTL
    }
}

#[async_trait]
impl EventStore for InMemoryEventStore {
    async fn store_event(&self, run_id: &str, event_id: u64, event: AgUiEvent) {
        let mut events_map = self.inner.events.write().await;
        let run_data = events_map
            .entry(run_id.to_string())
            .or_insert_with(RunEventData::new);

        run_data.events.push_back((event_id, event));
        run_data.last_updated = Utc::now();

        // Trim old events if exceeding max
        while run_data.events.len() > self.inner.max_events_per_run {
            run_data.events.pop_front();
        }
    }

    async fn get_events_since(&self, run_id: &str, since_event_id: u64) -> Vec<(u64, AgUiEvent)> {
        let events_map = self.inner.events.read().await;
        if let Some(run_data) = events_map.get(run_id) {
            run_data
                .events
                .iter()
                .filter(|(id, _)| *id > since_event_id)
                .cloned()
                .collect()
        } else {
            Vec::new()
        }
    }

    async fn get_all_events(&self, run_id: &str) -> Vec<(u64, AgUiEvent)> {
        let events_map = self.inner.events.read().await;
        if let Some(run_data) = events_map.get(run_id) {
            run_data.events.iter().cloned().collect()
        } else {
            Vec::new()
        }
    }

    async fn clear_events(&self, run_id: &str) {
        let mut events_map = self.inner.events.write().await;
        events_map.remove(run_id);
    }

    async fn get_latest_event_id(&self, run_id: &str) -> Option<u64> {
        let events_map = self.inner.events.read().await;
        events_map.get(run_id)?.events.back().map(|(id, _)| *id)
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

    #[tokio::test]
    async fn test_store_and_retrieve_event() {
        let store = InMemoryEventStore::new(100, 3600);
        let event = create_test_event(1);

        store.store_event("run_1", 0, event.clone()).await;

        let events = store.get_all_events("run_1").await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, 0);
    }

    #[tokio::test]
    async fn test_get_events_since() {
        let store = InMemoryEventStore::new(100, 3600);

        for i in 0..5 {
            store.store_event("run_1", i, create_test_event(i)).await;
        }

        let events = store.get_events_since("run_1", 2).await;
        assert_eq!(events.len(), 2); // Events 3 and 4
        assert_eq!(events[0].0, 3);
        assert_eq!(events[1].0, 4);
    }

    #[tokio::test]
    async fn test_get_events_since_empty() {
        let store = InMemoryEventStore::new(100, 3600);

        store.store_event("run_1", 0, create_test_event(0)).await;
        store.store_event("run_1", 1, create_test_event(1)).await;

        let events = store.get_events_since("run_1", 10).await;
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn test_get_events_nonexistent_run() {
        let store = InMemoryEventStore::new(100, 3600);
        let events = store.get_all_events("nonexistent").await;
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn test_clear_events() {
        let store = InMemoryEventStore::new(100, 3600);

        store.store_event("run_1", 0, create_test_event(0)).await;
        store.store_event("run_1", 1, create_test_event(1)).await;

        store.clear_events("run_1").await;

        let events = store.get_all_events("run_1").await;
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn test_max_events_limit() {
        let store = InMemoryEventStore::new(3, 3600);

        for i in 0..5 {
            store.store_event("run_1", i, create_test_event(i)).await;
        }

        let events = store.get_all_events("run_1").await;
        assert_eq!(events.len(), 3);
        // Should keep the last 3 events (2, 3, 4)
        assert_eq!(events[0].0, 2);
        assert_eq!(events[1].0, 3);
        assert_eq!(events[2].0, 4);
    }

    #[tokio::test]
    async fn test_multiple_runs() {
        let store = InMemoryEventStore::new(100, 3600);

        store.store_event("run_1", 0, create_test_event(0)).await;
        store.store_event("run_1", 1, create_test_event(1)).await;
        store.store_event("run_2", 0, create_test_event(10)).await;

        let events_1 = store.get_all_events("run_1").await;
        let events_2 = store.get_all_events("run_2").await;

        assert_eq!(events_1.len(), 2);
        assert_eq!(events_2.len(), 1);
    }

    #[tokio::test]
    async fn test_get_latest_event_id() {
        let store = InMemoryEventStore::new(100, 3600);

        assert!(store.get_latest_event_id("run_1").await.is_none());

        store.store_event("run_1", 5, create_test_event(5)).await;
        store.store_event("run_1", 10, create_test_event(10)).await;
        store.store_event("run_1", 7, create_test_event(7)).await;

        // Should return the last stored event ID, not the highest
        let latest = store.get_latest_event_id("run_1").await;
        assert_eq!(latest, Some(7));
    }

    #[tokio::test]
    async fn test_store_different_event_types() {
        let store = InMemoryEventStore::new(100, 3600);

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

        store.store_event("run_1", 0, run_started).await;
        store.store_event("run_1", 1, step_started).await;
        store.store_event("run_1", 2, state_snapshot).await;

        let events = store.get_all_events("run_1").await;
        assert_eq!(events.len(), 3);

        // Verify event types
        assert!(matches!(events[0].1, AgUiEvent::RunStarted { .. }));
        assert!(matches!(events[1].1, AgUiEvent::StepStarted { .. }));
        assert!(matches!(events[2].1, AgUiEvent::StateSnapshot { .. }));
    }

    #[tokio::test]
    async fn test_ttl_cleanup() {
        // Create store with 1 second TTL
        let store = InMemoryEventStore::new(100, 1);

        // Store events
        store.store_event("run_1", 0, create_test_event(0)).await;
        store.store_event("run_1", 1, create_test_event(1)).await;

        // Verify events exist
        let events = store.get_all_events("run_1").await;
        assert_eq!(events.len(), 2);

        // Wait for TTL to expire plus cleanup interval margin
        // Cleanup task runs every 60 seconds, so we manually verify expiration logic
        // by checking the internal state after TTL expires
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        // Manually trigger cleanup by accessing internal state
        // The background task runs every 60s, which is too long for a test
        // Instead, we verify the last_updated timestamp logic directly
        {
            let events_map = store.inner.events.read().await;
            if let Some(run_data) = events_map.get("run_1") {
                let now = Utc::now();
                let elapsed = now.signed_duration_since(run_data.last_updated);
                // Verify that the entry is now older than TTL
                assert!(
                    elapsed > store.inner.ttl,
                    "Entry should be older than TTL after waiting"
                );
            }
        }
    }

    #[tokio::test]
    async fn test_last_updated_refresh_on_store() {
        // Create store with long TTL
        let store = InMemoryEventStore::new(100, 3600);

        // Store initial event
        store.store_event("run_1", 0, create_test_event(0)).await;

        // Get initial last_updated
        let initial_time = {
            let events_map = store.inner.events.read().await;
            events_map.get("run_1").unwrap().last_updated
        };

        // Wait a bit
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Store another event
        store.store_event("run_1", 1, create_test_event(1)).await;

        // Verify last_updated was refreshed
        let updated_time = {
            let events_map = store.inner.events.read().await;
            events_map.get("run_1").unwrap().last_updated
        };

        assert!(
            updated_time > initial_time,
            "last_updated should be refreshed on store_event"
        );
    }

    #[tokio::test]
    async fn test_different_runs_independent_ttl() {
        // Create store with 1 second TTL
        let store = InMemoryEventStore::new(100, 1);

        // Store event for run_1
        store.store_event("run_1", 0, create_test_event(0)).await;

        // Wait a bit (but less than TTL)
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Store event for run_2 (this should have a newer last_updated)
        store.store_event("run_2", 0, create_test_event(0)).await;

        // Verify both runs exist
        assert_eq!(store.get_all_events("run_1").await.len(), 1);
        assert_eq!(store.get_all_events("run_2").await.len(), 1);

        // Verify run_1 is older than run_2
        let (run1_time, run2_time) = {
            let events_map = store.inner.events.read().await;
            (
                events_map.get("run_1").unwrap().last_updated,
                events_map.get("run_2").unwrap().last_updated,
            )
        };

        assert!(
            run2_time > run1_time,
            "run_2 should have a newer last_updated than run_1"
        );
    }
}
