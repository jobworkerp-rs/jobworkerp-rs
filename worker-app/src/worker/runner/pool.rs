use anyhow::{anyhow, Result};
use app::app::WorkerConfig;
use app_wrapper::runner::RunnerFactory;
use deadpool::managed::Timeouts;
use deadpool::{
    managed::{Manager, Metrics, Object, Pool, PoolConfig, RecycleResult},
    Runtime,
};
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::runner::RunnerTrait;
use proto::jobworkerp::data::{RunnerData, WorkerData};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing;

#[derive(Debug)]
pub struct RunnerPoolManagerImpl {
    runner_data: Arc<RunnerData>,
    worker: Arc<WorkerData>,
    runner_factory: Arc<RunnerFactory>,
}

impl RunnerPoolManagerImpl {
    pub async fn new(
        runner_data: Arc<RunnerData>,
        worker: Arc<WorkerData>,
        runner_factory: Arc<RunnerFactory>,
    ) -> Self {
        Self {
            runner_data,
            worker,
            runner_factory,
        }
    }
}

impl Manager for RunnerPoolManagerImpl {
    type Type = Arc<Mutex<Box<dyn RunnerTrait + Send + Sync>>>;
    type Error = anyhow::Error;

    async fn create(
        &self,
    ) -> Result<Arc<Mutex<Box<dyn RunnerTrait + Send + Sync>>>, anyhow::Error> {
        let mut runner = self
            .runner_factory
            .create_by_name(&self.runner_data.name, self.worker.use_static)
            .await
            .ok_or(JobWorkerError::InvalidParameter(format!(
                "runner not found: {:?}",
                &self.runner_data.name
            )))?;
        runner.load(self.worker.runner_settings.clone()).await?;
        tracing::debug!("runner created in pool: {}", runner.name());
        Ok(Arc::new(Mutex::new(runner)))
    }

    fn detach(&self, _runner: &mut Arc<Mutex<Box<dyn RunnerTrait + Send + Sync>>>) {
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
        runner: &mut Arc<Mutex<Box<dyn RunnerTrait + Send + Sync>>>,
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
        runner_data: Arc<RunnerData>,
        worker: Arc<WorkerData>,
        runner_factory: Arc<RunnerFactory>,
        worker_config: Arc<WorkerConfig>,
    ) -> Result<Self> {
        if !worker.use_static {
            return Err(JobWorkerError::InvalidParameter(format!(
                "worker must be static for runner pool: {:?}",
                &worker
            ))
            .into());
        }
        let manager =
            RunnerPoolManagerImpl::new(runner_data.clone(), worker.clone(), runner_factory.clone())
                .await;
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
    use infra::infra::job::rows::{JobqueueAndCodec, UseJobqueueAndCodec};
    use jobworkerp_runner::jobworkerp::runner::CommandRunnerSettings;
    use proto::jobworkerp::data::{RunnerType, WorkerData};

    #[tokio::test]
    async fn test_runner_pool() -> Result<()> {
        // dotenvy::dotenv()?;
        std::env::set_var("PLUGINS_RUNNER_DIR", "../target/debug/");

        let app_module = Arc::new(app::module::test::create_hybrid_test_app().await?);
        let runner_factory = RunnerFactory::new(app_module.clone());
        runner_factory.load_plugins().await;
        let ope = CommandRunnerSettings {
            name: "ls".to_string(),
        };
        let factory = RunnerFactoryWithPool::new(
            Arc::new(RunnerData {
                name: RunnerType::Command.as_str_name().to_string(),
                ..Default::default()
            }),
            Arc::new(WorkerData {
                runner_settings: JobqueueAndCodec::serialize_message(&ope),
                channel: None,
                use_static: true,
                ..Default::default()
            }),
            Arc::new(runner_factory),
            // default worker_config concurrency: 1
            Arc::new(WorkerConfig {
                default_concurrency: 1, // => runner pool size 1
                ..WorkerConfig::default()
            }),
        )
        .await?;
        let runner = factory.get().await?;
        let name = runner.lock().await.name();
        assert_eq!(name, RunnerType::Command.as_str_name());
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
        std::env::set_var("PLUGINS_RUNNER_DIR", "../target/debug/");
        // dotenvy::dotenv()?;
        let app_module = Arc::new(app::module::test::create_hybrid_test_app().await.unwrap());
        let runner_factory = RunnerFactory::new(app_module);
        runner_factory.load_plugins().await;
        let ope = CommandRunnerSettings {
            name: "ls".to_string(),
        };
        assert!(RunnerFactoryWithPool::new(
            Arc::new(RunnerData {
                name: RunnerType::Command.as_str_name().to_string(),
                ..Default::default()
            }),
            Arc::new(WorkerData {
                runner_settings: JobqueueAndCodec::serialize_message(&ope),
                channel: None,
                use_static: false,
                ..Default::default()
            }),
            Arc::new(runner_factory),
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
