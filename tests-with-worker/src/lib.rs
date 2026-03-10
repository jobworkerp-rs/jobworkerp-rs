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

/// Handle to a running test worker. Keeps the worker alive until dropped.
pub struct TestWorkerHandle {
    _lock: shutdown::ShutdownLock,
    _wait: shutdown::ShutdownWait,
}

impl TestWorkerHandle {
    /// Signal shutdown and wait for background tasks (with timeout).
    pub async fn shutdown(mut self) {
        self._lock.unlock();
        // Dispatcher tasks listen to OS signals, not lock state.
        // Use a short timeout to avoid hanging in tests.
        tokio::select! {
            _ = self._wait.wait() => {},
            _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {
                tracing::debug!("test worker shutdown timeout - proceeding");
            },
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
    let (lock, wait) = shutdown::create_lock_and_wait();

    let app_wrapper_module = Arc::new(create_test_app_wrapper_module(app_module.clone()));
    let mcp_clients = Arc::new(McpServerFactory::default());
    let runner_factory = Arc::new(RunnerFactory::new(
        app_module.clone(),
        app_wrapper_module,
        mcp_clients,
    ));

    let config_module = app_module.config_module.clone();
    let id_generator = Arc::new(IdGeneratorWrapper::new());

    let wm = WorkerModules::new(config_module, id_generator, app_module, runner_factory);

    // dispatch_jobs requires &'static self, so leak the dispatcher.
    // WARNING: Each call leaks memory. Tests must run with --test-threads=1
    // to minimize accumulation. Background tasks also persist because
    // dispatchers listen to OS signals (not ShutdownLock) for termination.
    // TODO: Refactor dispatch_jobs to use Arc<Self> to enable proper cleanup.
    let dispatcher: &'static dyn JobDispatcher = Box::leak(wm.job_dispatcher);
    dispatcher.dispatch_jobs(lock.clone())?;

    Ok(TestWorkerHandle {
        _lock: lock,
        _wait: wait,
    })
}
