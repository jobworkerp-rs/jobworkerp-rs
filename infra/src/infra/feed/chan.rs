use anyhow::Result;
use async_trait::async_trait;
use dashmap::DashMap;
use jobworkerp_runner::runner::FeedData;
use proto::jobworkerp::data::JobId;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Notify, mpsc};

use super::FeedPublisher;

/// Maximum number of feed items buffered per-job before the runner registers
/// its sender. When exceeded, the oldest entry is dropped and a warning is
/// logged — chosen to bound memory while still tolerating a few seconds of
/// pre-registration feeds (e.g. audio chunks for streaming ASR).
const PENDING_BUFFER_MAX: usize = 1024;

/// Per-job state combining the registered sender and the pre-registration
/// buffer under a single map entry, so that `publish_feed` and `register`
/// observe a consistent snapshot without a TOCTOU window across two DashMaps.
#[derive(Debug, Default)]
struct JobFeedState {
    /// The runner's sender, once registered.
    sender: Option<mpsc::Sender<FeedData>>,
    /// Feeds published before registration OR feeds that arrived while a
    /// background drain task is still flushing earlier pre-registration
    /// feeds. Either way, feeds always go through this buffer before they
    /// reach the runner, which preserves arrival order end-to-end.
    buffer: VecDeque<FeedData>,
    /// True while a drain task is actively forwarding `buffer` to `sender`.
    /// New publishes go to `buffer` (not directly to `sender`) so the drain
    /// task remains the sole writer and order is preserved.
    draining: bool,
    /// Set once the runner has signalled end-of-stream (via a final feed
    /// already forwarded). Subsequent publishes are rejected to avoid
    /// resurrecting a closed stream.
    closed: bool,
}

/// In-process feed sender store for Standalone mode.
/// Workers register a channel sender when starting a feed-enabled streaming job;
/// the gRPC handler looks it up to deliver feed data directly.
///
/// Registration invariant: entries are only inserted by `run_job()` for jobs that
/// satisfy all feed preconditions (Running state, streaming_type != None, use_static,
/// concurrency == 1, require_client_stream).
///
/// When the publisher (gRPC frontend) runs ahead of the runner's registration
/// (e.g. the previous job is still holding the static runner pool slot), feeds
/// are queued inside the per-job `JobFeedState.buffer` and drained on
/// `register()`. All feeds — both buffered and live — flow through the same
/// drain path so the runner observes them in publish order.
#[derive(Clone, Debug)]
pub struct ChanFeedSenderStore {
    states: Arc<DashMap<i64, JobFeedState>>,
    // Notify waiters when a feed sender is registered for a job
    feed_ready_notifiers: Arc<DashMap<i64, Arc<Notify>>>,
}

impl ChanFeedSenderStore {
    pub fn new() -> Self {
        Self {
            states: Arc::new(DashMap::new()),
            feed_ready_notifiers: Arc::new(DashMap::new()),
        }
    }

    /// Register a feed sender for a job.
    ///
    /// If any feeds were published before registration (because the runner pool
    /// was still busy with a prior streaming job), a background drain task
    /// flushes them to the sender in publish order. While the drain runs,
    /// subsequent publishes also go to the buffer (not directly to the sender)
    /// so the drain task remains the single writer and arrival order is
    /// preserved end-to-end.
    pub fn register(&self, job_id: i64, sender: mpsc::Sender<FeedData>) {
        // Atomically install the sender into the per-job state. Holding the
        // entry guard across set-up prevents `publish_feed` from observing a
        // partially-initialized state.
        let needs_drain = {
            let mut entry = self.states.entry(job_id).or_default();
            let state = entry.value_mut();
            if state.closed {
                // A final feed already passed through; do not resurrect.
                tracing::warn!(
                    "register called for closed feed state of job {}; ignoring",
                    job_id
                );
                false
            } else {
                state.sender = Some(sender);
                let has_buffer = !state.buffer.is_empty();
                if has_buffer {
                    state.draining = true;
                }
                has_buffer
            }
        };

        // Notify any waiting feed forwarder that the sender is now installed.
        // Even while a drain is in progress, `publish_feed` is safe to call
        // (it appends to the buffer rather than sending directly), so the
        // forwarder can be released without waiting for drain completion.
        if let Some((_, notify)) = self.feed_ready_notifiers.remove(&job_id) {
            notify.notify_one();
        }

        if needs_drain {
            let states = self.states.clone();
            tokio::spawn(async move {
                Self::drain_loop(states, job_id).await;
            });
        }
    }

