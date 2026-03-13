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
    config: Arc<JobStatusConfig>,
}

impl RdbJobProcessingStatusIndexRepository {
    pub fn new(rdb_pool: Arc<RdbPool>, config: Arc<JobStatusConfig>) -> Self {
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
                // CREATE: Don't create if record already exists with status >= RUNNING (2)
                // This prevents async race condition where late PENDING insert overwrites RUNNING
                #[cfg(feature = "mysql")]
                let rows_affected = sqlx::query(
                    "INSERT INTO job_processing_status
                     (job_id, status, worker_id, channel, priority, enqueue_time,
                      pending_time, is_streamable, broadcast_results, version, updated_at)
                     SELECT ?, 1, ?, ?, ?, ?, ?, ?, ?, 1, ? FROM DUAL
                     WHERE NOT EXISTS (
                       SELECT 1 FROM job_processing_status
                       WHERE job_id = ? AND (deleted_at IS NOT NULL OR status >= 2)
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

                #[cfg(not(feature = "mysql"))]
                let rows_affected = sqlx::query(
                    "INSERT INTO job_processing_status
                     (job_id, status, worker_id, channel, priority, enqueue_time,
                      pending_time, is_streamable, broadcast_results, version, updated_at)
                     SELECT ?, 1, ?, ?, ?, ?, ?, ?, ?, 1, ?
                     WHERE NOT EXISTS (
                       SELECT 1 FROM job_processing_status
                       WHERE job_id = ? AND (deleted_at IS NOT NULL OR status >= 2)
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
                    tracing::debug!(
                        job_id = job_id.value,
                        "Skipped PENDING insert: record already exists with higher status or deleted"
                    );
                }
            }

            JobProcessingStatus::Running => {
                // UPSERT: Insert or update to RUNNING status
                // Handles async race condition where RUNNING update arrives before PENDING insert
                #[cfg(feature = "mysql")]
                {
                    // IMPORTANT: start_time must be updated BEFORE status because MySQL evaluates
                    // ON DUPLICATE KEY UPDATE assignments left-to-right, and the IF condition
                    // checks `status < 2`. If status is updated first to 2, the start_time
                    // condition becomes false.
                    //
                    // TODO: VALUES() is deprecated in MySQL 8.0.20+. Replace with AS alias syntax
                    // when dropping support for MySQL < 8.0.19. Example:
                    //   INSERT INTO ... VALUES (...) AS new_row
                    //   ON DUPLICATE KEY UPDATE col = new_row.col
                    sqlx::query(
                        "INSERT INTO job_processing_status
                         (job_id, status, worker_id, channel, priority, enqueue_time,
                          start_time, is_streamable, broadcast_results, version, updated_at)
                         VALUES (?, 2, ?, ?, ?, ?, ?, ?, ?, 1, ?)
                         ON DUPLICATE KEY UPDATE
                           start_time = IF(status < 2 AND deleted_at IS NULL, COALESCE(start_time, VALUES(start_time)), start_time),
                           updated_at = IF(status < 2 AND deleted_at IS NULL, VALUES(updated_at), updated_at),
                           version = IF(status < 2 AND deleted_at IS NULL, version + 1, version),
                           status = IF(status < 2 AND deleted_at IS NULL, 2, status)",
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
                    .execute(&mut *conn)
                    .await?;
                }

                #[cfg(not(feature = "mysql"))]
                {
                    // SQLite: INSERT OR IGNORE + UPDATE (two queries for conditional upsert)
                    // First, try to update existing record with status < 2
                    let rows_affected = sqlx::query(
                        "UPDATE job_processing_status
                         SET status = 2, start_time = COALESCE(start_time, ?), version = version + 1, updated_at = ?
                         WHERE job_id = ? AND status < 2 AND deleted_at IS NULL",
                    )
                    .bind(now)
                    .bind(now)
                    .bind(job_id.value)
                    .execute(&mut *conn)
                    .await?
                    .rows_affected();

                    if rows_affected == 0 {
                        // No existing record with status < 2, try INSERT if not exists at all
                        let insert_result = sqlx::query(
                            "INSERT INTO job_processing_status
                             (job_id, status, worker_id, channel, priority, enqueue_time,
                              start_time, is_streamable, broadcast_results, version, updated_at)
                             SELECT ?, 2, ?, ?, ?, ?, ?, ?, ?, 1, ?
                             WHERE NOT EXISTS (
                               SELECT 1 FROM job_processing_status WHERE job_id = ?
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

                        if insert_result == 0 {
                            tracing::debug!(
                                job_id = job_id.value,
                                "Skipped RUNNING upsert: record already exists with same or higher status"
                            );
                        }
                    }
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

    /// Bulk logical deletion of stale records (orphaned_only=false)
    ///
    /// Marks stale records as deleted where:
    /// - deleted_at IS NULL (still active)
    /// - updated_at older than stale_threshold_hours
    /// - job_id NOT IN jobs with future run_after_time (excludes scheduled jobs)
    ///
    /// # Returns
    /// - `Ok((marked_count, cutoff_time))`: Number of marked records and cutoff timestamp
    pub async fn purge_stale_records(&self, stale_threshold_hours: u64) -> Result<(u64, i64)> {
        if !self.config.rdb_indexing_enabled {
            return Ok((0, 0));
        }

        let now = datetime::now_millis();
        let threshold_millis = stale_threshold_hours
            .checked_mul(3600)
            .and_then(|v| v.checked_mul(1000))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "stale_threshold_hours {} overflows millisecond conversion",
                    stale_threshold_hours
                )
            })?;
        let threshold_millis_i64 = i64::try_from(threshold_millis).map_err(|_| {
            anyhow::anyhow!(
                "stale_threshold_hours {} exceeds i64 range in milliseconds",
                stale_threshold_hours
            )
        })?;
        let cutoff_millis = now.saturating_sub(threshold_millis_i64);

        let mut conn = self.rdb_pool.acquire().await?;
        let result = sqlx::query(
            "UPDATE job_processing_status
             SET deleted_at = ?, updated_at = ?
             WHERE deleted_at IS NULL AND updated_at < ?
               AND job_id NOT IN (SELECT id FROM job WHERE run_after_time > ?)",
        )
        .bind(now)
        .bind(now)
        .bind(cutoff_millis)
        .bind(now)
        .execute(&mut *conn)
        .await?;

        Ok((result.rows_affected(), cutoff_millis))
    }

    /// Find stale job_ids for orphan checking (orphaned_only=true)
    ///
    /// Returns job_ids of stale records where:
    /// - deleted_at IS NULL (still active)
    /// - updated_at older than stale_threshold_hours
    /// - job_id NOT IN jobs with future run_after_time
    ///
    /// # Performance
    /// Loads all matching job_ids into memory without LIMIT.
    /// Each returned id is then checked individually via `find_job()` + `find_status()` (N+1).
    /// This is acceptable because this RPC is intended for infrequent admin use and
    /// `stale_threshold_hours` typically limits the result set to a small number.
    ///
    /// # Returns
    /// - `Ok((job_ids, cutoff_time))`: List of candidate job_ids and cutoff timestamp
    pub async fn find_stale_job_ids(&self, stale_threshold_hours: u64) -> Result<(Vec<i64>, i64)> {
        if !self.config.rdb_indexing_enabled {
            return Ok((vec![], 0));
        }

        let now = datetime::now_millis();
        let threshold_millis = stale_threshold_hours
            .checked_mul(3600)
            .and_then(|v| v.checked_mul(1000))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "stale_threshold_hours {} overflows millisecond conversion",
                    stale_threshold_hours
                )
            })?;
        let threshold_millis_i64 = i64::try_from(threshold_millis).map_err(|_| {
            anyhow::anyhow!(
                "stale_threshold_hours {} exceeds i64 range in milliseconds",
                stale_threshold_hours
            )
        })?;
        let cutoff_millis = now.saturating_sub(threshold_millis_i64);

        let mut conn = self.rdb_pool.acquire().await?;
        let rows: Vec<(i64,)> = sqlx::query_as(
            "SELECT job_id FROM job_processing_status
             WHERE deleted_at IS NULL AND updated_at < ?
               AND job_id NOT IN (SELECT id FROM job WHERE run_after_time > ?)",
        )
        .bind(cutoff_millis)
        .bind(now)
        .fetch_all(&mut *conn)
        .await?;

        Ok((rows.into_iter().map(|(id,)| id).collect(), cutoff_millis))
    }

    /// Physical deletion of logically deleted records (called from cleanup task)
    ///
    /// # Arguments
    /// - `retention_hours_override`: Override default retention hours (None uses config default)
    ///
    /// # Returns
    /// - `Ok((deleted_count, cutoff_time))`: Number of deleted records and cutoff timestamp used
    pub async fn cleanup_deleted_records(
        &self,
        retention_hours_override: Option<u64>,
    ) -> Result<(u64, i64)> {
        if !self.config.rdb_indexing_enabled {
            return Ok((0, 0));
        }

        let retention_hours = retention_hours_override.unwrap_or(self.config.retention_hours);
        let cutoff_millis = datetime::now_millis() - (retention_hours * 3600 * 1000) as i64;

        let mut conn = self.rdb_pool.acquire().await?;
        let result = sqlx::query(
            "DELETE FROM job_processing_status
             WHERE deleted_at IS NOT NULL AND deleted_at < ?",
        )
        .bind(cutoff_millis)
        .execute(&mut *conn)
        .await?;

        Ok((result.rows_affected(), cutoff_millis))
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
    use infra_utils::infra::test::{TEST_RUNTIME, setup_test_rdb_from};

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

            let repo = RdbJobProcessingStatusIndexRepository::new(
                Arc::new(pool.clone()),
                Arc::new(config),
            );

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

            let repo = RdbJobProcessingStatusIndexRepository::new(
                Arc::new(pool.clone()),
                Arc::new(config),
            );

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

            let repo = RdbJobProcessingStatusIndexRepository::new(
                Arc::new(pool.clone()),
                Arc::new(config),
            );

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

            let repo = RdbJobProcessingStatusIndexRepository::new(Arc::new(pool.clone()), Arc::new(config));

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
            let (deleted_count, _cutoff_time) = repo.cleanup_deleted_records(None).await?;

            assert_eq!(deleted_count, 1);
            Ok(())
        })
    }

