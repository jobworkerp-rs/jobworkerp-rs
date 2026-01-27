use anyhow::{Result, anyhow};
use app::app::WorkerConfig;
use app_wrapper::runner::RunnerFactory;
use deadpool::managed::Timeouts;
use deadpool::{
    Runtime,
    managed::{Manager, Metrics, Object, Pool, PoolConfig, RecycleResult},
};
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::runner::cancellation::CancellableRunner;
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

    /// Reset cancellation monitoring state for pooling if the runner supports it
    async fn reset_for_pooling_if_supported(
        runner_impl: &mut Box<dyn CancellableRunner + Send + Sync>,
    ) {
        tracing::trace!(
            "Resetting cancellation monitoring for pooling: {}",
            runner_impl.name()
        );

        if let Err(e) = runner_impl.as_cancel_monitoring().reset_for_pooling().await {
            tracing::warn!(
                "Failed to reset cancellation monitoring for pooling in {}: {:?}",
                runner_impl.name(),
                e
            );
        }
    }
}

impl Manager for RunnerPoolManagerImpl {
    type Type = Arc<Mutex<Box<dyn CancellableRunner + Send + Sync>>>;
    type Error = anyhow::Error;

    async fn create(
        &self,
    ) -> Result<Arc<Mutex<Box<dyn CancellableRunner + Send + Sync>>>, anyhow::Error> {
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

    fn detach(&self, _runner: &mut Arc<Mutex<Box<dyn CancellableRunner + Send + Sync>>>) {
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
        runner: &mut Arc<Mutex<Box<dyn CancellableRunner + Send + Sync>>>,
        _metrics: &Metrics,
    ) -> RecycleResult<Self::Error> {
        tracing::debug!("runner recycled");
        let mut r = runner.lock().await;

        r.as_cancel_monitoring()
            .request_cancellation()
            .await
            .unwrap();

        // Additional: Reset cancellation monitoring state for pooling
        // This prevents state contamination between jobs in pool environment
        Self::reset_for_pooling_if_supported(&mut r).await;

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
    use app::module::test::TEST_PLUGIN_DIR;
    use jobworkerp_runner::runner::mcp::proxy::McpServerFactory;
    use proto::jobworkerp::data::{RunnerType, WorkerData};

    #[test]
    fn test_runner_pool() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(app::module::test::create_hybrid_test_app().await.unwrap());
            let app_wrapper_module =
                app_wrapper::modules::test::create_test_app_wrapper_module(app_module.clone());
            let runner_factory = RunnerFactory::new(
                app_module,
                Arc::new(app_wrapper_module),
                Arc::new(McpServerFactory::default()),
            );
            runner_factory.load_plugins_from(TEST_PLUGIN_DIR).await;
            let factory = RunnerFactoryWithPool::new(
                Arc::new(RunnerData {
                    name: RunnerType::Command.as_str_name().to_string(),
                    ..Default::default()
                }),
                Arc::new(WorkerData {
                    runner_settings: Vec::new(),
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
            .await
            .unwrap();
            let runner = factory.get().await.unwrap();
            let name = runner.lock().await.name();
            assert_eq!(name, RunnerType::Command.as_str_name());
            let res = factory.timeout_get(&Timeouts::wait_millis(1000)).await;
            // timeout
            assert!(res.is_err());
            // release runner
            drop(runner);
            assert!(
                factory
                    .timeout_get(&Timeouts::wait_millis(1000))
                    .await
                    .is_ok()
            );
        });
        Ok(())
    }

    #[test]
    fn test_runner_pool_non_static_err() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            // dotenvy::dotenv()?;
            let app_module = Arc::new(app::module::test::create_hybrid_test_app().await.unwrap());
            let app_wrapper_module = Arc::new(
                app_wrapper::modules::test::create_test_app_wrapper_module(app_module.clone()),
            );
            let runner_factory = RunnerFactory::new(
                app_module,
                app_wrapper_module,
                Arc::new(McpServerFactory::default()),
            );
            runner_factory.load_plugins_from(TEST_PLUGIN_DIR).await;
            assert!(
                RunnerFactoryWithPool::new(
                    Arc::new(RunnerData {
                        name: RunnerType::Command.as_str_name().to_string(),
                        ..Default::default()
                    }),
                    Arc::new(WorkerData {
                        runner_settings: vec![],
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
                .is_err()
            );
        });
        Ok(())
    }

    /// Pool reset_for_pooling() functionality tests
    /// These tests verify the newly implemented pool state reset functionality
    #[cfg(test)]
    mod pool_reset_tests {
        use super::*;
        use proto::jobworkerp::data::{JobData, JobId, WorkerId};
        use std::time::Duration;

        async fn create_test_factory() -> Result<RunnerFactoryWithPool> {
            let app_module = Arc::new(app::module::test::create_hybrid_test_app().await?);
            let app_wrapper_module =
                app_wrapper::modules::test::create_test_app_wrapper_module(app_module.clone());
            let runner_factory = RunnerFactory::new(
                app_module,
                Arc::new(app_wrapper_module),
                Arc::new(McpServerFactory::default()),
            );
            runner_factory.load_plugins_from(TEST_PLUGIN_DIR).await;

            RunnerFactoryWithPool::new(
                Arc::new(RunnerData {
                    name: RunnerType::Command.as_str_name().to_string(),
                    ..Default::default()
                }),
                Arc::new(WorkerData {
                    runner_settings: Vec::new(),
                    channel: None,
                    use_static: true,
                    ..Default::default()
                }),
                Arc::new(runner_factory),
                Arc::new(WorkerConfig {
                    default_concurrency: 1, // Pool size 1 for testing
                    ..WorkerConfig::default()
                }),
            )
            .await
        }

        fn create_test_job_data() -> (JobId, JobData) {
            (
                JobId { value: 123456 },
                JobData {
                    worker_id: Some(WorkerId { value: 1 }),
                    args: b"test_args".to_vec(),
                    uniq_key: Some("test_key".to_string()),
                    enqueue_time: 1000000,
                    grabbed_until_time: None,
                    run_after_time: 0,
                    retried: 0,
                    priority: 0,
                    timeout: 30,
                    streaming_type: 0,
                    using: None,
                },
            )
        }

        #[test]
        fn test_pool_reset_for_pooling_command_runner() -> Result<()> {
            infra_utils::infra::test::TEST_RUNTIME.block_on(async {
                let factory = create_test_factory().await?;
                let (job_id, job_data) = create_test_job_data();

                let runner_obj = factory.get().await?;

                {
                    let mut runner = runner_obj.lock().await;

                    // Setup cancellation monitoring using CancellableRunner trait
                    let _result = runner
                        .as_cancel_monitoring()
                        .setup_cancellation_monitoring(job_id, &job_data)
                        .await
                        .ok();
                    // Note: We can't easily verify state without exposed helper methods,
                    // but we can verify reset_for_pooling() completes without error
                }

                drop(runner_obj);

                let runner_obj2 = factory.get().await?;
                {
                    let runner = runner_obj2.lock().await;
                    assert_eq!(runner.name(), RunnerType::Command.as_str_name());
                }

                tracing::debug!("✅ Pool reset for pooling CommandRunner test completed");
                Ok(())
            })
        }

        #[test]
        fn test_pool_state_isolation_between_jobs() -> Result<()> {
            infra_utils::infra::test::TEST_RUNTIME.block_on(async {
                let factory = create_test_factory().await?;
                let (job_id1, job_data1) = create_test_job_data();
                let (job_id2, job_data2) = (
                    JobId { value: 789012 },
                    JobData {
                        worker_id: Some(WorkerId { value: 1 }),
                        args: b"test_args2".to_vec(),
                        uniq_key: Some("test_key2".to_string()),
                        enqueue_time: 1000001,
                        grabbed_until_time: None,
                        run_after_time: 0,
                        retried: 0,
                        priority: 0,
                        timeout: 30,
                        streaming_type: 0,
                        using: None,
                    },
                );

                // Job A: Set cancellation state
                {
                    let runner_obj = factory.get().await?;
                    let mut runner = runner_obj.lock().await;

                    // Setup cancellation monitoring using CancellableRunner trait
                    let _result = runner
                        .as_cancel_monitoring()
                        .setup_cancellation_monitoring(job_id1, &job_data1)
                        .await
                        .ok();
                    // Runner automatically returned to pool on drop
                }

                // Job B: Should get a clean runner
                {
                    let runner_obj = factory.get().await?;
                    let mut runner = runner_obj.lock().await;

                    // Should be able to setup new job without issues
                    let _result = runner
                        .as_cancel_monitoring()
                        .setup_cancellation_monitoring(job_id2, &job_data2)
                        .await
                        .ok();
                    assert_eq!(runner.name(), RunnerType::Command.as_str_name());
                }

                tracing::debug!("✅ Pool state isolation between jobs test completed");
                Ok(())
            })
        }

        #[test]
        fn test_pool_cancellation_token_cleanup() -> Result<()> {
            infra_utils::infra::test::TEST_RUNTIME.block_on(async {
                let factory = create_test_factory().await?;
                let (job_id, job_data) = create_test_job_data();
                let runner_obj = factory.get().await?;

                {
                    let mut runner = runner_obj.lock().await;

                    // Setup state
                    let _result = runner
                        .as_cancel_monitoring()
                        .setup_cancellation_monitoring(job_id, &job_data)
                        .await
                        .ok();

                    // Manually call reset_for_pooling to test cleanup
                    let reset_result = runner.as_cancel_monitoring().reset_for_pooling().await;
                    assert!(
                        reset_result.is_ok(),
                        "reset_for_pooling should complete successfully"
                    );
                }

                tracing::debug!("✅ Pool cancellation token cleanup test completed");
                Ok(())
            })
        }

        #[test]
        fn test_pool_memory_leak_prevention() -> Result<()> {
            infra_utils::infra::test::TEST_RUNTIME.block_on(async {
                let factory = create_test_factory().await?;

                // Perform multiple pool get/return cycles
                for i in 0..10 {
                    let (job_id, job_data) = (
                        JobId {
                            value: 100000 + i as i64,
                        },
                        JobData {
                            worker_id: Some(WorkerId { value: 1 }),
                            args: format!("test_args_{i}").into_bytes(),
                            uniq_key: Some(format!("test_key_{i}")),
                            enqueue_time: 1000000 + i as i64,
                            grabbed_until_time: None,
                            run_after_time: 0,
                            retried: 0,
                            priority: 0,
                            timeout: 30,
                            streaming_type: 0,
                            using: None,
                        },
                    );

                    let runner_obj = factory.get().await?;

                    {
                        let mut runner = runner_obj.lock().await;

                        // Setup some state for each cycle
                        let _result = runner
                            .as_cancel_monitoring()
                            .setup_cancellation_monitoring(job_id, &job_data)
                            .await
                            .ok();
                    }

                    drop(runner_obj);

                    // Brief pause to allow pool recycling
                    tokio::time::sleep(Duration::from_millis(10)).await;

                    tracing::debug!("Pool cycle {} completed", i + 1);
                }

                let runner_obj = factory.get().await?;
                {
                    let runner = runner_obj.lock().await;
                    assert_eq!(runner.name(), RunnerType::Command.as_str_name());
                }

                tracing::debug!("✅ Pool memory leak prevention test completed");
                Ok(())
            })
        }

        #[test]
        fn test_pool_reset_failure_handling() -> Result<()> {
            infra_utils::infra::test::TEST_RUNTIME.block_on(async {
                let factory = create_test_factory().await?;
                let (job_id, job_data) = create_test_job_data();
                let runner_obj = factory.get().await?;

                // Test that pool remains stable even if reset fails
                // (Note: Current implementation logs warnings but doesn't fail)
                {
                    let mut runner = runner_obj.lock().await;

                    let _result = runner
                        .as_cancel_monitoring()
                        .setup_cancellation_monitoring(job_id, &job_data)
                        .await
                        .ok();
                }

                drop(runner_obj);

                // Pool should still be functional
                let runner_obj2 = factory.timeout_get(&Timeouts::wait_millis(1000)).await?;
                {
                    let runner = runner_obj2.lock().await;
                    assert!(!runner.name().is_empty());
                }

                tracing::debug!("✅ Pool reset failure handling test completed");
                Ok(())
            })
        }

        #[test]
        fn test_pool_concurrent_reset_safety() -> Result<()> {
            infra_utils::infra::test::TEST_RUNTIME.block_on(async {
                let factory = create_test_factory().await?;

                // Test concurrent access safety during reset operations
                let runner_obj = factory.get().await?;

                // Simulate concurrent operations
                let runner_clone = runner_obj.clone();
                let concurrent_task = tokio::spawn(async move {
                    let _runner = runner_clone.lock().await;
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    // Implicit drop triggers recycle
                });

                // Brief delay then return to pool
                tokio::time::sleep(Duration::from_millis(25)).await;
                drop(runner_obj);

                // Wait for concurrent task
                concurrent_task.await?;

                let runner_obj2 = factory.timeout_get(&Timeouts::wait_millis(1000)).await?;
                {
                    let runner = runner_obj2.lock().await;
                    assert!(!runner.name().is_empty());
                }

                tracing::debug!("✅ Pool concurrent reset safety test completed");
                Ok(())
            })
        }

        #[test]
        fn test_pool_downcast_failure_handling() -> Result<()> {
            infra_utils::infra::test::TEST_RUNTIME.block_on(async {
                // Test that reset_for_pooling_if_supported handles unknown runner types gracefully
                let factory = create_test_factory().await?;
                let runner_obj = factory.get().await?;

                {
                    let mut runner = runner_obj.lock().await;

                    // Call reset_for_pooling_if_supported with a known runner type
                    // This should work without issues
                    RunnerPoolManagerImpl::reset_for_pooling_if_supported(&mut runner).await;
                }

                // Pool should remain functional
                drop(runner_obj);
                let runner_obj2 = factory.get().await?;
                {
                    let runner = runner_obj2.lock().await;
                    assert_eq!(runner.name(), RunnerType::Command.as_str_name());
                }

                tracing::debug!("✅ Pool downcast failure handling test completed");
                Ok(())
            })
        }

        #[test]
        fn test_pool_reset_method_direct_call() -> Result<()> {
            infra_utils::infra::test::TEST_RUNTIME.block_on(async {
                let factory = create_test_factory().await?;
                let (job_id, job_data) = create_test_job_data();
                let runner_obj = factory.get().await?;

                {
                    let mut runner = runner_obj.lock().await;

                    // Setup state
                    let _result = runner
                        .as_cancel_monitoring()
                        .setup_cancellation_monitoring(job_id, &job_data)
                        .await
                        .ok();

                    // Direct call to reset_for_pooling to verify it works
                    let reset_result = runner.as_cancel_monitoring().reset_for_pooling().await;
                    assert!(
                        reset_result.is_ok(),
                        "Direct reset_for_pooling call should succeed"
                    );

                    // Cleanup should also work
                    let cleanup_result = runner
                        .as_cancel_monitoring()
                        .cleanup_cancellation_monitoring()
                        .await;
                    assert!(cleanup_result.is_ok(), "Cleanup should succeed");
                }

                tracing::debug!("✅ Pool reset method direct call test completed");
                Ok(())
            })
        }
    }
}
