use futures::StreamExt;
use jobworkerp_runner::runner::FeedData;
use proto::jobworkerp::data::JobId;
use tokio::sync::mpsc;

/// Spawn a background task that subscribes to feed data from Redis Pub/Sub
/// and forwards it to the runner's feed channel (Scalable mode).
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
        match infra::infra::feed::redis::subscribe_feed(&redis_client, &job_id).await {
            Ok(stream) => {
                let mut stream = Box::pin(stream);
                while let Some(feed) = stream.next().await {
                    let is_final = feed.is_final;
                    if feed_sender.send(feed).await.is_err() {
                        tracing::debug!("Feed bridge: receiver dropped for job {}", job_id.value);
                        break;
                    }
                    if is_final {
                        tracing::debug!(
                            "Feed bridge: final feed received for job {}",
                            job_id.value
                        );
                        break;
                    }
                }
            }
            Err(e) => {
                tracing::error!(
                    "Feed bridge: failed to subscribe to feed for job {}: {:?}",
                    job_id.value,
                    e
                );
            }
        }
    })
}