    #[test]
    fn test_index_status_running_without_pending_inserts_directly() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_db().await;
            let config = JobStatusConfig {
                rdb_indexing_enabled: true,
                cleanup_interval_hours: 1,
                retention_hours: 24,
            };

            let repo = RdbJobProcessingStatusIndexRepository::new(
                Arc::new(pool.clone()),
                Arc::new(config),
            );

            // RUNNING can be inserted directly without PENDING (race condition fix)
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

            assert!(result.is_ok());

            // Record should be created with RUNNING status
            let count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM job_processing_status WHERE job_id = 300")
                    .fetch_one(pool)
                    .await?;

            assert_eq!(count, 1);

            // Verify status is RUNNING (2)
            let status: i32 =
                sqlx::query_scalar("SELECT status FROM job_processing_status WHERE job_id = 300")
                    .fetch_one(pool)
                    .await?;

            assert_eq!(status, JobProcessingStatus::Running as i32);
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

            let repo = RdbJobProcessingStatusIndexRepository::new(Arc::new(pool.clone()), Arc::new(config));

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

            let repo = RdbJobProcessingStatusIndexRepository::new(
                Arc::new(pool.clone()),
                Arc::new(config),
            );

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

            let repo = RdbJobProcessingStatusIndexRepository::new(
                Arc::new(pool.clone()),
                Arc::new(config),
            );

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
            let repo = RdbJobProcessingStatusIndexRepository::new(
                Arc::new(pool.clone()),
                Arc::new(config),
            );

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
    fn test_update_status_by_job_id_preserves_existing_start_time() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_db().await;
            let config = JobStatusConfig {
                rdb_indexing_enabled: true,
                cleanup_interval_hours: 1,
                retention_hours: 24,
            };
            let repo = RdbJobProcessingStatusIndexRepository::new(
                Arc::new(pool.clone()),
                Arc::new(config),
            );

