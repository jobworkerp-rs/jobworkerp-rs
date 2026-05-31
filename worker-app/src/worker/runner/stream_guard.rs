use super::pool::RunnerPoolManagerImpl;
use deadpool::managed::Object;
use futures::Stream;
use futures::stream::BoxStream;
use jobworkerp_runner::runner::cancellation_helper::CancelMonitoringHelper;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

/// Stream wrapper that enforces an inter-chunk (idle) timeout.
///
/// Background: V2 runners now return their `BoxStream` to the host
/// immediately, before the plugin emits its first chunk
/// (`runner/src/runner/plugins/impls.rs::run_stream_v2`). The pre-rewrite
/// `tokio::select!` in `run_and_stream` therefore only guards the initial
/// stream handoff and no longer monitors actual plugin progress. If a
/// plugin hangs while draining feed data (e.g. whisper after receiving its
/// final feed) the host would otherwise hold the static runner pool slot
/// forever and the next streaming job would deadlock.
///
/// `IdleTimeoutStream` invokes `on_timeout` (typically: cancel the
/// runner's cancellation token) and finishes the stream with
/// `Poll::Ready(None)` after `idle_window` has elapsed without a new
/// chunk. `StreamWithPoolGuard` releases the pool object on `None`, so
/// downstream subscribers see a clean termination and the runner becomes
/// available again.
///
/// `idle_window` is the same value as `JobData::timeout` (re-used as
/// "max gap between two chunks" instead of "total job duration") so we do
/// not introduce a new configuration knob. `idle_window == 0` should be
/// handled by the caller (skip wrapping).
pub struct IdleTimeoutStream<T> {
    stream: BoxStream<'static, T>,
    on_timeout: Option<Box<dyn FnOnce() + Send>>,
    /// Item the caller wants emitted right after `on_timeout` fires. Lets
    /// downstream code observe an "I gave up here" sentinel (for
    /// `ResultOutputItem` streams: an End trailer with error metadata)
    /// before the channel terminates. `None` skips the synthetic emit.
    on_timeout_item: Option<T>,
    idle_window: Duration,
    /// Sleep timer reset on every successful chunk poll. Boxed so we can
    /// safely `Pin::new` against `&mut self` without requiring `T: Unpin`.
    sleep: Pin<Box<tokio::time::Sleep>>,
    /// Drives the post-timeout sequence: `Running` while the inner stream
    /// is being polled, `EmitTimeoutItem` after `on_timeout` fired and
    /// the synthetic item is queued, `Done` once `None` was returned to
    /// the consumer.
    state: IdleState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdleState {
    Running,
    EmitTimeoutItem,
    Done,
}

impl<T> IdleTimeoutStream<T> {
    pub fn new(
        stream: BoxStream<'static, T>,
        idle_window: Duration,
        on_timeout: impl FnOnce() + Send + 'static,
    ) -> Self {
        Self::with_timeout_item(stream, idle_window, on_timeout, None)
    }

