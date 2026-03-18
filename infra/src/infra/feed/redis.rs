use anyhow::Result;
use async_trait::async_trait;
use debug_stub_derive::DebugStub;
use infra_utils::infra::redis::RedisClient;
use prost::Message;
use proto::jobworkerp::data::{FeedDataTransport, JobId};
use std::time::Duration;

use super::{FeedPublisher, job_feed_buf_key};

/// Redis List based feed publisher for Scalable mode.
/// Publishes feed data to `job_feed_buf:{job_id}` list via RPUSH.
#[derive(Clone, DebugStub)]
pub struct RedisFeedPublisher {
    #[debug_stub = "&'static RedisClient"]
    pub redis_client: RedisClient,
    pub feed_dispatch_timeout: Duration,
}

impl RedisFeedPublisher {
    pub fn new(redis_client: RedisClient, feed_dispatch_timeout: Duration) -> Self {
        Self {
            redis_client,
            feed_dispatch_timeout,
        }
    }
}

// NOTE: RedisFeedPublisher intentionally uses the default `has_active_feed` (returns None),
// so the fast path in `feed_to_stream_with_fast_path` is never taken for Redis/Scalable mode.
// In Scalable mode, feed senders live on remote worker processes, so the local process
// cannot know whether a feed channel is active. Full validation via job/status lookup
// is always required.
#[async_trait]
impl FeedPublisher for RedisFeedPublisher {
    async fn publish_feed(&self, job_id: &JobId, data: Vec<u8>, is_final: bool) -> Result<()> {
        let buf_key = job_feed_buf_key(job_id);
        let msg = FeedDataTransport { data, is_final };
        let serialized = msg.encode_to_vec();
        // TTL is refreshed on every publish_feed call (EXPIRE in the pipeline below),
        // so the key only expires when no data has been published for `ttl_secs`.
        // This prevents stale keys from accumulating while keeping the list alive
        // during active streaming.
        let ttl_secs = (self.feed_dispatch_timeout.as_secs() * 2).max(10) as i64;

        let mut conn = self.redis_client.get_multiplexed_async_connection().await?;

        // Push to buffer and refresh TTL atomically
        redis::pipe()
            .rpush(&buf_key, &serialized)
            .expire(&buf_key, ttl_secs)
            .exec_async(&mut conn)
            .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use infra_utils::infra::redis::new_redis_client;
    use infra_utils::infra::test::REDIS_CONFIG;
    use redis::AsyncCommands;

    fn make_publisher() -> RedisFeedPublisher {
        let client = new_redis_client(REDIS_CONFIG.clone()).unwrap();
        RedisFeedPublisher::new(client, Duration::from_secs(5))
    }

    async fn cleanup_key(publisher: &RedisFeedPublisher, job_id: &JobId) {
        let key = job_feed_buf_key(job_id);
        let mut conn = publisher
            .redis_client
            .get_multiplexed_async_connection()
            .await
            .unwrap();
        let _: () = conn.del(&key).await.unwrap();
    }

    #[tokio::test]
    async fn test_publish_feed_normal() {
        let publisher = make_publisher();
        let job_id = JobId { value: 900001 };
        cleanup_key(&publisher, &job_id).await;

        // Publish two messages
        publisher
            .publish_feed(&job_id, vec![1, 2, 3], false)
            .await
            .unwrap();
        publisher
            .publish_feed(&job_id, vec![4, 5], false)
            .await
            .unwrap();

        // Verify messages in Redis List
        let key = job_feed_buf_key(&job_id);
        let mut conn = publisher
            .redis_client
            .get_multiplexed_async_connection()
            .await
            .unwrap();
        let len: i64 = conn.llen(&key).await.unwrap();
        assert_eq!(len, 2);

        // LPOP and decode first message
        let raw: Vec<u8> = conn.lpop(&key, None).await.unwrap();
        let msg = FeedDataTransport::decode(raw.as_slice()).unwrap();
        assert_eq!(msg.data, vec![1, 2, 3]);
        assert!(!msg.is_final);

        // LPOP and decode second message
        let raw: Vec<u8> = conn.lpop(&key, None).await.unwrap();
        let msg = FeedDataTransport::decode(raw.as_slice()).unwrap();
        assert_eq!(msg.data, vec![4, 5]);
        assert!(!msg.is_final);

        cleanup_key(&publisher, &job_id).await;
    }

    #[tokio::test]
    async fn test_publish_feed_is_final() {
        let publisher = make_publisher();
        let job_id = JobId { value: 900002 };
        cleanup_key(&publisher, &job_id).await;

        publisher
            .publish_feed(&job_id, vec![10, 20], true)
            .await
            .unwrap();

        let key = job_feed_buf_key(&job_id);
        let mut conn = publisher
            .redis_client
            .get_multiplexed_async_connection()
            .await
            .unwrap();

        let raw: Vec<u8> = conn.lpop(&key, None).await.unwrap();
        let msg = FeedDataTransport::decode(raw.as_slice()).unwrap();
        assert_eq!(msg.data, vec![10, 20]);
        assert!(msg.is_final);

        cleanup_key(&publisher, &job_id).await;
    }

    #[tokio::test]
    async fn test_publish_feed_empty_data_final() {
        let publisher = make_publisher();
        let job_id = JobId { value: 900003 };
        cleanup_key(&publisher, &job_id).await;

        // Empty data with is_final=true (termination signal)
        publisher.publish_feed(&job_id, vec![], true).await.unwrap();

        let key = job_feed_buf_key(&job_id);
        let mut conn = publisher
            .redis_client
            .get_multiplexed_async_connection()
            .await
            .unwrap();

        let raw: Vec<u8> = conn.lpop(&key, None).await.unwrap();
        let msg = FeedDataTransport::decode(raw.as_slice()).unwrap();
        assert!(msg.data.is_empty());
        assert!(msg.is_final);

        // TTL should be set
        let ttl: i64 = conn.ttl(&key).await.unwrap();
        // Key should be expired now (no more items) but TTL was set before lpop
        // After lpop removed the only item, the key is gone
        assert!(
            ttl <= 0,
            "key should be expired after lpop removed all items"
        );

        cleanup_key(&publisher, &job_id).await;
    }
}
