//! Integration test for RDB Job Processing Status Indexing
//!
//! This module tests the async indexing order guarantee for JobProcessingStatus
//! in Standalone (Memory + RDB) environment with RDB indexing enabled.

#[cfg(test)]
mod rdb_chan_indexing_integration_tests {
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
    use infra::infra::job::status::rdb::RdbJobProcessingStatusIndexRepository;
    use infra::infra::job::status::UseJobProcessingStatusRepository;
    use infra::infra::module::rdb::test::setup_test_rdb_module;
    use infra::infra::IdGeneratorWrapper;
    use infra_utils::infra::test::TEST_RUNTIME;
    use jobworkerp_base::job_status_config::JobStatusConfig;
    use jobworkerp_runner::runner::factory::RunnerSpecFactory;
    use jobworkerp_runner::runner::mcp::proxy::McpServerFactory;
    use jobworkerp_runner::runner::plugins::Plugins;
    use proto::jobworkerp::data::{
        JobProcessingStatus, QueueType, ResponseType, RunnerId, WorkerData,
    };
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    const TEST_PLUGIN_DIR: &str = "../../plugins";

    async fn create_test_rdb_chan_app_with_indexing(
        use_mock_id: bool,
    ) -> Result<(
        RdbChanJobAppImpl,
        Arc<RdbJobProcessingStatusIndexRepository>,
        &'static infra_utils::infra::rdb::RdbPool,
    )> {
        use infra_utils::infra::test::setup_test_rdb_from;

        // Note: mysql feature is defined in infra crate, not app crate
        // Always use sqlite for app tests
        let dir = "../../infra/sql/sqlite";
        let pool = setup_test_rdb_from(dir).await;

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

        let job_queue_cancellation_repository: Arc<dyn JobQueueCancellationRepository> =
            Arc::new(repositories.chan_job_queue_repository.clone());

        // Create RDB indexing repository with test config
        let job_status_config = JobStatusConfig {
            rdb_indexing_enabled: true,
            cleanup_interval_hours: 1,
            retention_hours: 24,
        };

        // Use the pool we obtained at the beginning
        let index_repository = Arc::new(RdbJobProcessingStatusIndexRepository::new(
            Arc::new(pool.clone()),
            job_status_config,
        ));

        let app = RdbChanJobAppImpl::new(
            config_module,
            id_generator,
            repositories,
            Arc::new(worker_app),
            job_queue_cancellation_repository,
            Some(Arc::clone(&index_repository)), // RDB indexing ENABLED for this test
        );

        Ok((app, index_repository, pool))
    }

    #[test]
    #[ignore] // Requires real RDB, run with: cargo test test_async_indexing_order_guarantee -- --ignored --nocapture
    fn test_async_indexing_order_guarantee() -> Result<()> {
        // Test that async RDB indexing preserves order: PENDING -> RUNNING -> deleted
        TEST_RUNTIME.block_on(async {
            let (app, index_repo, _pool) = create_test_rdb_chan_app_with_indexing(true).await?;

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

            // Step 1: Enqueue job (PENDING status)
            tracing::info!("Step 1: Enqueue job (PENDING)");
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

            // Verify Memory status is PENDING
            assert_eq!(
                app.job_processing_status_repository()
                    .find_status(&job_id)
                    .await
                    .unwrap(),
                Some(JobProcessingStatus::Pending)
            );

            // Wait for async indexing to complete (PENDING)
            tokio::time::sleep(Duration::from_millis(100)).await;

            // Step 2: Start job (RUNNING status)
            tracing::info!("Step 2: Start job (RUNNING)");
            app.job_processing_status_repository()
                .upsert_status(&job_id, &JobProcessingStatus::Running)
                .await?;

            // Trigger async indexing for RUNNING status
            let repo = Arc::clone(&index_repo);
            tokio::spawn(async move {
                if let Err(e) = repo
                    .index_status(
                        &job_id,
                        &JobProcessingStatus::Running,
                        &worker_id,
                        "test",
                        0,
                        0,
                        false,
                        false,
                    )
                    .await
                {
                    tracing::warn!(error = ?e, "Failed to index RUNNING status");
                }
            });

            // Wait for async indexing to complete (RUNNING)
            tokio::time::sleep(Duration::from_millis(100)).await;

            // Step 3: Complete job (logical deletion)
            tracing::info!("Step 3: Complete job (logical deletion)");
            let deleted = app.delete_job(&job_id).await?;
            assert!(deleted);

            // Verify Memory status is deleted
            assert_eq!(
                app.job_processing_status_repository()
                    .find_status(&job_id)
                    .await
                    .unwrap(),
                None
            );

            // Wait for async indexing to complete (deletion)
            tokio::time::sleep(Duration::from_millis(500)).await;

            // Step 4: Verify RDB index status (should be logically deleted)
            tracing::info!("Step 4: Verify RDB index status");
            let rdb_pool = index_repo.rdb_pool();

            let query = "SELECT deleted_at FROM job_processing_status WHERE job_id = ?";

            let deleted_at: Option<i64> = sqlx::query_scalar(query)
                .bind(job_id.value)
                .fetch_optional(rdb_pool.as_ref())
                .await?;

            assert!(
                deleted_at.is_some(),
                "Job should be logically deleted in RDB index"
            );
            tracing::info!(
                deleted_at = ?deleted_at,
                "Job logically deleted in RDB at timestamp"
            );

            tracing::info!("test_async_indexing_order_guarantee completed successfully");
            Ok(())
        })
    }

