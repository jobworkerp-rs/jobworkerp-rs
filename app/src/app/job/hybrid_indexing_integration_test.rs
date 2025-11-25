//! Integration test for RDB Job Processing Status Indexing in Hybrid mode
//!
//! This module tests the async indexing order guarantee for JobProcessingStatus
//! in Scalable (Redis + RDB) environment with RDB indexing enabled.

#[cfg(test)]
mod hybrid_indexing_integration_tests {
    use crate::module::test::create_hybrid_test_app_with_indexing;

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
    fn test_async_indexing_cancelling_transition_hybrid() -> Result<()> {
        // Test that async RDB indexing preserves order: PENDING -> CANCELLING (via delete_job)
        //
        // NOTE: This test verifies CANCELLING state transition indexing in Hybrid mode.
        // Production state transitions in HybridJobAppImpl automatically trigger
        // index_job_status_async() hooks:
        // - PENDING: enqueue_job() in HybridJobAppImpl
        // - CANCELLING: delete_job() in HybridJobAppImpl (lines 319-363, 383-427)
        TEST_RUNTIME.block_on(async {
            let app_module = create_hybrid_test_app_with_indexing(true).await?;
            let app = &app_module.job_app;
            let repositories = &app_module.repositories;
            let rdb_module = repositories
                .rdb_module
                .as_ref()
                .expect("RDB module should exist");
            let index_repo = rdb_module
                .rdb_job_processing_status_index_repository
                .as_ref()
                .expect("RDB indexing should be enabled");
            let _pool = rdb_module.job_repository.db_pool();

            // Get status repository from redis module for Hybrid mode
            let redis_module = repositories
                .redis_module
                .as_ref()
                .expect("Redis module should exist");
            let status_repo = redis_module
                .redis_job_repository
                .redis_job_processing_status_repository
                .as_ref();

            // Create test worker
            let runner_settings = infra::infra::job::rows::JobqueueAndCodec::serialize_message(
                &proto::TestRunnerSettings {
                    name: "ls".to_string(),
                },
            );
            let wd = WorkerData {
                name: "testworker_hybrid".to_string(),
                description: "Hybrid indexing test".to_string(),
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
                    None, // sub_method
                )
                .await?;

            assert!(job_id.value > 0);
            assert!(res.is_none());

            // Verify Redis status is PENDING
            assert_eq!(
                status_repo.find_status(&job_id).await.unwrap(),
                Some(JobProcessingStatus::Pending)
            );

            // Wait for async indexing to complete (PENDING)
            tokio::time::sleep(Duration::from_millis(100)).await;

            // Verify RDB index has PENDING status
            let rdb_pool = index_repo.db_pool();
            let query =
                "SELECT status FROM job_processing_status WHERE job_id = ? AND deleted_at IS NULL";
            let status: Option<i32> = sqlx::query_scalar(query)
                .bind(job_id.value)
                .fetch_optional(rdb_pool)
                .await?;

            assert_eq!(
                status,
                Some(JobProcessingStatus::Pending as i32),
                "RDB index should have PENDING status"
            );

            // Step 2: Cancel job (PENDING -> CANCELLING)
            tracing::info!("Step 2: Cancel job (PENDING -> CANCELLING)");
            let deleted = app.delete_job(&job_id).await?;
            assert!(deleted);

            // Verify Redis status is deleted
            assert_eq!(status_repo.find_status(&job_id).await.unwrap(), None);

            // Step 3: Verify RDB index status (should be logically deleted)
            // Wait and retry for async indexing to complete (deletion)
            // MySQL may have more latency than SQLite
            tracing::info!("Step 3: Verify RDB index status");
            let query = "SELECT deleted_at FROM job_processing_status WHERE job_id = ?";

            let mut deleted_at_result = None;
            for attempt in 0..10 {
                tokio::time::sleep(Duration::from_millis(100 * (attempt + 1))).await;

                // fetch_optional returns None if row doesn't exist
                // The column deleted_at itself can be NULL, so we need Option<Option<i64>>
                let row_result: Option<Option<i64>> = sqlx::query_scalar(query)
                    .bind(job_id.value)
                    .fetch_optional(rdb_pool)
                    .await?;

                match row_result {
                    Some(Some(timestamp)) => {
                        deleted_at_result = Some(timestamp);
                        break;
                    }
                    Some(None) => {
                        tracing::debug!(
                            attempt,
                            "Job row exists but deleted_at is still NULL, retrying..."
                        );
                        continue;
                    }
                    None => {
                        panic!("Job row does not exist in RDB index");
                    }
                }
            }

            match deleted_at_result {
                Some(timestamp) => {
                    tracing::info!(
                        deleted_at = timestamp,
                        "Job logically deleted in RDB at timestamp"
                    );
                }
                None => {
                    panic!("Job row exists but deleted_at is still NULL after retries");
                }
            }

            tracing::info!(
                "test_async_indexing_cancelling_transition_hybrid completed successfully"
            );
            Ok(())
        })
    }

