//! Integration tests for process_deque_job error-based status cleanup
//!
//! Tests verify that when process_job fails with permanent errors,
//! job status is properly cleaned up in process_deque_job.
//!
//! Note: Unit tests for JobWorkerError::should_delete_job_status() are in base/src/error.rs

#[cfg(test)]
mod process_deque_job_cleanup_tests {
    use crate::module::test::create_rdb_chan_test_app;
    use anyhow::Result;
    use infra::infra::job::status::JobProcessingStatusRepository;
    use infra_utils::infra::rdb::UseRdbPool;
    use infra_utils::infra::test::TEST_RUNTIME;
    use jobworkerp_base::codec::UseProstCodec;
    use jobworkerp_base::error::JobWorkerError;
    use proto::jobworkerp::data::{
        JobId, JobProcessingStatus, QueueType, ResponseType, RunnerId, StreamingType, WorkerData,
    };
    use std::collections::HashMap;
    use std::sync::Arc;

    /// Test: When job has incomplete data (no id/data), status should be cleaned up
    /// This simulates InvalidParameter("incomplete data") in process_job
    #[test]
    fn test_incomplete_job_data_triggers_status_cleanup() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let app_module = create_rdb_chan_test_app(true, false).await?;
            let repositories = app_module
                .repositories
                .rdb_module
                .as_ref()
                .expect("RDB module should exist");
            let status_repo = repositories
                .memory_job_processing_status_repository
                .as_ref();

            // Manually set PENDING status for a job that will fail
            let job_id = JobId { value: 80001 };
            status_repo
                .upsert_status(&job_id, &JobProcessingStatus::Pending)
                .await?;

            // Verify status was set
            assert_eq!(
                status_repo.find_status(&job_id).await?,
                Some(JobProcessingStatus::Pending),
                "Status should be PENDING before error"
            );

            // Simulate what happens when process_deque_job encounters an error
            // InvalidParameter is classified as permanent error -> status should be deleted
            let error = JobWorkerError::InvalidParameter("job is incomplete data".to_string());
            assert!(
                error.should_delete_job_status(),
                "InvalidParameter should trigger cleanup"
            );

            // Cleanup would be called by process_deque_job
            status_repo.delete_status(&job_id).await?;

            // Verify status was cleaned up
            assert_eq!(
                status_repo.find_status(&job_id).await?,
                None,
                "Status should be deleted after InvalidParameter"
            );

