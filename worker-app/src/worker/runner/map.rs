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
        // creates static runner pool
        if worker_data.use_static {
            tracing::debug!(
                "add_and_get_runner: {}: {}",
                worker_id.value,
                &worker_data.name
            );
            // XXX shold clone runner?
            let p = RunnerFactoryWithPool::new(
                runner_data,
                worker_data,
                self.runner_factory.clone(),
                self.worker_config.clone(),
            )
            .await?;
            let runner_impl = p.get().await?;
            self.pools.write().await.insert(worker_id.value, p);
            Ok(Some(runner_impl))
        } else {
            tracing::warn!(
                "add_and_get_runner: worker_id:{} not static",
                worker_id.value
            );
            // no op for non-static
            Ok(None)
        }
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
        if worker_data.use_static {
            let mp = self.pools.read().await;
            if let Some(p) = mp.get(&worker_id.value).cloned() {
                let timeouts = if let Some(to) = timeout {
                    Timeouts::wait_millis(to.as_millis() as u64)
                } else {
                    Timeouts::default()
                };
                p.timeout_get(&timeouts)
                    .await
                    .inspect_err(|e| tracing::error!("error in timeout_get: {:?}", e))
                    .map(Some)
            } else {
                // release read guard
                drop(mp);
                // add created runner pool to map
                self.add_and_get_runner(
                    Arc::new(runner_data.clone()),
                    worker_id,
                    Arc::new(worker_data.clone()),
                )
                .await
            }
        } else {
            Ok(None)
        }
    }
}

pub trait UseRunnerPoolMap: Send + Sync {
    fn runner_pool_map(&self) -> &RunnerFactoryWithPoolMap;
}
