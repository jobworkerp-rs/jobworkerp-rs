use anyhow::Result;
use async_trait::async_trait;
use dashmap::DashMap;
use jobworkerp_runner::runner::FeedData;
use proto::jobworkerp::data::JobId;
use std::sync::Arc;
use tokio::sync::mpsc;

use super::FeedPublisher;

/// In-process feed sender store for Standalone mode.
/// Workers register a channel sender when starting a feed-enabled streaming job;
/// the gRPC handler looks it up to deliver feed data directly.
#[derive(Clone, Debug)]
pub struct ChanFeedSenderStore {
    senders: Arc<DashMap<i64, mpsc::Sender<FeedData>>>,
}

impl ChanFeedSenderStore {
    pub fn new() -> Self {
        Self {
            senders: Arc::new(DashMap::new()),
        }
    }

    /// Register a feed sender for a job.
    pub fn register(&self, job_id: i64, sender: mpsc::Sender<FeedData>) {
        self.senders.insert(job_id, sender);
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

    /// Access the underlying DashMap (for StreamWithFeedGuard cleanup).
    pub fn store(&self) -> &Arc<DashMap<i64, mpsc::Sender<FeedData>>> {
        &self.senders
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
            anyhow::anyhow!("No feed channel registered for job {}", job_id.value)
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
}
