use anyhow::Result;
use command_utils::util::datetime;
use infra_utils::infra::rdb::RdbPool;
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{JobId, WorkerId};
use sqlx::Executor;
use sqlx::FromRow;
use std::sync::Arc;

/// A RUNNING index row which still belongs to a single logical worker instance.
#[derive(Debug, Clone, FromRow, PartialEq, Eq)]
pub struct RunningStatusCandidate {
    pub job_id: i64,
    pub worker_id: i64,
    pub worker_instance_id: i64,
    pub version: i64,
    pub start_time: Option<i64>,
    pub updated_at: i64,
}

/// Snapshot returned only after a recovery claim has committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryClaim {
    pub candidate: RunningStatusCandidate,
    pub claimed_version: i64,
    pub deleted_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    Claimed,
    Conflict,
}

/// Input captured before the RDB dispatcher obtains a durable execution slot.
pub struct RdbDispatchStart<'a> {
    pub job_id: &'a JobId,
    pub worker_id: &'a WorkerId,
    pub worker_instance_id: i64,
    pub channel: Option<&'a str>,
    pub priority: i32,
    pub enqueue_time: i64,
    pub is_streamable: bool,
    pub broadcast_results: bool,
    pub timeout: Option<u64>,
    pub original_grabbed_until_time: i64,
}

/// Durable execution snapshot returned after the grab/status transaction commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RdbDispatchExecution {
    pub job_id: JobId,
    pub worker_instance_id: i64,
    pub status_version: i64,
}

/// Repository for recovery-only CAS operations. It intentionally does not
/// implement the live Redis status repository: this table remains an index.
#[derive(Clone, Debug)]
pub struct RdbJobStatusExecutionRepository {
    pool: Arc<RdbPool>,
}

impl RdbJobStatusExecutionRepository {
    pub fn new(pool: Arc<RdbPool>) -> Self {
        Self { pool }
    }

    pub async fn find_running_by_instance(
        &self,
        instance_id: i64,
        after_job_id: i64,
        limit: i64,
    ) -> Result<Vec<RunningStatusCandidate>> {
        Ok(sqlx::query_as(
            "SELECT job_id, worker_id, worker_instance_id, version, start_time, updated_at
             FROM job_processing_status
             WHERE status = 2 AND deleted_at IS NULL AND worker_instance_id = ? AND job_id > ?
             ORDER BY job_id LIMIT ?",
        )
        .bind(instance_id)
        .bind(after_job_id)
        .bind(limit)
        .fetch_all(&*self.pool)
        .await?)
    }

