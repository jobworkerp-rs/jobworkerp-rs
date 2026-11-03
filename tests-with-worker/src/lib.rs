//! Common test helpers that require a running backend worker.
//!
//! This crate hosts tests that need a backend worker (JobDispatcher)
//! processing jobs from the queue. Tests are organized by the crate
//! whose internal functions they exercise.

use anyhow::Result;
use app::module::AppModule;
use app_wrapper::modules::test::create_test_app_wrapper_module;
use app_wrapper::runner::RunnerFactory;
use command_utils::util::shutdown;
use infra::infra::IdGeneratorWrapper;
use jobworkerp_runner::runner::mcp::proxy::McpServerFactory;
use std::sync::Arc;
use worker_app::WorkerModules;
use worker_app::worker::dispatcher::JobDispatcher;

/// Handle to a running test worker. Dropping it stops the worker's job
/// dispatchers; [`shutdown`] also cleans up after them so later tests in the
/// same process are not affected. Call [`shutdown`] unless the worker lives
/// until the end of the process.
///
/// [`shutdown`]: TestWorkerHandle::shutdown
pub struct TestWorkerHandle {
    lock: Option<shutdown::ShutdownLock>,
    _wait: shutdown::ShutdownWait,
    shutdown_send: tokio::sync::watch::Sender<bool>,
    uses_redis: bool,
}

impl Drop for TestWorkerHandle {
    fn drop(&mut self) {
        let _ = self.shutdown_send.send(true);
    }
}

impl TestWorkerHandle {
    /// Signal shutdown, wait for background tasks (with timeout), and close the
    /// BLPOP waits the stopped dispatchers left behind.
    pub async fn shutdown(mut self) {
        let _ = self.shutdown_send.send(true);
        if let Some(lock) = self.lock.take() {
            lock.unlock();
        }
        // Bound the wait so a runner stuck in a job cannot hang the test.
        tokio::select! {
            _ = self._wait.wait() => {},
            _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {
                tracing::debug!("test worker shutdown timeout - proceeding");
            },
        }
        // A stopped dispatcher's BLPOP keeps waiting on the server, on a
        // connection of the process-wide test pool, and would pop the next
        // test's jobs. Done after the wait so no dispatcher sends another one.
        if self.uses_redis
            && let Err(e) = infra_utils::infra::test::release_test_redis_blocking_clients().await
        {
            tracing::warn!("failed to release blocked test redis clients: {e:?}");
        }
    }
}

/// Start a backend worker that processes jobs from the queue.
///
/// Uses the same `app_module` for both the worker and the caller,
/// mirroring production `boot_all_in_one` where a single `AppModule`
/// is shared by the worker and the gRPC front.
///
/// The caller is responsible for providing an `AppModule` whose
/// `config_module` has correct `StorageType` and `WorkerConfig`.
/// `create_hybrid_test_app()` satisfies this requirement.
pub async fn start_test_worker(app_module: Arc<AppModule>) -> Result<TestWorkerHandle> {
    start_test_worker_inner(app_module, false).await
}

/// Start a test worker with the production SANDBOX cleanup coordinator.
pub async fn start_test_worker_with_sandbox(
    app_module: Arc<AppModule>,
) -> Result<TestWorkerHandle> {
    start_test_worker_inner(app_module, true).await
}

