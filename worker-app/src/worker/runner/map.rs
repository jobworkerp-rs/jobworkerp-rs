use anyhow::Result;
use app::app::WorkerConfig;
use command_utils::util::result::TapErr;
use deadpool::managed::{Object, Timeouts};
use proto::jobworkerp::data::{WorkerData, WorkerId};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use crate::plugins::Plugins;

use super::factory::{RunnerFactory, RunnerFactoryImpl};
use super::pool::{RunnerFactoryWithPool, RunnerPoolManagerImpl};
use super::Runner;

pub struct RunnerFactoryWithPoolMap {
    // TODO not implement as map? or keep as static ?
    pub pools: Arc<RwLock<HashMap<i64, RunnerFactoryWithPool>>>,
    pub factory: Arc<RunnerFactoryImpl>,
    plugins: Arc<Plugins>,
    worker_config: Arc<WorkerConfig>,
}

impl RunnerFactoryWithPoolMap {
    pub fn new(plugins: Arc<Plugins>, worker_config: Arc<WorkerConfig>) -> Self {
        Self {
            pools: Arc::new(RwLock::new(HashMap::<i64, RunnerFactoryWithPool>::new())),
            factory: Arc::new(RunnerFactoryImpl::new(plugins.clone())),
            plugins,
            worker_config,
        }
    }

    pub async fn add_and_get_runner(
        &self,
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
                worker_data,
                self.plugins.clone(),
                self.worker_config.clone(),
            )
            .await?;
            let runner = p.get().await?;
            self.pools.write().await.insert(worker_id.value, p);
            Ok(Some(runner))
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
        worker_data: &WorkerData,
    ) -> Result<Box<dyn Runner + Send + Sync>> {
        self.factory.create(worker_data).await
    }

    pub async fn get_or_create_static_runner(
        &self,
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
                    .tap_err(|e| tracing::error!("error in timeout_get: {:?}", e))
                    .map(Some)
            } else {
                // release read guard
                drop(mp);
                // add created runner pool to map
                self.add_and_get_runner(worker_id, Arc::new(worker_data.clone()))
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