    /// Variant that emits `on_timeout_item` as the final item before
    /// returning `None`. Useful when the consumer interprets a bare
    /// `None` as a successful EOF (e.g. `collect_stream` for plugin
    /// outputs) — sending an item lets the consumer distinguish stall
    /// from completion.
    pub fn with_timeout_item(
        stream: BoxStream<'static, T>,
        idle_window: Duration,
        on_timeout: impl FnOnce() + Send + 'static,
        on_timeout_item: Option<T>,
    ) -> Self {
        Self {
            stream,
            on_timeout: Some(Box::new(on_timeout)),
            on_timeout_item,
            idle_window,
            sleep: Box::pin(tokio::time::sleep(idle_window)),
            state: IdleState::Running,
        }
    }
}

// `T: Unpin` lets us reach the struct fields through `Pin<&mut Self>`
// without unsafe pinning: the inner `BoxStream` / `Pin<Box<Sleep>>` are
// themselves heap-pinned, and `Option<T>` only needs to mutate the
// outer `Option` cell — `T` is moved out by value when we hand it back.
impl<T: Unpin> Stream for IdleTimeoutStream<T> {
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // After timeout fired, hand back the synthetic item (if any)
        // before terminating. Splitting this off keeps the inner-stream
        // poll branch below from racing against a still-pending plugin
        // task once cancellation is already in flight.
        match self.state {
            IdleState::Done => return Poll::Ready(None),
            IdleState::EmitTimeoutItem => {
                let item = self.on_timeout_item.take();
                self.state = IdleState::Done;
                return Poll::Ready(item);
            }
            IdleState::Running => {}
        }
        match Pin::new(&mut self.stream).poll_next(cx) {
            Poll::Ready(Some(item)) => {
                // Got progress: reset the idle timer to the full window.
                let new_deadline = tokio::time::Instant::now() + self.idle_window;
                self.sleep.as_mut().reset(new_deadline);
                Poll::Ready(Some(item))
            }
            Poll::Ready(None) => {
                self.state = IdleState::Done;
                Poll::Ready(None)
            }
            Poll::Pending => match self.sleep.as_mut().poll(cx) {
                Poll::Ready(()) => {
                    tracing::warn!(
                        "IdleTimeoutStream: no chunk within {:?}; cancelling and ending stream",
                        self.idle_window
                    );
                    if let Some(cb) = self.on_timeout.take() {
                        cb();
                    }
                    // If the caller seeded an `on_timeout_item`, surface
                    // it now so the consumer can tell a stall apart from
                    // an EOF; otherwise terminate immediately.
                    if self.on_timeout_item.is_some() {
                        self.state = IdleState::EmitTimeoutItem;
                        let item = self.on_timeout_item.take();
                        self.state = IdleState::Done;
                        Poll::Ready(item)
                    } else {
                        self.state = IdleState::Done;
                        Poll::Ready(None)
                    }
                }
                Poll::Pending => Poll::Pending,
            },
        }
    }
}

/// Stream Wrapper with Pool Object (for use_static=true only)
/// Automatically returns Pool Object when stream ends
pub struct StreamWithPoolGuard<T> {
    stream: BoxStream<'static, T>,
    _pool_guard: Option<Object<RunnerPoolManagerImpl>>, // deadpool::Object
    on_complete: Option<Box<dyn FnOnce() + Send>>,
}

/// Stream Wrapper with Cancel Helper (for use_static=false only)
/// Automatically destroys CancelHelper when stream ends (to maintain cancellation monitoring)
pub struct StreamWithCancelGuard<T> {
    stream: BoxStream<'static, T>,
    _cancel_guard: Option<CancelMonitoringHelper>,
}

impl<T> StreamWithPoolGuard<T> {
    /// Create Stream with Pool Guard (use only when use_static=true)
    pub fn new(stream: BoxStream<'static, T>, pool_object: Object<RunnerPoolManagerImpl>) -> Self {
        tracing::debug!(
            "Created StreamWithPoolGuard - pool object will be held until stream completion"
        );
        Self {
            stream,
            _pool_guard: Some(pool_object),
            on_complete: None,
        }
    }

    /// Create Stream with Pool Guard and a cleanup callback invoked on stream completion or drop
    pub fn with_on_complete(
        stream: BoxStream<'static, T>,
        pool_object: Object<RunnerPoolManagerImpl>,
        on_complete: impl FnOnce() + Send + 'static,
    ) -> Self {
        tracing::debug!(
            "Created StreamWithPoolGuard with on_complete - pool object will be held until stream completion"
        );
        Self {
            stream,
            _pool_guard: Some(pool_object),
            on_complete: Some(Box::new(on_complete)),
        }
    }
}

impl<T> Stream for StreamWithPoolGuard<T> {
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let result = Pin::new(&mut self.stream).poll_next(cx);

        // Release pool guard and run cleanup when stream ends
        if let Poll::Ready(None) = result {
            if self._pool_guard.take().is_some() {
                tracing::debug!(
                    "Stream completed, releasing pool object (reset_for_pooling will be called)"
                );
            }
            if let Some(cb) = self.on_complete.take() {
                tracing::debug!("Stream completed, running on_complete callback");
                cb();
            }
        }

        result
    }
}

