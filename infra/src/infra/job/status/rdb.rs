use anyhow::Result;
use command_utils::util::datetime;
use infra_utils::infra::rdb::{RdbPool, UseRdbPool};
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_base::job_status_config::JobStatusConfig;
use proto::jobworkerp::data::{JobId, JobProcessingStatus, WorkerId};
use std::sync::Arc;

/// RDB-specific JobProcessingStatus index repository
///
/// # Responsibilities
/// - INSERT/UPDATE/DELETE operations on job_processing_status table
/// - Advanced search functionality (FindByCondition)
/// - Cleanup of logically deleted records
///
/// # Non-responsibilities
/// - Does NOT implement JobProcessingStatusRepository trait (different purpose)
/// - Does NOT save to Redis/Memory (that's RedisRepository's responsibility)
#[derive(Clone, Debug)]
pub struct RdbJobProcessingStatusIndexRepository {
    rdb_pool: Arc<RdbPool>,
    config: JobStatusConfig,
}

impl RdbJobProcessingStatusIndexRepository {
    pub fn new(rdb_pool: Arc<RdbPool>, config: JobStatusConfig) -> Self {
        Self { rdb_pool, config }
    }

    /// Get RDB pool reference (for testing purposes)
    #[cfg(any(test, feature = "test-utils"))]
    pub fn rdb_pool(&self) -> &Arc<RdbPool> {
        &self.rdb_pool
    }

    /// Check if RDB indexing feature is enabled
    pub fn is_enabled(&self) -> bool {
        self.config.rdb_indexing_enabled
    }

    /// Index job status to RDB (expected to be called asynchronously)
    ///
    /// # Arguments
    /// - All fields are explicitly passed (not constrained by trait)
    ///
    /// # Returns
    /// - `Ok(())`: Success (including when feature is disabled)
    /// - `Err(e)`: RDB error (warn log recommended, continue processing)
    #[allow(clippy::too_many_arguments)]
    pub async fn index_status(
        &self,
        job_id: &JobId,
        status: &JobProcessingStatus,
        worker_id: &WorkerId,
        channel: &str,
        priority: i32,
        enqueue_time: i64,
        is_streamable: bool,
        broadcast_results: bool,
    ) -> Result<()> {
        // Do nothing if feature is disabled
        if !self.config.rdb_indexing_enabled {
            return Ok(());
        }

        let now = datetime::now_millis();
        let mut conn = self.rdb_pool.acquire().await?;

        match status {
            JobProcessingStatus::Pending => {
                // CREATE: Don't create if deleted record exists
                let rows_affected = sqlx::query(
                    "INSERT INTO job_processing_status
                     (job_id, status, worker_id, channel, priority, enqueue_time,
                      pending_time, is_streamable, broadcast_results, version, updated_at)
                     SELECT ?, 1, ?, ?, ?, ?, ?, ?, ?, 1, ?
                     WHERE NOT EXISTS (
                       SELECT 1 FROM job_processing_status
                       WHERE job_id = ? AND deleted_at IS NOT NULL
                     )",
                )
                .bind(job_id.value)
                .bind(worker_id.value)
                .bind(channel)
                .bind(priority)
                .bind(enqueue_time)
                .bind(now)
                .bind(is_streamable)
                .bind(broadcast_results)
                .bind(now)
                .bind(job_id.value)
                .execute(&mut *conn)
                .await?
                .rows_affected();

                if rows_affected == 0 {
                    tracing::warn!(
                        job_id = job_id.value,
                        "Failed to insert PENDING status: deleted record exists"
                    );
                }
            }

            JobProcessingStatus::Running => {
                // UPDATE: Only allow PENDING→RUNNING transition (optimistic lock)
                let rows_affected = sqlx::query(
                    "UPDATE job_processing_status
                     SET status = 2, start_time = ?, version = version + 1, updated_at = ?
                     WHERE job_id = ? AND status = 1 AND deleted_at IS NULL",
                )
                .bind(now)
                .bind(now)
                .bind(job_id.value)
                .execute(&mut *conn)
                .await?
                .rows_affected();

                if rows_affected == 0 {
                    tracing::warn!(
                        job_id = job_id.value,
                        "Failed to update RUNNING status: not in PENDING state or already deleted"
                    );
                }
            }

            JobProcessingStatus::WaitResult => {
                // Logical deletion (RUNNING → WAIT_RESULT)
                sqlx::query(
                    "UPDATE job_processing_status
                     SET status = 3, deleted_at = ?, version = version + 1, updated_at = ?
                     WHERE job_id = ? AND deleted_at IS NULL",
                )
                .bind(now)
                .bind(now)
                .bind(job_id.value)
                .execute(&mut *conn)
                .await?;
            }

            JobProcessingStatus::Cancelling => {
                // Logical deletion (PENDING or RUNNING → CANCELLING)
                sqlx::query(
                    "UPDATE job_processing_status
                     SET status = 4, deleted_at = ?, version = version + 1, updated_at = ?
                     WHERE job_id = ? AND deleted_at IS NULL",
                )
                .bind(now)
                .bind(now)
                .bind(job_id.value)
                .execute(&mut *conn)
                .await?;
            }

            _ => {
                // Unknown etc: do nothing
            }
        }

        Ok(())
    }

