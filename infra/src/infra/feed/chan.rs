use anyhow::Result;
use async_trait::async_trait;
use dashmap::DashMap;
use jobworkerp_runner::runner::FeedData;
use proto::jobworkerp::data::JobId;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Notify, mpsc};

use super::FeedPublisher;

/// In-process feed sender store for Standalone mode.
/// Workers register a channel sender when starting a feed-enabled streaming job;
/// the gRPC handler looks it up to deliver feed data directly.
///
/// Registration invariant: entries are only inserted by `run_job()` for jobs that
/// satisfy all feed preconditions (Running state, streaming_type != None, use_static,
/// concurrency == 1, require_client_stream).
#[derive(Clone, Debug)]
pub struct ChanFeedSenderStore {
    senders: Arc<DashMap<i64, mpsc::Sender<FeedData>>>,
    // Notify waiters when a feed sender is registered for a job
    feed_ready_notifiers: Arc<DashMap<i64, Arc<Notify>>>,
}

impl ChanFeedSenderStore {
    pub fn new() -> Self {
        Self {
            senders: Arc::new(DashMap::new()),
            feed_ready_notifiers: Arc::new(DashMap::new()),
        }
    }

    /// Register a feed sender for a job.
    pub fn register(&self, job_id: i64, sender: mpsc::Sender<FeedData>) {
        self.senders.insert(job_id, sender);
        // Notify any waiting feed forwarder
        if let Some((_, notify)) = self.feed_ready_notifiers.remove(&job_id) {
            notify.notify_one();
        }
    }

    /// Remove and return the sender for a job (cleanup on completion/drop).
    pub fn remove(&self, job_id: i64) -> Option<mpsc::Sender<FeedData>> {
        self.senders.remove(&job_id).map(|(_, v)| v)
    }

    /// Get a clone of the sender (for publishing without removal).
    pub fn get(&self, job_id: i64) -> Option<mpsc::Sender<FeedData>> {
        self.senders
            .get(&job_id)
            .map(|r: dashmap::mapref::one::Ref<'_, i64, mpsc::Sender<FeedData>>| r.value().clone())
    }

    /// Wait until a feed channel is registered for the given job_id.
    /// Returns Ok(()) when feed channel is ready, Err on timeout.
    pub async fn wait_for_feed_ready(&self, job_id: i64, timeout: Duration) -> Result<()> {
        // Fast path: already registered
        if self.senders.contains_key(&job_id) {
            return Ok(());
        }

        // Register notifier
        let notify = Arc::new(Notify::new());
        self.feed_ready_notifiers.insert(job_id, notify.clone());

        // Double-check after registration to avoid race condition
        if self.senders.contains_key(&job_id) {
            self.feed_ready_notifiers.remove(&job_id);
            return Ok(());
        }

        // Wait with timeout
        match tokio::time::timeout(timeout, notify.notified()).await {
            Ok(()) => {
                self.feed_ready_notifiers.remove(&job_id);
                Ok(())
            }
            Err(_) => {
                self.feed_ready_notifiers.remove(&job_id);
                Err(anyhow::anyhow!(
                    "Feed channel not ready for job {} within {:?}",
                    job_id,
                    timeout
                ))
            }
        }
    }
}

impl Default for ChanFeedSenderStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl FeedPublisher for ChanFeedSenderStore {
    async fn publish_feed(&self, job_id: &JobId, data: Vec<u8>, is_final: bool) -> Result<()> {
        let sender: mpsc::Sender<FeedData> = self.get(job_id.value).ok_or_else(|| {
            anyhow::anyhow!(
                "No feed channel registered for job {} (not registered, already finalized, or stream completed)",
                job_id.value
            )
        })?;

        let feed = FeedData { data, is_final };
        if let Err(e) = sender.send(feed).await {
            // Receiver dropped: remove stale entry before returning error
            self.remove(job_id.value);
            return Err(anyhow::anyhow!(
                "Failed to send feed data to job {}: {:?}",
                job_id.value,
                e
            ));
        }

        if is_final {
            self.remove(job_id.value);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_register_get_remove() {
        let store = ChanFeedSenderStore::new();
        let (tx, mut rx) = mpsc::channel::<FeedData>(16);

        store.register(42, tx);

        // get should return a sender
        assert!(store.get(42).is_some());
        assert!(store.get(999).is_none());

        // publish via FeedPublisher
        let job_id = JobId { value: 42 };
        store
            .publish_feed(&job_id, b"hello".to_vec(), false)
            .await
            .unwrap();

        let feed = rx.recv().await.unwrap();
        assert_eq!(feed.data, b"hello");
        assert!(!feed.is_final);

        // publish final should auto-remove
        store
            .publish_feed(&job_id, b"done".to_vec(), true)
            .await
            .unwrap();

        let feed = rx.recv().await.unwrap();
        assert_eq!(feed.data, b"done");
        assert!(feed.is_final);

        // sender should be removed after final
        assert!(store.get(42).is_none());
    }

    #[tokio::test]
    async fn test_publish_to_missing_job_returns_error() {
        let store = ChanFeedSenderStore::new();
        let job_id = JobId { value: 999 };
        let result = store.publish_feed(&job_id, b"data".to_vec(), false).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_remove() {
        let store = ChanFeedSenderStore::new();
        let (tx, _rx) = mpsc::channel::<FeedData>(16);

        store.register(1, tx);
        assert!(store.remove(1).is_some());
        assert!(store.remove(1).is_none());
    }

    #[tokio::test]
    async fn test_publish_to_dropped_receiver_removes_stale_entry() {
        let store = ChanFeedSenderStore::new();
        let (tx, rx) = mpsc::channel::<FeedData>(16);

        store.register(77, tx);
        assert!(store.get(77).is_some());

        // Drop receiver to simulate disconnected consumer
        drop(rx);

        let job_id = JobId { value: 77 };
        let result = store.publish_feed(&job_id, b"data".to_vec(), false).await;
        assert!(result.is_err());

        // Stale entry should be removed
        assert!(store.get(77).is_none());
    }

    #[tokio::test]
    async fn test_wait_for_feed_ready_already_registered() {
        let store = ChanFeedSenderStore::new();
        let (tx, _rx) = mpsc::channel::<FeedData>(16);
        store.register(100, tx);

        // Should return immediately since already registered
        let result = store
            .wait_for_feed_ready(100, Duration::from_millis(100))
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_wait_for_feed_ready_delayed_registration() {
        let store = ChanFeedSenderStore::new();
        let store_clone = store.clone();

        // Spawn delayed registration
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let (tx, _rx) = mpsc::channel::<FeedData>(16);
            store_clone.register(200, tx);
        });

        // Should wait and succeed
        let result = store.wait_for_feed_ready(200, Duration::from_secs(5)).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_wait_for_feed_ready_timeout() {
        let store = ChanFeedSenderStore::new();

        // Should timeout since no registration happens
        let result = store
            .wait_for_feed_ready(300, Duration::from_millis(50))
            .await;
        assert!(result.is_err());

        // Notifier should be cleaned up
        assert!(!store.feed_ready_notifiers.contains_key(&300));
    }
}
