use super::pool::{RunnerFactoryWithPool, RunnerPoolManagerImpl};
use anyhow::{Result, anyhow};
use app::app::WorkerConfig;
use app_wrapper::runner::RunnerFactory;
use deadpool::managed::{Object, Timeouts};
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::runner::cancellation::CancellableRunner;
use jobworkerp_runner::runner::sandbox::SandboxCleanupRegistry;
use proto::jobworkerp::data::{RunnerData, RunnerType, WorkerData, WorkerId};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{OwnedRwLockReadGuard, RwLock, watch};

#[cfg(test)]
#[path = "shutdown_tests.rs"]
mod shutdown_tests;

pub struct RunnerFactoryWithPoolMap {
    // TODO not implement as map? or keep as static ?
    pub pools: Arc<RwLock<HashMap<i64, RunnerFactoryWithPool>>>,
    runner_factory: Arc<RunnerFactory>,
    worker_config: Arc<WorkerConfig>,
    sandbox_cleanup_registry: Option<SandboxCleanupRegistry>,
    shutdown: Option<watch::Receiver<bool>>,
    checkout_gate: Arc<RwLock<bool>>,
}

impl RunnerFactoryWithPoolMap {
    pub fn new(runner_factory: Arc<RunnerFactory>, worker_config: Arc<WorkerConfig>) -> Self {
        Self::new_inner(runner_factory, worker_config, None, None)
    }

    pub fn new_with_cleanup_registry(
        runner_factory: Arc<RunnerFactory>,
        worker_config: Arc<WorkerConfig>,
        cleanup_registry: SandboxCleanupRegistry,
    ) -> Self {
        Self::new_inner(runner_factory, worker_config, Some(cleanup_registry), None)
    }

    pub fn new_with_cleanup_registry_and_shutdown(
        runner_factory: Arc<RunnerFactory>,
        worker_config: Arc<WorkerConfig>,
        cleanup_registry: SandboxCleanupRegistry,
        shutdown: watch::Receiver<bool>,
    ) -> Self {
        Self::new_inner(
            runner_factory,
            worker_config,
            Some(cleanup_registry),
            Some(shutdown),
        )
    }

    fn new_inner(
        runner_factory: Arc<RunnerFactory>,
        worker_config: Arc<WorkerConfig>,
        sandbox_cleanup_registry: Option<SandboxCleanupRegistry>,
        shutdown: Option<watch::Receiver<bool>>,
    ) -> Self {
        Self {
            pools: Arc::new(RwLock::new(HashMap::<i64, RunnerFactoryWithPool>::new())),
            runner_factory,
            worker_config,
            sandbox_cleanup_registry,
            shutdown,
            checkout_gate: Arc::new(RwLock::new(false)),
        }
    }

    pub(crate) async fn checkout_guard(
        &self,
        runner_data: &RunnerData,
    ) -> Result<OwnedRwLockReadGuard<bool>> {
        if self.process_shutdown_requested() {
            return Err(anyhow!("runner pool map is shutting down"));
        }
        let guard = self.checkout_gate.clone().read_owned().await;
        if *guard || self.process_shutdown_requested() {
            return Err(anyhow!("runner pool map is shutting down"));
        }
        if is_sandbox_runner(runner_data) {
            let registry = self
                .sandbox_cleanup_registry
                .as_ref()
                .ok_or_else(|| anyhow!("SANDBOX requires a process cleanup registry"))?;
            if registry.is_shutting_down().await {
                return Err(anyhow!("SANDBOX cleanup registry is shutting down"));
            }
        }
        if self.process_shutdown_requested() {
            return Err(anyhow!("runner pool map is shutting down"));
        }
        Ok(guard)
    }

    fn process_shutdown_requested(&self) -> bool {
        self.shutdown
            .as_ref()
            .is_some_and(|shutdown| *shutdown.borrow() || shutdown.has_changed().is_err())
    }

    async fn await_static_checkout(
        &self,
        pool: &RunnerFactoryWithPool,
        runner_data: &RunnerData,
        timeouts: Option<&Timeouts>,
    ) -> Result<Object<RunnerPoolManagerImpl>> {
        let get = async {
            match timeouts {
                Some(timeouts) => pool.timeout_get(timeouts).await,
                None => pool.get().await,
            }
        };
        let runner = match self.shutdown.clone() {
            Some(shutdown) => tokio::select! {
                result = get => result?,
                _ = wait_for_shutdown(shutdown) => return Err(anyhow!("runner pool map is shutting down")),
            },
            None => get.await?,
        };
        // A checkout may have completed concurrently with the shutdown signal.
        let _checkout = self.checkout_guard(runner_data).await?;
        Ok(runner)
    }

    /// Prevents new runner checkouts before dropping every cached static pool.
    pub async fn shutdown_and_clear(&self) {
        let mut shutting_down = self.checkout_gate.write().await;
        *shutting_down = true;
        self.clear().await;
    }