    /// Update job status in RDB index by job_id
    ///
    /// This is a simplified status update operation that only requires job_id.
    /// Use this when updating status where status metadata is no longer available.
    ///
    /// # Returns
    /// - `Ok(())`: Success (including when feature is disabled)
    /// - `Err(e)`: RDB error
    pub async fn update_status_by_job_id(
        &self,
        job_id: &JobId,
        status: &JobProcessingStatus,
    ) -> Result<()> {
        // Do nothing if feature is disabled
        if !self.config.rdb_indexing_enabled {
            return Ok(());
        }

        let now = datetime::now_millis();
        let mut conn = self.rdb_pool.acquire().await?;
        let status_code = *status as i32;

        match status {
            JobProcessingStatus::Running => {
                // Preserve monitoring fields when the dispatcher only knows job_id.
                // start_time should be populated for elapsed-time queries, and
                // version should be bumped to keep in sync with the Redis path.
                sqlx::query(
                    "UPDATE job_processing_status
                     SET status = ?, start_time = COALESCE(start_time, ?), version = version + 1, updated_at = ?
                     WHERE job_id = ? AND deleted_at IS NULL",
                )
                .bind(status_code)
                .bind(now)
                .bind(now)
                .bind(job_id.value)
                .execute(&mut *conn)
                .await?;
            }
            _ => {
                // Generic status update (used by cancellation path etc.)
                sqlx::query(
                    "UPDATE job_processing_status
                     SET status = ?, version = version + 1, updated_at = ?
                     WHERE job_id = ? AND deleted_at IS NULL",
                )
                .bind(status_code)
                .bind(now)
                .bind(job_id.value)
                .execute(&mut *conn)
                .await?;
            }
        }
        Ok(())
    }

    /// Mark job as logically deleted in RDB index (set deleted_at timestamp)
    ///
    /// This is a simplified deletion operation that only requires job_id.
    /// Use this when cleaning up jobs where status metadata is no longer available.
    ///
    /// # Returns
    /// - `Ok(())`: Success (including when feature is disabled)
    /// - `Err(e)`: RDB error
    pub async fn mark_deleted_by_job_id(&self, job_id: &JobId) -> Result<()> {
        // Do nothing if feature is disabled
        if !self.config.rdb_indexing_enabled {
            return Ok(());
        }

        let now = datetime::now_millis();
        let mut conn = self.rdb_pool.acquire().await?;

        // Set deleted_at if not already deleted
        sqlx::query(
            "UPDATE job_processing_status
             SET deleted_at = ?, updated_at = ?
             WHERE job_id = ? AND deleted_at IS NULL",
        )
        .bind(now)
        .bind(now)
        .bind(job_id.value)
        .execute(&mut *conn)
        .await?;

        Ok(())
    }

