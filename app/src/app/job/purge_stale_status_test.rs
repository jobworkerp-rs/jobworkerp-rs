//! Integration tests for purge_stale_job_processing_status
//!
//! Tests verify the orphaned_only and bulk purge modes of the PurgeStaleJobs API
//! at the app layer (RdbChanJobAppImpl).

#[cfg(test)]
mod purge_stale_status_tests {
    use crate::module::test::create_rdb_chan_test_app;

    use anyhow::Result;
    use infra::infra::job::status::JobProcessingStatusRepository;
    use infra_utils::infra::rdb::{RdbPool, UseRdbPool};
    use infra_utils::infra::test::TEST_RUNTIME;
    use jobworkerp_base::codec::UseProstCodec;
    use proto::jobworkerp::data::{
        JobProcessingStatus, QueueType, ResponseType, RunnerId, StreamingType, WorkerData,
    };
    use std::collections::HashMap;
    use std::sync::Arc;

    /// Poll until the given job_id appears in the RDB index table.
    async fn poll_until_indexed(pool: &RdbPool, job_id_value: i64) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let exists: Option<(i64,)> =
                sqlx::query_as("SELECT job_id FROM job_processing_status WHERE job_id = ?")
                    .bind(job_id_value)
                    .fetch_optional(pool)
                    .await
                    .unwrap();
            if exists.is_some() {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "Timed out waiting for job_id {} to appear in RDB index",
                job_id_value
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    fn test_worker_data(name: &str) -> WorkerData {
        let runner_settings = jobworkerp_base::codec::ProstMessageCodec::serialize_message(
            &proto::TestRunnerSettings {
                name: "ls".to_string(),
            },
        )
        .unwrap();
        WorkerData {
            name: name.to_string(),
            description: "Worker for purge test".to_string(),
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
        }
    }

    #[test]
    fn test_purge_stale_status_all_stale() -> Result<()> {
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

            let worker_id = app_module
                .worker_app
                .create(&test_worker_data("purge_all_stale"))
                .await?;
            let jargs =
                jobworkerp_base::codec::ProstMessageCodec::serialize_message(&proto::TestArgs {
                    args: vec!["/".to_string()],
                })?;

            // Enqueue a job (creates PENDING status + RDB index entry)
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
                    None, // overrides
                )
                .await?;

            // Wait for async indexing (poll instead of fixed sleep)
            let rdb_pool = index_repo.db_pool();
            poll_until_indexed(rdb_pool, job_id.value).await;

            // Backdate updated_at to make the record stale
            let stale_time = chrono::Utc::now().timestamp_millis() - 3600 * 1000 * 2;
            sqlx::query("UPDATE job_processing_status SET updated_at = ? WHERE job_id = ?")
                .bind(stale_time)
                .bind(job_id.value)
                .execute(rdb_pool)
                .await?;

            // Purge with orphaned_only=false (bulk mode, threshold=1 hour)
            let (purged_count, _cutoff) = app.purge_stale_job_processing_status(1, false).await?;

            assert!(
                purged_count >= 1,
                "Should purge at least 1 stale record, got {}",
                purged_count
            );

            // Verify the record is now marked as deleted
            let deleted_at: Option<Option<i64>> =
                sqlx::query_scalar("SELECT deleted_at FROM job_processing_status WHERE job_id = ?")
                    .bind(job_id.value)
                    .fetch_optional(rdb_pool)
                    .await?;

            assert!(
                matches!(deleted_at, Some(Some(_))),
                "Record should be logically deleted after purge"
            );

            Ok(())
        })
    }

    #[test]
    fn test_purge_stale_status_orphaned_only() -> Result<()> {
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

            let worker_id = app_module.worker_app.create(&test_worker_data("purge_orphaned")).await?;

            // Insert an orphan record directly into RDB index (no corresponding job/status)
            let orphan_job_id = proto::jobworkerp::data::JobId { value: 999999 };
            let now = chrono::Utc::now().timestamp_millis();
            let stale_time = now - 3600 * 1000 * 2;
            let rdb_pool = index_repo.db_pool();
            sqlx::query(
                "INSERT INTO job_processing_status (job_id, worker_id, status, channel, priority, enqueue_time, pending_time, version, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"
            )
            .bind(orphan_job_id.value)
            .bind(worker_id.value)
            .bind(JobProcessingStatus::Pending as i32)
            .bind("default")
            .bind(0)
            .bind(stale_time)
            .bind(stale_time)
            .bind(1i64)
            .bind(stale_time)
            .execute(rdb_pool)
            .await?;

            // Purge with orphaned_only=true (threshold=1 hour)
            let (purged_count, _cutoff) = app
                .purge_stale_job_processing_status(1, true)
                .await?;

            assert_eq!(
                purged_count, 1,
                "Should purge exactly 1 orphan record"
            );

            // Verify the orphan record is marked as deleted
            let deleted_at: Option<Option<i64>> =
                sqlx::query_scalar("SELECT deleted_at FROM job_processing_status WHERE job_id = ?")
                    .bind(orphan_job_id.value)
                    .fetch_optional(rdb_pool)
                    .await?;

            assert!(
                matches!(deleted_at, Some(Some(_))),
                "Orphan record should be logically deleted"
            );

            Ok(())
        })
    }

    #[test]
    fn test_purge_stale_status_orphaned_only_skips_active() -> Result<()> {
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

            let worker_id = app_module
                .worker_app
                .create(&test_worker_data("purge_skip_active"))
                .await?;
            let jargs =
                jobworkerp_base::codec::ProstMessageCodec::serialize_message(&proto::TestArgs {
                    args: vec!["/".to_string()],
                })?;

            // Enqueue a job (creates PENDING status in memory + RDB index)
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
                    None, // overrides
                )
                .await?;

            // Wait for async indexing (poll instead of fixed sleep)
            let rdb_pool = index_repo.db_pool();
            poll_until_indexed(rdb_pool, job_id.value).await;

            // Verify the job has PENDING status in memory
            assert_eq!(
                status_repo.find_status(&job_id).await?,
                Some(JobProcessingStatus::Pending)
            );

            // Backdate updated_at to make the RDB index record stale
            let stale_time = chrono::Utc::now().timestamp_millis() - 3600 * 1000 * 2;
            sqlx::query("UPDATE job_processing_status SET updated_at = ?, deleted_at = NULL WHERE job_id = ?")
                .bind(stale_time)
                .bind(job_id.value)
                .execute(rdb_pool)
                .await?;

            // Purge with orphaned_only=true (threshold=1 hour)
            // The job still has in-memory PENDING status, so it should be skipped
            let (purged_count, _cutoff) = app.purge_stale_job_processing_status(1, true).await?;

            assert_eq!(
                purged_count, 0,
                "Should not purge active job with in-memory status"
            );

            // Verify record is NOT marked as deleted
            let deleted_at: Option<Option<i64>> =
                sqlx::query_scalar("SELECT deleted_at FROM job_processing_status WHERE job_id = ?")
                    .bind(job_id.value)
                    .fetch_optional(rdb_pool)
                    .await?;

            assert!(
                matches!(deleted_at, Some(None)),
                "Active job record should NOT be deleted"
            );

            Ok(())
        })
    }

    #[test]
    fn test_purge_stale_status_requires_indexing() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let app_module = create_rdb_chan_test_app(true, false).await?;
            let app = &app_module.job_app;

            // Should return error when RDB indexing is disabled
            let result = app.purge_stale_job_processing_status(1, false).await;

            assert!(result.is_err(), "Should fail when RDB indexing is disabled");

            let err_msg = result.unwrap_err().to_string();
            assert!(
                err_msg.contains("JOB_STATUS_RDB_INDEXING"),
                "Error should mention JOB_STATUS_RDB_INDEXING, got: {}",
                err_msg
            );

            Ok(())
        })
    }
}