    /// Atomically claim an RDB queue row and publish its RUNNING index row.
    ///
    /// The dispatcher must not invoke a runner unless this transaction commits.
    /// Replacing an old/deleted status row is intentional: the successful grab
    /// comparison is the authoritative boundary for a new RDB-dispatched run.
    #[allow(clippy::too_many_arguments)]
    pub async fn grab_and_mark_running(
        &self,
        start: RdbDispatchStart<'_>,
    ) -> Result<Option<RdbDispatchExecution>> {
        let now = datetime::now_millis();
        let timeout = start.timeout.unwrap_or(0);
        let timeout = if timeout == 0 {
            1000 * 60 * 60 * 24 * 365 * 100
        } else {
            timeout
        };
        let grabbed_until = now + i64::try_from(timeout)? + 10_000;
        let mut transaction = self.pool.begin().await?;
        let grabbed = sqlx::query(
            "UPDATE job SET grabbed_until_time = ? WHERE id = ? AND grabbed_until_time = ?",
        )
        .bind(grabbed_until)
        .bind(start.job_id.value)
        .bind(start.original_grabbed_until_time)
        .execute(&mut *transaction)
        .await
        .map_err(JobWorkerError::DBError)?
        .rows_affected()
            == 1;
        if !grabbed {
            transaction.rollback().await?;
            return Ok(None);
        }

        let channel = start.channel.unwrap_or("");
        #[cfg(feature = "mysql")]
        sqlx::query(
            "INSERT INTO job_processing_status
             (job_id, status, worker_id, channel, priority, enqueue_time, start_time,
              is_streamable, broadcast_results, worker_instance_id, version, updated_at, deleted_at)
             VALUES (?, 2, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, NULL)
             ON DUPLICATE KEY UPDATE status = 2, worker_id = VALUES(worker_id),
               channel = VALUES(channel), priority = VALUES(priority), enqueue_time = VALUES(enqueue_time),
               start_time = VALUES(start_time), is_streamable = VALUES(is_streamable),
               broadcast_results = VALUES(broadcast_results), worker_instance_id = VALUES(worker_instance_id),
               deleted_at = NULL, version = version + 1, updated_at = VALUES(updated_at)",
        )
        .bind(start.job_id.value)
        .bind(start.worker_id.value)
        .bind(channel)
        .bind(start.priority)
        .bind(start.enqueue_time)
        .bind(now)
        .bind(start.is_streamable)
        .bind(start.broadcast_results)
        .bind(start.worker_instance_id)
        .bind(now)
        .execute(&mut *transaction)
        .await?;

        #[cfg(not(feature = "mysql"))]
        sqlx::query(
            "INSERT INTO job_processing_status
             (job_id, status, worker_id, channel, priority, enqueue_time, start_time,
              is_streamable, broadcast_results, worker_instance_id, version, updated_at, deleted_at)
             VALUES (?, 2, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, NULL)
             ON CONFLICT(job_id) DO UPDATE SET status = 2, worker_id = excluded.worker_id,
               channel = excluded.channel, priority = excluded.priority, enqueue_time = excluded.enqueue_time,
               start_time = excluded.start_time, is_streamable = excluded.is_streamable,
               broadcast_results = excluded.broadcast_results, worker_instance_id = excluded.worker_instance_id,
               deleted_at = NULL, version = job_processing_status.version + 1,
               updated_at = excluded.updated_at",
        )
        .bind(start.job_id.value)
        .bind(start.worker_id.value)
        .bind(channel)
        .bind(start.priority)
        .bind(start.enqueue_time)
        .bind(now)
        .bind(start.is_streamable)
        .bind(start.broadcast_results)
        .bind(start.worker_instance_id)
        .bind(now)
        .execute(&mut *transaction)
        .await?;

        let status_version: i64 = sqlx::query_scalar(
            "SELECT version FROM job_processing_status WHERE job_id = ? AND worker_instance_id = ? AND status = 2 AND deleted_at IS NULL",
        )
        .bind(start.job_id.value)
        .bind(start.worker_instance_id)
        .fetch_one(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(Some(RdbDispatchExecution {
            job_id: *start.job_id,
            worker_instance_id: start.worker_instance_id,
            status_version,
        }))
    }

    pub async fn claim_running(
        &self,
        candidate: &RunningStatusCandidate,
    ) -> Result<Option<RecoveryClaim>> {
        let now = datetime::now_millis();
        let result = sqlx::query(
            "UPDATE job_processing_status
             SET deleted_at = ?, updated_at = ?, version = version + 1
             WHERE job_id = ? AND worker_instance_id = ? AND version = ?
               AND status = 2 AND deleted_at IS NULL",
        )
        .bind(now)
        .bind(now)
        .bind(candidate.job_id)
        .bind(candidate.worker_instance_id)
        .bind(candidate.version)
        .execute(&*self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        Ok(Some(RecoveryClaim {
            candidate: candidate.clone(),
            claimed_version: candidate.version + 1,
            deleted_at: now,
        }))
    }

    pub async fn restore_running(&self, claim: &RecoveryClaim) -> Result<ClaimOutcome> {
        let result = sqlx::query(
            "UPDATE job_processing_status
             SET deleted_at = NULL, updated_at = ?, version = version + 1
             WHERE job_id = ? AND worker_instance_id = ? AND version = ? AND deleted_at = ?",
        )
        .bind(datetime::now_millis())
        .bind(claim.candidate.job_id)
        .bind(claim.candidate.worker_instance_id)
        .bind(claim.claimed_version)
        .bind(claim.deleted_at)
        .execute(&*self.pool)
        .await?;
        Ok(if result.rows_affected() == 1 {
            ClaimOutcome::Claimed
        } else {
            ClaimOutcome::Conflict
        })
    }

    pub async fn reset_claim_to_pending(&self, claim: &RecoveryClaim) -> Result<ClaimOutcome> {
        let mut transaction = self.pool.begin().await?;
        let outcome = self
            .reset_claim_to_pending_tx(&mut *transaction, claim)
            .await?;
        transaction.commit().await?;
        Ok(outcome)
    }

    pub async fn reset_claim_to_pending_tx<'c, E>(
        &self,
        executor: E,
        claim: &RecoveryClaim,
    ) -> Result<ClaimOutcome>
    where
        E: Executor<'c, Database = infra_utils::infra::rdb::Rdb>,
    {
        let now = datetime::now_millis();
        let result = sqlx::query(
            "UPDATE job_processing_status
             SET status = 1, worker_instance_id = NULL, start_time = NULL, deleted_at = NULL,
                 pending_time = ?, updated_at = ?, version = version + 1
             WHERE job_id = ? AND worker_instance_id = ? AND version = ? AND deleted_at = ?",
        )
        .bind(now)
        .bind(now)
        .bind(claim.candidate.job_id)
        .bind(claim.candidate.worker_instance_id)
        .bind(claim.claimed_version)
        .bind(claim.deleted_at)
        .execute(executor)
        .await?;
        Ok(if result.rows_affected() == 1 {
            ClaimOutcome::Claimed
        } else {
            ClaimOutcome::Conflict
        })
    }

    pub fn candidate_job_id(candidate: &RunningStatusCandidate) -> JobId {
        JobId {
            value: candidate.job_id,
        }
    }

    pub fn candidate_worker_id(candidate: &RunningStatusCandidate) -> WorkerId {
        WorkerId {
            value: candidate.worker_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use infra_utils::infra::test::{TEST_RUNTIME, setup_test_rdb_from};

    #[test]
    fn claim_and_reset_are_guarded_by_the_observed_owner_and_version() {
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_rdb_from("sql/sqlite").await;
            sqlx::query(
                "INSERT INTO job_processing_status
                 (job_id, worker_id, status, channel, priority, enqueue_time,
                  worker_instance_id, version, start_time, updated_at)
                 VALUES (1, 2, 2, '', 0, 1, 3, 4, 5, 6)",
            )
            .execute(pool)
            .await
            .unwrap();
            let repository = RdbJobStatusExecutionRepository::new(Arc::new(pool.clone()));

            let candidates = repository.find_running_by_instance(3, 0, 10).await.unwrap();
            assert_eq!(candidates.len(), 1);
            let claim = repository
                .claim_running(&candidates[0])
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                repository.claim_running(&candidates[0]).await.unwrap(),
                None
            );
            assert_eq!(
                repository.reset_claim_to_pending(&claim).await.unwrap(),
                ClaimOutcome::Claimed
            );
            let row: (i64, Option<i64>) = sqlx::query_as(
                "SELECT status, worker_instance_id FROM job_processing_status WHERE job_id = 1",
            )
            .fetch_one(pool)
            .await
            .unwrap();
            assert_eq!(row, (1, None));
        });
    }