            let job_id = JobId { value: 101 };
            let worker_id = WorkerId { value: 1 };

            repo.index_status(
                &job_id,
                &JobProcessingStatus::Pending,
                &worker_id,
                "test_channel",
                1,
                101,
                false,
                false,
            )
            .await?;

            let existing_start_time = 123_456i64;
            let existing_version = 5i64;

            sqlx::query(
                "UPDATE job_processing_status SET start_time = ?, version = ? WHERE job_id = ?",
            )
            .bind(existing_start_time)
            .bind(existing_version)
            .bind(job_id.value)
            .execute(pool)
            .await?;

            repo.update_status_by_job_id(&job_id, &JobProcessingStatus::Running)
                .await?;

            let (start_time, version): (Option<i64>, i64) = sqlx::query_as(
                "SELECT start_time, version FROM job_processing_status WHERE job_id = ?",
            )
            .bind(job_id.value)
            .fetch_one(pool)
            .await?;

            assert_eq!(start_time, Some(existing_start_time));
            assert_eq!(version, existing_version + 1);
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
            let repo = RdbJobProcessingStatusIndexRepository::new(
                Arc::new(pool.clone()),
                Arc::new(config),
            );

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
            let repo = RdbJobProcessingStatusIndexRepository::new(
                Arc::new(pool.clone()),
                Arc::new(config),
            );

