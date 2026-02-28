pub mod chan;
pub mod redis;

use anyhow::Result;
use async_trait::async_trait;
use jobworkerp_runner::runner::FeedData;
use proto::jobworkerp::data::JobId;

/// Publish feed data to a running streaming job.
/// Implementations deliver data to the runner via in-process channels (Standalone)
/// or Redis Pub/Sub (Scalable).
#[async_trait]
pub trait FeedPublisher: Send + Sync + std::fmt::Debug {
    async fn publish_feed(&self, job_id: &JobId, data: Vec<u8>, is_final: bool) -> Result<()>;
}

/// Channel name for Redis Pub/Sub feed delivery
pub fn job_feed_pubsub_channel_name(job_id: &JobId) -> String {
    format!("job_feed:{}", job_id.value)
}

/// Serialization format for feed data over Redis Pub/Sub
#[derive(serde::Serialize, serde::Deserialize)]
struct FeedMessage {
    data: Vec<u8>,
    is_final: bool,
}

impl From<FeedMessage> for FeedData {
    fn from(msg: FeedMessage) -> Self {
        FeedData {
            data: msg.data,
            is_final: msg.is_final,
        }
    }
}
