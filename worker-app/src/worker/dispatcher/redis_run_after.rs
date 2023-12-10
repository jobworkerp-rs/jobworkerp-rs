use anyhow::Result;
use app::app::job::JobApp;
use app::app::job::UseJobApp;
use app::module::AppModule;
use async_trait::async_trait;
use common::util::datetime;
use common::util::shutdown::ShutdownLock;
use infra::infra::JobQueueConfig;
use infra::infra::UseJobQueueConfig;
use std::sync::Arc;
use std::time::Duration;
use tracing;

// for redis run_after queue
// pop job from redis queue by blpop and execute, send result to redis
// TODO NOW TESTING (not valid)
#[async_trait]
pub trait RedisRunAfterJobDispatcher: UseJobApp + UseJobQueueConfig {
    fn execute(&'static self, lock: ShutdownLock) -> Result<()>
    where
        Self: Send + Sync + 'static,
    {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(
                self.job_queue_config().fetch_interval as u64,
            ));
            loop {
                // using tokio::select and tokio::signal::ctrl_c, break loop by ctrl-c
                tokio::select! {
                    _ = interval.tick() => {
                        tracing::debug!("execute pop and enqueue run_after job");
                        let _ = self.pop_and_enqueue().await.map_err(|e| {
                            tracing::error!("failed to pop and enqueue: {:?}", e);
                            e
                        });
                    }
                    _ = tokio::signal::ctrl_c() => {
                        tracing::info!("break execute pop and enqueue run_after job");
                        lock.unlock();
                        break;
                    }
                }
            }
        });
        tracing::info!("end execute pop and enqueue run_after job");
        Ok(())
    }

    // pop jobs using pop_run_after_jobs_to_run(), and enqueue them to redis for execute
    async fn pop_and_enqueue(&self) -> Result<()> {
        tracing::debug!("run pop_and_enqueue: time:{}", datetime::now().to_rfc3339());
        let jobs = self.job_app().pop_run_after_jobs_to_run().await?;
        for job in jobs {
            // enqueue
            self.job_app().update_job(&job).await?;
        }
        Ok(())
    }
}

pub struct RedisRunAfterJobDispatcherImpl {
    job_queue_config: Arc<JobQueueConfig>,
    app_module: Arc<AppModule>,
}
impl RedisRunAfterJobDispatcherImpl {
    pub fn new(job_queue_config: Arc<JobQueueConfig>, app_module: Arc<AppModule>) -> Self {
        Self {
            job_queue_config,
            app_module,
        }
    }
}
impl UseJobApp for RedisRunAfterJobDispatcherImpl {
    fn job_app(&self) -> &Arc<dyn JobApp + 'static> {
        &self.app_module.job_app
    }
}
impl UseJobQueueConfig for RedisRunAfterJobDispatcherImpl {
    fn job_queue_config(&self) -> &JobQueueConfig {
        &self.job_queue_config
    }
}

impl RedisRunAfterJobDispatcher for RedisRunAfterJobDispatcherImpl {}

pub trait UseRedisRunAfterJobDispatcher {
    fn redis_run_after_job_dispatcher(&self) -> Option<&RedisRunAfterJobDispatcherImpl>;
}
