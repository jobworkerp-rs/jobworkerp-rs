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
    use infra::infra::job::status::UseJobProcessingStatusRepository;
    use infra_utils::infra::test::TEST_RUNTIME;
    use jobworkerp_base::codec::UseProstCodec;
    use proto::jobworkerp::data::{
        JobId, JobProcessingStatus, QueueType, ResponseType, RunnerId, StreamingType, WorkerData,
    };
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    const TEST_RUNNER_ID: RunnerId = RunnerId { value: 100000000 };

    #[test]
    fn test_find_list_with_status_all_statuses() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let (app, _) = create_test_app(true).await?;

            let runner_settings = jobworkerp_base::codec::ProstMessageCodec::serialize_message(
                &proto::TestRunnerSettings {
                    name: "ls".to_string(),
                },
            )?;
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
                jobworkerp_base::codec::ProstMessageCodec::serialize_message(&proto::TestArgs {
                    args: vec!["/".to_string()],
                })?;

            // Test finding jobs with different statuses
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
                    StreamingType::None,
                    None, // using
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

            let runner_settings = jobworkerp_base::codec::ProstMessageCodec::serialize_message(
                &proto::TestRunnerSettings {
                    name: "ls".to_string(),
                },
            )?;
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
                jobworkerp_base::codec::ProstMessageCodec::serialize_message(&proto::TestArgs {
                    args: vec!["/".to_string()],
                })?;

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
                        StreamingType::None,
                        None, // using
                    )
                    .await?;

                assert!(job_id.value > 0);
                assert!(res.is_none());
                job_ids.push(job_id);
            }

            // Small delay to ensure async operations complete
            tokio::time::sleep(Duration::from_millis(50)).await;

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

            let runner_settings = jobworkerp_base::codec::ProstMessageCodec::serialize_message(
                &proto::TestRunnerSettings {
                    name: "ls".to_string(),
                },
            )?;
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
                jobworkerp_base::codec::ProstMessageCodec::serialize_message(&proto::TestArgs {
                    args: vec!["/".to_string()],
                })?;

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
                    StreamingType::None,
                    None, // using
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

    #[test]
    fn test_running_job_visibility_with_individual_ttl() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let (app, _) = create_test_app(true).await?;

            let runner_settings = jobworkerp_base::codec::ProstMessageCodec::serialize_message(
                &proto::TestRunnerSettings {
                    name: "sleep".to_string(),
                },
            )?;
            let wd = WorkerData {
                name: "long_running_worker".to_string(),
                description: "Worker for testing running job visibility".to_string(),
                runner_id: Some(TEST_RUNNER_ID),
                runner_settings,
                channel: None,
                response_type: ResponseType::NoResult as i32,
                periodic_interval: 0,
                retry_policy: None,
                queue_type: QueueType::Normal as i32, // Critical: Normal queue type
                store_failure: false,
                store_success: false,
                use_static: false,
                broadcast_results: false,
            };

            let worker_id = app.worker_app().create(&wd).await?;
            let jargs =
                jobworkerp_base::codec::ProstMessageCodec::serialize_message(&proto::TestArgs {
                    args: vec!["10".to_string()], // Sleep 10 seconds
                })?;

            // Enqueue long-running job
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
                    30000, // 30 second timeout
                    None,
                    StreamingType::None,
                    None, // using
                )
                .await?;

            assert!(job_id.value > 0);
            assert!(res.is_none());

            // Small delay to ensure enqueue operations complete
            tokio::time::sleep(Duration::from_millis(100)).await;

            let pending_jobs = app
                .find_list_with_processing_status(JobProcessingStatus::Pending, Some(&10))
                .await?;

            let found_pending = pending_jobs
                .iter()
                .any(|(job, _)| job.id.as_ref().map(|id| id.value) == Some(job_id.value));
            assert!(found_pending, "Job should be found in Pending status");

            // Simulate job being picked up by worker (transition to Running)
            app.job_processing_status_repository()
                .upsert_status(&job_id, &JobProcessingStatus::Running)
                .await?;

            let found_job = app.find_job(&job_id).await?;
            assert!(
                found_job.is_some(),
                "Running job should be findable via individual TTL key"
            );

            let running_jobs = app
                .find_list_with_processing_status(JobProcessingStatus::Running, Some(&10))
                .await?;

            let found_running = running_jobs.iter().any(|(job, status)| {
                job.id.as_ref().map(|id| id.value) == Some(job_id.value)
                    && *status == JobProcessingStatus::Running
            });
            assert!(
                found_running,
                "Running job should be visible in find_list_with_processing_status"
            );

            if let Some((found_job, status)) = running_jobs
                .iter()
                .find(|(job, _)| job.id.as_ref().map(|id| id.value) == Some(job_id.value))
            {
                assert_eq!(*status, JobProcessingStatus::Running);
                assert_eq!(found_job.data.as_ref().unwrap().timeout, 30000);
            }

            tracing::info!(
                "test_running_job_visibility_with_individual_ttl completed successfully"
            );
            Ok(())
        })
    }

    #[test]
    fn test_find_list_with_processing_status_performance() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let (app, _) = create_test_app(true).await?;

            let runner_settings = jobworkerp_base::codec::ProstMessageCodec::serialize_message(
                &proto::TestRunnerSettings {
                    name: "echo".to_string(),
                },
            )?;
            let wd = WorkerData {
                name: "perf_test_worker".to_string(),
                description: "Worker for performance testing".to_string(),
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
                jobworkerp_base::codec::ProstMessageCodec::serialize_message(&proto::TestArgs {
                    args: vec!["test".to_string()],
                })?;

            let metadata = Arc::new(HashMap::new());
            let mut job_ids = Vec::new();

            let start_time = std::time::Instant::now();

            for i in 0..10 {
                let (job_id, _, _) = app
                    .enqueue_job(
                        metadata.clone(),
                        Some(&worker_id),
                        None,
                        jargs.clone(),
                        Some(format!("perf_test_{i}")),
                        0,
                        0,
                        5000,
                        None,
                        StreamingType::None,
                        None, // using
                    )
                    .await?;
                job_ids.push(job_id);
            }

            let enqueue_time = start_time.elapsed();
            tracing::info!("Enqueued 10 jobs in {:?}", enqueue_time);

            // Small delay to ensure all jobs are enqueued
            tokio::time::sleep(Duration::from_millis(100)).await;

            // Test find_list_with_processing_status performance
            let find_start = std::time::Instant::now();
            let pending_jobs = app
                .find_list_with_processing_status(JobProcessingStatus::Pending, Some(&20))
                .await?;
            let find_time = find_start.elapsed();

            tracing::info!(
                "Found {} Pending jobs in {:?}",
                pending_jobs.len(),
                find_time
            );

            let found_count = job_ids
                .iter()
                .filter(|job_id| {
                    pending_jobs
                        .iter()
                        .any(|(job, _)| job.id.as_ref().map(|id| id.value) == Some(job_id.value))
                })
                .count();

            assert_eq!(
                found_count, 10,
                "All 10 test jobs should be found in Pending status"
            );

            // Performance assertion: should complete within reasonable time
            assert!(
                find_time < Duration::from_secs(2),
                "find_list_with_processing_status should complete within 2 seconds, took {find_time:?}"
            );

            tracing::info!(
                "test_find_list_with_processing_status_performance completed successfully"
            );
            Ok(())
        })
    }
}