    /// Background drain: repeatedly pop one feed from the buffer (under the
    /// entry guard) and send it on the registered sender. The drain task is
    /// the sole writer to the channel, so even concurrent `publish_feed`
    /// calls — which append to the buffer — preserve arrival order.
    async fn drain_loop(states: Arc<DashMap<i64, JobFeedState>>, job_id: i64) {
        loop {
            // Atomically pop the next feed (and snapshot the sender) under
            // the entry guard, so we never read a stale sender or skip a
            // concurrent append.
            let (next, sender) = {
                let mut entry = match states.get_mut(&job_id) {
                    Some(e) => e,
                    None => return, // remove() raced; nothing to do.
                };
                let state = entry.value_mut();
                let next = state.buffer.pop_front();
                if next.is_none() {
                    // Buffer drained: clear draining flag and exit. Any later
                    // `publish_feed` will see `draining=false` and append to
                    // the (now empty) buffer, which will be re-drained by the
                    // next call below — but to avoid leaving feeds stranded
                    // when we exit, we re-check inside the guard.
                    state.draining = false;
                    return;
                }
                (next, state.sender.clone())
            };

            let feed = next.unwrap();
            let is_final = feed.is_final;
            let Some(sender) = sender else {
                // Sender was removed (e.g. via `remove()`); drop the rest.
                tracing::debug!(
                    "drain_loop: sender removed for job {}; aborting drain",
                    job_id
                );
                // If `remove()` already deleted the entry there is nothing
                // to clear; otherwise just clear the draining flag.
                if let Some(mut entry) = states.get_mut(&job_id) {
                    entry.value_mut().draining = false;
                }
                return;
            };

            if let Err(e) = sender.send(feed).await {
                tracing::warn!("Drained feed send failed for job {}: {:?}", job_id, e);
                // Receiver gone: discard remaining buffered feeds and remove.
                states.remove(&job_id);
                return;
            }

            if is_final {
                // End-of-stream: mark closed and drop the entry. Any later
                // publish will be rejected by `publish_feed` instead of
                // resurrecting the stream. If a concurrent `remove()` has
                // already deleted the entry, skip straight to the no-op
                // `remove`.
                if let Some(mut entry) = states.get_mut(&job_id) {
                    let state = entry.value_mut();
                    state.closed = true;
                    state.sender = None;
                    state.draining = false;
                    // Discard any residual buffer (would be a protocol violation).
                    state.buffer.clear();
                    drop(entry);
                }
                states.remove(&job_id);
                return;
            }
        }
    }

    /// Remove the sender for a job (cleanup on completion/drop).
    /// Returns the sender if it was registered. Also discards any pending
    /// buffer for this job and marks the entry closed so a stray drain task
    /// or late publish does not resurrect it.
    pub fn remove(&self, job_id: i64) -> Option<mpsc::Sender<FeedData>> {
        let mut sender = None;
        if let Some(mut entry) = self.states.get_mut(&job_id) {
            let state = entry.value_mut();
            sender = state.sender.take();
            state.buffer.clear();
            state.closed = true;
            state.draining = false;
        }
        self.states.remove(&job_id);
        sender
    }

    /// Discard any feeds buffered for `job_id` before runner registration.
    /// Called by the gRPC frontend when the feed forwarder gives up (e.g.
    /// `wait_for_feed_ready` timed out) so the buffer does not leak.
    pub fn cleanup_pending(&self, job_id: i64) {
        if let Some(mut entry) = self.states.get_mut(&job_id) {
            let state = entry.value_mut();
            // Only drop if sender is not registered: registered jobs are
            // owned by the runner and use `remove()` for cleanup.
            if state.sender.is_none() && !state.buffer.is_empty() {
                state.buffer.clear();
                state.closed = true;
                drop(entry);
                self.states.remove(&job_id);
                tracing::debug!(
                    "ChanFeedSenderStore: discarded pending buffer for job {}",
                    job_id
                );
            }
        }
    }

    /// Get a clone of the sender (for tests / inspection).
    /// Returns None if no sender is registered or the state is closed.
    pub fn get(&self, job_id: i64) -> Option<mpsc::Sender<FeedData>> {
        self.states
            .get(&job_id)
            .and_then(|r| r.value().sender.clone())
    }

