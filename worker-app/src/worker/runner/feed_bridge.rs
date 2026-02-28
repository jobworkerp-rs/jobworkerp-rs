use futures::StreamExt;
use jobworkerp_runner::runner::FeedData;
use proto::jobworkerp::data::JobId;
use tokio::sync::mpsc;

const MAX_SUBSCRIBE_RETRIES: u32 = 3;
const INITIAL_RETRY_DELAY_MS: u64 = 100;

/// Spawn a background task that subscribes to feed data from Redis Pub/Sub
/// and forwards it to the runner's feed channel (Scalable mode).
///
/// Retries subscription up to `MAX_SUBSCRIBE_RETRIES` times with exponential
/// backoff. On final failure, `feed_sender` is dropped so the runner's
/// receive channel closes and it can detect the disconnect.
///
/// Returns a JoinHandle that can be aborted when the stream ends.
pub fn spawn_redis_feed_bridge(
    redis_client: &redis::Client,
    job_id: &JobId,
    feed_sender: mpsc::Sender<FeedData>,
) -> tokio::task::JoinHandle<()> {
    let redis_client = redis_client.clone();
    let job_id = *job_id;

    tokio::spawn(async move {
        let mut last_err = None;
        for attempt in 0..=MAX_SUBSCRIBE_RETRIES {
            if feed_sender.is_closed() {
                tracing::debug!(
                    "Feed bridge: sender closed before subscribe for job {}",
                    job_id.value
                );
                return;
            }

            match infra::infra::feed::redis::subscribe_feed(&redis_client, &job_id).await {
                Ok(stream) => {
                    let mut stream = Box::pin(stream);
                    while let Some(feed) = stream.next().await {
                        let is_final = feed.is_final;
                        if feed_sender.send(feed).await.is_err() {
                            tracing::debug!(
                                "Feed bridge: receiver dropped for job {}",
                                job_id.value
                            );
                            return;
                        }
                        if is_final {
                            tracing::debug!(
                                "Feed bridge: final feed received for job {}",
                                job_id.value
                            );
                            return;
                        }
                    }
                    // Stream ended without is_final (e.g., Redis disconnected)
                    return;
                }
                Err(e) => {
                    last_err = Some(e);
                    if attempt < MAX_SUBSCRIBE_RETRIES {
                        let delay_ms = INITIAL_RETRY_DELAY_MS * 2u64.pow(attempt);
                        tracing::warn!(
                            "Feed bridge: subscribe failed for job {} (attempt {}/{}), retrying in {}ms",
                            job_id.value,
                            attempt + 1,
                            MAX_SUBSCRIBE_RETRIES,
                            delay_ms
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    }
                }
            }
        }

        // All retries exhausted; drop feed_sender so runner detects disconnect
        tracing::error!(
            "Feed bridge: failed to subscribe to feed for job {} after {} retries: {:?}",
            job_id.value,
            MAX_SUBSCRIBE_RETRIES,
            last_err
        );
        drop(feed_sender);
    })
}
