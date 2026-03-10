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
}

impl TestWorkerHandle {
    pub fn shutdown(self) {
        self._lock.unlock();
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
    let (lock, _wait) = shutdown::create_lock_and_wait();

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
    // Acceptable in tests: the process is short-lived.
    let dispatcher: &'static dyn JobDispatcher = Box::leak(wm.job_dispatcher);
    dispatcher.dispatch_jobs(lock.clone())?;

    Ok(TestWorkerHandle { _lock: lock })
}

/// Create a hybrid test app with a running backend worker.
///
/// Convenience wrapper combining `create_hybrid_test_app` + `start_test_worker`.
pub async fn create_hybrid_test_app_with_worker() -> Result<(Arc<AppModule>, TestWorkerHandle)> {
    let app_module = Arc::new(app::module::test::create_hybrid_test_app().await?);
    let handle = start_test_worker(app_module.clone()).await?;
    Ok((app_module, handle))
}