    /// Advanced search (for FindByCondition)
    ///
    /// # Error
    /// - When feature is disabled: `Err("JOB_STATUS_RDB_INDEXING is disabled")`
    #[allow(clippy::too_many_arguments)]
    pub async fn find_by_condition(
        &self,
        status: Option<JobProcessingStatus>,
        worker_id: Option<i64>,
        channel: Option<String>,
        min_elapsed_time_ms: Option<i64>,
        limit: i32,
        offset: i32,
        descending: bool,
    ) -> Result<Vec<JobProcessingStatusDetail>> {
        // Error if feature is disabled
        if !self.config.rdb_indexing_enabled {
            return Err(anyhow::anyhow!(
                "Advanced job status search is disabled. \
                 Enable JOB_STATUS_RDB_INDEXING=true to use this feature."
            ));
        }

        let now = datetime::now_millis();
        let mut conn = self.rdb_pool.acquire().await?;

        // Build query using QueryBuilder for type-safe dynamic query construction
        let mut query_builder = sqlx::QueryBuilder::new(
            "SELECT job_id, status, worker_id, channel, priority, enqueue_time, \
                    start_time, pending_time, is_streamable, broadcast_results, updated_at \
             FROM job_processing_status \
             WHERE deleted_at IS NULL",
        );

        // Add dynamic filters with automatic binding
        if let Some(s) = status {
            query_builder.push(" AND status = ").push_bind(s as i32);
        }
        if let Some(w) = worker_id {
            query_builder.push(" AND worker_id = ").push_bind(w);
        }
        if let Some(ref c) = channel {
            query_builder.push(" AND channel = ").push_bind(c);
        }
        if let Some(elapsed) = min_elapsed_time_ms {
            let cutoff = now - elapsed;
            query_builder
                .push(" AND start_time IS NOT NULL AND start_time < ")
                .push_bind(cutoff);
        }

        // ORDER BY
        let order = if descending { "DESC" } else { "ASC" };
        query_builder.push(format!(" ORDER BY start_time {order}"));

        // LIMIT/OFFSET
        query_builder.push(" LIMIT ").push_bind(limit);
        query_builder.push(" OFFSET ").push_bind(offset);

        // Execute query with type-safe row mapping
        let rows: Vec<JobProcessingStatusDetailRow> = query_builder
            .build_query_as::<JobProcessingStatusDetailRow>()
            .fetch_all(&mut *conn)
            .await
            .map_err(JobWorkerError::DBError)?;

        // Convert rows to domain objects
        Ok(rows
            .into_iter()
            .map(|row| JobProcessingStatusDetail {
                job_id: JobId { value: row.job_id },
                status: JobProcessingStatus::try_from(row.status)
                    .unwrap_or(JobProcessingStatus::Unknown),
                worker_id: row.worker_id,
                channel: row.channel,
                priority: row.priority,
                enqueue_time: row.enqueue_time,
                start_time: row.start_time,
                pending_time: row.pending_time,
                is_streamable: row.is_streamable,
                broadcast_results: row.broadcast_results,
                updated_at: row.updated_at,
            })
            .collect())
    }

    /// Physical deletion of logically deleted records (called from cleanup task)
    ///
    /// # Returns
    /// - `Ok(u64)`: Number of deleted records
    pub async fn cleanup_deleted_records(&self) -> Result<u64> {
        if !self.config.rdb_indexing_enabled {
            return Ok(0);
        }

        let cutoff_millis =
            datetime::now_millis() - (self.config.retention_hours * 3600 * 1000) as i64;

        let mut conn = self.rdb_pool.acquire().await?;
        let result = sqlx::query(
            "DELETE FROM job_processing_status
             WHERE deleted_at IS NOT NULL AND deleted_at < ?",
        )
        .bind(cutoff_millis)
        .execute(&mut *conn)
        .await?;

        Ok(result.rows_affected())
    }
}

impl UseRdbPool for RdbJobProcessingStatusIndexRepository {
    fn db_pool(&self) -> &RdbPool {
        &self.rdb_pool
    }
}

/// Search result detail (corresponds to Proto definition)
#[derive(Debug, Clone)]
pub struct JobProcessingStatusDetail {
    pub job_id: JobId,
    pub status: JobProcessingStatus,
    pub worker_id: i64,
    pub channel: String,
    pub priority: i32,
    pub enqueue_time: i64,
    pub start_time: Option<i64>,
    pub pending_time: Option<i64>,
    pub is_streamable: bool,
    pub broadcast_results: bool,
    pub updated_at: i64,
}