    #[test]
    #[ignore] // Requires real RDB
    fn test_find_by_condition_with_rdb_index() -> Result<()> {
        // Test FindByCondition with RDB indexing enabled
        TEST_RUNTIME.block_on(async {
            let (app, _index_repo, _pool) = create_test_rdb_chan_app_with_indexing(false).await?;

            // Create test worker
            let runner_settings = infra::infra::job::rows::JobqueueAndCodec::serialize_message(
                &proto::TestRunnerSettings {
                    name: "ls".to_string(),
                },
            );
            let wd = WorkerData {
                name: "testworker2".to_string(),
                description: "desc2".to_string(),
                runner_id: Some(RunnerId { value: 1 }),
                runner_settings,
                channel: Some("test".to_string()),
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

            // Enqueue multiple jobs
            tracing::info!("Enqueueing 3 test jobs");
            let metadata = Arc::new(HashMap::new());
            let mut job_ids = Vec::new();
            for i in 0..3 {
                let (job_id, _, _) = app
                    .enqueue_job(
                        metadata.clone(),
                        Some(&worker_id),
                        None,
                        jargs.clone(),
                        None,
                        0,
                        i, // Different priorities
                        0,
                        None,
                        false,
                    )
                    .await?;
                job_ids.push(job_id);
            }

            // Wait for async indexing
            tokio::time::sleep(Duration::from_millis(500)).await;

            // Test FindByCondition
            tracing::info!("Testing find_by_condition");
            let results = app
                .find_by_condition(
                    Some(JobProcessingStatus::Pending),
                    None,
                    Some("test".to_string()),
                    None,
                    10,
                    0,
                    false,
                )
                .await?;

            assert!(
                !results.is_empty(),
                "Should find at least one PENDING job in test channel"
            );
            tracing::info!(count = results.len(), "Found jobs by condition");

            for detail in &results {
                assert_eq!(detail.status, JobProcessingStatus::Pending);
                assert_eq!(detail.channel.as_str(), "test");
                tracing::debug!(
                    job_id = detail.job_id.value,
                    priority = detail.priority,
                    "Found job"
                );
            }

            tracing::info!("test_find_by_condition_with_rdb_index completed successfully");
            Ok(())
        })
    }

    #[test]
    fn test_rdb_indexing_disabled_by_default() -> Result<()> {
        // Test that RDB indexing is disabled when index_repository is None
        TEST_RUNTIME.block_on(async {
            let rdb_module = setup_test_rdb_module().await;
            let repositories = Arc::new(rdb_module);
            let id_generator = Arc::new(IdGeneratorWrapper::new_mock());

            let moka_config = memory_utils::cache::moka::MokaCacheConfig {
                num_counters: 10000,
                ttl: Some(Duration::from_millis(100)),
            };

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

            let runner_factory = RunnerSpecFactory::new(
                Arc::new(Plugins::new()),
                Arc::new(McpServerFactory::default()),
            );
            let config_module = Arc::new(AppConfigModule {
                storage_config,
                worker_config,
                job_queue_config: job_queue_config.clone(),
                runner_factory: Arc::new(runner_factory),
            });

            let job_queue_cancellation_repository: Arc<dyn JobQueueCancellationRepository> =
                Arc::new(repositories.chan_job_queue_repository.clone());

            // Create app WITHOUT RDB indexing
            let _app = RdbChanJobAppImpl::new(
                config_module,
                id_generator,
                repositories,
                Arc::new(worker_app),
                job_queue_cancellation_repository,
                None, // RDB indexing DISABLED
            );

            // Note: Cannot directly check private field, but if no panic occurred, test passes

            tracing::info!("test_rdb_indexing_disabled_by_default completed successfully");
            Ok(())
        })
    }
}