impl<T> Drop for StreamWithPoolGuard<T> {
    fn drop(&mut self) {
        if self._pool_guard.is_some() {
            tracing::debug!(
                "StreamWithPoolGuard dropped with active pool guard - emergency release"
            );
        }
        if let Some(cb) = self.on_complete.take() {
            tracing::debug!("StreamWithPoolGuard dropped, running on_complete callback");
            cb();
        }
    }
}

impl<T> StreamWithCancelGuard<T> {
    /// Create Stream with Cancel Guard (use only when use_static=false)
    pub fn new(stream: BoxStream<'static, T>, cancel_helper: CancelMonitoringHelper) -> Self {
        tracing::debug!(
            "Created StreamWithCancelGuard - cancel helper will be held until stream completion"
        );
        Self {
            stream,
            _cancel_guard: Some(cancel_helper),
        }
    }
}

impl<T> Stream for StreamWithCancelGuard<T> {
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let result = Pin::new(&mut self.stream).poll_next(cx);

        // Release cancel guard when stream ends
        if let Poll::Ready(None) = result
            && self._cancel_guard.take().is_some()
        {
            tracing::debug!(
                "Stream completed, releasing cancel helper (cancellation monitoring cleanup will happen)"
            );
        }

        result
    }
}

impl<T> Drop for StreamWithCancelGuard<T> {
    fn drop(&mut self) {
        if self._cancel_guard.is_some() {
            tracing::debug!(
                "StreamWithCancelGuard dropped with active cancel guard - emergency release"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::pool::RunnerFactoryWithPool;
    use super::*;
    use anyhow::Result;
    use app::module::test::TEST_PLUGIN_DIR;
    use app::{app::WorkerConfig, module::test::create_hybrid_test_app};
    use app_wrapper::runner::RunnerFactory;
    use futures::{StreamExt, stream};
    use jobworkerp_runner::runner::mcp::proxy::McpServerFactory;
    use proto::jobworkerp::data::{RunnerType, WorkerData};
    use std::sync::Arc;

    async fn create_test_pool() -> Result<RunnerFactoryWithPool> {
        let app_module = Arc::new(create_hybrid_test_app().await?);
        let app_wrapper_module = Arc::new(
            app_wrapper::modules::test::create_test_app_wrapper_module(app_module.clone()),
        );
        let runner_factory = RunnerFactory::new(
            app_module,
            app_wrapper_module,
            Arc::new(McpServerFactory::default()),
        );
        runner_factory.load_plugins_from(TEST_PLUGIN_DIR).await;

        RunnerFactoryWithPool::new(
            Arc::new(proto::jobworkerp::data::RunnerData {
                name: RunnerType::Command.as_str_name().to_string(),
                ..Default::default()
            }),
            Arc::new(WorkerData {
                runner_settings: Vec::new(),
                channel: None,
                use_static: true, // Use pool
                ..Default::default()
            }),
            Arc::new(runner_factory),
            Arc::new(WorkerConfig {
                default_concurrency: 1,
                ..WorkerConfig::default()
            }),
        )
        .await
    }

    #[test]
    fn test_stream_with_pool_guard_creation() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let pool = create_test_pool().await?;
            let pool_object = pool.get().await?;

            let test_stream = Box::pin(stream::iter(vec![1, 2, 3]));

            let guard_stream = StreamWithPoolGuard::new(test_stream, pool_object);

            let items: Vec<i32> = guard_stream.collect().await;
            assert_eq!(items, vec![1, 2, 3]);

            tracing::debug!("✅ StreamWithPoolGuard creation test completed");
            Ok(())
        })
    }

    #[test]
    fn test_stream_with_pool_guard_early_drop() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let pool = create_test_pool().await?;
            let pool_object = pool.get().await?;

            let test_stream = Box::pin(stream::iter(vec![1, 2, 3]));

            let guard_stream = StreamWithPoolGuard::new(test_stream, pool_object);

            // Drop midway
            drop(guard_stream);

            // (Actually verified through log output and pool reuse)
            let pool_object2 = pool.get().await?;
            assert!(!pool_object2.lock().await.name().is_empty());

