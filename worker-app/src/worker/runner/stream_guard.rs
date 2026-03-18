use super::pool::RunnerPoolManagerImpl;
use deadpool::managed::Object;
use futures::Stream;
use futures::stream::BoxStream;
use jobworkerp_runner::runner::cancellation_helper::CancelMonitoringHelper;
use std::pin::Pin;
use std::task::{Context, Poll};

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
}