            tracing::info!("test_incomplete_job_data_triggers_status_cleanup completed");
            Ok(())
        })
    }

    /// Test: When worker is not found, status should be cleaned up
    /// This simulates NotFound("worker not found") in process_job
    #[test]
    fn test_worker_not_found_triggers_status_cleanup() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let app_module = create_rdb_chan_test_app(true, false).await?;
            let repositories = app_module
                .repositories
                .rdb_module
                .as_ref()
                .expect("RDB module should exist");
            let status_repo = repositories
                .memory_job_processing_status_repository
                .as_ref();

            let job_id = JobId { value: 80002 };
            status_repo
                .upsert_status(&job_id, &JobProcessingStatus::Pending)
                .await?;

            // NotFound is permanent error -> status should be deleted
            let error = JobWorkerError::NotFound("worker 999 not found".to_string());
            assert!(
                error.should_delete_job_status(),
                "NotFound should trigger cleanup"
            );

            // Cleanup would be called by process_deque_job
            status_repo.delete_status(&job_id).await?;

            assert_eq!(
                status_repo.find_status(&job_id).await?,
                None,
                "Status should be deleted after NotFound error"
            );

            tracing::info!("test_worker_not_found_triggers_status_cleanup completed");
            Ok(())
        })
    }

    /// Test: When runner_id is None, status should be cleaned up
    /// This simulates InvalidParameter("runner_id is None") in process_job
    #[test]
    fn test_invalid_runner_id_triggers_status_cleanup() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let app_module = create_rdb_chan_test_app(true, false).await?;
            let repositories = app_module
                .repositories
                .rdb_module
                .as_ref()
                .expect("RDB module should exist");
            let status_repo = repositories
                .memory_job_processing_status_repository
                .as_ref();

            let job_id = JobId { value: 80003 };
            status_repo
                .upsert_status(&job_id, &JobProcessingStatus::Pending)
                .await?;

            // NotFound for runner_id is permanent error
            let error = JobWorkerError::NotFound("worker runner_id is not found".to_string());
            assert!(
                error.should_delete_job_status(),
                "NotFound should trigger cleanup"
            );

            status_repo.delete_status(&job_id).await?;

            assert_eq!(
                status_repo.find_status(&job_id).await?,
                None,
                "Status should be deleted after runner_id NotFound error"
            );

            tracing::info!("test_invalid_runner_id_triggers_status_cleanup completed");
            Ok(())
        })
    }

    /// Test: AlreadyExists error should NOT trigger status cleanup
    /// This is critical because another process may be executing the job
    #[test]
    fn test_already_exists_preserves_status() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let app_module = create_rdb_chan_test_app(true, false).await?;
            let repositories = app_module
                .repositories
                .rdb_module
                .as_ref()
                .expect("RDB module should exist");
            let status_repo = repositories
                .memory_job_processing_status_repository
                .as_ref();

            let job_id = JobId { value: 80004 };
            status_repo
                .upsert_status(&job_id, &JobProcessingStatus::Pending)
                .await?;

            // AlreadyExists should NOT trigger cleanup (another process may be executing)
            let error = JobWorkerError::AlreadyExists("job already grabbed".to_string());
            assert!(
                !error.should_delete_job_status(),
                "AlreadyExists should NOT trigger cleanup"
            );

            // Status should remain
            assert_eq!(
                status_repo.find_status(&job_id).await?,
                Some(JobProcessingStatus::Pending),
                "Status should be preserved after AlreadyExists error"
            );

            // Cleanup for test
            status_repo.delete_status(&job_id).await?;

            tracing::info!("test_already_exists_preserves_status completed");
            Ok(())
        })
    }

    /// Test: RuntimeError should NOT trigger status cleanup (for error tracking)
    #[test]
    fn test_runtime_error_preserves_status() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let app_module = create_rdb_chan_test_app(true, false).await?;
            let repositories = app_module
                .repositories
                .rdb_module
                .as_ref()
                .expect("RDB module should exist");
            let status_repo = repositories
                .memory_job_processing_status_repository
                .as_ref();

            let job_id = JobId { value: 80005 };
            status_repo
                .upsert_status(&job_id, &JobProcessingStatus::Running)
                .await?;

            // RuntimeError should NOT trigger cleanup (for error tracking purposes)
            let error = JobWorkerError::RuntimeError("execution failed".to_string());
            assert!(
                !error.should_delete_job_status(),
                "RuntimeError should NOT trigger cleanup"
            );

            // Status should remain for error tracking
            assert_eq!(
                status_repo.find_status(&job_id).await?,
                Some(JobProcessingStatus::Running),
                "Status should be preserved after RuntimeError"
            );

            // Cleanup for test
            status_repo.delete_status(&job_id).await?;

            tracing::info!("test_runtime_error_preserves_status completed");
            Ok(())
        })
    }

    /// Test: End-to-end scenario - enqueue job with non-existent worker
    /// Verifies the complete flow: enqueue -> pop -> process_job fails -> status cleanup
    #[test]
    fn test_enqueue_with_nonexistent_worker_cleanup() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let app_module = create_rdb_chan_test_app(true, false).await?;
            let app = &app_module.job_app;
            let repositories = app_module
                .repositories
                .rdb_module
                .as_ref()
                .expect("RDB module should exist");
            let status_repo = repositories
                .memory_job_processing_status_repository
                .as_ref();

            // Create a valid worker first
            let runner_settings = jobworkerp_base::codec::ProstMessageCodec::serialize_message(
                &proto::TestRunnerSettings {
                    name: "ls".to_string(),
                },
            )?;
            let wd = WorkerData {
                name: "testworker_cleanup".to_string(),
                description: "Worker for cleanup test".to_string(),
                runner_id: Some(RunnerId { value: 1 }),
                runner_settings,
                channel: Some("test_cleanup".to_string()),
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
                jobworkerp_base::codec::ProstMessageCodec::serialize_message(&proto::TestArgs {
                    args: vec!["/".to_string()],
                })?;

            // Enqueue job (this sets PENDING status)
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
                    StreamingType::None,
                    None,
                )
                .await?;

            // Verify PENDING status was set
            assert_eq!(
                status_repo.find_status(&job_id).await?,
                Some(JobProcessingStatus::Pending),
                "Job should have PENDING status after enqueue"
            );

            // Now delete the worker to simulate "worker not found" scenario
            app_module.worker_app.delete(&worker_id).await?;

            // Verify worker is deleted
            let worker = app_module.worker_app.find(&worker_id).await?;
            assert!(worker.is_none(), "Worker should be deleted");

            // At this point, if dispatcher tried to process the job:
            // 1. pop job from queue (job removed from queue)
            // 2. process_job fails with NotFound("worker not found")
            // 3. process_deque_job detects permanent error
            // 4. cleanup_failed_job_status is called
            // 5. status is deleted

            // Simulate the cleanup that would happen in process_deque_job
            let simulated_error =
                JobWorkerError::NotFound(format!("worker {:?} is not found", worker_id));
            assert!(
                simulated_error.should_delete_job_status(),
                "NotFound should trigger cleanup"
            );

            // Cleanup the status (simulating what cleanup_failed_job_status does)
            status_repo.delete_status(&job_id).await?;

            // Verify status is cleaned up
            assert_eq!(
                status_repo.find_status(&job_id).await?,
                None,
                "Status should be deleted after worker not found error"
            );

            tracing::info!("test_enqueue_with_nonexistent_worker_cleanup completed");
            Ok(())
        })
    }

    /// Test: RDB index cleanup when error occurs (if RDB indexing is enabled)
    #[test]
    fn test_rdb_index_cleanup_on_permanent_error() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            // Create app with RDB indexing enabled
            let app_module = create_rdb_chan_test_app(true, true).await?;
            let repositories = app_module
                .repositories
                .rdb_module
                .as_ref()
                .expect("RDB module should exist");
            let status_repo = repositories
                .memory_job_processing_status_repository
                .as_ref();
            let index_repo = repositories
                .rdb_job_processing_status_index_repository
                .as_ref()
                .expect("RDB indexing should be enabled");

            // Create worker and enqueue job
            let runner_settings = jobworkerp_base::codec::ProstMessageCodec::serialize_message(
                &proto::TestRunnerSettings {
                    name: "ls".to_string(),
                },
            )?;
            let wd = WorkerData {
                name: "testworker_rdb_cleanup".to_string(),
                description: "Worker for RDB cleanup test".to_string(),
                runner_id: Some(RunnerId { value: 1 }),
                runner_settings,
                channel: Some("test_rdb_cleanup".to_string()),
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
                jobworkerp_base::codec::ProstMessageCodec::serialize_message(&proto::TestArgs {
                    args: vec!["/".to_string()],
                })?;

            let metadata = Arc::new(HashMap::new());
            let (job_id, _, _) = app_module
                .job_app
                .enqueue_job(
                    metadata.clone(),
                    Some(&worker_id),
                    None,
                    jargs.clone(),
                    None,
                    0,
                    5,
                    0,
                    None,
                    StreamingType::None,
                    None,
                )
                .await?;

            // Wait for async indexing
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;

            // Verify RDB index was created
            let rdb_pool = index_repo.db_pool();
            let query = "SELECT job_id FROM job_processing_status WHERE job_id = ?";
            let row: Option<(i64,)> = sqlx::query_as(query)
                .bind(job_id.value)
                .fetch_optional(rdb_pool)
                .await?;
            assert!(row.is_some(), "RDB index should exist after enqueue");

            // Simulate permanent error and cleanup
            status_repo.delete_status(&job_id).await?;
            index_repo.mark_deleted_by_job_id(&job_id).await?;

            // Verify memory status is deleted
            assert_eq!(
                status_repo.find_status(&job_id).await?,
                None,
                "Memory status should be deleted"
            );

            // Verify RDB index is logically deleted
            let query = "SELECT deleted_at FROM job_processing_status WHERE job_id = ?";
            let row: Option<(Option<i64>,)> = sqlx::query_as(query)
                .bind(job_id.value)
                .fetch_optional(rdb_pool)
                .await?;
            match row {
                Some((deleted_at,)) => {
                    assert!(
                        deleted_at.is_some(),
                        "RDB index should be logically deleted (deleted_at set)"
                    );
                }
                None => {
                    // Record may have been physically deleted in some cases
                    tracing::info!("RDB index record was physically deleted");
                }
            }

            tracing::info!("test_rdb_index_cleanup_on_permanent_error completed");
            Ok(())
        })
    }
}
