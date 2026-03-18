use infra::infra::feed::{feed_data_from_transport, job_feed_buf_key};
use infra_utils::infra::redis::RedisClient;
use jobworkerp_runner::runner::FeedData;
use prost::Message;
use proto::jobworkerp::data::{FeedDataTransport, JobId};
use redis::{AsyncCommands, AsyncConnectionConfig};
use tokio::sync::mpsc;

/// BLPOP polling interval in seconds.
/// When no data arrives, BLPOP returns after this timeout and we check
/// if the feed_sender is still alive before retrying.
const BLPOP_TIMEOUT_SECS: f64 = 5.0;

/// Spawn a background task that reads feed data from a Redis List via BLPOP
/// and forwards it to the runner's feed channel (Scalable mode).
///
/// Uses a dedicated connection with no response timeout since BLPOP blocks.
/// On successful completion (final feed received), DEL is called to clean up the Redis key.
/// On error or early termination (receiver dropped), DEL is skipped to let TTL expire naturally.
///
/// Returns a JoinHandle that can be aborted when the stream ends.
pub fn spawn_redis_feed_bridge(
    redis_client: &RedisClient,
    job_id: &JobId,
    feed_sender: mpsc::Sender<FeedData>,
) -> tokio::task::JoinHandle<()> {
    let redis_client = redis_client.clone();
    let job_id = *job_id;

    tokio::spawn(async move {
        let success = match bridge_loop(&redis_client, &job_id, &feed_sender).await {
            Ok(completed) => completed,
            Err(e) => {
                tracing::warn!("Feed bridge error for job {}: {:?}", job_id.value, e);
                false
            }
        };

        // Only cleanup Redis key on success; on error, let TTL expire naturally
        // to avoid deleting data that a retry might need.
        if success {
            let buf_key = job_feed_buf_key(&job_id);
            if let Ok(mut conn) = redis_client.get_multiplexed_async_connection().await {
                let _: Result<(), _> = conn.del(&buf_key).await;
            }
        }
    })
}

/// Exposed for testing
#[cfg(test)]
pub(crate) async fn bridge_loop_test(
    redis_client: &RedisClient,
    job_id: &JobId,
    feed_sender: &mpsc::Sender<FeedData>,
) -> anyhow::Result<bool> {
    bridge_loop(redis_client, job_id, feed_sender).await
}

/// Returns Ok(true) on normal completion (final feed received),
/// Ok(false) when receiver was dropped before final feed.
async fn bridge_loop(
    redis_client: &RedisClient,
    job_id: &JobId,
    feed_sender: &mpsc::Sender<FeedData>,
) -> anyhow::Result<bool> {
    let buf_key = job_feed_buf_key(job_id);

    // BLPOP requires a dedicated connection with no response timeout
    // because it blocks the connection until data arrives or timeout.
    let config = AsyncConnectionConfig::new().set_response_timeout(None);
    let mut conn = redis_client
        .get_multiplexed_async_connection_with_config(&config)
        .await?;

    loop {
        // BLPOP: block until data arrives or timeout
        let result: Option<(String, Vec<u8>)> = conn.blpop(&buf_key, BLPOP_TIMEOUT_SECS).await?;

        match result {
            Some((_key, bytes)) => {
                let transport = FeedDataTransport::decode(bytes.as_slice())?;
                let feed = feed_data_from_transport(transport);
                let is_final = feed.is_final;
                if feed_sender.send(feed).await.is_err() {
                    tracing::debug!("Feed bridge: receiver dropped for job {}", job_id.value);
                    return Ok(false);
                }
                if is_final {
                    tracing::debug!("Feed bridge: final feed received for job {}", job_id.value);
                    return Ok(true);
                }
            }
            None => {
                // Timeout: check if receiver is still alive
                if feed_sender.is_closed() {
                    tracing::debug!("Feed bridge: sender closed for job {}", job_id.value);
                    return Ok(false);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use infra::infra::feed::job_feed_buf_key;
    use infra_utils::infra::redis::new_redis_client;
    use infra_utils::infra::test::REDIS_CONFIG;
    use redis::AsyncCommands;

    fn make_client() -> RedisClient {
        new_redis_client(REDIS_CONFIG.clone()).unwrap()
    }

    async fn cleanup_key(client: &RedisClient, job_id: &JobId) {
        let key = job_feed_buf_key(job_id);
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();
        let _: () = conn.del(&key).await.unwrap();
    }

    async fn rpush_feed(client: &RedisClient, job_id: &JobId, data: Vec<u8>, is_final: bool) {
        let key = job_feed_buf_key(job_id);
        let msg = FeedDataTransport { data, is_final };
        let serialized = msg.encode_to_vec();
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();
        let _: () = conn.rpush(&key, &serialized).await.unwrap();
    }

    #[tokio::test]
    async fn test_bridge_normal_flow() {
        let client = make_client();
        let job_id = JobId { value: 800001 };
        cleanup_key(&client, &job_id).await;

        let (tx, mut rx) = mpsc::channel::<FeedData>(16);

        // Push 3 messages: 2 normal + 1 final
        rpush_feed(&client, &job_id, vec![1, 2], false).await;
        rpush_feed(&client, &job_id, vec![3, 4], false).await;
        rpush_feed(&client, &job_id, vec![5], true).await;

        // Run bridge_loop (should consume all 3 and exit on is_final)
        bridge_loop_test(&client, &job_id, &tx).await.unwrap();

        // Verify received messages
        let m1 = rx.recv().await.unwrap();
        assert_eq!(m1.data, vec![1, 2]);
        assert!(!m1.is_final);

        let m2 = rx.recv().await.unwrap();
        assert_eq!(m2.data, vec![3, 4]);
        assert!(!m2.is_final);

        let m3 = rx.recv().await.unwrap();
        assert_eq!(m3.data, vec![5]);
        assert!(m3.is_final);

        cleanup_key(&client, &job_id).await;
    }

    #[tokio::test]
    async fn test_bridge_sender_drop_exits() {
        let client = make_client();
        let job_id = JobId { value: 800002 };
        cleanup_key(&client, &job_id).await;

        let (tx, rx) = mpsc::channel::<FeedData>(16);

        // Drop receiver so sender.send() returns error
        drop(rx);

        // Push one message
        rpush_feed(&client, &job_id, vec![1], false).await;

        // bridge_loop should exit gracefully when send fails, returning Ok(false)
        let result = bridge_loop_test(&client, &job_id, &tx).await;
        assert!(!result.unwrap());

        cleanup_key(&client, &job_id).await;
    }

    #[tokio::test]
    async fn test_bridge_cleanup_deletes_key() {
        let client = make_client();
        let job_id = JobId { value: 800003 };
        cleanup_key(&client, &job_id).await;

        let (tx, _rx) = mpsc::channel::<FeedData>(16);

        // Push final message
        rpush_feed(&client, &job_id, vec![], true).await;

        // Use spawn_redis_feed_bridge which does cleanup
        let handle = spawn_redis_feed_bridge(&client, &job_id, tx);
        handle.await.unwrap();

        // Verify key was deleted
        let key = job_feed_buf_key(&job_id);
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();
        let exists: bool = conn.exists(&key).await.unwrap();
        assert!(
            !exists,
            "Redis key should be deleted after bridge completes"
        );
    }
}
