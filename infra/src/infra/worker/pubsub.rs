use anyhow::Result;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use memory_utils::chan::broadcast::BroadcastChan;
use proto::jobworkerp::data::{Worker, WorkerData, WorkerId};

/// Memory-based worker change pubsub for standalone mode.
/// Mirrors the Redis-based worker pubsub (`UseWorkerPublish` / `UseSubscribeWorker`)
/// using a single `BroadcastChan` instead of Redis pubsub.
#[derive(Clone, Debug)]
pub struct ChanWorkerPubSubRepositoryImpl {
    broadcast_chan: BroadcastChan<Vec<u8>>,
}

impl ChanWorkerPubSubRepositoryImpl {
    pub fn new(capacity: usize) -> Self {
        Self {
            broadcast_chan: BroadcastChan::new(capacity),
        }
    }

    pub fn publish_worker_changed(&self, id: &WorkerId, data: &WorkerData) -> Result<bool> {
        let worker = Worker {
            id: Some(*id),
            data: Some(data.clone()),
        };
        let worker_data = ProstMessageCodec::serialize_message(&worker)?;
        self.broadcast_chan.send(worker_data)
    }

    pub fn publish_worker_deleted(&self, worker_id: &WorkerId) -> Result<bool> {
        let worker = Worker {
            id: Some(*worker_id),
            data: None,
        };
        let worker_data = ProstMessageCodec::serialize_message(&worker)?;
        self.broadcast_chan.send(worker_data)
    }

    pub fn publish_worker_all_deleted(&self) -> Result<bool> {
        let worker = Worker {
            id: None,
            data: None,
        };
        let worker_data = ProstMessageCodec::serialize_message(&worker)?;
        self.broadcast_chan.send(worker_data)
    }

    pub async fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Vec<u8>> {
        self.broadcast_chan.receiver().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::Duration;

    #[tokio::test]
    async fn test_publish_and_subscribe_worker_changed() -> Result<()> {
        let repo = ChanWorkerPubSubRepositoryImpl::new(16);
        let mut receiver = repo.subscribe().await;

        let worker_id = WorkerId { value: 42 };
        let worker_data = WorkerData {
            name: "test_worker".to_string(),
            ..Default::default()
        };

        repo.publish_worker_changed(&worker_id, &worker_data)?;

        let payload = tokio::time::timeout(Duration::from_secs(1), receiver.recv()).await??;
        let worker = ProstMessageCodec::deserialize_message::<Worker>(&payload)?;
        assert_eq!(worker.id.unwrap().value, 42);
        assert!(worker.data.is_some());
        assert_eq!(worker.data.unwrap().name, "test_worker");
        Ok(())
    }

    #[tokio::test]
    async fn test_publish_and_subscribe_worker_deleted() -> Result<()> {
        let repo = ChanWorkerPubSubRepositoryImpl::new(16);
        let mut receiver = repo.subscribe().await;

        let worker_id = WorkerId { value: 99 };
        repo.publish_worker_deleted(&worker_id)?;

        let payload = tokio::time::timeout(Duration::from_secs(1), receiver.recv()).await??;
        let worker = ProstMessageCodec::deserialize_message::<Worker>(&payload)?;
        assert_eq!(worker.id.unwrap().value, 99);
        assert!(worker.data.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn test_publish_and_subscribe_worker_all_deleted() -> Result<()> {
        let repo = ChanWorkerPubSubRepositoryImpl::new(16);
        let mut receiver = repo.subscribe().await;

        repo.publish_worker_all_deleted()?;

        let payload = tokio::time::timeout(Duration::from_secs(1), receiver.recv()).await??;
        let worker = ProstMessageCodec::deserialize_message::<Worker>(&payload)?;
        assert!(worker.id.is_none());
        assert!(worker.data.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn test_multiple_subscribers() -> Result<()> {
        let repo = ChanWorkerPubSubRepositoryImpl::new(16);
        let mut recv1 = repo.subscribe().await;
        let mut recv2 = repo.subscribe().await;

        let worker_id = WorkerId { value: 7 };
        repo.publish_worker_deleted(&worker_id)?;

        let p1 = tokio::time::timeout(Duration::from_secs(1), recv1.recv()).await??;
        let p2 = tokio::time::timeout(Duration::from_secs(1), recv2.recv()).await??;

        let w1 = ProstMessageCodec::deserialize_message::<Worker>(&p1)?;
        let w2 = ProstMessageCodec::deserialize_message::<Worker>(&p2)?;
        assert_eq!(w1.id.unwrap().value, 7);
        assert_eq!(w2.id.unwrap().value, 7);
        Ok(())
    }

    #[tokio::test]
    async fn test_no_message_before_subscribe() -> Result<()> {
        let repo = ChanWorkerPubSubRepositoryImpl::new(16);

        // Publish before subscribing
        let worker_id = WorkerId { value: 1 };
        repo.publish_worker_deleted(&worker_id)?;

        // Subscribe after publish - should not receive the message
        let mut receiver = repo.subscribe().await;
        let result = tokio::time::timeout(Duration::from_millis(100), receiver.recv()).await;
        assert!(
            result.is_err(),
            "should timeout since message was sent before subscribe"
        );
        Ok(())
    }
}