    #[test]
    fn rdb_dispatch_grab_and_running_index_commit_together() {
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_rdb_from("sql/sqlite").await;
            sqlx::query(
                "INSERT INTO job
                 (id, worker_id, args, enqueue_time, grabbed_until_time, run_after_time,
                  retried, priority, timeout, request_streaming, `using`)
                 VALUES (11, 12, X'', 1, 0, 0, 0, 3, 30, 0, '')",
            )
            .execute(pool)
            .await
            .unwrap();
            let repository = RdbJobStatusExecutionRepository::new(Arc::new(pool.clone()));
            let job_id = JobId { value: 11 };
            let worker_id = WorkerId { value: 12 };
            let execution = repository
                .grab_and_mark_running(RdbDispatchStart {
                    job_id: &job_id,
                    worker_id: &worker_id,
                    worker_instance_id: 13,
                    channel: None,
                    priority: 3,
                    enqueue_time: 1,
                    is_streamable: false,
                    broadcast_results: false,
                    timeout: Some(30),
                    original_grabbed_until_time: 0,
                })
                .await
                .unwrap()
                .unwrap();
            assert_eq!(execution.worker_instance_id, 13);
            let row: (i64, i64, i64) = sqlx::query_as(
                "SELECT status, worker_instance_id, version FROM job_processing_status WHERE job_id = 11",
            )
            .fetch_one(pool)
            .await
            .unwrap();
            assert_eq!(row.0, 2);
            assert_eq!(row.1, 13);
            assert!(row.2 >= 1);
            assert!(repository
                .grab_and_mark_running(RdbDispatchStart {
                    job_id: &job_id,
                    worker_id: &worker_id,
                    worker_instance_id: 14,
                    channel: None,
                    priority: 3,
                    enqueue_time: 1,
                    is_streamable: false,
                    broadcast_results: false,
                    timeout: Some(30),
                    original_grabbed_until_time: 0,
                })
                .await
                .unwrap()
                .is_none());
        });
    }
}
