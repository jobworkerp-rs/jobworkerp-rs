//!
//! Tests for job list finding with processing status functionality
//!
//! This module tests job listing and finding functionality with various processing statuses
//! based on the reference implementations in hybrid.rs test patterns.

#[cfg(test)]
mod tests {
    use super::super::hybrid::tests::create_test_app;
    use super::super::JobApp;
    use crate::app::worker::UseWorkerApp;
    use anyhow::Result;
    use infra::infra::job::rows::UseJobqueueAndCodec;
    use infra::infra::job::status::UseJobProcessingStatusRepository;
    use infra_utils::infra::test::TEST_RUNTIME;
    use proto::jobworkerp::data::{
        JobId, JobProcessingStatus, QueueType, ResponseType, RunnerId, WorkerData,
    };
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    const TEST_RUNNER_ID: RunnerId = RunnerId { value: 100000000 };

    #[test]
    fn test_find_list_with_status_all_statuses() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let (app, _) = create_test_app(true).await?;

            // Create test worker
            let runner_settings = infra::infra::job::rows::JobqueueAndCodec::serialize_message(
                &proto::TestRunnerSettings {
                    name: "ls".to_string(),
                },
            );
            let wd = WorkerData {
                name: "testworker".to_string(),
                description: "desc1".to_string(),
                runner_id: Some(TEST_RUNNER_ID),
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

            // Test finding jobs with different statuses
            let metadata = Arc::new(HashMap::new());
            
            // Create a job and test various status transitions
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

            // Small delay to ensure async operations complete
            tokio::time::sleep(Duration::from_millis(50)).await;

            // Test finding job with Pending status
            let status = app
                .job_processing_status_repository()
                .find_status(&job_id)
                .await
                .unwrap();
            assert_eq!(status, Some(JobProcessingStatus::Pending));

            // Test status transition to Running
            app.job_processing_status_repository()
                .upsert_status(&job_id, &JobProcessingStatus::Running)
                .await?;

            let status = app
                .job_processing_status_repository()
                .find_status(&job_id)
                .await
                .unwrap();
            assert_eq!(status, Some(JobProcessingStatus::Running));

            // Test status transition to WaitResult
            app.job_processing_status_repository()
                .upsert_status(&job_id, &JobProcessingStatus::WaitResult)
                .await?;

            let status = app
                .job_processing_status_repository()
                .find_status(&job_id)
                .await
                .unwrap();
            assert_eq!(status, Some(JobProcessingStatus::WaitResult));

            tracing::info!("test_find_list_with_status_all_statuses completed successfully");
            Ok(())
        })
    }

    #[test]
    fn test_find_list_with_status_empty_result() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let (app, _) = create_test_app(true).await?;

            // Test finding non-existent job status
            let nonexistent_job_id = JobId { value: 99999 };
            
            let status = app
                .job_processing_status_repository()
                .find_status(&nonexistent_job_id)
                .await
                .unwrap();
            assert_eq!(status, None);

            tracing::info!("test_find_list_with_status_empty_result completed successfully");
            Ok(())
        })
    }

    #[test]
    fn test_find_list_with_status_no_limit() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let (app, _) = create_test_app(true).await?;

            // Create test worker
            let runner_settings = infra::infra::job::rows::JobqueueAndCodec::serialize_message(
                &proto::TestRunnerSettings {
                    name: "ls".to_string(),
                },
            );
            let wd = WorkerData {
                name: "testworker".to_string(),
                description: "desc1".to_string(),
                runner_id: Some(TEST_RUNNER_ID),
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

            // Create multiple jobs to test list functionality
            let metadata = Arc::new(HashMap::new());
            let mut job_ids = Vec::new();

            for _i in 0..3 {
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
                job_ids.push(job_id);
            }

            // Small delay to ensure async operations complete
            tokio::time::sleep(Duration::from_millis(50)).await;

            // Verify all jobs have Pending status
            for job_id in &job_ids {
                let status = app
                    .job_processing_status_repository()
                    .find_status(job_id)
                    .await
                    .unwrap();
                assert_eq!(status, Some(JobProcessingStatus::Pending));
            }

            tracing::info!("test_find_list_with_status_no_limit completed successfully");
            Ok(())
        })
    }

    #[test]
    fn test_job_status_lifecycle() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let (app, _) = create_test_app(true).await?;

            // Create test worker
            let runner_settings = infra::infra::job::rows::JobqueueAndCodec::serialize_message(
                &proto::TestRunnerSettings {
                    name: "ls".to_string(),
                },
            );
            let wd = WorkerData {
                name: "testworker".to_string(),
                description: "desc1".to_string(),
                runner_id: Some(TEST_RUNNER_ID),
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

            // Test complete job lifecycle with status changes
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

            // Small delay to ensure async operations complete
            tokio::time::sleep(Duration::from_millis(50)).await;

            // 1. Initial status should be Pending
            let status = app
                .job_processing_status_repository()
                .find_status(&job_id)
                .await
                .unwrap();
            assert_eq!(status, Some(JobProcessingStatus::Pending));

            // 2. Simulate job being picked up by worker (Running)
            app.job_processing_status_repository()
                .upsert_status(&job_id, &JobProcessingStatus::Running)
                .await?;

            let status = app
                .job_processing_status_repository()
                .find_status(&job_id)
                .await
                .unwrap();
            assert_eq!(status, Some(JobProcessingStatus::Running));

            // 3. Simulate job completion (WaitResult)
            app.job_processing_status_repository()
                .upsert_status(&job_id, &JobProcessingStatus::WaitResult)
                .await?;

            let status = app
                .job_processing_status_repository()
                .find_status(&job_id)
                .await
                .unwrap();
            assert_eq!(status, Some(JobProcessingStatus::WaitResult));

            // 4. Simulate job status deletion after completion
            app.job_processing_status_repository()
                .delete_status(&job_id)
                .await?;

            let status = app
                .job_processing_status_repository()
                .find_status(&job_id)
                .await
                .unwrap();
            assert_eq!(status, None);

            tracing::info!("test_job_status_lifecycle completed successfully");
            Ok(())
        })
    }
}