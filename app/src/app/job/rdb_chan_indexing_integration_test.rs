//! Integration test for RDB Job Processing Status Indexing
//!
//! This module tests the async indexing order guarantee for JobProcessingStatus
//! in Standalone (Memory + RDB) environment with RDB indexing enabled.

#[cfg(test)]
mod rdb_chan_indexing_integration_tests {
    use crate::module::test::create_rdb_chan_test_app;

    use anyhow::Result;
    use infra::infra::job::rows::UseJobqueueAndCodec;
    use infra::infra::job::status::JobProcessingStatusRepository;
    use infra_utils::infra::rdb::UseRdbPool;
    use infra_utils::infra::test::TEST_RUNTIME;
    use proto::jobworkerp::data::{
        JobProcessingStatus, QueueType, ResponseType, RunnerId, WorkerData,
    };
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn test_async_indexing_order_guarantee() -> Result<()> {
        command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
        // Test that async RDB indexing preserves order: PENDING -> RUNNING -> deleted
        //
        // NOTE: This test manually calls index_status() for demonstration purposes.
        // In production, state transitions in RedisJobDispatcherImpl (worker-app) and
        // RdbChanJobAppImpl (app) automatically trigger index_job_status_async() hooks:
        // - PENDING: enqueue_job() in RdbChanJobAppImpl
        // - RUNNING: RedisJobDispatcherImpl lines 284, 304 (after upsert_status)
        // - WAIT_RESULT: RedisJobDispatcherImpl line 340
        // - CANCELLING: RdbChanJobAppImpl lines 203, 221 (in delete_job)
        TEST_RUNTIME.block_on(async {
            let app_module = create_rdb_chan_test_app(true, true).await?;
            let app = &app_module.job_app;
            let repositories = app_module
                .repositories
                .rdb_module
                .as_ref()
                .expect("RDB module should exist");
            let index_repo = repositories
                .rdb_job_processing_status_index_repository
                .as_ref()
                .expect("RDB indexing should be enabled");
            let _pool = repositories.job_repository.db_pool();

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

            let worker_id = app_module.worker_app.create(&wd).await?;
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

            // Get status repository from rdb module for Standalone mode
            let status_repo = repositories
                .memory_job_processing_status_repository
                .as_ref();

            // Verify Memory status is PENDING
            assert_eq!(
                status_repo.find_status(&job_id).await.unwrap(),
                Some(JobProcessingStatus::Pending)
            );

            // Wait for async indexing to complete (PENDING)
            tokio::time::sleep(Duration::from_millis(100)).await;

            // Step 2: Start job (RUNNING status)
            tracing::info!("Step 2: Start job (RUNNING)");
            status_repo
                .upsert_status(&job_id, &JobProcessingStatus::Running)
                .await?;

            // Trigger async indexing for RUNNING status
            let repo = Arc::clone(index_repo);
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
            assert_eq!(status_repo.find_status(&job_id).await.unwrap(), None);

            // Step 4: Verify RDB index status (should be logically deleted)
            tracing::info!("Step 4: Verify RDB index status");
            let rdb_pool = index_repo.db_pool();
            let query = "SELECT deleted_at FROM job_processing_status WHERE job_id = ?";

            // fetch_optional returns None if row doesn't exist
            // The column deleted_at itself can be NULL, so we need Option<Option<i64>>
            let row_result: Option<Option<i64>> = sqlx::query_scalar(query)
                .bind(job_id.value)
                .fetch_optional(rdb_pool)
                .await?;

            match row_result {
                Some(Some(timestamp)) => {
                    tracing::info!(
                        deleted_at = timestamp,
                        "Job logically deleted in RDB at timestamp"
                    );
                }
                Some(None) => {
                    panic!("Job row exists but deleted_at is NULL (should be set by cleanup_job)");
                }
                None => {
                    panic!("Job row does not exist in RDB index");
                }
            }

            tracing::info!("test_async_indexing_order_guarantee completed successfully");
            Ok(())
        })
    }