    /// Wait until a feed channel is registered for the given job_id.
    /// Returns Ok(()) when feed channel is ready, Err on timeout.
    pub async fn wait_for_feed_ready(&self, job_id: i64, timeout: Duration) -> Result<()> {
        // Fast path: already registered
        if self.sender_registered(job_id) {
            return Ok(());
        }

        // Register notifier
        let notify = Arc::new(Notify::new());
        self.feed_ready_notifiers.insert(job_id, notify.clone());

        // Double-check after registration to avoid race condition
        if self.sender_registered(job_id) {
            self.feed_ready_notifiers.remove(&job_id);
            return Ok(());
        }

        // Wait with timeout
        match tokio::time::timeout(timeout, notify.notified()).await {
            Ok(()) => {
                self.feed_ready_notifiers.remove(&job_id);
                Ok(())
            }
            Err(_) => {
                self.feed_ready_notifiers.remove(&job_id);
                // Runner never registered: discard any buffered feeds to
                // avoid leaking memory across abandoned jobs.
                self.cleanup_pending(job_id);
                Err(anyhow::anyhow!(
                    "Feed channel not ready for job {} within {:?}",
                    job_id,
                    timeout
                ))
            }
        }
    }

    fn sender_registered(&self, job_id: i64) -> bool {
        self.states
            .get(&job_id)
            .is_some_and(|r| r.value().sender.is_some())
    }
}