    #[test]
    fn test_cancel_running_job_with_rdb_indexing_hybrid() -> Result<()> {
        // Test RUNNING -> CANCELLING transition with RDB indexing in Hybrid mode
        TEST_RUNTIME.block_on(async {
            let app_module = create_hybrid_test_app_with_indexing(true).await?;
            let app = &app_module.job_app;
            let repositories = &app_module.repositories;
            let rdb_module = repositories
                .rdb_module
                .as_ref()
                .expect("RDB module should exist");
            let index_repo = rdb_module
                .rdb_job_processing_status_index_repository
                .as_ref()
                .expect("RDB indexing should be enabled");
            let _pool = rdb_module.job_repository.db_pool();

            // Get status repository from redis module for Hybrid mode
            let redis_module = repositories
                .redis_module
                .as_ref()
                .expect("Redis module should exist");
            let status_repo = redis_module
                .redis_job_repository
                .redis_job_processing_status_repository
                .as_ref();

            // Create test worker
            let runner_settings = infra::infra::job::rows::JobqueueAndCodec::serialize_message(
                &proto::TestRunnerSettings {
                    name: "sleep".to_string(),
                },
            );
            let wd = WorkerData {
                name: "testworker_running_hybrid".to_string(),
                description: "Test RUNNING cancellation".to_string(),
                runner_id: Some(RunnerId { value: 1 }),
                runner_settings,
                channel: Some("test".to_string()),
                response_type: ResponseType::NoResult as i32,
                periodic_interval: 0,
                retry_policy: None,
                queue_type: QueueType::Normal as i32,
                store_failure: true,
                store_success: true,
                use_static: false,
                broadcast_results: false,
            };

            let worker_id = app_module.worker_app.create(&wd).await?;
            let jargs =
                infra::infra::job::rows::JobqueueAndCodec::serialize_message(&proto::TestArgs {
                    args: vec!["5".to_string()], // Sleep 5 seconds
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
                    None, // sub_method
                )
                .await?;

            assert!(job_id.value > 0);
            assert!(res.is_none());

            // Wait for async indexing (PENDING)
            tokio::time::sleep(Duration::from_millis(100)).await;

            // Manually set status to RUNNING (simulating worker execution)
            status_repo
                .upsert_status(&job_id, &JobProcessingStatus::Running)
                .await?;

            // NOTE: In production, RUNNING transition would trigger index_status()
            // in RedisJobDispatcherImpl (worker-app/src/worker/dispatcher/redis.rs).
            // For this test, we manually verify the CANCELLING transition only.

            // Verify job is RUNNING
            let status = status_repo.find_status(&job_id).await.unwrap();
            assert_eq!(
                status,
                Some(JobProcessingStatus::Running),
                "Job should be in RUNNING status"
            );

            // Cancel RUNNING job (triggers RUNNING -> CANCELLING indexing hook)
            let result = app.delete_job(&job_id).await?;
            assert!(result, "RUNNING job cancellation should succeed");

            // Verify Redis status is deleted
            assert_eq!(
                status_repo.find_status(&job_id).await.unwrap(),
                None,
                "RUNNING job status should be deleted after cancellation"
            );

            // Verify RDB index status (should be logically deleted)
            // Wait and retry for async indexing to complete (deletion)
            // MySQL may have more latency than SQLite
            let rdb_pool = index_repo.db_pool();
            let query = "SELECT deleted_at FROM job_processing_status WHERE job_id = ?";

            let mut deleted_at_result = None;
            for attempt in 0..10 {
                tokio::time::sleep(Duration::from_millis(100 * (attempt + 1))).await;

                // fetch_optional returns None if row doesn't exist
                // The column deleted_at itself can be NULL, so we need Option<Option<i64>>
                let row_result: Option<Option<i64>> = sqlx::query_scalar(query)
                    .bind(job_id.value)
                    .fetch_optional(rdb_pool)
                    .await?;

                match row_result {
                    Some(Some(timestamp)) => {
                        deleted_at_result = Some(timestamp);
                        break;
                    }
                    Some(None) => {
                        tracing::debug!(
                            attempt,
                            "RUNNING job row exists but deleted_at is still NULL, retrying..."
                        );
                        continue;
                    }
                    None => {
                        panic!("RUNNING job row does not exist in RDB index after cancellation");
                    }
                }
            }

            match deleted_at_result {
                Some(timestamp) => {
                    tracing::info!(
                        deleted_at = timestamp,
                        "RUNNING job logically deleted in RDB at timestamp"
                    );
                }
                None => {
                    panic!("RUNNING job row exists but deleted_at is still NULL after retries");
                }
            }

            tracing::info!(
                "test_cancel_running_job_with_rdb_indexing_hybrid completed successfully"
            );
            Ok(())
        })
    }

    #[test]
    fn test_find_by_condition_with_rdb_index_hybrid() -> Result<()> {
        // Test FindByCondition with RDB indexing enabled in Hybrid mode
        TEST_RUNTIME.block_on(async {
            let app_module = create_hybrid_test_app_with_indexing(true).await?;
            let app = &app_module.job_app;

            // Create test worker
            let runner_settings = infra::infra::job::rows::JobqueueAndCodec::serialize_message(
                &proto::TestRunnerSettings {
                    name: "ls".to_string(),
                },
            );
            let wd = WorkerData {
                name: "testworker_find_hybrid".to_string(),
                description: "Test find_by_condition".to_string(),
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
                        None, // sub_method
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

            tracing::info!("test_find_by_condition_with_rdb_index_hybrid completed successfully");
            Ok(())
        })
    }

    #[test]
    fn test_rdb_indexing_disabled_by_default_hybrid() -> Result<()> {
        // Test that RDB indexing uses default config from repositories when not explicitly enabled
        TEST_RUNTIME.block_on(async {
            let app_module = create_hybrid_test_app_with_indexing(false).await?;
            let app = &app_module.job_app;
            let repositories = &app_module.repositories;

            // Verify RDB indexing is disabled
            let rdb_module = repositories
                .rdb_module
                .as_ref()
                .expect("RDB module should exist");
            assert!(
                rdb_module
                    .rdb_job_processing_status_index_repository
                    .is_none(),
                "RDB indexing should be disabled when enable_rdb_indexing=false"
            );

            // Create test worker
            let runner_settings = infra::infra::job::rows::JobqueueAndCodec::serialize_message(
                &proto::TestRunnerSettings {
                    name: "ls".to_string(),
                },
            );
            let wd = WorkerData {
                name: "testworker_default".to_string(),
                description: "Test default RDB indexing config".to_string(),
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

            // Enqueue job (should use default RDB indexing config from repositories)
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
                    None, // sub_method
                )
                .await?;

            assert!(job_id.value > 0);
            assert!(res.is_none());

            // Note: Cannot directly check private field, but if no panic occurred, test passes
            // The actual RDB indexing behavior is controlled by JOB_STATUS_RDB_INDEXING env var

            tracing::info!("test_rdb_indexing_disabled_by_default_hybrid completed successfully");
            Ok(())
        })
    }
}