    #[test]
    fn test_find_by_condition_with_rdb_index() -> Result<()> {
        // Test FindByCondition with RDB indexing enabled
        TEST_RUNTIME.block_on(async {
            let app_module = create_rdb_chan_test_app(false, true).await?;
            let app = &app_module.job_app;

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

            let worker_id = app_module.worker_app.create(&wd).await?;
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
        // Test that RDB indexing is disabled when enable_rdb_indexing=false
        TEST_RUNTIME.block_on(async {
            let app_module = create_rdb_chan_test_app(true, false).await?;
            let repositories = app_module
                .repositories
                .rdb_module
                .as_ref()
                .expect("RDB module should exist");

            // Verify that RDB indexing repository is None
            assert!(
                repositories
                    .rdb_job_processing_status_index_repository
                    .is_none(),
                "RDB indexing should be disabled by default"
            );

            tracing::info!("test_rdb_indexing_disabled_by_default completed successfully");
            Ok(())
        })
    }

    #[test]
    fn test_cancel_pending_job_with_rdb_indexing() -> Result<()> {
        command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
        // Test that cancelling a PENDING job updates RDB index with CANCELLING status
        TEST_RUNTIME.block_on(async {
            let app_module = create_rdb_chan_test_app(true, true).await?;
            let app = &app_module.job_app;
            let repositories = app_module
                .repositories
                .rdb_module
                .as_ref()
                .expect("RDB module should exist");
            let index_repo = repositories
                .rdb_job_processing_status_index_repository
                .as_ref()
                .expect("RDB indexing should be enabled");
            let status_repo = repositories
                .memory_job_processing_status_repository
                .as_ref();

            // Create test worker with NoResult response type
            let runner_settings = infra::infra::job::rows::JobqueueAndCodec::serialize_message(
                &proto::TestRunnerSettings {
                    name: "sleep".to_string(),
                },
            );
            let wd = WorkerData {
                name: "testworker_cancel".to_string(),
                description: "Worker for cancellation test".to_string(),
                runner_id: Some(RunnerId { value: 1 }),
                runner_settings,
                channel: Some("test_cancel".to_string()),
                response_type: ResponseType::NoResult as i32,
                periodic_interval: 0,
                retry_policy: None,
                queue_type: QueueType::Normal as i32,
                store_failure: false,
                store_success: false,
                use_static: false,
                broadcast_results: false,
            };

            let worker_id = app_module.worker_app.create(&wd).await?;
            let jargs =
                infra::infra::job::rows::JobqueueAndCodec::serialize_message(&proto::TestArgs {
                    args: vec!["10".to_string()], // Long sleep
                });

            // Enqueue job (PENDING status)
            tracing::info!("Enqueueing job for cancellation test");
            let metadata = Arc::new(HashMap::new());
            let (job_id, _, _) = app
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

            // Verify Memory status is PENDING
            assert_eq!(
                status_repo.find_status(&job_id).await?,
                Some(JobProcessingStatus::Pending)
            );

            // Wait for async PENDING indexing
            tokio::time::sleep(Duration::from_millis(100)).await;

            // Cancel the job while it's still PENDING
            tracing::info!("Cancelling PENDING job");
            let cancelled = app.delete_job(&job_id).await?;
            assert!(cancelled, "Job cancellation should succeed");

            // Verify Memory status is deleted
            assert_eq!(status_repo.find_status(&job_id).await?, None);

            // Wait for async cancellation indexing
            tokio::time::sleep(Duration::from_millis(100)).await;

            // Verify RDB index shows CANCELLING status before deletion
            let rdb_pool = index_repo.db_pool();
            let query = "SELECT status, deleted_at FROM job_processing_status WHERE job_id = ?";

            let row: Option<(i32, Option<i64>)> = sqlx::query_as(query)
                .bind(job_id.value)
                .fetch_optional(rdb_pool)
                .await?;

            match row {
                Some((status_code, deleted_at)) => {
                    tracing::info!(
                        status_code,
                        ?deleted_at,
                        "RDB index state for cancelled job"
                    );
                    // Status should be CANCELLING (4)
                    assert_eq!(
                        status_code,
                        JobProcessingStatus::Cancelling as i32,
                        "Cancelled job should have CANCELLING status in RDB index"
                    );
                    // Job should be logically deleted
                    assert!(
                        deleted_at.is_some(),
                        "Cancelled job should be logically deleted in RDB index"
                    );
                }
                None => {
                    panic!("Cancelled job should still exist in RDB index (logically deleted)");
                }
            }

            tracing::info!("test_cancel_pending_job_with_rdb_indexing completed successfully");
            Ok(())
        })
    }
}
