//! Test module for RdbChanJobAppImpl cancellation functionality
//!
//! This module tests job cancellation in Standalone (Memory + RDB) environment

#[cfg(test)]
mod rdb_chan_cancellation_tests {
    use super::super::rdb_chan::RdbChanJobAppImpl;
    use super::super::JobApp;
    use crate::app::runner::rdb::RdbRunnerAppImpl;
    use crate::app::runner::RunnerApp;
    use crate::app::worker::rdb::RdbWorkerAppImpl;
    use crate::app::worker::UseWorkerApp;
    use crate::app::{StorageConfig, StorageType, WorkerConfig};
    use crate::module::AppConfigModule;
    use anyhow::Result;
    use infra::infra::job::queue::JobQueueCancellationRepository;
    use infra::infra::job::rows::UseJobqueueAndCodec;
    use infra::infra::job::status::UseJobProcessingStatusRepository;
    use infra::infra::module::rdb::test::setup_test_rdb_module;
    use infra::infra::IdGeneratorWrapper;
    use infra_utils::infra::test::TEST_RUNTIME;
    use jobworkerp_runner::runner::factory::RunnerSpecFactory;
    use jobworkerp_runner::runner::mcp::proxy::McpServerFactory;
    use jobworkerp_runner::runner::plugins::Plugins;
    use proto::jobworkerp::data::{
        JobId, JobProcessingStatus, QueueType, ResponseType, RunnerId, WorkerData,
    };
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    const TEST_PLUGIN_DIR: &str = "../../plugins";

    async fn create_test_rdb_chan_app(use_mock_id: bool) -> Result<RdbChanJobAppImpl> {
        let rdb_module = setup_test_rdb_module().await;
        let repositories = Arc::new(rdb_module);

        // mock id generator (generate 1 until called set method)
        let id_generator = if use_mock_id {
            Arc::new(IdGeneratorWrapper::new_mock())
        } else {
            Arc::new(IdGeneratorWrapper::new())
        };

        let moka_config = memory_utils::cache::moka::MokaCacheConfig {
            num_counters: 10000,
            ttl: Some(Duration::from_millis(100)),
        };

        // UseMemoryCache is auto-initialized in RdbChanJobAppImpl::new(), explicit creation unnecessary
        let storage_config = Arc::new(StorageConfig {
            r#type: StorageType::Standalone,
            restore_at_startup: Some(false),
        });
        let job_queue_config = Arc::new(infra::infra::JobQueueConfig {
            expire_job_result_seconds: 10,
            fetch_interval: 1000,
        });
        let worker_config = Arc::new(WorkerConfig {
            default_concurrency: 4,
            channels: vec!["test".to_string()],
            channel_concurrencies: vec![2],
        });

        let descriptor_cache =
            Arc::new(memory_utils::cache::moka::MokaCacheImpl::new(&moka_config));
        let runner_app = Arc::new(RdbRunnerAppImpl::new(
            TEST_PLUGIN_DIR.to_string(),
            storage_config.clone(),
            &moka_config,
            repositories.clone(),
            descriptor_cache.clone(),
        ));
        let worker_app = RdbWorkerAppImpl::new(
            storage_config.clone(),
            id_generator.clone(),
            &moka_config,
            repositories.clone(),
            descriptor_cache,
            runner_app.clone(),
        );
        let _ = runner_app
            .create_test_runner(&RunnerId { value: 1 }, "Test")
            .await?;

        let runner_factory = RunnerSpecFactory::new(
            Arc::new(Plugins::new()),
            Arc::new(McpServerFactory::default()),
        );
        runner_factory.load_plugins_from(TEST_PLUGIN_DIR).await;
        let config_module = Arc::new(AppConfigModule {
            storage_config,
            worker_config,
            job_queue_config: job_queue_config.clone(),
            runner_factory: Arc::new(runner_factory),
        });

        // Create JobQueueCancellationRepository for RdbChanJobAppImpl (Memory environment)
        let job_queue_cancellation_repository: Arc<dyn JobQueueCancellationRepository> =
            Arc::new(repositories.chan_job_queue_repository.clone());

        Ok(RdbChanJobAppImpl::new(
            config_module,
            id_generator,
            repositories,
            Arc::new(worker_app),
            job_queue_cancellation_repository,
        ))
    }