            tracing::debug!("✅ StreamWithPoolGuard early drop test completed");
            Ok(())
        })
    }

    #[test]
    fn test_stream_with_pool_guard_normal_completion() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let pool = create_test_pool().await?;
            let pool_object = pool.get().await?;

            let test_stream = Box::pin(stream::iter(vec![1, 2, 3]));

            let mut guard_stream = StreamWithPoolGuard::new(test_stream, pool_object);

            // Manually get stream elements sequentially
            use futures::StreamExt;
            assert_eq!(guard_stream.next().await, Some(1));
            assert_eq!(guard_stream.next().await, Some(2));
            assert_eq!(guard_stream.next().await, Some(3));
            assert_eq!(guard_stream.next().await, None); // Pool Guard released here

            let pool_object2 = pool.get().await?;
            assert!(!pool_object2.lock().await.name().is_empty());

            tracing::debug!("✅ StreamWithPoolGuard normal completion test completed");
            Ok(())
        })
    }

    #[test]
    fn test_multiple_pool_objects_with_stream_guard() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let pool = create_test_pool().await?;

            for i in 0..3 {
                let pool_object = pool.get().await?;
                let test_stream = Box::pin(stream::iter(vec![i, i + 10]));
                let guard_stream = StreamWithPoolGuard::new(test_stream, pool_object);

                let items: Vec<i32> = guard_stream.collect().await;
                assert_eq!(items, vec![i, i + 10]);

                tracing::debug!("Completed stream guard cycle {}", i);
            }

            let final_pool_object = pool.get().await?;
            assert!(!final_pool_object.lock().await.name().is_empty());

            tracing::debug!("✅ Multiple pool objects with stream guard test completed");
            Ok(())
        })
    }

    #[test]
    fn test_on_complete_called_on_stream_completion() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let pool = create_test_pool().await?;
            let pool_object = pool.get().await?;

            let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let called_clone = called.clone();

            let test_stream = Box::pin(stream::iter(vec![1, 2, 3]));
            let guard_stream =
                StreamWithPoolGuard::with_on_complete(test_stream, pool_object, move || {
                    called_clone.store(true, std::sync::atomic::Ordering::SeqCst);
                });

            let items: Vec<i32> = guard_stream.collect().await;
            assert_eq!(items, vec![1, 2, 3]);
            assert!(called.load(std::sync::atomic::Ordering::SeqCst));

            Ok(())
        })
    }

    #[test]
    fn test_on_complete_called_on_early_drop() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let pool = create_test_pool().await?;
            let pool_object = pool.get().await?;

            let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let called_clone = called.clone();

            let test_stream = Box::pin(stream::iter(vec![1, 2, 3]));
            let guard_stream =
                StreamWithPoolGuard::with_on_complete(test_stream, pool_object, move || {
                    called_clone.store(true, std::sync::atomic::Ordering::SeqCst);
                });

            drop(guard_stream);
            assert!(called.load(std::sync::atomic::Ordering::SeqCst));

            Ok(())
        })
    }

    /// A stream that yields chunks at controlled gaps so the idle timer
    /// can be exercised deterministically. Each `next()` waits for the
    /// provided delay before producing the corresponding value; once the
    /// iterator is exhausted the stream returns `None`.
    struct DelayedStream {
        items: std::collections::VecDeque<(std::time::Duration, u8)>,
        sleep: Option<Pin<Box<tokio::time::Sleep>>>,
        pending_value: Option<u8>,
    }
    impl DelayedStream {
        fn new(items: Vec<(std::time::Duration, u8)>) -> Self {
            Self {
                items: items.into(),
                sleep: None,
                pending_value: None,
            }
        }
    }
    impl Stream for DelayedStream {
        type Item = u8;
        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            if self.sleep.is_none() {
                match self.items.pop_front() {
                    Some((delay, value)) => {
                        self.sleep = Some(Box::pin(tokio::time::sleep(delay)));
                        self.pending_value = Some(value);
                    }
                    None => return Poll::Ready(None),
                }
            }
            let sleep = self.sleep.as_mut().expect("sleep set above");
            match sleep.as_mut().poll(cx) {
                Poll::Ready(()) => {
                    self.sleep = None;
                    Poll::Ready(self.pending_value.take())
                }
                Poll::Pending => Poll::Pending,
            }
        }
    }

    /// Stream that delivers chunks well within the idle window must pass
    /// through unmodified and the timeout callback must NOT fire.
    #[test]
    fn idle_timeout_passes_chunks_when_within_window() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            use futures::StreamExt;
            let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let fired_clone = fired.clone();
            let inner = Box::pin(DelayedStream::new(vec![
                (Duration::from_millis(30), 1),
                (Duration::from_millis(30), 2),
                (Duration::from_millis(30), 3),
            ]));
            let mut wrapped =
                IdleTimeoutStream::new(inner, Duration::from_millis(200), move || {
                    fired_clone.store(true, std::sync::atomic::Ordering::SeqCst);
                });
            let mut got: Vec<u8> = Vec::new();
            while let Some(v) = wrapped.next().await {
                got.push(v);
            }
            assert_eq!(got, vec![1, 2, 3]);
            assert!(!fired.load(std::sync::atomic::Ordering::SeqCst));
            Ok(())
        })
    }

    /// A stalled inner stream (no chunks within the idle window) must
    /// trigger the timeout callback exactly once and finish with `None`.
    #[test]
    fn idle_timeout_fires_callback_and_ends_stream_on_stall() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            use futures::StreamExt;
            let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let fired_clone = fired.clone();
            // Inner stream tries to deliver a chunk after 500ms but the
            // idle window expires after 100ms; the wrapper must short-
            // circuit to None and fire `on_timeout` once.
            let inner = Box::pin(DelayedStream::new(vec![(Duration::from_millis(500), 1)]));
            let mut wrapped =
                IdleTimeoutStream::new(inner, Duration::from_millis(100), move || {
                    fired_clone.store(true, std::sync::atomic::Ordering::SeqCst);
                });
            assert!(wrapped.next().await.is_none());
            assert!(fired.load(std::sync::atomic::Ordering::SeqCst));
            // Subsequent polls remain None and do not fire the callback
            // a second time.
            assert!(wrapped.next().await.is_none());
            Ok(())
        })
    }

    /// When the caller seeds `on_timeout_item`, the wrapper must emit it
    /// as the final item before terminating. This is the regression test
    /// for the case where `collect_stream` interpreted a bare `None`
    /// from a stalled plugin as a clean EOF and persisted partial data
    /// as a successful result.
    #[test]
    fn idle_timeout_emits_synthetic_item_before_terminating_on_stall() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            use futures::StreamExt;
            let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let fired_clone = fired.clone();
            let inner = Box::pin(DelayedStream::new(vec![(Duration::from_millis(500), 1)]));
            let mut wrapped = IdleTimeoutStream::with_timeout_item(
                inner,
                Duration::from_millis(100),
                move || {
                    fired_clone.store(true, std::sync::atomic::Ordering::SeqCst);
                },
                Some(0xFFu8),
            );
            // First poll after the idle window: receive the synthetic
            // sentinel that the caller seeded, NOT a chunk from the
            // underlying stream (which would still be sleeping at 500ms).
            assert_eq!(wrapped.next().await, Some(0xFFu8));
            assert!(fired.load(std::sync::atomic::Ordering::SeqCst));
            // Subsequent polls remain None.
            assert!(wrapped.next().await.is_none());
            assert!(wrapped.next().await.is_none());
            Ok(())
        })
    }

    /// `on_timeout_item = None` keeps the legacy behaviour: stall ends
    /// the stream with a bare `None`, no synthetic item emitted.
    #[test]
    fn idle_timeout_without_synthetic_item_falls_back_to_none() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            use futures::StreamExt;
            let inner = Box::pin(DelayedStream::new(vec![(Duration::from_millis(500), 1)]));
            let mut wrapped = IdleTimeoutStream::with_timeout_item(
                inner,
                Duration::from_millis(100),
                || {},
                None::<u8>,
            );
            assert!(wrapped.next().await.is_none());
            Ok(())
        })
    }
}
