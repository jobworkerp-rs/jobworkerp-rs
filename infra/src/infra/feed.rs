pub mod chan;
pub mod redis;

use anyhow::Result;
use async_trait::async_trait;
use jobworkerp_runner::runner::FeedData;
use proto::jobworkerp::data::{FeedDataTransport, JobId};

/// Publish feed data to a running streaming job.
/// Implementations deliver data to the runner via in-process channels (Standalone)
/// or Redis List (Scalable).
#[async_trait]
pub trait FeedPublisher: Send + Sync + std::fmt::Debug {
    async fn publish_feed(&self, job_id: &JobId, data: Vec<u8>, is_final: bool) -> Result<()>;

    /// Check if an active feed channel exists for the given job.
    /// Returns Some(true/false) for definitive answer, None if unknown (falls back to job record lookup).
    fn has_active_feed(&self, job_id: &JobId) -> Option<bool> {
        let _ = job_id;
        None
    }
}

/// Redis List key for buffered feed data delivery (Scalable mode)
pub fn job_feed_buf_key(job_id: &JobId) -> String {
    format!("job_feed_buf:{}", job_id.value)
}

/// Channel name for Redis Pub/Sub feed delivery (deprecated, kept for FeedToStream compatibility)
pub fn job_feed_pubsub_channel_name(job_id: &JobId) -> String {
    format!("job_feed:{}", job_id.value)
}

pub fn feed_data_from_transport(msg: FeedDataTransport) -> FeedData {
    FeedData {
        data: msg.data,
        is_final: msg.is_final,
    }
}