async fn start_test_worker_inner(
    app_module: Arc<AppModule>,
    with_sandbox: bool,
) -> Result<TestWorkerHandle> {
    let (lock, wait) = shutdown::create_lock_and_wait();

    let app_wrapper_module = Arc::new(create_test_app_wrapper_module(app_module.clone()));
    let mcp_clients = Arc::new(McpServerFactory::default());
    let runner_factory = Arc::new(RunnerFactory::new(
        app_module.clone(),
        app_wrapper_module,
        mcp_clients,
    ));

    let config_module = app_module.config_module.clone();
    let uses_redis = config_module.storage_type() == proto::jobworkerp::data::StorageType::Scalable;
    let id_generator = Arc::new(IdGeneratorWrapper::new());

    let (shutdown_send, shutdown_recv) = tokio::sync::watch::channel(false);
    let wm = if with_sandbox {
        WorkerModules::new_with_session_and_shutdown(
            config_module,
            id_generator,
            app_module,
            runner_factory,
            None,
            lock.clone(),
            shutdown_recv.clone(),
        )?
    } else {
        WorkerModules::new(config_module, id_generator, app_module, runner_factory)
    };

    // dispatch_jobs_until requires &'static self, so leak the dispatcher.
    // Each call leaks the dispatcher's memory (tests run with --test-threads=1
    // to limit the accumulation); its job loops stop on shutdown_send.
    // TODO: Refactor dispatch_jobs_until to use Arc<Self> to free the dispatcher.
    let dispatcher: &'static dyn JobDispatcher = Box::leak(wm.job_dispatcher);
    dispatcher.dispatch_jobs_until(lock.clone(), shutdown_recv)?;

    Ok(TestWorkerHandle {
        lock: Some(lock),
        _wait: wait,
        shutdown_send,
        uses_redis,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use infra_utils::infra::test::{REDIS_CONFIG, TEST_RUNTIME, setup_test_redis_blocking_pool};
    use std::time::Duration;

    #[test]
    fn shutdown_releases_the_blocking_redis_connections() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_redis_blocking_pool().await;
            let app_module = Arc::new(app::module::test::create_hybrid_test_app().await?);
            let worker_handle = start_test_worker(app_module).await?;
            // Let the dispatchers take their connections and block in BLPOP.
            tokio::time::sleep(Duration::from_millis(500)).await;
            assert!(pool.state().connections > pool.state().idle_connections);

            worker_handle.shutdown().await;

            // Every slot can be taken again, and each connection answers at
            // once: one still blocked in a pending BLPOP would not.
            let max_size = REDIS_CONFIG.pool_size;
            let mut connections = Vec::with_capacity(max_size);
            for _ in 0..max_size {
                connections.push(tokio::time::timeout(Duration::from_secs(5), pool.get()).await??);
            }
            for connection in connections.iter_mut() {
                let pong: String = tokio::time::timeout(
                    Duration::from_secs(2),
                    redis::cmd("PING").query_async(&mut **connection),
                )
                .await??;
                assert_eq!(pong, "PONG");
            }
            Ok(())
        })
    }

    #[test]
    fn stopped_worker_leaves_new_jobs_in_the_queue() -> Result<()> {
        use infra::infra::job::rows::{JobqueueAndCodec, UseJobqueueAndCodec};
        use redis::AsyncCommands;

        TEST_RUNTIME.block_on(async {
            let pool = setup_test_redis_blocking_pool().await;
            let app_module = Arc::new(app::module::test::create_hybrid_test_app().await?);
            let (channel, _) = app_module
                .config_module
                .worker_config
                .channel_concurrency_pair()
                .into_iter()
                .next()
                .expect("a dispatched channel");
            let queue = JobqueueAndCodec::queue_channel_name(
                channel,
                Some(&(proto::jobworkerp::data::Priority::Medium as i32)),
            );
            let worker_handle = start_test_worker(app_module).await?;
            tokio::time::sleep(Duration::from_millis(500)).await;
            worker_handle.shutdown().await;

            // Nothing of the stopped worker, including a BLPOP it gave up on,
            // may take an item pushed after it stopped.
            let mut connection = pool.get().await?;
            let _: () = connection.del(&queue).await?;
            let _: () = connection.rpush(&queue, b"not-a-job".to_vec()).await?;
            // Long enough for a dispatcher or a leftover BLPOP to take the item.
            tokio::time::sleep(Duration::from_millis(1500)).await;
            let remaining: usize = connection.llen(&queue).await?;
            let _: () = connection.del(&queue).await?;
            assert_eq!(remaining, 1);
            Ok(())
        })
    }
}
