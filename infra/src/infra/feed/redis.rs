use anyhow::Result;
use async_trait::async_trait;
use debug_stub_derive::DebugStub;
use futures::StreamExt;
use infra_utils::infra::redis::{RedisClient, UseRedisClient};
use jobworkerp_runner::runner::FeedData;
use prost::Message;
use proto::jobworkerp::data::{FeedDataTransport, JobId};

use super::{FeedPublisher, feed_data_from_transport, job_feed_pubsub_channel_name};

/// Redis Pub/Sub based feed publisher for Scalable mode.
/// Publishes feed data to `job_feed:{job_id}` channel.
#[derive(Clone, DebugStub)]
pub struct RedisFeedPublisher {
    #[debug_stub = "&'static RedisClient"]
    pub redis_client: RedisClient,
}

impl RedisFeedPublisher {
    pub fn new(redis_client: RedisClient) -> Self {
        Self { redis_client }
    }
}

impl UseRedisClient for RedisFeedPublisher {
    fn redis_client(&self) -> &RedisClient {
        &self.redis_client
    }
}

#[async_trait]
impl FeedPublisher for RedisFeedPublisher {
    async fn publish_feed(&self, job_id: &JobId, data: Vec<u8>, is_final: bool) -> Result<()> {
        let ch = job_feed_pubsub_channel_name(job_id);
        let msg = FeedDataTransport { data, is_final };
        let serialized = msg.encode_to_vec();
        self.publish_multi_if_listen(&[ch], &serialized).await?;
        Ok(())
    }
}

/// Subscribe to feed data for a specific job via Redis Pub/Sub.
/// Returns an unbounded stream of FeedData. The caller is responsible for
/// terminating consumption when `is_final == true` is received (e.g., via `break`
/// or `take_while`); the stream itself does not auto-close on the final marker.
pub async fn subscribe_feed(
    redis_client: &RedisClient,
    job_id: &JobId,
) -> Result<impl futures::Stream<Item = FeedData>> {
    let ch = job_feed_pubsub_channel_name(job_id);
    let mut pubsub = redis_client.get_async_pubsub().await?;
    pubsub.subscribe(&ch).await?;

    let stream = pubsub.into_on_message().filter_map(|msg| async move {
        let payload: Vec<u8> = match msg.get_payload() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("Failed to get feed message payload: {:?}", e);
                return None;
            }
        };
        match FeedDataTransport::decode(payload.as_slice()) {
            Ok(feed_msg) => Some(feed_data_from_transport(feed_msg)),
            Err(e) => {
                tracing::warn!("Failed to deserialize feed message: {:?}", e);
                None
            }
        }
    });

    Ok(stream)
}
