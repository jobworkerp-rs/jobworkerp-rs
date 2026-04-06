use super::pool::{RunnerFactoryWithPool, RunnerPoolManagerImpl};
use anyhow::Result;
use app::app::WorkerConfig;
use app_wrapper::runner::RunnerFactory;
use deadpool::managed::{Object, Timeouts};
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::runner::cancellation::CancellableRunner;
use proto::jobworkerp::data::{RunnerData, WorkerData, WorkerId};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

pub struct RunnerFactoryWithPoolMap {
    // TODO not implement as map? or keep as static ?
    pub pools: Arc<RwLock<HashMap<i64, RunnerFactoryWithPool>>>,
    runner_factory: Arc<RunnerFactory>,
    worker_config: Arc<WorkerConfig>,
}

impl RunnerFactoryWithPoolMap {
    pub fn new(runner_factory: Arc<RunnerFactory>, worker_config: Arc<WorkerConfig>) -> Self {
        Self {
            pools: Arc::new(RwLock::new(HashMap::<i64, RunnerFactoryWithPool>::new())),
            runner_factory,
            worker_config,
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
        let mut pools = self.pools.write().await;
        if let Some(existing) = pools.get(&worker_id.value).cloned() {
            drop(pools);
            return existing.get().await.map(Some);
        }
        tracing::debug!(
            "add_and_get_runner: {}: {}",
            worker_id.value,
            &worker_data.name
        );
        let p = RunnerFactoryWithPool::new(
            runner_data,
            worker_data,
            self.runner_factory.clone(),
            self.worker_config.clone(),
        )
        .await?;
        let pool_clone = p.clone();
        pools.insert(worker_id.value, p);
        drop(pools);
        pool_clone.get().await.map(Some)
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
    ) -> Result<Box<dyn CancellableRunner + Send + Sync>> {
        let mut r = self
            .runner_factory
            .create_by_name(&runner_data.name, worker_data.use_static)
            .await
            .ok_or(JobWorkerError::NotFound(format!(
                "runner not found: {}",
                runner_data.name
            )))?;
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

        let timeouts = if let Some(to) = timeout {
            Timeouts::wait_millis(to.as_millis() as u64)
        } else {
            Timeouts::default()
        };

        // Acquire write lock to prevent TOCTOU race on pool creation.
        // Pool creation (RunnerFactoryWithPool::new) is lightweight (no runner instantiation),
        // so lock contention is negligible.
        let mut pools = self.pools.write().await;
        if let Some(p) = pools.get(&worker_id.value).cloned() {
            drop(pools);
            p.timeout_get(&timeouts)
                .await
                .inspect_err(|e| tracing::error!("error in timeout_get: {:?}", e))
                .map(Some)
        } else {
            tracing::debug!(
                "get_or_create_static_runner: creating pool for {}: {}",
                worker_id.value,
                &worker_data.name
            );
            let p = RunnerFactoryWithPool::new(
                Arc::new(runner_data.clone()),
                Arc::new(worker_data.clone()),
                self.runner_factory.clone(),
                self.worker_config.clone(),
            )
            .await?;
            let pool_clone = p.clone();
            pools.insert(worker_id.value, p);
            drop(pools);
            pool_clone
                .timeout_get(&timeouts)
                .await
                .inspect_err(|e| tracing::error!("error in timeout_get: {:?}", e))
                .map(Some)
        }
    }
}

pub trait UseRunnerPoolMap: Send + Sync {
    fn runner_pool_map(&self) -> &RunnerFactoryWithPoolMap;
}