    #[test]
    fn test_cancel_pending_job_rdb_chan() -> Result<()> {
        // Test pending job cancellation in memory environment
        TEST_RUNTIME.block_on(async {
            let app = create_test_rdb_chan_app(true).await?;

            // Create test worker
            let runner_settings = infra::infra::job::rows::JobqueueAndCodec::serialize_message(
                &proto::TestRunnerSettings {
                    name: "ls".to_string(),
                },
            );
            let wd = WorkerData {
                name: "testworker".to_string(),
                description: "desc1".to_string(),
                runner_id: Some(RunnerId { value: 1 }),
                runner_settings,
                channel: None,
                response_type: ResponseType::NoResult as i32,
                periodic_interval: 0,
                retry_policy: None,
                queue_type: QueueType::Normal as i32,
                store_failure: false,
                store_success: false,
                use_static: false,
                broadcast_results: false,
            };

            let worker_id = app.worker_app().create(&wd).await?;
            let jargs =
                infra::infra::job::rows::JobqueueAndCodec::serialize_message(&proto::TestArgs {
                    args: vec!["/".to_string()],
                });

            // Enqueue job
            let metadata = Arc::new(HashMap::new());
            let (job_id, res, _) = app
                .enqueue_job(
                    metadata.clone(),
                    Some(&worker_id),
                    None,
                    jargs.clone(),
                    None,
                    0,
                    0,
                    0,
                    None,
                    false,
                )
                .await?;

            assert!(job_id.value > 0);
            assert!(res.is_none());

            // Verify job is pending
            assert_eq!(
                app.job_processing_status_repository()
                    .find_status(&job_id)
                    .await
                    .unwrap(),
                Some(JobProcessingStatus::Pending)
            );

            // Cancel the job using delete_job (which now calls cancel_job_with_cleanup)
            let cancelled = app.delete_job(&job_id).await?;
            assert!(cancelled);

            // Verify job is cancelled
            let status = app
                .job_processing_status_repository()
                .find_status(&job_id)
                .await
                .unwrap();
            assert_eq!(status, None);

            tracing::info!("test_cancel_pending_job_rdb_chan completed successfully");
            Ok(())
        })
    }

    #[test]
    fn test_cancel_nonexistent_job_rdb_chan() -> Result<()> {
        // Test cancelling non-existent job
        TEST_RUNTIME.block_on(async {
            let app = create_test_rdb_chan_app(true).await?;

            let nonexistent_job_id = JobId { value: 99999 };

            // Cancel non-existent job should return false
            let cancelled = app.delete_job(&nonexistent_job_id).await?;
            assert!(!cancelled);

            tracing::info!("test_cancel_nonexistent_job_rdb_chan completed successfully");
            Ok(())
        })
    }

    #[test]
    fn test_cancel_job_pending_states() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let app = create_test_rdb_chan_app(true).await?;

            // Test with no status (should return false)
            let job_id = JobId { value: 54321 };
            let cancelled = app.delete_job(&job_id).await?;
            assert!(!cancelled);

            // Set status to Pending and test cancellation
            app.job_processing_status_repository()
                .upsert_status(&job_id, &JobProcessingStatus::Pending)
                .await?;

            let cancelled = app.delete_job(&job_id).await?;
            assert!(cancelled);

            // Verify status changed to Cancelling
            let status = app
                .job_processing_status_repository()
                .find_status(&job_id)
                .await
                .unwrap();
            assert_eq!(status, None);

            tracing::info!("test_cancel_job_pending_states completed successfully");
            Ok(())
        })
    }

    #[test]
    fn test_cancel_running_job_memory() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let app = create_test_rdb_chan_app(true).await?;

            let job_id = JobId { value: 67890 };

            // Set status to Running
            app.job_processing_status_repository()
                .upsert_status(&job_id, &JobProcessingStatus::Running)
                .await?;

            // Cancel the running job
            let cancelled = app.delete_job(&job_id).await?;
            assert!(cancelled);

            // Verify status changed to Cancelling
            let status = app
                .job_processing_status_repository()
                .find_status(&job_id)
                .await
                .unwrap();
            assert_eq!(status, None);

            tracing::info!("test_cancel_running_job_memory completed successfully");
            Ok(())
        })
    }
}
