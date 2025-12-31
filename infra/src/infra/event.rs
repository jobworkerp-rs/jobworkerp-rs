use crate::infra::job::rows::UseJobqueueAndCodec;
use anyhow::Result;
use async_trait::async_trait;
use infra_utils::infra::redis::UseRedisClient;
use prost::Message;
// use proto::jobworkerp::data::{Worker, WorkerData, WorkerId};

#[async_trait]
pub trait UsePublishChanged<ID: Message, DATA: Message>:
    UseJobqueueAndCodec + UseRedisClient + Send + Sync
{
    // event pubsub channel name (for cache clear)
    fn channel_name(&self) -> &'static str;

    // publish worker changed event using redis
    async fn publish_worker_changed(&self, id: &ID) -> Result<u32> {
        let id_data = Self::serialize_message(id)?;
        self.publish(self.channel_name(), &id_data).await
    }
}