    async fn create_pool_for_worker_id(
        &self,
        runner_data: Arc<RunnerData>,
        worker_data: Arc<WorkerData>,
        worker_id: WorkerId,
    ) -> Result<RunnerFactoryWithPool> {
        match self.sandbox_cleanup_registry.clone() {
            Some(registry) => {
                RunnerFactoryWithPool::new_for_worker_id_with_cleanup_registry(
                    runner_data,
                    worker_data,
                    self.runner_factory.clone(),
                    self.worker_config.clone(),
                    worker_id,
                    registry,
                )
                .await
            }
            None => {
                RunnerFactoryWithPool::new_for_worker_id(
                    runner_data,
                    worker_data,
                    self.runner_factory.clone(),
                    self.worker_config.clone(),
                    worker_id,
                )
                .await
            }
        }
    }

    pub async fn add_and_get_runner(
        &self,
        runner_data: Arc<RunnerData>,
        worker_id: &WorkerId,
        worker_data: Arc<WorkerData>,
    ) -> Result<Option<Object<RunnerPoolManagerImpl>>> {
        if !worker_data.use_static {
            tracing::warn!(
                "add_and_get_runner: worker_id:{} not static",
                worker_id.value
            );
            return Ok(None);
        }
        let checkout = self.checkout_guard(&runner_data).await?;
        let mut pools = self.pools.write().await;
        if let Some(existing) = pools.get(&worker_id.value).cloned() {
            drop(pools);
            drop(checkout);
            return self
                .await_static_checkout(&existing, &runner_data, None)
                .await
                .map(Some);
        }
        tracing::debug!(
            "add_and_get_runner: {}: {}",
            worker_id.value,
            &worker_data.name
        );
        let p = self
            .create_pool_for_worker_id(runner_data.clone(), worker_data, *worker_id)
            .await?;
        let pool_clone = p.clone();
        pools.insert(worker_id.value, p);
        drop(pools);
        drop(checkout);
        self.await_static_checkout(&pool_clone, &runner_data, None)
            .await
            .map(Some)
    }

    pub async fn clear(&self) {
        self.pools.write().await.clear()
    }

    pub async fn delete_runner(&self, id: &WorkerId) {
        self.pools.write().await.remove(&id.value);
    }

    // create by factory every time
    pub async fn get_non_static_runner(
        &self,
        runner_data: &RunnerData,
        worker_data: &WorkerData,
        worker_id: &WorkerId,
    ) -> Result<Box<dyn CancellableRunner + Send + Sync>> {
        let _checkout = self.checkout_guard(runner_data).await?;
        let mut r = self
            .runner_factory
            .create_by_name(&runner_data.name, worker_data.use_static)
            .await
            .ok_or(JobWorkerError::NotFound(format!(
                "runner not found: {}",
                runner_data.name
            )))?;
        super::pool::set_sandbox_worker_context(&mut r, worker_id, worker_data.use_static)?;
        super::pool::set_sandbox_cleanup_registry(&mut r, self.sandbox_cleanup_registry.as_ref())?;
        r.load(worker_data.runner_settings.clone()).await?;
        Ok(r)
    }

    pub async fn get_or_create_static_runner(
        &self,
        runner_data: &RunnerData,
        worker_id: &WorkerId,
        worker_data: &WorkerData,
        timeout: Option<Duration>,
    ) -> Result<Option<Object<RunnerPoolManagerImpl>>> {
        if !worker_data.use_static {
            return Ok(None);
        }
        let checkout = self.checkout_guard(runner_data).await?;

        let timeouts = if let Some(to) = timeout {
            Timeouts::wait_millis(to.as_millis() as u64)
        } else {
            Timeouts::default()
        };

        // Acquire write lock to prevent TOCTOU race on pool creation.
        // Pool creation (RunnerFactoryWithPool::new) is lightweight (no runner instantiation),
        // so lock contention is negligible.
        let mut pools = self.pools.write().await;
        let pool = if let Some(p) = pools.get(&worker_id.value).cloned() {
            p
        } else {
            tracing::debug!(
                "get_or_create_static_runner: creating pool for {}: {}",
                worker_id.value,
                &worker_data.name
            );
            let p = self
                .create_pool_for_worker_id(
                    Arc::new(runner_data.clone()),
                    Arc::new(worker_data.clone()),
                    *worker_id,
                )
                .await?;
            let pool_clone = p.clone();
            pools.insert(worker_id.value, p);
            pool_clone
        };
        drop(pools);
        drop(checkout);
        self.await_static_checkout(&pool, runner_data, Some(&timeouts))
            .await
            .inspect_err(|e| tracing::error!("error in timeout_get: {:?}", e))
            .map(Some)
    }
}

async fn wait_for_shutdown(mut shutdown: watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow() || shutdown.changed().await.is_err() {
            return;
        }
    }
}

fn is_sandbox_runner(runner_data: &RunnerData) -> bool {
    runner_data.name == RunnerType::Sandbox.as_str_name()
}

pub trait UseRunnerPoolMap: Send + Sync {
    fn runner_pool_map(&self) -> &RunnerFactoryWithPoolMap;
}