/// Row type for sqlx query mapping
#[derive(Debug, sqlx::FromRow)]
struct JobProcessingStatusDetailRow {
    job_id: i64,
    status: i32,
    worker_id: i64,
    channel: String,
    priority: i32,
    enqueue_time: i64,
    start_time: Option<i64>,
    pending_time: Option<i64>,
    is_streamable: bool,
    broadcast_results: bool,
    updated_at: i64,
}

/// Trait for DI of RdbJobProcessingStatusIndexRepository (optional)
pub trait UseRdbJobProcessingStatusIndexRepository {
    fn rdb_job_processing_status_index_repository(
        &self,
    ) -> Option<Arc<RdbJobProcessingStatusIndexRepository>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use infra_utils::infra::test::{setup_test_rdb_from, TEST_RUNTIME};

    async fn setup_test_db() -> &'static RdbPool {
        let pool = if cfg!(feature = "mysql") {
            setup_test_rdb_from("sql/mysql").await
        } else {
            setup_test_rdb_from("sql/sqlite").await
        };

        // Apply migration if not already applied
        let _ = sqlx::query(
            "CREATE TABLE IF NOT EXISTS job_processing_status (
                job_id BIGINT PRIMARY KEY,
                status INT NOT NULL,
                worker_id BIGINT NOT NULL,
                channel TEXT NOT NULL,
                priority INT NOT NULL,
                enqueue_time BIGINT NOT NULL,
                pending_time BIGINT,
                start_time BIGINT,
                is_streamable BOOLEAN NOT NULL DEFAULT 0,
                broadcast_results BOOLEAN NOT NULL DEFAULT 0,
                version BIGINT NOT NULL,
                deleted_at BIGINT,
                updated_at BIGINT NOT NULL
            )",
        )
        .execute(pool)
        .await;

        // Clean up test data
        let _ = sqlx::query("DELETE FROM job_processing_status")
            .execute(pool)
            .await;

        pool
    }

    #[test]
    fn test_config_is_enabled() {
        let config_enabled = JobStatusConfig {
            rdb_indexing_enabled: true,
            cleanup_interval_hours: 1,
            retention_hours: 24,
        };

        let config_disabled = JobStatusConfig {
            rdb_indexing_enabled: false,
            cleanup_interval_hours: 1,
            retention_hours: 24,
        };

        assert!(config_enabled.rdb_indexing_enabled);
        assert!(!config_disabled.rdb_indexing_enabled);
    }

    #[test]
    fn test_index_status_disabled() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_db().await;
            let config = JobStatusConfig {
                rdb_indexing_enabled: false,
                cleanup_interval_hours: 1,
                retention_hours: 24,
            };

            let repo = RdbJobProcessingStatusIndexRepository::new(Arc::new(pool.clone()), config);

            // When disabled, should succeed without doing anything
            let result = repo
                .index_status(
                    &JobId { value: 1 },
                    &JobProcessingStatus::Pending,
                    &WorkerId { value: 1 },
                    "test",
                    0,
                    datetime::now_millis(),
                    false,
                    false,
                )
                .await;

            assert!(result.is_ok());
            Ok(())
        })
    }

    #[test]
    fn test_index_status_pending() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_db().await;
            let config = JobStatusConfig {
                rdb_indexing_enabled: true,
                cleanup_interval_hours: 1,
                retention_hours: 24,
            };

            let repo = RdbJobProcessingStatusIndexRepository::new(Arc::new(pool.clone()), config);

            // Insert PENDING status
            let result = repo
                .index_status(
                    &JobId { value: 100 },
                    &JobProcessingStatus::Pending,
                    &WorkerId { value: 1 },
                    "test_channel",
                    10,
                    datetime::now_millis(),
                    false,
                    false,
                )
                .await;

            assert!(result.is_ok());

            // Verify record was created
            let count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM job_processing_status WHERE job_id = 100")
                    .fetch_one(pool)
                    .await?;

            assert_eq!(count, 1);
            Ok(())
        })
    }

    #[test]
    fn test_find_by_condition_disabled() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_db().await;
            let config = JobStatusConfig {
                rdb_indexing_enabled: false,
                cleanup_interval_hours: 1,
                retention_hours: 24,
            };

            let repo = RdbJobProcessingStatusIndexRepository::new(Arc::new(pool.clone()), config);

            let result = repo
                .find_by_condition(None, None, None, None, 100, 0, true)
                .await;

            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("disabled"));
            Ok(())
        })
    }

    #[test]
    fn test_cleanup_deleted_records() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_db().await;
            let config = JobStatusConfig {
                rdb_indexing_enabled: true,
                cleanup_interval_hours: 1,
                retention_hours: 24,
            };

            let repo = RdbJobProcessingStatusIndexRepository::new(Arc::new(pool.clone()), config);

            // Insert a deleted record with old timestamp (25 hours ago)
            let old_timestamp = datetime::now_millis() - (25 * 3600 * 1000);
            sqlx::query(
                "INSERT INTO job_processing_status
                 (job_id, status, worker_id, channel, priority, enqueue_time, version, deleted_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"
            )
            .bind(200)
            .bind(3) // WAIT_RESULT
            .bind(1)
            .bind("test")
            .bind(0)
            .bind(old_timestamp)
            .bind(1)
            .bind(old_timestamp)
            .bind(old_timestamp)
            .execute(pool)
            .await?;

            // Run cleanup
            let deleted_count = repo.cleanup_deleted_records().await?;

            assert_eq!(deleted_count, 1);
            Ok(())
        })
    }

    #[test]
    fn test_index_status_running_requires_pending() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_db().await;
            let config = JobStatusConfig {
                rdb_indexing_enabled: true,
                cleanup_interval_hours: 1,
                retention_hours: 24,
            };

            let repo = RdbJobProcessingStatusIndexRepository::new(Arc::new(pool.clone()), config);

            // Try to update to RUNNING without PENDING record (optimistic lock failure)
            let result = repo
                .index_status(
                    &JobId { value: 300 },
                    &JobProcessingStatus::Running,
                    &WorkerId { value: 1 },
                    "test_channel",
                    10,
                    datetime::now_millis(),
                    false,
                    false,
                )
                .await;

            // Should succeed (no error) but log warning
            assert!(result.is_ok());

            // Verify no record was created
            let count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM job_processing_status WHERE job_id = 300")
                    .fetch_one(pool)
                    .await?;

            assert_eq!(count, 0);
            Ok(())
        })
    }

    #[test]
    fn test_index_status_skips_if_deleted_exists() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_db().await;
            let config = JobStatusConfig {
                rdb_indexing_enabled: true,
                cleanup_interval_hours: 1,
                retention_hours: 24,
            };

            let repo = RdbJobProcessingStatusIndexRepository::new(Arc::new(pool.clone()), config);

            // Insert a deleted record
            let now = datetime::now_millis();
            sqlx::query(
                "INSERT INTO job_processing_status
                 (job_id, status, worker_id, channel, priority, enqueue_time, version, deleted_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"
            )
            .bind(400)
            .bind(3) // WAIT_RESULT
            .bind(1)
            .bind("test")
            .bind(0)
            .bind(now)
            .bind(1)
            .bind(now)
            .bind(now)
            .execute(pool)
            .await?;

            // Try to insert PENDING status (should be skipped due to deleted record)
            let result = repo
                .index_status(
                    &JobId { value: 400 },
                    &JobProcessingStatus::Pending,
                    &WorkerId { value: 1 },
                    "test_channel",
                    10,
                    now,
                    false,
                    false,
                )
                .await;

            // Should succeed but not insert
            assert!(result.is_ok());

            // Verify only one record exists (the deleted one)
            let count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM job_processing_status WHERE job_id = 400")
                    .fetch_one(pool)
                    .await?;

            assert_eq!(count, 1);
            Ok(())
        })
    }

    #[test]
    fn test_find_by_condition_with_status_filter() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_db().await;
            let config = JobStatusConfig {
                rdb_indexing_enabled: true,
                cleanup_interval_hours: 1,
                retention_hours: 24,
            };

            let repo = RdbJobProcessingStatusIndexRepository::new(Arc::new(pool.clone()), config);

            // Insert PENDING job
            repo.index_status(
                &JobId { value: 500 },
                &JobProcessingStatus::Pending,
                &WorkerId { value: 1 },
                "test_channel",
                10,
                datetime::now_millis(),
                false,
                false,
            )
            .await?;

            // Insert RUNNING job
            repo.index_status(
                &JobId { value: 501 },
                &JobProcessingStatus::Pending,
                &WorkerId { value: 1 },
                "test_channel",
                10,
                datetime::now_millis(),
                false,
                false,
            )
            .await?;

            repo.index_status(
                &JobId { value: 501 },
                &JobProcessingStatus::Running,
                &WorkerId { value: 1 },
                "test_channel",
                10,
                datetime::now_millis(),
                false,
                false,
            )
            .await?;

            // Search for PENDING jobs only
            let results = repo
                .find_by_condition(
                    Some(JobProcessingStatus::Pending),
                    None,
                    None,
                    None,
                    100,
                    0,
                    true,
                )
                .await?;

            assert_eq!(results.len(), 1);
            assert_eq!(results[0].job_id.value, 500);
            assert_eq!(results[0].status, JobProcessingStatus::Pending);
            Ok(())
        })
    }

    #[test]
    fn test_find_by_condition_with_worker_id_filter() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_db().await;
            let config = JobStatusConfig {
                rdb_indexing_enabled: true,
                cleanup_interval_hours: 1,
                retention_hours: 24,
            };

            let repo = RdbJobProcessingStatusIndexRepository::new(Arc::new(pool.clone()), config);

            // Insert jobs with different worker_ids
            repo.index_status(
                &JobId { value: 600 },
                &JobProcessingStatus::Pending,
                &WorkerId { value: 1 },
                "test_channel",
                10,
                datetime::now_millis(),
                false,
                false,
            )
            .await?;

            repo.index_status(
                &JobId { value: 601 },
                &JobProcessingStatus::Pending,
                &WorkerId { value: 2 },
                "test_channel",
                10,
                datetime::now_millis(),
                false,
                false,
            )
            .await?;

            // Search for worker_id=1 only
            let results = repo
                .find_by_condition(None, Some(1), None, None, 100, 0, true)
                .await?;

            assert_eq!(results.len(), 1);
            assert_eq!(results[0].job_id.value, 600);
            assert_eq!(results[0].worker_id, 1);
            Ok(())
        })
    }

    #[test]
    fn test_update_status_by_job_id() -> Result<()> {
        // Test updating status by job_id without fetching job data
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_db().await;
            let config = JobStatusConfig {
                rdb_indexing_enabled: true,
                cleanup_interval_hours: 1,
                retention_hours: 24,
            };
            let repo = RdbJobProcessingStatusIndexRepository::new(Arc::new(pool.clone()), config);

            let job_id = JobId { value: 100 };
            let worker_id = WorkerId { value: 1 };

            // First, create a PENDING status
            repo.index_status(
                &job_id,
                &JobProcessingStatus::Pending,
                &worker_id,
                "test_channel",
                1,
                100,
                false,
                false,
            )
            .await?;

            // Update to RUNNING using update_status_by_job_id
            repo.update_status_by_job_id(&job_id, &JobProcessingStatus::Running)
                .await?;

            // Verify the status was updated and monitoring columns populated
            let query =
                "SELECT status, start_time, version FROM job_processing_status WHERE job_id = ?";
            let (status, start_time, version): (i32, Option<i64>, i64) = sqlx::query_as(query)
                .bind(job_id.value)
                .fetch_one(pool)
                .await?;

            assert_eq!(status, JobProcessingStatus::Running as i32);
            assert!(start_time.is_some(), "start_time should be set for RUNNING");
            assert_eq!(
                version, 2,
                "version should bump when transitioning to RUNNING"
            );
            Ok(())
        })
    }

    #[test]
    fn test_mark_deleted_by_job_id() -> Result<()> {
        // Test marking job as deleted by job_id
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_db().await;
            let config = JobStatusConfig {
                rdb_indexing_enabled: true,
                cleanup_interval_hours: 1,
                retention_hours: 24,
            };
            let repo = RdbJobProcessingStatusIndexRepository::new(Arc::new(pool.clone()), config);

            let job_id = JobId { value: 200 };
            let worker_id = WorkerId { value: 1 };

            // First, create a PENDING status
            repo.index_status(
                &job_id,
                &JobProcessingStatus::Pending,
                &worker_id,
                "test_channel",
                1,
                200,
                false,
                false,
            )
            .await?;

            // Mark as deleted using mark_deleted_by_job_id
            repo.mark_deleted_by_job_id(&job_id).await?;

            // Verify deleted_at was set
            let query = "SELECT deleted_at FROM job_processing_status WHERE job_id = ?";
            let deleted_at: Option<i64> = sqlx::query_scalar(query)
                .bind(job_id.value)
                .fetch_one(pool)
                .await?;

            assert!(deleted_at.is_some(), "deleted_at should be set");
            assert!(
                deleted_at.unwrap() > 0,
                "deleted_at should be a valid timestamp"
            );
            Ok(())
        })
    }

    #[test]
    fn test_update_status_by_job_id_only_updates_non_deleted() -> Result<()> {
        // Test that update_status_by_job_id only updates non-deleted records
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_db().await;
            let config = JobStatusConfig {
                rdb_indexing_enabled: true,
                cleanup_interval_hours: 1,
                retention_hours: 24,
            };
            let repo = RdbJobProcessingStatusIndexRepository::new(Arc::new(pool.clone()), config);

            let job_id = JobId { value: 300 };
            let worker_id = WorkerId { value: 1 };

            // Create a PENDING status
            repo.index_status(
                &job_id,
                &JobProcessingStatus::Pending,
                &worker_id,
                "test_channel",
                1,
                300,
                false,
                false,
            )
            .await?;

            // Mark as deleted
            repo.mark_deleted_by_job_id(&job_id).await?;

            // Try to update status (should not affect deleted records)
            repo.update_status_by_job_id(&job_id, &JobProcessingStatus::Running)
                .await?;

            // Verify status was NOT updated (should still be PENDING)
            let query = "SELECT status FROM job_processing_status WHERE job_id = ?";
            let status: i32 = sqlx::query_scalar(query)
                .bind(job_id.value)
                .fetch_one(pool)
                .await?;

            assert_eq!(
                status,
                JobProcessingStatus::Pending as i32,
                "Status should not change for deleted records"
            );
            Ok(())
        })
    }

    #[test]
    fn test_mark_deleted_by_job_id_idempotent() -> Result<()> {
        // Test that mark_deleted_by_job_id is idempotent
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_db().await;
            let config = JobStatusConfig {
                rdb_indexing_enabled: true,
                cleanup_interval_hours: 1,
                retention_hours: 24,
            };
            let repo = RdbJobProcessingStatusIndexRepository::new(Arc::new(pool.clone()), config);

            let job_id = JobId { value: 400 };
            let worker_id = WorkerId { value: 1 };

            // Create a PENDING status
            repo.index_status(
                &job_id,
                &JobProcessingStatus::Pending,
                &worker_id,
                "test_channel",
                1,
                400,
                false,
                false,
            )
            .await?;

            // Mark as deleted first time
            repo.mark_deleted_by_job_id(&job_id).await?;

            let query = "SELECT deleted_at FROM job_processing_status WHERE job_id = ?";
            let first_deleted_at: Option<i64> = sqlx::query_scalar(query)
                .bind(job_id.value)
                .fetch_one(pool)
                .await?;

            // Small delay to ensure timestamp would differ if updated
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

            // Mark as deleted second time (should be idempotent)
            repo.mark_deleted_by_job_id(&job_id).await?;

            let second_deleted_at: Option<i64> = sqlx::query_scalar(query)
                .bind(job_id.value)
                .fetch_one(pool)
                .await?;

            // Both calls should result in same timestamp (only updates if deleted_at IS NULL)
            assert_eq!(
                first_deleted_at, second_deleted_at,
                "Multiple mark_deleted calls should be idempotent"
            );
            Ok(())
        })
    }
}
