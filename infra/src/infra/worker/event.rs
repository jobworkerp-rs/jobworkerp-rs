use crate::infra::job::rows::UseJobqueueAndCodec;
use anyhow::Result;
use async_trait::async_trait;
use infra_utils::infra::redis::UseRedisClient;
use proto::jobworkerp::data::{Worker, WorkerData, WorkerId};

// TODO merge super::event::UsePublishChanged ?
#[async_trait]
pub trait UseWorkerPublish: UseJobqueueAndCodec + UseRedisClient + Send + Sync {
    // publish worker changed event using redis<
    async fn publish_worker_changed(&self, id: &WorkerId, data: &WorkerData) -> Result<bool> {
        let worker = Worker {
            id: Some(*id),
            data: Some(data.clone()),
        };
        let worker_data = Self::serialize_worker(&worker);
        self.publish(Self::WORKER_PUBSUB_CHANNEL_NAME, &worker_data)
            .await
            .map(|r| r > 0)
    }
    // publish worker deleted event using redis
    async fn publish_worker_deleted(&self, worker_id: &WorkerId) -> Result<bool> {
        let worker = Worker {
            id: Some(*worker_id),
            data: None,
        };
        let worker_data = Self::serialize_worker(&worker);
        self.publish(Self::WORKER_PUBSUB_CHANNEL_NAME, &worker_data)
            .await
            .map(|r| r > 0)
    }
    // publish worker deleted event using redis
    async fn publish_worker_all_deleted(&self) -> Result<bool> {
        let worker = Worker {
            id: None,
            data: None,
        };
        let worker_data = Self::serialize_worker(&worker);
        self.publish(Self::WORKER_PUBSUB_CHANNEL_NAME, &worker_data)
            .await
            .map(|r| r > 0)
    }
}
