use super::factory::RunnerFactoryImpl;
use super::Runner;
use crate::plugins::Plugins;
use crate::worker::runner::factory::RunnerFactory;
use anyhow::{anyhow, Result};
use app::app::WorkerConfig;
use deadpool::managed::Timeouts;
use deadpool::{
    managed::{Manager, Metrics, Object, Pool, PoolConfig, RecycleResult},
    Runtime,
};
use infra::error::JobWorkerError;
use proto::jobworkerp::data::WorkerData;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing;

#[derive(Debug)]
pub struct RunnerPoolManagerImpl {
    worker: Arc<WorkerData>,
    runner_factory: RunnerFactoryImpl,
}

impl RunnerPoolManagerImpl {
    pub async fn new(worker: Arc<WorkerData>, plugins: Arc<Plugins>) -> Self {
        Self {
            worker,
            runner_factory: RunnerFactoryImpl::new(plugins),
        }
    }
}

impl Manager for RunnerPoolManagerImpl {
    type Type = Arc<Mutex<Box<dyn Runner + Send + Sync>>>;
    type Error = anyhow::Error;

    async fn create(&self) -> Result<Arc<Mutex<Box<dyn Runner + Send + Sync>>>, anyhow::Error> {
        let runner = self.runner_factory.create(&self.worker).await?;
        tracing::debug!("runner created in pool: {}", runner.name().await);
        Ok(Arc::new(Mutex::new(runner)))
    }

    fn detach(&self, _runner: &mut Arc<Mutex<Box<dyn Runner + Send + Sync>>>) {
        tracing::warn!(
            "Static Runner detached from pool: maybe re-init: {:?}",
            &self
        );
        // if need canceling in detach, cancel? (if independent from pool, neednot cancel)
        // (but not sure this is good idea)
        //match tokio::runtime::Runtime::new() { // bad idea: create inner tokio
        //    Ok(rt) => rt.block_on(async { runner.lock().await.cancel().await }), // maybe panic occurred
        //    Err(e) => {
        //        tracing::error!("error detach of runner pool in tokio runtime: {:?}", e);
        //    }
        //}
    }

    async fn recycle(
        &self,
        runner: &mut Arc<Mutex<Box<dyn Runner + Send + Sync>>>,
        _metrics: &Metrics,
    ) -> RecycleResult<Self::Error> {
        tracing::debug!("runner recycled");
        let mut r = runner.lock().await;
        r.cancel().await;
        Ok(())
    }
}

#[derive(Clone)]
pub struct RunnerFactoryWithPool {
    pool: Pool<RunnerPoolManagerImpl, Object<RunnerPoolManagerImpl>>,
}
impl RunnerFactoryWithPool {
    pub async fn new(
        worker: Arc<WorkerData>,
        plugins: Arc<Plugins>,
        worker_config: Arc<WorkerConfig>,
    ) -> Result<Self> {
        if !worker.use_static {
            return Err(JobWorkerError::InvalidParameter(format!(
                "worker must be static for runner pool: {:?}",
                &worker
            ))
            .into());
        }
        let manager = RunnerPoolManagerImpl::new(worker.clone(), plugins.clone()).await;
        let max_size = if let Some(c) = worker_config.get_concurrency(worker.channel.as_ref()) {
            Ok(c)
        } else {
            // must not be reached (run in not assigned channel, maybe bug? report to developer)
            Err(anyhow!(
                "this channel {:?} is not configured in this worker: {:?}",
                &worker.channel,
                &worker
            ))
        }?;
        let config = PoolConfig::new(max_size as usize);
        Ok(RunnerFactoryWithPool {
            pool: Pool::builder(manager)
                .config(config)
                .runtime(Runtime::Tokio1)
                .build()
                .unwrap(),
        })
    }
    /// get runner from pool (delegate to pool)
    pub async fn get(&self) -> Result<Object<RunnerPoolManagerImpl>> {
        self.pool
            .get()
            .await
            .map_err(|e| anyhow!("Error in getting runner from pool: {:?}", e))
    }
    /// get runner from pool (delegate to pool)
    pub async fn timeout_get(&self, timeouts: &Timeouts) -> Result<Object<RunnerPoolManagerImpl>> {
        self.pool
            .timeout_get(timeouts)
            .await
            .map_err(|e| anyhow!("Error in getting runner from pool: {:?}", e))
    }
}

// create test for RunnerFactoryWithPool
#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use app::app::worker::builtin::slack::{SLACK_RUNNER_OPERATION, SLACK_WORKER_NAME};
    use proto::jobworkerp::data::{RunnerType, WorkerData};

    #[tokio::test]
    async fn test_runner_pool() -> Result<()> {
        // dotenvy::dotenv()?;
        std::env::set_var("PLUGINS_RUNNER_DIR", "target/debug/");
        let mut plugins = Plugins::new();
        plugins.load_plugins_from_env()?;
        let factory = RunnerFactoryWithPool::new(
            Arc::new(WorkerData {
                r#type: RunnerType::SlackInternal as i32,
                operation: Some(SLACK_RUNNER_OPERATION.clone()),
                channel: None,
                use_static: true,
                ..Default::default()
            }),
            Arc::new(plugins),
            // default worker_config concurrency: 1
            Arc::new(WorkerConfig {
                default_concurrency: 1, // => runner pool size 1
                ..WorkerConfig::default()
            }),
        )
        .await?;
        let runner = factory.get().await?;
        let name = runner.lock().await.name().await;
        assert_eq!(name, SLACK_WORKER_NAME);
        let res = factory.timeout_get(&Timeouts::wait_millis(1000)).await;
        // timeout
        assert!(res.is_err());
        // release runner
        drop(runner);
        assert!(factory
            .timeout_get(&Timeouts::wait_millis(1000))
            .await
            .is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn test_runner_pool_non_static_err() -> Result<()> {
        std::env::set_var("PLUGINS_RUNNER_DIR", "target/debug/");
        // dotenvy::dotenv()?;
        let mut plugins = Plugins::new();
        plugins.load_plugins_from_env()?;
        assert!(RunnerFactoryWithPool::new(
            Arc::new(WorkerData {
                r#type: RunnerType::SlackInternal as i32,
                operation: Some(SLACK_RUNNER_OPERATION.clone()),
                channel: None,
                use_static: false,
                ..Default::default()
            }),
            Arc::new(plugins),
            Arc::new(WorkerConfig {
                default_concurrency: 1, // => runner pool size 1
                ..WorkerConfig::default()
            }),
        )
        .await
        .is_err());
        Ok(())
    }
}