            let job_id = JobId { value: 300 };
            let worker_id = WorkerId { value: 1 };

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
            let repo = RdbJobProcessingStatusIndexRepository::new(
                Arc::new(pool.clone()),
                Arc::new(config),
            );

            let job_id = JobId { value: 400 };
            let worker_id = WorkerId { value: 1 };

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

    async fn setup_job_table(pool: &RdbPool) {
        let _ = sqlx::query(
            "CREATE TABLE IF NOT EXISTS job (
                id INTEGER PRIMARY KEY,
                worker_id BIGINT NOT NULL,
                args BLOB NOT NULL,
                uniq_key TEXT,
                enqueue_time BIGINT NOT NULL DEFAULT 0,
                grabbed_until_time BIGINT DEFAULT 0,
                run_after_time BIGINT NOT NULL DEFAULT 0,
                retried INT NOT NULL DEFAULT 0,
                priority INT NOT NULL DEFAULT 0,
                timeout BIGINT NOT NULL DEFAULT 0,
                request_streaming BOOLEAN NOT NULL DEFAULT 0,
                using TEXT
            )",
        )
        .execute(pool)
        .await;

        let _ = sqlx::query("DELETE FROM job").execute(pool).await;
    }

    #[test]
    fn test_purge_stale_records() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_db().await;
            setup_job_table(pool).await;
            let config = JobStatusConfig {
                rdb_indexing_enabled: true,
                cleanup_interval_hours: 1,
                retention_hours: 24,
            };
            let repo = RdbJobProcessingStatusIndexRepository::new(
                Arc::new(pool.clone()),
                Arc::new(config),
            );

            let now = datetime::now_millis();
            let old_timestamp = now - (3 * 3600 * 1000); // 3 hours ago

            // Insert a stale record (updated_at = 3 hours ago)
            sqlx::query(
                "INSERT INTO job_processing_status
                 (job_id, status, worker_id, channel, priority, enqueue_time, version, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(700)
            .bind(1) // PENDING
            .bind(1)
            .bind("test")
            .bind(0)
            .bind(old_timestamp)
            .bind(1)
            .bind(old_timestamp)
            .execute(pool)
            .await?;

            // Insert a recent record (should NOT be purged)
            sqlx::query(
                "INSERT INTO job_processing_status
                 (job_id, status, worker_id, channel, priority, enqueue_time, version, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(701)
            .bind(1)
            .bind(1)
            .bind("test")
            .bind(0)
            .bind(now)
            .bind(1)
            .bind(now)
            .execute(pool)
            .await?;

            // Purge with 2-hour threshold (should purge job 700, keep job 701)
            let (marked_count, cutoff_time) = repo.purge_stale_records(2).await?;

            assert_eq!(marked_count, 1);
            assert!(cutoff_time > 0);

            // Verify stale record is marked deleted
            let deleted_at: Option<i64> = sqlx::query_scalar(
                "SELECT deleted_at FROM job_processing_status WHERE job_id = 700",
            )
            .fetch_one(pool)
            .await?;
            assert!(deleted_at.is_some());

            // Verify recent record is still active
            let deleted_at: Option<i64> = sqlx::query_scalar(
                "SELECT deleted_at FROM job_processing_status WHERE job_id = 701",
            )
            .fetch_one(pool)
            .await?;
            assert!(deleted_at.is_none());

            Ok(())
        })
    }

    #[test]
    fn test_purge_stale_records_excludes_scheduled_jobs() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_db().await;
            setup_job_table(pool).await;
            let config = JobStatusConfig {
                rdb_indexing_enabled: true,
                cleanup_interval_hours: 1,
                retention_hours: 24,
            };
            let repo = RdbJobProcessingStatusIndexRepository::new(
                Arc::new(pool.clone()),
                Arc::new(config),
            );

            let now = datetime::now_millis();
            let old_timestamp = now - (3 * 3600 * 1000);
            let future_time = now + (3600 * 1000); // 1 hour in future

            // Insert stale status record
            sqlx::query(
                "INSERT INTO job_processing_status
                 (job_id, status, worker_id, channel, priority, enqueue_time, version, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(710)
            .bind(1)
            .bind(1)
            .bind("test")
            .bind(0)
            .bind(old_timestamp)
            .bind(1)
            .bind(old_timestamp)
            .execute(pool)
            .await?;

            // Insert corresponding job with future run_after_time
            sqlx::query(
                "INSERT INTO job (id, worker_id, args, enqueue_time, run_after_time, retried, priority, timeout, request_streaming)
                 VALUES (?, ?, ?, ?, ?, 0, 0, 0, 0)",
            )
            .bind(710)
            .bind(1)
            .bind(b"test" as &[u8])
            .bind(old_timestamp)
            .bind(future_time)
            .execute(pool)
            .await?;

            // Purge should skip job 710 (has future run_after_time)
            let (marked_count, _) = repo.purge_stale_records(2).await?;
            assert_eq!(marked_count, 0);

            Ok(())
        })
    }

    #[test]
    fn test_find_stale_job_ids() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_db().await;
            setup_job_table(pool).await;
            let config = JobStatusConfig {
                rdb_indexing_enabled: true,
                cleanup_interval_hours: 1,
                retention_hours: 24,
            };
            let repo = RdbJobProcessingStatusIndexRepository::new(
                Arc::new(pool.clone()),
                Arc::new(config),
            );

            let now = datetime::now_millis();
            let old_timestamp = now - (3 * 3600 * 1000);

            // Insert stale records
            for job_id in [720, 721] {
                sqlx::query(
                    "INSERT INTO job_processing_status
                     (job_id, status, worker_id, channel, priority, enqueue_time, version, updated_at)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(job_id)
                .bind(1)
                .bind(1)
                .bind("test")
                .bind(0)
                .bind(old_timestamp)
                .bind(1)
                .bind(old_timestamp)
                .execute(pool)
                .await?;
            }

            // Insert recent record (should NOT appear)
            sqlx::query(
                "INSERT INTO job_processing_status
                 (job_id, status, worker_id, channel, priority, enqueue_time, version, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(722)
            .bind(1)
            .bind(1)
            .bind("test")
            .bind(0)
            .bind(now)
            .bind(1)
            .bind(now)
            .execute(pool)
            .await?;

            let (ids, cutoff_time) = repo.find_stale_job_ids(2).await?;

            assert_eq!(ids.len(), 2);
            assert!(ids.contains(&720));
            assert!(ids.contains(&721));
            assert!(!ids.contains(&722));
            assert!(cutoff_time > 0);

            Ok(())
        })
    }

    #[test]
    fn test_purge_stale_records_disabled() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_db().await;
            let config = JobStatusConfig {
                rdb_indexing_enabled: false,
                cleanup_interval_hours: 1,
                retention_hours: 24,
            };
            let repo = RdbJobProcessingStatusIndexRepository::new(
                Arc::new(pool.clone()),
                Arc::new(config),
            );

            let (count, cutoff) = repo.purge_stale_records(2).await?;
            assert_eq!(count, 0);
            assert_eq!(cutoff, 0);

            Ok(())
        })
    }
}