impl Default for ChanFeedSenderStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl FeedPublisher for ChanFeedSenderStore {
    async fn publish_feed(&self, job_id: &JobId, data: Vec<u8>, is_final: bool) -> Result<()> {
        let feed = FeedData { data, is_final };

        // Decide under the entry guard whether to append to the buffer or to
        // send directly. Doing both lookups inside one DashMap entry closes
        // the TOCTOU window where `register` could land between a `sender`
        // check and a buffer push, leaving a feed orphaned in the buffer.
        let direct_sender: Option<mpsc::Sender<FeedData>> = {
            let mut entry = self.states.entry(job_id.value).or_default();
            let state = entry.value_mut();
            if state.closed {
                return Err(anyhow::anyhow!(
                    "Feed stream for job {} is already closed",
                    job_id.value
                ));
            }
            // Use the buffer when:
            //   - no sender yet (pre-registration), OR
            //   - a drain task is in flight (so the drain remains the sole
            //     writer and arrival order is preserved end-to-end).
            if state.sender.is_none() || state.draining {
                if state.buffer.len() >= PENDING_BUFFER_MAX {
                    // Drop oldest non-final element to bound memory; if a
                    // final marker is at the front, retain it (the runner
                    // must observe end-of-stream).
                    if state.buffer.front().is_some_and(|f| !f.is_final) {
                        state.buffer.pop_front();
                    }
                    tracing::warn!(
                        "Pending feed buffer for job {} reached max ({}); dropping oldest non-final entry",
                        job_id.value,
                        PENDING_BUFFER_MAX,
                    );
                }
                state.buffer.push_back(feed);
                // If a sender is registered but no drain is in flight, kick
                // one off so the appended feed is forwarded promptly. Flip
                // `draining` before releasing the entry guard so a
                // concurrent publisher cannot also start a drain task.
                if state.sender.is_some() && !state.draining {
                    state.draining = true;
                    let states = self.states.clone();
                    let jid = job_id.value;
                    drop(entry);
                    tokio::spawn(async move {
                        Self::drain_loop(states, jid).await;
                    });
                }
                return Ok(());
            }
            state.sender.clone()
        };

        let Some(sender) = direct_sender else {
            // Unreachable: handled above.
            return Ok(());
        };

        // Fast path: sender already registered and no drain in flight.
        {
            if let Err(e) = sender.send(feed).await {
                // Receiver dropped: remove stale entry before returning error
                self.remove(job_id.value);
                return Err(anyhow::anyhow!(
                    "Failed to send feed data to job {}: {:?}",
                    job_id.value,
                    e
                ));
            }

            if is_final {
                self.remove(job_id.value);
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl ChanFeedSenderStore {
        /// Test helper: true when a per-job state entry exists (buffered or registered).
        fn has_state(&self, job_id: i64) -> bool {
            self.states.contains_key(&job_id)
        }

        /// Test helper: current buffer length for a job (0 if no state).
        fn buffer_len(&self, job_id: i64) -> usize {
            self.states
                .get(&job_id)
                .map(|r| r.value().buffer.len())
                .unwrap_or(0)
        }
    }

    #[tokio::test]
    async fn test_register_get_remove() {
        let store = ChanFeedSenderStore::new();
        let (tx, mut rx) = mpsc::channel::<FeedData>(16);

        store.register(42, tx);

        // get should return a sender
        assert!(store.get(42).is_some());
        assert!(store.get(999).is_none());

        // publish via FeedPublisher
        let job_id = JobId { value: 42 };
        store
            .publish_feed(&job_id, b"hello".to_vec(), false)
            .await
            .unwrap();

        let feed = rx.recv().await.unwrap();
        assert_eq!(feed.data, b"hello");
        assert!(!feed.is_final);

        // publish final should auto-remove
        store
            .publish_feed(&job_id, b"done".to_vec(), true)
            .await
            .unwrap();

        let feed = rx.recv().await.unwrap();
        assert_eq!(feed.data, b"done");
        assert!(feed.is_final);

        // sender should be removed after final
        assert!(store.get(42).is_none());
    }

    #[tokio::test]
    async fn test_publish_to_unregistered_job_buffers_instead_of_failing() {
        // Behavior change: publishing before runner registration now succeeds
        // and buffers the feed (drained on register). Previously this returned
        // an error, which caused EnqueueWithClientStream to time out the
        // second consecutive call against a busy static runner pool.
        let store = ChanFeedSenderStore::new();
        let job_id = JobId { value: 999 };
        let result = store.publish_feed(&job_id, b"data".to_vec(), false).await;
        assert!(result.is_ok());
        assert_eq!(store.buffer_len(999), 1);
    }

    #[tokio::test]
    async fn test_remove() {
        let store = ChanFeedSenderStore::new();
        let (tx, _rx) = mpsc::channel::<FeedData>(16);

        store.register(1, tx);
        assert!(store.remove(1).is_some());
        assert!(store.remove(1).is_none());
    }

    #[tokio::test]
    async fn test_publish_to_dropped_receiver_removes_stale_entry() {
        let store = ChanFeedSenderStore::new();
        let (tx, rx) = mpsc::channel::<FeedData>(16);

        store.register(77, tx);
        assert!(store.get(77).is_some());

        // Drop receiver to simulate disconnected consumer
        drop(rx);

        let job_id = JobId { value: 77 };
        let result = store.publish_feed(&job_id, b"data".to_vec(), false).await;
        assert!(result.is_err());

        // Stale entry should be removed
        assert!(store.get(77).is_none());
    }

    #[tokio::test]
    async fn test_wait_for_feed_ready_already_registered() {
        let store = ChanFeedSenderStore::new();
        let (tx, _rx) = mpsc::channel::<FeedData>(16);
        store.register(100, tx);

        // Should return immediately since already registered
        let result = store
            .wait_for_feed_ready(100, Duration::from_millis(100))
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_wait_for_feed_ready_delayed_registration() {
        let store = ChanFeedSenderStore::new();
        let store_clone = store.clone();

        // Spawn delayed registration
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let (tx, _rx) = mpsc::channel::<FeedData>(16);
            store_clone.register(200, tx);
        });

        // Should wait and succeed
        let result = store.wait_for_feed_ready(200, Duration::from_secs(5)).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_wait_for_feed_ready_timeout() {
        let store = ChanFeedSenderStore::new();

        // Should timeout since no registration happens
        let result = store
            .wait_for_feed_ready(300, Duration::from_millis(50))
            .await;
        assert!(result.is_err());

        // Notifier should be cleaned up
        assert!(!store.feed_ready_notifiers.contains_key(&300));
    }

    /// Feeds published before the runner registers must be buffered and
    /// delivered (in order) when the sender is later registered. This is the
    /// core regression fix for `EnqueueWithClientStream` invoked while the
    /// static runner pool is still occupied by the previous job.
    #[tokio::test]
    async fn test_publish_before_register_buffers_data() {
        let store = ChanFeedSenderStore::new();
        let job_id = JobId { value: 401 };

        store
            .publish_feed(&job_id, b"chunk-1".to_vec(), false)
            .await
            .unwrap();
        store
            .publish_feed(&job_id, b"chunk-2".to_vec(), false)
            .await
            .unwrap();
        store
            .publish_feed(&job_id, b"chunk-3".to_vec(), false)
            .await
            .unwrap();

        // Sender registers after the publishes — drain should hand them over.
        let (tx, mut rx) = mpsc::channel::<FeedData>(16);
        store.register(401, tx);

        let f1 = rx.recv().await.unwrap();
        let f2 = rx.recv().await.unwrap();
        let f3 = rx.recv().await.unwrap();
        assert_eq!(f1.data, b"chunk-1");
        assert_eq!(f2.data, b"chunk-2");
        assert_eq!(f3.data, b"chunk-3");
        assert!(!f1.is_final && !f2.is_final && !f3.is_final);
        assert_eq!(
            store.buffer_len(401),
            0,
            "pending buffer should be drained after register"
        );
    }

    /// A buffered `is_final` must end the stream and remove the sender, so
    /// the runner observes EOF and shuts down its receive loop cleanly.
    #[tokio::test]
    async fn test_publish_before_register_preserves_final_and_removes_sender() {
        let store = ChanFeedSenderStore::new();
        let job_id = JobId { value: 402 };

        store
            .publish_feed(&job_id, b"chunk-1".to_vec(), false)
            .await
            .unwrap();
        store
            .publish_feed(&job_id, b"final".to_vec(), true)
            .await
            .unwrap();

        let (tx, mut rx) = mpsc::channel::<FeedData>(16);
        store.register(402, tx);

        let f1 = rx.recv().await.unwrap();
        let f2 = rx.recv().await.unwrap();
        assert_eq!(f1.data, b"chunk-1");
        assert!(!f1.is_final);
        assert_eq!(f2.data, b"final");
        assert!(f2.is_final);

        // The drain task removes the sender after `is_final`. Allow it to
        // run — generous timeout because CI runners can have multi-hundred-ms
        // tokio scheduling delays under load.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline && store.get(402).is_some() {
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            store.get(402).is_none(),
            "final feed should remove sender after drain"
        );
    }

    /// `cleanup_pending` discards an orphan buffer when the runner never
    /// starts (e.g. job cancelled before registration).
    #[tokio::test]
    async fn test_cleanup_pending_removes_unused_buffer() {
        let store = ChanFeedSenderStore::new();
        let job_id = JobId { value: 403 };

        store
            .publish_feed(&job_id, b"orphan".to_vec(), false)
            .await
            .unwrap();
        assert!(store.has_state(403));

        store.cleanup_pending(403);
        assert!(!store.has_state(403));

        // No-op on missing key is safe.
        store.cleanup_pending(403);
    }

    /// `wait_for_feed_ready` timing out must also drop the orphan buffer
    /// so failed-to-start jobs do not leak memory.
    #[tokio::test]
    async fn test_wait_for_feed_ready_timeout_clears_pending_buffer() {
        let store = ChanFeedSenderStore::new();
        let job_id = JobId { value: 404 };

        store
            .publish_feed(&job_id, b"orphan".to_vec(), false)
            .await
            .unwrap();
        assert!(store.has_state(404));

        let result = store
            .wait_for_feed_ready(404, Duration::from_millis(50))
            .await;
        assert!(result.is_err());
        assert!(!store.has_state(404), "timeout should clear pending buffer");
    }

    /// Publishing more than `PENDING_BUFFER_MAX` items before registration
    /// must bound memory by dropping the oldest entry rather than failing.
    #[tokio::test]
    async fn test_pending_buffer_overflow_drops_oldest() {
        let store = ChanFeedSenderStore::new();
        let job_id = JobId { value: 405 };

        for i in 0..(PENDING_BUFFER_MAX + 5) {
            store
                .publish_feed(&job_id, vec![(i & 0xff) as u8], false)
                .await
                .unwrap();
        }

        assert_eq!(
            store.buffer_len(405),
            PENDING_BUFFER_MAX,
            "buffer size must be capped at PENDING_BUFFER_MAX"
        );
    }

    /// Regression scenario for `EnqueueWithClientStream` invoked twice in
    /// succession against a busy `use_static=true, concurrency=1` worker:
    /// the second job's publisher races ahead while the first job's runner
    /// is still draining its stream and holding the pool slot. Once the
    /// runner finally registers its sender, all buffered feeds — including
    /// the final marker — must reach the runner in order.
    #[tokio::test]
    async fn test_concurrent_jobs_second_publishes_before_register() {
        let store = ChanFeedSenderStore::new();

        // Job A is already registered (simulating the first streaming job).
        let (tx_a, _rx_a) = mpsc::channel::<FeedData>(16);
        store.register(501, tx_a);

        // Frontend for Job B begins forwarding feeds before its runner is
        // free to register. With the buffer in place these must not fail.
        let job_b = JobId { value: 502 };
        let store_publisher = store.clone();
        let publish_task = tokio::spawn(async move {
            for i in 0..10u8 {
                store_publisher
                    .publish_feed(&job_b, vec![i], false)
                    .await
                    .unwrap();
            }
            store_publisher
                .publish_feed(&job_b, vec![0xff], true)
                .await
                .unwrap();
        });

        // Wait for publisher to enqueue all items into the buffer.
        publish_task.await.unwrap();
        assert!(store.has_state(502));

        // Runner for Job B finally registers (the first job's pool guard was
        // released). Buffered feeds should now be drained in order.
        let (tx_b, mut rx_b) = mpsc::channel::<FeedData>(32);
        store.register(502, tx_b);

        let mut received = Vec::new();
        // 10 non-final + 1 final
        for _ in 0..11 {
            received.push(rx_b.recv().await.expect("drain must deliver buffered feed"));
        }
        let payloads: Vec<Vec<u8>> = received.iter().map(|f| f.data.clone()).collect();
        let expected: Vec<Vec<u8>> = (0..10u8).map(|i| vec![i]).chain([vec![0xff]]).collect();
        assert_eq!(
            payloads, expected,
            "buffered feeds must reach the runner in publish order"
        );
        assert!(received.last().unwrap().is_final);
    }

    /// Order preservation under concurrent live publish during drain:
    /// while the background drain task is still flushing pre-registration
    /// feeds, additional publishes that arrive on the live path must be
    /// appended to the buffer so they trail the buffered feeds rather than
    /// racing the drain task on the channel.
    #[tokio::test]
    async fn test_drain_preserves_order_when_live_publish_races() {
        let store = ChanFeedSenderStore::new();
        let job_id = JobId { value: 700 };

        // Pre-register: queue 5 feeds in the buffer.
        for i in 0..5u8 {
            store.publish_feed(&job_id, vec![i], false).await.unwrap();
        }

        // Register a small channel so the drain task yields between sends.
        let (tx, mut rx) = mpsc::channel::<FeedData>(2);
        store.register(700, tx);

        // Immediately publish live feeds that would otherwise race the drain.
        for i in 5..10u8 {
            store.publish_feed(&job_id, vec![i], false).await.unwrap();
        }
        store.publish_feed(&job_id, vec![0xff], true).await.unwrap();

        let mut received: Vec<u8> = Vec::new();
        for _ in 0..11 {
            let f = rx.recv().await.unwrap();
            received.push(f.data[0]);
            if f.is_final {
                break;
            }
        }
        let expected: Vec<u8> = (0..10u8).chain(std::iter::once(0xff)).collect();
        assert_eq!(
            received, expected,
            "live publishes during drain must trail buffered feeds in arrival order"
        );
    }

    /// `remove` (used on stream completion) must discard the entire per-job
    /// state, including any buffered feeds that the drain task has not yet
    /// forwarded — so a later job that happens to reuse the id starts clean.
    #[tokio::test]
    async fn test_remove_also_clears_pending() {
        let store = ChanFeedSenderStore::new();
        let job_id = JobId { value: 406 };

        // Buffer a feed, then register a sender whose receiver we keep alive
        // but never read from. The drain task will block on send() forever
        // (channel size 1, full after first item). Calling `remove()` must
        // wipe both the buffer and the sender entry.
        store
            .publish_feed(&job_id, b"x".to_vec(), false)
            .await
            .unwrap();
        let (tx, _rx) = mpsc::channel::<FeedData>(1);
        store.register(406, tx);

        let _ = store.remove(406);
        assert!(!store.has_state(406), "state must be wiped on remove");
        assert!(store.get(406).is_none());
    }
}
