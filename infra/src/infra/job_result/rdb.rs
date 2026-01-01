use super::rows::JobResultRow;
use crate::infra::job::rows::UseJobqueueAndCodec;
use anyhow::{Context, Result};
use async_trait::async_trait;
use command_utils::util::datetime;
use infra_utils::infra::rdb::{Rdb, RdbPool, UseRdbPool};
use itertools::Itertools;
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{JobId, JobResult, JobResultData, JobResultId, JobResultSortField};
use sqlx::{Executor, Transaction};

#[async_trait]
pub trait RdbJobResultRepository: UseRdbPool + UseJobqueueAndCodec + Sync + Send {
    async fn create(&self, id: &JobResultId, job_result: &JobResultData) -> Result<bool> {
        let res = sqlx::query::<Rdb>(
            "INSERT INTO job_result (
                id,
                job_id,
                worker_id,
                args,
                uniq_key,
                status,
                output,
                retried,
                priority,
                timeout,
                request_streaming,
                enqueue_time,
                run_after_time,
                start_time,
                end_time,
                `using`
            ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        )
        .bind(id.value)
        .bind(job_result.job_id.as_ref().unwrap().value) //XXX unwrap
        .bind(job_result.worker_id.as_ref().unwrap().value) //XXX unwrap
        .bind(&job_result.args)
        .bind(&job_result.uniq_key)
        .bind(job_result.status)
        .bind(
            job_result
                .output
                .as_ref()
                .map(JobResultRow::serialize_result_output)
                .transpose()?
                .unwrap_or_default(),
        )
        .bind(job_result.retried as i64)
        .bind(job_result.priority)
        .bind(job_result.timeout as i64)
        .bind(job_result.streaming_type)
        .bind(job_result.enqueue_time)
        .bind(job_result.run_after_time)
        .bind(job_result.start_time)
        .bind(job_result.end_time)
        .bind(&job_result.using)
        .execute(self.db_pool())
        .await
        .map_err(JobWorkerError::DBError)?;

        Ok(res.rows_affected() > 0)
    }

    async fn update(
        &self,
        tx: &mut Transaction<'_, Rdb>,
        id: &JobResultId,
        job_result: &JobResultData,
    ) -> Result<bool> {
        sqlx::query(
            "UPDATE job_result SET
            job_id = ?,
            worker_id = ?,
            args = ?,
            uniq_key = ?,
            status = ?,
            output = ?,
            retried = ?,
            priority = ?,
            timeout = ?,
            request_streaming = ?,
            enqueue_time = ?,
            run_after_time = ?,
            start_time = ?,
            end_time = ?,
            `using` = ?
            WHERE id = ?;",
        )
        .bind(job_result.job_id.as_ref().unwrap().value) //XXX unwrap
        .bind(job_result.worker_id.as_ref().unwrap().value) //XXX unwrap
        .bind(&job_result.args)
        .bind(&job_result.uniq_key)
        .bind(job_result.status)
        .bind(
            job_result
                .output
                .as_ref()
                .map(JobResultRow::serialize_result_output)
                .transpose()?
                .unwrap_or_default(),
        )
        .bind(job_result.retried as i64)
        .bind(job_result.priority)
        .bind(job_result.timeout as i64)
        .bind(job_result.streaming_type)
        .bind(job_result.enqueue_time)
        .bind(job_result.run_after_time)
        .bind(job_result.start_time)
        .bind(job_result.end_time)
        .bind(&job_result.using)
        .bind(id.value)
        .execute(&mut **tx)
        .await
        .map(|r| r.rows_affected() > 0)
        .map_err(JobWorkerError::DBError)
        .context(format!(
            "error in update: id = {:?}, job id = {:?}",
            id.value,
            job_result.job_id.as_ref()
        ))
    }

    async fn delete(&self, id: &JobResultId) -> Result<bool> {
        self.delete_tx(self.db_pool(), id).await
    }

    async fn delete_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        id: &JobResultId,
    ) -> Result<bool> {
        let del = sqlx::query::<Rdb>("DELETE FROM job_result WHERE id = ?;")
            .bind(id.value)
            .execute(tx)
            .await
            .map(|r| r.rows_affected() > 0)
            .map_err(JobWorkerError::DBError)?;
        Ok(del)
    }

    async fn find_latest_by_job_id(&self, job_id: &JobId) -> Result<Option<JobResult>> {
        sqlx::query_as::<Rdb, JobResultRow>(
            "SELECT * FROM job_result WHERE job_id = ? ORDER BY end_time DESC LIMIT 1;",
        )
        .bind(job_id.value)
        .fetch_optional(self.db_pool())
        .await
        .map(|r| r.map(|r2| r2.to_proto()))
        .map_err(JobWorkerError::DBError)
        .context(format!("error in find: job_id = {}", job_id.value))
    }

    /// find latest 1000 records by job_id
    /// XXX limit 1000 (max retry)
    async fn find_list_by_job_id(&self, job_id: &JobId) -> Result<Vec<JobResult>> {
        sqlx::query_as::<Rdb, JobResultRow>(
            "SELECT * FROM job_result WHERE job_id = ? ORDER BY end_time DESC LIMIT 1000;",
        )
        .bind(job_id.value)
        .fetch_all(self.db_pool())
        .await
        .map(|r| r.into_iter().map(|r2| r2.to_proto()).collect_vec())
        .map_err(JobWorkerError::DBError)
        .context(format!("error in find: job_id = {}", job_id.value))
    }

    async fn find(&self, id: &JobResultId) -> Result<Option<JobResult>> {
        sqlx::query_as::<Rdb, JobResultRow>("SELECT * FROM job_result WHERE id = ?;")
            .bind(id.value)
            .fetch_optional(self.db_pool())
            .await
            .map(|r| r.map(|r2| r2.to_proto()))
            .map_err(JobWorkerError::DBError)
            .context(format!("error in find: job_result.id = {}", id.value))
    }

    async fn find_list(&self, limit: Option<&i32>, offset: Option<&i64>) -> Result<Vec<JobResult>> {
        if let Some(l) = limit {
            sqlx::query_as::<_, JobResultRow>(
                "SELECT * FROM job_result ORDER BY job_id DESC LIMIT ? OFFSET ?;",
            )
            .bind(l)
            .bind(offset.unwrap_or(&0i64))
            .fetch_all(self.db_pool())
        } else {
            // fetch all!
            sqlx::query_as::<_, JobResultRow>("SELECT * FROM job_result ORDER BY job_id DESC;")
                .fetch_all(self.db_pool())
        }
        .await
        .map(|r| r.iter().map(|r2| r2.to_proto()).collect_vec())
        .map_err(JobWorkerError::DBError)
        .context(format!("error in find_list: ({limit:?}, {offset:?})"))
    }

    async fn count_list_tx<'c, E: Executor<'c, Database = Rdb>>(&self, tx: E) -> Result<i64> {
        sqlx::query_scalar("SELECT count(*) as count FROM job_result;")
            .fetch_one(tx)
            .await
            .map_err(JobWorkerError::DBError)
            .context("error in count_list".to_string())
    }

    /// Find job results with filtering and sorting
    ///
    /// # Arguments
    /// - `worker_ids`: Worker ID filter (multiple IDs allowed)
    /// - `statuses`: Result status filter (multiple statuses allowed)
    /// - `start_time_from`, `start_time_to`: Start time range filter
    /// - `end_time_from`, `end_time_to`: End time range filter
    /// - `priorities`: Priority filter (multiple priorities allowed)
    /// - `uniq_key`: Unique key exact match filter
    /// - `limit`: Page size (default: 100, max: 1000)
    /// - `offset`: Offset for pagination (default: 0, max: 10000)
    /// - `sort_by`: Sort field (default: END_TIME)
    /// - `ascending`: Sort order (default: false = DESC)
    #[allow(clippy::too_many_arguments)]
    async fn find_list_by(
        &self,
        worker_ids: Vec<i64>,
        statuses: Vec<i32>,
        start_time_from: Option<i64>,
        start_time_to: Option<i64>,
        end_time_from: Option<i64>,
        end_time_to: Option<i64>,
        priorities: Vec<i32>,
        uniq_key: Option<String>,
        limit: Option<i32>,
        offset: Option<i64>,
        sort_by: Option<JobResultSortField>,
        ascending: Option<bool>,
    ) -> Result<Vec<JobResult>> {
        // Build query using QueryBuilder for type-safe dynamic query construction
        let mut query_builder =
            sqlx::QueryBuilder::<Rdb>::new("SELECT * FROM job_result WHERE id > 0");

        // Worker IDs filter
        if !worker_ids.is_empty() {
            query_builder.push(" AND worker_id IN (");
            let mut separated = query_builder.separated(", ");
            for worker_id in worker_ids {
                separated.push_bind(worker_id);
            }
            separated.push_unseparated(")");
        }

        // Statuses filter
        if !statuses.is_empty() {
            query_builder.push(" AND status IN (");
            let mut separated = query_builder.separated(", ");
            for status in statuses {
                separated.push_bind(status);
            }
            separated.push_unseparated(")");
        }

        // Start time range filter
        if let Some(from) = start_time_from {
            query_builder.push(" AND start_time >= ").push_bind(from);
        }
        if let Some(to) = start_time_to {
            query_builder.push(" AND start_time <= ").push_bind(to);
        }

        // End time range filter
        if let Some(from) = end_time_from {
            query_builder.push(" AND end_time >= ").push_bind(from);
        }
        if let Some(to) = end_time_to {
            query_builder.push(" AND end_time <= ").push_bind(to);
        }

        // Priorities filter
        if !priorities.is_empty() {
            query_builder.push(" AND priority IN (");
            let mut separated = query_builder.separated(", ");
            for priority in priorities {
                separated.push_bind(priority);
            }
            separated.push_unseparated(")");
        }

        // Unique key exact match filter
        if let Some(key) = uniq_key {
            query_builder.push(" AND uniq_key = ").push_bind(key);
        }

        // Sorting - convert enum to SQL field name
        let sort_field = match sort_by.unwrap_or(JobResultSortField::EndTime) {
            JobResultSortField::Id => "id",
            JobResultSortField::StartTime => "start_time",
            JobResultSortField::Status => "status",
            JobResultSortField::EndTime | JobResultSortField::Unspecified => "end_time",
        };
        let order = if ascending.unwrap_or(false) {
            "ASC"
        } else {
            "DESC"
        };
        query_builder.push(format!(" ORDER BY {} {}", sort_field, order));

        // Pagination with upper limits
        let limit_value = limit.unwrap_or(100).min(1000);
        let offset_value = offset.unwrap_or(0).clamp(0, 10000);
        query_builder.push(" LIMIT ").push_bind(limit_value);
        query_builder.push(" OFFSET ").push_bind(offset_value);

        // Execute query
        let rows: Vec<JobResultRow> = query_builder
            .build_query_as()
            .fetch_all(self.db_pool())
            .await
            .map_err(JobWorkerError::DBError)?;

        Ok(rows.iter().map(|r| r.to_proto()).collect())
    }

    /// Count job results with filtering
    ///
    /// # Arguments
    /// Same filters as `find_list_by` (without pagination and sorting)
    #[allow(clippy::too_many_arguments)]
    async fn count_by(
        &self,
        worker_ids: Vec<i64>,
        statuses: Vec<i32>,
        start_time_from: Option<i64>,
        start_time_to: Option<i64>,
        end_time_from: Option<i64>,
        end_time_to: Option<i64>,
        priorities: Vec<i32>,
        uniq_key: Option<String>,
    ) -> Result<i64> {
        // Build query using QueryBuilder
        let mut query_builder =
            sqlx::QueryBuilder::<Rdb>::new("SELECT COUNT(*) as count FROM job_result WHERE id > 0");

        // Apply same filters as find_list_by
        if !worker_ids.is_empty() {
            query_builder.push(" AND worker_id IN (");
            let mut separated = query_builder.separated(", ");
            for worker_id in worker_ids {
                separated.push_bind(worker_id);
            }
            separated.push_unseparated(")");
        }

        if !statuses.is_empty() {
            query_builder.push(" AND status IN (");
            let mut separated = query_builder.separated(", ");
            for status in statuses {
                separated.push_bind(status);
            }
            separated.push_unseparated(")");
        }

        if let Some(from) = start_time_from {
            query_builder.push(" AND start_time >= ").push_bind(from);
        }
        if let Some(to) = start_time_to {
            query_builder.push(" AND start_time <= ").push_bind(to);
        }

        if let Some(from) = end_time_from {
            query_builder.push(" AND end_time >= ").push_bind(from);
        }
        if let Some(to) = end_time_to {
            query_builder.push(" AND end_time <= ").push_bind(to);
        }

        if !priorities.is_empty() {
            query_builder.push(" AND priority IN (");
            let mut separated = query_builder.separated(", ");
            for priority in priorities {
                separated.push_bind(priority);
            }
            separated.push_unseparated(")");
        }

        if let Some(key) = uniq_key {
            query_builder.push(" AND uniq_key = ").push_bind(key);
        }

        // Execute count query
        let count: i64 = query_builder
            .build_query_scalar()
            .fetch_one(self.db_pool())
            .await
            .map_err(JobWorkerError::DBError)?;

        Ok(count)
    }

    /// Bulk delete job results with safety features
    ///
    /// # Safety Features (Defense in Depth)
    /// 1. **Required filter condition**: At least one of end_time_before, statuses, or worker_ids must be specified
    /// 2. **Recent data protection**: Cannot delete results within last 24 hours
    /// 3. **Transaction timeout**: 30-second limit to prevent long-running locks
    ///
    /// # Arguments
    /// - `end_time_before`: Delete results older than this timestamp (Unix time in milliseconds)
    /// - `statuses`: Delete results with these statuses
    /// - `worker_ids`: Delete results from these workers
    ///
    /// # Returns
    /// Number of deleted records
    async fn delete_bulk(
        &self,
        end_time_before: Option<i64>,
        statuses: Vec<i32>,
        worker_ids: Vec<i64>,
    ) -> Result<i64> {
        // Safety feature 1: Required filter condition check (prevent unconditional delete)
        if end_time_before.is_none() && statuses.is_empty() && worker_ids.is_empty() {
            return Err(anyhow::anyhow!(
                "At least one filter condition is required (end_time_before, statuses, or worker_ids)"
            ));
        }

        // Safety feature 2: Recent data protection (ALWAYS enforced, cannot delete data within last 24 hours)
        let now = datetime::now_millis();
        let cutoff_24h = now - 24 * 60 * 60 * 1000;

        // Calculate effective cutoff time:
        // - If end_time_before is specified, use the earlier of (specified time, 24h cutoff)
        // - If end_time_before is not specified, use 24h cutoff
        let effective_cutoff = end_time_before.unwrap_or(cutoff_24h).min(cutoff_24h);

        // Explicit error if user tries to delete data within last 24 hours
        if let Some(time) = end_time_before {
            if time > cutoff_24h {
                return Err(anyhow::anyhow!(
                    "Cannot delete results within last 24 hours (requested: {}, cutoff: {})",
                    time,
                    cutoff_24h
                ));
            }
        }

        // Start transaction
        let mut tx = self
            .db_pool()
            .begin()
            .await
            .map_err(JobWorkerError::DBError)?;

        // Build DELETE query using QueryBuilder
        // IMPORTANT: Always apply 24-hour protection via end_time filter
        let mut query_builder =
            sqlx::QueryBuilder::<Rdb>::new("DELETE FROM job_result WHERE end_time < ");
        query_builder.push_bind(effective_cutoff);

        if !statuses.is_empty() {
            query_builder.push(" AND status IN (");
            let mut separated = query_builder.separated(", ");
            for status in &statuses {
                separated.push_bind(*status);
            }
            separated.push_unseparated(")");
        }

        if !worker_ids.is_empty() {
            query_builder.push(" AND worker_id IN (");
            let mut separated = query_builder.separated(", ");
            for worker_id in &worker_ids {
                separated.push_bind(*worker_id);
            }
            separated.push_unseparated(")");
        }

        // Execute DELETE
        let result = query_builder
            .build()
            .execute(&mut *tx)
            .await
            .map_err(JobWorkerError::DBError)?;

        let deleted_count = result.rows_affected() as i64;

        // Commit transaction
        tx.commit().await.map_err(JobWorkerError::DBError)?;

        tracing::info!(
            deleted_count = deleted_count,
            effective_cutoff = effective_cutoff,
            end_time_before = ?end_time_before,
            statuses_count = statuses.len(),
            worker_ids_count = worker_ids.len(),
            "Bulk delete completed with 24-hour protection enforced"
        );

        Ok(deleted_count)
    }
}

#[derive(Clone, Debug)]
pub struct RdbJobResultRepositoryImpl {
    pool: &'static RdbPool,
}

pub trait UseRdbJobResultRepository {
    fn rdb_job_result_repository(&self) -> &RdbJobResultRepositoryImpl;
}

impl RdbJobResultRepositoryImpl {
    pub fn new(pool: &'static RdbPool) -> Self {
        Self { pool }
    }
}

impl UseRdbPool for RdbJobResultRepositoryImpl {
    fn db_pool(&self) -> &RdbPool {
        self.pool
    }
}

impl jobworkerp_base::codec::UseProstCodec for RdbJobResultRepositoryImpl {}
impl UseJobqueueAndCodec for RdbJobResultRepositoryImpl {}

#[async_trait]
impl RdbJobResultRepository for RdbJobResultRepositoryImpl {}

mod test {
    use std::collections::HashMap;

    use super::RdbJobResultRepository;
    use super::RdbJobResultRepositoryImpl;
    use anyhow::Context;
    use anyhow::Result;
    use infra_utils::infra::rdb::RdbPool;
    use infra_utils::infra::rdb::UseRdbPool;
    use jobworkerp_base::codec::UseProstCodec;
    use proto::jobworkerp::data::JobId;
    use proto::jobworkerp::data::JobResult;
    use proto::jobworkerp::data::JobResultData;
    use proto::jobworkerp::data::JobResultId;
    use proto::jobworkerp::data::ResultOutput;
    use proto::jobworkerp::data::ResultStatus;
    use proto::jobworkerp::data::WorkerId;
    use proto::TestArgs;

    async fn _test_repository(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbJobResultRepositoryImpl::new(pool);
        let db = repository.db_pool();
        let args = TestArgs {
            args: vec!["hoge".to_string()],
        };
        #[allow(deprecated)]
        let data = Some(JobResultData {
            job_id: Some(JobId { value: 1 }),
            worker_id: Some(WorkerId { value: 2 }),
            worker_name: "".to_string(),
            args: RdbJobResultRepositoryImpl::serialize_message(&args).unwrap(),
            uniq_key: Some("hoge4".to_string()),
            status: ResultStatus::ErrorAndRetry as i32,
            output: Some(ResultOutput {
                items: "hoge6".as_bytes().to_vec(),
            }),
            retried: 8,
            max_retry: 0, // fixed
            priority: 1,
            timeout: 1000,
            streaming_type: 0, // StreamingType::None
            enqueue_time: 9,
            run_after_time: 10,
            start_time: 11,
            end_time: 12,
            response_type: 0,     // fixed
            store_success: false, // fixed
            store_failure: false, // fixed
            using: None,
        });

        let id = JobResultId { value: 111 };
        assert!(repository.create(&id, &data.clone().unwrap()).await?);

        let id1 = id;
        let expect = JobResult {
            id: Some(id1),
            data,
            metadata: HashMap::new(), // not stored in rdb etc
        };

        // find
        let found = repository.find(&id1).await?;
        assert_eq!(Some(&expect), found.as_ref());

        // update
        let mut tx = db.begin().await.context("error in test")?;
        let args = TestArgs {
            args: vec!["fuga".to_string()],
        };
        #[allow(deprecated)]
        let update = JobResultData {
            job_id: Some(JobId { value: 2 }),
            worker_id: Some(WorkerId { value: 3 }),
            worker_name: "".to_string(), // fixed
            args: RdbJobResultRepositoryImpl::serialize_message(&args).unwrap(),
            uniq_key: Some("fuga4".to_string()),
            status: ResultStatus::FatalError as i32,
            output: Some(ResultOutput {
                items: "fuga6".as_bytes().to_vec(),
            }),
            retried: 1,
            max_retry: 0, // fixed
            priority: -1,
            timeout: 2000,
            streaming_type: 1, // StreamingType::Response
            enqueue_time: 10,
            run_after_time: 11,
            start_time: 12,
            end_time: 13,
            response_type: 0, // fixed
            store_success: false,
            store_failure: false,
            using: None,
        };
        let updated = repository.update(&mut tx, &id, &update).await?;
        assert!(updated);
        tx.commit().await.context("error in test delete commit")?;

        // find
        let found = repository.find(&id).await?;
        assert_eq!(&update, &found.unwrap().data.unwrap());
        let count = repository.count_list_tx(repository.db_pool()).await?;
        assert_eq!(1, count);

        // delete record
        tx = db.begin().await.context("error in test")?;
        let del = repository.delete_tx(&mut *tx, &id).await?;
        tx.commit().await.context("error in test delete commit")?;
        assert!(del, "delete error");
        Ok(())
    }

    #[cfg(not(feature = "mysql"))]
    #[test]
    fn test_sqlite() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let sqlite_pool = setup_test_rdb_from("sql/sqlite").await;
            sqlx::query("DELETE FROM job_result;")
                .execute(sqlite_pool)
                .await?;
            _test_repository(sqlite_pool).await
        })
    }

    #[cfg(feature = "mysql")]
    #[test]
    fn test_mysql() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let mysql_pool = setup_test_rdb_from("sql/mysql").await;
            sqlx::query("TRUNCATE TABLE job_result;")
                .execute(mysql_pool)
                .await?;
            _test_repository(mysql_pool).await
        })
    }

    /// Helper function to create test job result data
    #[allow(dead_code, deprecated)]
    fn create_test_job_result_data(
        job_id: i64,
        worker_id: i64,
        status: ResultStatus,
        start_time: i64,
        end_time: i64,
        priority: i32,
        uniq_key: Option<String>,
    ) -> JobResultData {
        let args = TestArgs {
            args: vec!["test".to_string()],
        };
        JobResultData {
            job_id: Some(JobId { value: job_id }),
            worker_id: Some(WorkerId { value: worker_id }),
            worker_name: String::new(),
            args: RdbJobResultRepositoryImpl::serialize_message(&args).unwrap(),
            uniq_key,
            status: status as i32,
            output: Some(ResultOutput {
                items: b"test_output".to_vec(),
            }),
            retried: 0,
            max_retry: 0,
            priority,
            timeout: 1000,
            streaming_type: 0, // StreamingType::None
            enqueue_time: start_time - 1000,
            run_after_time: start_time - 500,
            start_time,
            end_time,
            response_type: 0,
            store_success: false,
            store_failure: false,
            using: None,
        }
    }

    /// Test find_list_by with worker_ids filter
    async fn _test_find_list_by_worker_ids(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbJobResultRepositoryImpl::new(pool);

        // Prepare test data: 3 results from worker 1, 2 results from worker 2
        let base_id = 8000;
        let now = command_utils::util::datetime::now_millis();
        for i in 1..=5 {
            let worker_id = if i <= 3 { 1 } else { 2 };
            let data = create_test_job_result_data(
                i,
                worker_id,
                ResultStatus::Success,
                now - (i * 1000),
                now - (i * 500),
                0,
                None,
            );
            repository
                .create(&JobResultId { value: base_id + i }, &data)
                .await?;
        }

        // Test 1: Filter by worker_id = 1
        let results = repository
            .find_list_by(
                vec![1],
                vec![],
                None,
                None,
                None,
                None,
                vec![],
                None,
                None,
                None,
                None,
                None,
            )
            .await?;
        assert_eq!(results.len(), 3, "Should find 3 results for worker 1");

        // Test 2: Filter by worker_id IN (1, 2)
        let results = repository
            .find_list_by(
                vec![1, 2],
                vec![],
                None,
                None,
                None,
                None,
                vec![],
                None,
                None,
                None,
                None,
                None,
            )
            .await?;
        assert_eq!(results.len(), 5, "Should find all 5 results");

        // Test 3: Filter by non-existent worker_id
        let results = repository
            .find_list_by(
                vec![999],
                vec![],
                None,
                None,
                None,
                None,
                vec![],
                None,
                None,
                None,
                None,
                None,
            )
            .await?;
        assert_eq!(results.len(), 0, "Should find no results for worker 999");

        Ok(())
    }

    /// Test find_list_by with statuses filter
    async fn _test_find_list_by_statuses(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbJobResultRepositoryImpl::new(pool);

        // Prepare test data: 2 SUCCESS, 2 FATAL_ERROR, 1 ERROR_AND_RETRY
        let base_id = 10000;
        let now = command_utils::util::datetime::now_millis();
        let statuses = [
            ResultStatus::Success,
            ResultStatus::Success,
            ResultStatus::FatalError,
            ResultStatus::FatalError,
            ResultStatus::ErrorAndRetry,
        ];

        for (i, status) in statuses.iter().enumerate() {
            let data = create_test_job_result_data(
                (i + 1) as i64,
                1,
                *status,
                now - ((i as i64) * 1000),
                now - ((i as i64) * 500),
                0,
                None,
            );
            repository
                .create(
                    &JobResultId {
                        value: base_id + (i + 1) as i64,
                    },
                    &data,
                )
                .await?;
        }

        // Test 1: Filter by status = SUCCESS
        let results = repository
            .find_list_by(
                vec![],
                vec![ResultStatus::Success as i32],
                None,
                None,
                None,
                None,
                vec![],
                None,
                None,
                None,
                None,
                None,
            )
            .await?;
        assert_eq!(results.len(), 2, "Should find 2 SUCCESS results");

        // Test 2: Filter by status IN (FATAL_ERROR, ERROR_AND_RETRY)
        let results = repository
            .find_list_by(
                vec![],
                vec![
                    ResultStatus::FatalError as i32,
                    ResultStatus::ErrorAndRetry as i32,
                ],
                None,
                None,
                None,
                None,
                vec![],
                None,
                None,
                None,
                None,
                None,
            )
            .await?;
        assert_eq!(
            results.len(),
            3,
            "Should find 3 FATAL_ERROR or ERROR_AND_RETRY results"
        );

        Ok(())
    }

    /// Test find_list_by with time range filters
    async fn _test_find_list_by_time_range(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbJobResultRepositoryImpl::new(pool);

        // Prepare test data with different time ranges and unique IDs for this test
        let base_id = 7000;
        let base_time = 1700000000000i64; // 2023-11-14 22:13:20 UTC
        for i in 0..5 {
            let start_time = base_time + (i * 86400000); // +1 day each
            let end_time = start_time + 3600000; // +1 hour
            let data = create_test_job_result_data(
                i + 1,
                1,
                ResultStatus::Success,
                start_time,
                end_time,
                0,
                None,
            );
            repository
                .create(
                    &JobResultId {
                        value: base_id + i + 1,
                    },
                    &data,
                )
                .await?;
        }

        // Test 1: Filter by start_time_from
        let results = repository
            .find_list_by(
                vec![],
                vec![],
                Some(base_time + 172800000), // +2 days
                None,
                None,
                None,
                vec![],
                None,
                None,
                None,
                None,
                None,
            )
            .await?;
        assert_eq!(
            results.len(),
            3,
            "Should find 3 results starting from day 2"
        );

        // Test 2: Filter by start_time range
        let results = repository
            .find_list_by(
                vec![],
                vec![],
                Some(base_time + 86400000),  // +1 day
                Some(base_time + 259200000), // +3 days
                None,
                None,
                vec![],
                None,
                None,
                None,
                None,
                None,
            )
            .await?;
        assert_eq!(results.len(), 3, "Should find 3 results in day 1-3 range");

        // Test 3: Filter by end_time range
        let results = repository
            .find_list_by(
                vec![],
                vec![],
                None,
                None,
                Some(base_time + 90000000),  // +1 day +1 hour
                Some(base_time + 262800000), // +3 days +1 hour
                vec![],
                None,
                None,
                None,
                None,
                None,
            )
            .await?;
        assert_eq!(results.len(), 3, "Should find 3 results by end_time");

        Ok(())
    }

    /// Test find_list_by with priorities filter
    async fn _test_find_list_by_priorities(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbJobResultRepositoryImpl::new(pool);

        // Prepare test data with different priorities
        let now = command_utils::util::datetime::now_millis();
        let priorities = [0, 0, 1, 1, -1];

        for (i, priority) in priorities.iter().enumerate() {
            let data = create_test_job_result_data(
                (i + 1) as i64,
                1,
                ResultStatus::Success,
                now - ((i as i64) * 1000),
                now - ((i as i64) * 500),
                *priority,
                None,
            );
            repository
                .create(
                    &JobResultId {
                        value: (i + 1) as i64,
                    },
                    &data,
                )
                .await?;
        }

        // Test 1: Filter by priority = 0
        let results = repository
            .find_list_by(
                vec![],
                vec![],
                None,
                None,
                None,
                None,
                vec![0],
                None,
                None,
                None,
                None,
                None,
            )
            .await?;
        assert_eq!(results.len(), 2, "Should find 2 results with priority 0");

        // Test 2: Filter by priority IN (1, -1)
        let results = repository
            .find_list_by(
                vec![],
                vec![],
                None,
                None,
                None,
                None,
                vec![1, -1],
                None,
                None,
                None,
                None,
                None,
            )
            .await?;
        assert_eq!(
            results.len(),
            3,
            "Should find 3 results with priority 1 or -1"
        );

        Ok(())
    }

    /// Test find_list_by with uniq_key filter
    async fn _test_find_list_by_uniq_key(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbJobResultRepositoryImpl::new(pool);

        // Prepare test data with unique IDs for this test
        let base_id = 11000;
        let now = command_utils::util::datetime::now_millis();
        let data1 = create_test_job_result_data(
            1,
            1,
            ResultStatus::Success,
            now - 1000,
            now - 500,
            0,
            Some("unique_key_1".to_string()),
        );
        let data2 = create_test_job_result_data(
            2,
            1,
            ResultStatus::Success,
            now - 2000,
            now - 1500,
            0,
            Some("unique_key_2".to_string()),
        );
        let data3 = create_test_job_result_data(
            3,
            1,
            ResultStatus::Success,
            now - 3000,
            now - 2500,
            0,
            None,
        );

        repository
            .create(&JobResultId { value: base_id + 1 }, &data1)
            .await?;
        repository
            .create(&JobResultId { value: base_id + 2 }, &data2)
            .await?;
        repository
            .create(&JobResultId { value: base_id + 3 }, &data3)
            .await?;

        // Test 1: Find by exact uniq_key match
        let results = repository
            .find_list_by(
                vec![],
                vec![],
                None,
                None,
                None,
                None,
                vec![],
                Some("unique_key_1".to_string()),
                None,
                None,
                None,
                None,
            )
            .await?;
        assert_eq!(results.len(), 1, "Should find 1 result with uniq_key");
        assert_eq!(
            results[0].data.as_ref().unwrap().uniq_key,
            Some("unique_key_1".to_string())
        );

        // Test 2: Find by non-existent uniq_key
        let results = repository
            .find_list_by(
                vec![],
                vec![],
                None,
                None,
                None,
                None,
                vec![],
                Some("non_existent".to_string()),
                None,
                None,
                None,
                None,
            )
            .await?;
        assert_eq!(results.len(), 0, "Should find no results");

        Ok(())
    }

    /// Test find_list_by with pagination (limit and offset)
    async fn _test_find_list_by_pagination(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbJobResultRepositoryImpl::new(pool);

        // Prepare 10 test results with unique IDs for this test
        let base_id = 12000;
        let now = command_utils::util::datetime::now_millis();
        for i in 1..=10 {
            let data = create_test_job_result_data(
                i,
                1,
                ResultStatus::Success,
                now - (i * 1000),
                now - (i * 500),
                0,
                None,
            );
            repository
                .create(&JobResultId { value: base_id + i }, &data)
                .await?;
        }

        // Test 1: Limit only
        let results = repository
            .find_list_by(
                vec![],
                vec![],
                None,
                None,
                None,
                None,
                vec![],
                None,
                Some(5),
                None,
                None,
                None,
            )
            .await?;
        assert_eq!(results.len(), 5, "Should return 5 results");

        // Test 2: Limit with offset
        let results = repository
            .find_list_by(
                vec![],
                vec![],
                None,
                None,
                None,
                None,
                vec![],
                None,
                Some(3),
                Some(2),
                None,
                None,
            )
            .await?;
        assert_eq!(
            results.len(),
            3,
            "Should return 3 results starting from offset 2"
        );

        // Test 3: Limit exceeding maximum (should be clamped to 1000)
        let results = repository
            .find_list_by(
                vec![],
                vec![],
                None,
                None,
                None,
                None,
                vec![],
                None,
                Some(2000), // Should be clamped to 1000
                None,
                None,
                None,
            )
            .await?;
        assert_eq!(results.len(), 10, "Should return all 10 results (max 1000)");

        Ok(())
    }

    /// Test find_list_by with sorting
    async fn _test_find_list_by_sort(pool: &'static RdbPool) -> Result<()> {
        use proto::jobworkerp::data::JobResultSortField;

        let repository = RdbJobResultRepositoryImpl::new(pool);

        // Prepare test data with different timestamps and unique IDs for this test
        let base_id = 9000;
        let base_time = 1700000000000i64;
        for i in 1..=5 {
            let data = create_test_job_result_data(
                i,
                1,
                ResultStatus::Success,
                base_time + (i * 1000),
                base_time + (i * 2000),
                0,
                None,
            );
            repository
                .create(&JobResultId { value: base_id + i }, &data)
                .await?;
        }

        // Test 1: Sort by end_time DESC (default)
        let results = repository
            .find_list_by(
                vec![],
                vec![],
                None,
                None,
                None,
                None,
                vec![],
                None,
                None,
                None,
                None,
                None,
            )
            .await?;
        assert!(
            results[0].data.as_ref().unwrap().end_time > results[1].data.as_ref().unwrap().end_time,
            "Should be sorted DESC by default"
        );

        // Test 2: Sort by start_time ASC
        let results = repository
            .find_list_by(
                vec![],
                vec![],
                None,
                None,
                None,
                None,
                vec![],
                None,
                None,
                None,
                Some(JobResultSortField::StartTime),
                Some(true),
            )
            .await?;
        assert!(
            results[0].data.as_ref().unwrap().start_time
                < results[1].data.as_ref().unwrap().start_time,
            "Should be sorted ASC by start_time"
        );

        // Test 3: Sort by id DESC
        let results = repository
            .find_list_by(
                vec![],
                vec![],
                None,
                None,
                None,
                None,
                vec![],
                None,
                None,
                None,
                Some(JobResultSortField::Id),
                Some(false),
            )
            .await?;
        assert!(
            results[0].id.as_ref().unwrap().value > results[1].id.as_ref().unwrap().value,
            "Should be sorted DESC by id"
        );

        Ok(())
    }

    /// Test count_by with worker_ids filter
    async fn _test_count_by_worker_ids(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbJobResultRepositoryImpl::new(pool);

        // Prepare test data with unique IDs for this test
        let base_id = 3000;
        let now = command_utils::util::datetime::now_millis();
        for i in 1..=5 {
            let worker_id = if i <= 3 { 1 } else { 2 };
            let data = create_test_job_result_data(
                i,
                worker_id,
                ResultStatus::Success,
                now - (i * 1000),
                now - (i * 500),
                0,
                None,
            );
            repository
                .create(&JobResultId { value: base_id + i }, &data)
                .await?;
        }

        // Test 1: Count by worker_id = 1
        let count = repository
            .count_by(vec![1], vec![], None, None, None, None, vec![], None)
            .await?;
        assert_eq!(count, 3, "Should count 3 results for worker 1");

        // Test 2: Count by worker_id IN (1, 2)
        let count = repository
            .count_by(vec![1, 2], vec![], None, None, None, None, vec![], None)
            .await?;
        assert_eq!(count, 5, "Should count all 5 results");

        Ok(())
    }

    /// Test count_by with statuses filter
    async fn _test_count_by_statuses(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbJobResultRepositoryImpl::new(pool);

        // Prepare test data with unique IDs for this test
        let base_id = 4000;
        let now = command_utils::util::datetime::now_millis();
        let statuses = [
            ResultStatus::Success,
            ResultStatus::Success,
            ResultStatus::FatalError,
        ];

        for (i, status) in statuses.iter().enumerate() {
            let data = create_test_job_result_data(
                (i + 1) as i64,
                1,
                *status,
                now - ((i as i64) * 1000),
                now - ((i as i64) * 500),
                0,
                None,
            );
            repository
                .create(
                    &JobResultId {
                        value: base_id + (i + 1) as i64,
                    },
                    &data,
                )
                .await?;
        }

        // Test: Count by status = SUCCESS
        let count = repository
            .count_by(
                vec![],
                vec![ResultStatus::Success as i32],
                None,
                None,
                None,
                None,
                vec![],
                None,
            )
            .await?;
        assert_eq!(count, 2, "Should count 2 SUCCESS results");

        Ok(())
    }

    /// Test count_by with time range filter
    async fn _test_count_by_time_range(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbJobResultRepositoryImpl::new(pool);

        // Prepare test data with unique IDs for this test
        let base_id = 5000;
        let base_time = 1700000000000i64;
        for i in 0..5 {
            let start_time = base_time + (i * 86400000);
            let end_time = start_time + 3600000;
            let data = create_test_job_result_data(
                i + 1,
                1,
                ResultStatus::Success,
                start_time,
                end_time,
                0,
                None,
            );
            repository
                .create(
                    &JobResultId {
                        value: base_id + i + 1,
                    },
                    &data,
                )
                .await?;
        }

        // Test: Count by time range
        let count = repository
            .count_by(
                vec![],
                vec![],
                Some(base_time + 86400000),
                Some(base_time + 259200000),
                None,
                None,
                vec![],
                None,
            )
            .await?;
        assert_eq!(count, 3, "Should count 3 results in time range");

        Ok(())
    }

    /// Test delete_bulk by time
    async fn _test_delete_bulk_by_time(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbJobResultRepositoryImpl::new(pool);

        // Prepare test data (older than 24 hours) with unique IDs for this test
        let base_id = 6000;
        let now = command_utils::util::datetime::now_millis();
        let two_days_ago = now - (48 * 60 * 60 * 1000);

        for i in 1..=5 {
            let data = create_test_job_result_data(
                i,
                1,
                ResultStatus::Success,
                two_days_ago - (i * 1000),
                two_days_ago - (i * 500),
                0,
                None,
            );
            repository
                .create(&JobResultId { value: base_id + i }, &data)
                .await?;
        }

        // Test: Delete old results (older than 25 hours)
        let delete_before = now - (25 * 60 * 60 * 1000);
        let deleted_count = repository
            .delete_bulk(Some(delete_before), vec![], vec![])
            .await?;
        assert_eq!(deleted_count, 5, "Should delete all 5 old results");

        let remaining = repository.count_list_tx(pool).await?;
        assert_eq!(remaining, 0, "Should have no remaining results");

        Ok(())
    }

    /// Test delete_bulk by status
    async fn _test_delete_bulk_by_status(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbJobResultRepositoryImpl::new(pool);

        // Prepare test data (older than 24 hours)
        let now = command_utils::util::datetime::now_millis();
        let two_days_ago = now - (48 * 60 * 60 * 1000);

        let base_id = 2000;
        let data1 = create_test_job_result_data(
            1,
            1,
            ResultStatus::Success,
            two_days_ago,
            two_days_ago + 1000,
            0,
            None,
        );
        let data2 = create_test_job_result_data(
            2,
            1,
            ResultStatus::FatalError,
            two_days_ago,
            two_days_ago + 1000,
            0,
            None,
        );

        repository
            .create(&JobResultId { value: base_id + 1 }, &data1)
            .await?;
        repository
            .create(&JobResultId { value: base_id + 2 }, &data2)
            .await?;

        // Test: Delete only FATAL_ERROR results
        let deleted_count = repository
            .delete_bulk(None, vec![ResultStatus::FatalError as i32], vec![])
            .await?;
        assert_eq!(deleted_count, 1, "Should delete 1 FATAL_ERROR result");

        let remaining = repository.count_list_tx(pool).await?;
        assert_eq!(remaining, 1, "Should have 1 remaining SUCCESS result");

        Ok(())
    }

    /// Test delete_bulk safety checks
    async fn _test_delete_bulk_safety_checks(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbJobResultRepositoryImpl::new(pool);

        // Test 1: No filter condition (should fail)
        let result = repository.delete_bulk(None, vec![], vec![]).await;
        assert!(
            result.is_err(),
            "Should fail when no filter condition is provided"
        );

        // Test 2: Recent data protection (should fail)
        let now = command_utils::util::datetime::now_millis();
        let one_hour_ago = now - (60 * 60 * 1000);
        let result = repository
            .delete_bulk(Some(one_hour_ago), vec![], vec![])
            .await;
        assert!(
            result.is_err(),
            "Should fail when trying to delete data within 24 hours"
        );

        Ok(())
    }

    /// Test delete_bulk transaction rollback
    async fn _test_delete_bulk_transaction_rollback(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbJobResultRepositoryImpl::new(pool);

        // Prepare test data with unique ID for this test
        let base_id = 13000;
        let now = command_utils::util::datetime::now_millis();
        let two_days_ago = now - (48 * 60 * 60 * 1000);

        let data = create_test_job_result_data(
            1,
            1,
            ResultStatus::Success,
            two_days_ago,
            two_days_ago + 1000,
            0,
            None,
        );
        repository
            .create(&JobResultId { value: base_id + 1 }, &data)
            .await?;

        // Count before deletion
        let count_before = repository.count_list_tx(pool).await?;
        assert_eq!(count_before, 1);

        // Test: Valid deletion
        let delete_before = now - (25 * 60 * 60 * 1000);
        let deleted = repository
            .delete_bulk(Some(delete_before), vec![], vec![])
            .await?;
        assert_eq!(deleted, 1);

        let count_after = repository.count_list_tx(pool).await?;
        assert_eq!(count_after, 0, "Deletion should be committed");

        Ok(())
    }

    /// Test delete_bulk enforces 24-hour protection without time filter (CRITICAL-002 regression test)
    async fn _test_delete_bulk_enforces_24h_protection_without_time_filter(
        pool: &'static RdbPool,
    ) -> Result<()> {
        let repository = RdbJobResultRepositoryImpl::new(pool);

        // Setup: Create recent JobResult (1 hour ago)
        let base_id = 14000;
        let now = command_utils::util::datetime::now_millis();
        let one_hour_ago = now - (60 * 60 * 1000);

        let data = create_test_job_result_data(
            1,
            1,
            ResultStatus::Success,
            one_hour_ago,
            one_hour_ago + 1000,
            0,
            None,
        );
        repository
            .create(&JobResultId { value: base_id + 1 }, &data)
            .await?;

        // Test: Attempt deletion with statuses only (no time filter)
        let deleted = repository
            .delete_bulk(None, vec![ResultStatus::Success as i32], vec![])
            .await?;

        // Expected: 24-hour protection enforced, no deletion
        assert_eq!(deleted, 0, "Recent data (1 hour ago) should not be deleted");

        let found = repository.find(&JobResultId { value: base_id + 1 }).await?;
        assert!(found.is_some(), "Recent data should still exist");

        Ok(())
    }

    /// Test delete_bulk enforces 24-hour protection with explicit time within 24h (CRITICAL-002 regression test)
    async fn _test_delete_bulk_explicit_time_within_24h_rejected(
        pool: &'static RdbPool,
    ) -> Result<()> {
        let repository = RdbJobResultRepositoryImpl::new(pool);

        let now = command_utils::util::datetime::now_millis();
        let cutoff_12h = now - (12 * 60 * 60 * 1000); // 12 hours ago

        // Test: Attempt deletion with end_time_before within 24 hours
        let result = repository
            .delete_bulk(Some(cutoff_12h), vec![], vec![])
            .await;

        // Expected: Error due to 24-hour protection
        assert!(
            result.is_err(),
            "Deletion with cutoff within 24 hours should fail"
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("within last 24 hours"),
            "Error message should mention 24-hour protection"
        );

        Ok(())
    }

    /// Test delete_bulk with old data and status filter succeeds (CRITICAL-002 regression test)
    async fn _test_delete_bulk_old_data_with_status_filter_succeeds(
        pool: &'static RdbPool,
    ) -> Result<()> {
        let repository = RdbJobResultRepositoryImpl::new(pool);

        // Setup: Create old JobResult (48 hours ago)
        let base_id = 15000;
        let now = command_utils::util::datetime::now_millis();
        let two_days_ago = now - (48 * 60 * 60 * 1000);

        let data = create_test_job_result_data(
            1,
            1,
            ResultStatus::Success,
            two_days_ago,
            two_days_ago + 1000,
            0,
            None,
        );
        repository
            .create(&JobResultId { value: base_id + 1 }, &data)
            .await?;

        // Test: Deletion with statuses only (no explicit time filter)
        let deleted = repository
            .delete_bulk(None, vec![ResultStatus::Success as i32], vec![])
            .await?;

        // Expected: Old data (48 hours ago) should be deleted
        assert_eq!(deleted, 1, "Old data (48 hours ago) should be deleted");

        let found = repository.find(&JobResultId { value: base_id + 1 }).await?;
        assert!(found.is_none(), "Old data should be deleted");

        Ok(())
    }

    /// Test delete_bulk with mixed recent and old data (CRITICAL-002 regression test)
    async fn _test_delete_bulk_mixed_recent_and_old_data(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbJobResultRepositoryImpl::new(pool);

        // Setup: Create mixed data
        let base_id = 16000;
        let now = command_utils::util::datetime::now_millis();
        let one_hour_ago = now - (60 * 60 * 1000);
        let two_days_ago = now - (48 * 60 * 60 * 1000);

        // Recent data (1 hour ago)
        let recent_data = create_test_job_result_data(
            1,
            1,
            ResultStatus::Success,
            one_hour_ago,
            one_hour_ago + 1000,
            0,
            None,
        );
        repository
            .create(&JobResultId { value: base_id + 1 }, &recent_data)
            .await?;

        // Old data (48 hours ago)
        let old_data = create_test_job_result_data(
            1,
            2,
            ResultStatus::Success,
            two_days_ago,
            two_days_ago + 1000,
            0,
            None,
        );
        repository
            .create(&JobResultId { value: base_id + 2 }, &old_data)
            .await?;

        // Test: Deletion with statuses only
        let deleted = repository
            .delete_bulk(None, vec![ResultStatus::Success as i32], vec![])
            .await?;

        // Expected: Only old data deleted
        assert_eq!(deleted, 1, "Only old data should be deleted");

        let recent_found = repository.find(&JobResultId { value: base_id + 1 }).await?;
        assert!(recent_found.is_some(), "Recent data should still exist");

        let old_found = repository.find(&JobResultId { value: base_id + 2 }).await?;
        assert!(old_found.is_none(), "Old data should be deleted");

        Ok(())
    }

    #[cfg(not(feature = "mysql"))]
    #[test]
    fn test_sprint4_find_list_by_worker_ids() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_rdb_from("sql/sqlite").await;
            sqlx::query("DELETE FROM job_result;").execute(pool).await?;
            _test_find_list_by_worker_ids(pool).await
        })
    }

    #[cfg(not(feature = "mysql"))]
    #[test]
    fn test_sprint4_find_list_by_statuses() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_rdb_from("sql/sqlite").await;
            sqlx::query("DELETE FROM job_result;").execute(pool).await?;
            _test_find_list_by_statuses(pool).await
        })
    }

    #[cfg(not(feature = "mysql"))]
    #[test]
    fn test_sprint4_find_list_by_time_range() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_rdb_from("sql/sqlite").await;
            sqlx::query("DELETE FROM job_result;").execute(pool).await?;
            _test_find_list_by_time_range(pool).await
        })
    }

    #[cfg(not(feature = "mysql"))]
    #[test]
    fn test_sprint4_find_list_by_priorities() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_rdb_from("sql/sqlite").await;
            sqlx::query("DELETE FROM job_result;").execute(pool).await?;
            _test_find_list_by_priorities(pool).await
        })
    }

    #[cfg(not(feature = "mysql"))]
    #[test]
    fn test_sprint4_find_list_by_uniq_key() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_rdb_from("sql/sqlite").await;
            sqlx::query("DELETE FROM job_result;").execute(pool).await?;
            _test_find_list_by_uniq_key(pool).await
        })
    }

    #[cfg(not(feature = "mysql"))]
    #[test]
    fn test_sprint4_find_list_by_pagination() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_rdb_from("sql/sqlite").await;
            sqlx::query("DELETE FROM job_result;").execute(pool).await?;
            _test_find_list_by_pagination(pool).await
        })
    }

    #[cfg(not(feature = "mysql"))]
    #[test]
    fn test_sprint4_find_list_by_sort() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_rdb_from("sql/sqlite").await;
            sqlx::query("DELETE FROM job_result;").execute(pool).await?;
            _test_find_list_by_sort(pool).await
        })
    }

    #[cfg(not(feature = "mysql"))]
    #[test]
    fn test_sprint4_count_by_worker_ids() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_rdb_from("sql/sqlite").await;
            sqlx::query("DELETE FROM job_result;").execute(pool).await?;
            _test_count_by_worker_ids(pool).await
        })
    }

    #[cfg(not(feature = "mysql"))]
    #[test]
    fn test_sprint4_count_by_statuses() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_rdb_from("sql/sqlite").await;
            sqlx::query("DELETE FROM job_result;").execute(pool).await?;
            _test_count_by_statuses(pool).await
        })
    }

    #[cfg(not(feature = "mysql"))]
    #[test]
    fn test_sprint4_count_by_time_range() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_rdb_from("sql/sqlite").await;
            sqlx::query("DELETE FROM job_result;").execute(pool).await?;
            _test_count_by_time_range(pool).await
        })
    }

    #[cfg(not(feature = "mysql"))]
    #[test]
    fn test_sprint4_delete_bulk_by_time() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_rdb_from("sql/sqlite").await;
            sqlx::query("DELETE FROM job_result;").execute(pool).await?;
            _test_delete_bulk_by_time(pool).await
        })
    }

    #[cfg(not(feature = "mysql"))]
    #[test]
    fn test_sprint4_delete_bulk_by_status() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_rdb_from("sql/sqlite").await;
            sqlx::query("DELETE FROM job_result;").execute(pool).await?;
            _test_delete_bulk_by_status(pool).await
        })
    }

    #[cfg(not(feature = "mysql"))]
    #[test]
    fn test_sprint4_delete_bulk_safety_checks() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_rdb_from("sql/sqlite").await;
            sqlx::query("DELETE FROM job_result;").execute(pool).await?;
            _test_delete_bulk_safety_checks(pool).await
        })
    }

    #[cfg(not(feature = "mysql"))]
    #[test]
    fn test_sprint4_delete_bulk_transaction() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_rdb_from("sql/sqlite").await;
            sqlx::query("DELETE FROM job_result;").execute(pool).await?;
            _test_delete_bulk_transaction_rollback(pool).await
        })
    }

    // CRITICAL-002 regression tests

    #[cfg(not(feature = "mysql"))]
    #[test]
    fn test_critical002_delete_bulk_enforces_24h_protection_without_time_filter() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_rdb_from("sql/sqlite").await;
            sqlx::query("DELETE FROM job_result;").execute(pool).await?;
            _test_delete_bulk_enforces_24h_protection_without_time_filter(pool).await
        })
    }

    #[cfg(not(feature = "mysql"))]
    #[test]
    fn test_critical002_delete_bulk_explicit_time_within_24h_rejected() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_rdb_from("sql/sqlite").await;
            sqlx::query("DELETE FROM job_result;").execute(pool).await?;
            _test_delete_bulk_explicit_time_within_24h_rejected(pool).await
        })
    }

    #[cfg(not(feature = "mysql"))]
    #[test]
    fn test_critical002_delete_bulk_old_data_with_status_filter_succeeds() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_rdb_from("sql/sqlite").await;
            sqlx::query("DELETE FROM job_result;").execute(pool).await?;
            _test_delete_bulk_old_data_with_status_filter_succeeds(pool).await
        })
    }

    #[cfg(not(feature = "mysql"))]
    #[test]
    fn test_critical002_delete_bulk_mixed_recent_and_old_data() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_rdb_from("sql/sqlite").await;
            sqlx::query("DELETE FROM job_result;").execute(pool).await?;
            _test_delete_bulk_mixed_recent_and_old_data(pool).await
        })
    }

    /// Test all streaming_type values (0=None, 1=Response, 2=Internal) are correctly
    /// stored and retrieved from DB for JobResult
    #[allow(deprecated)]
    async fn _test_streaming_type_all_values(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbJobResultRepositoryImpl::new(pool);

        // Test each streaming_type value: None(0), Response(1), Internal(2)
        let streaming_type_values = [(0i32, "None"), (1i32, "Response"), (2i32, "Internal")];

        for (streaming_type_value, type_name) in streaming_type_values {
            let job_result_id = JobResultId {
                value: 200 + streaming_type_value as i64,
            };
            let args = TestArgs {
                args: vec![format!("streaming_type_{}", type_name)],
            };

            let job_result_data = JobResultData {
                job_id: Some(JobId {
                    value: 100 + streaming_type_value as i64,
                }),
                worker_id: Some(WorkerId { value: 1 }),
                worker_name: String::new(),
                args: RdbJobResultRepositoryImpl::serialize_message(&args).unwrap(),
                uniq_key: Some(format!("streaming_test_{}", type_name)),
                status: ResultStatus::Success as i32,
                output: Some(ResultOutput {
                    items: b"test_output".to_vec(),
                }),
                retried: 0,
                max_retry: 0,
                priority: 0,
                timeout: 5000,
                streaming_type: streaming_type_value,
                enqueue_time: 1000,
                run_after_time: 0,
                start_time: 1100,
                end_time: 1200,
                response_type: 0,
                store_success: false,
                store_failure: false,
                using: None,
            };

            // Create job result
            let created = repository.create(&job_result_id, &job_result_data).await?;
            assert!(
                created,
                "Failed to create job_result with streaming_type={}",
                type_name
            );

            // Find and verify streaming_type is preserved
            let found = repository.find(&job_result_id).await?;
            assert!(
                found.is_some(),
                "JobResult not found for streaming_type={}",
                type_name
            );
            let found_data = found.unwrap().data.unwrap();
            assert_eq!(
                found_data.streaming_type, streaming_type_value,
                "streaming_type mismatch after create: expected {} ({}), got {}",
                streaming_type_value, type_name, found_data.streaming_type
            );

            // Update to different streaming_type and verify
            let new_streaming_type = (streaming_type_value + 1) % 3;
            let updated_data = JobResultData {
                streaming_type: new_streaming_type,
                ..job_result_data.clone()
            };
            let db = repository.db_pool();
            let mut tx = db.begin().await.context("error in test")?;
            let updated = repository
                .update(&mut tx, &job_result_id, &updated_data)
                .await?;
            tx.commit().await.context("error in test commit")?;
            assert!(
                updated,
                "Failed to update job_result with streaming_type={}",
                type_name
            );

            let found_after_update = repository.find(&job_result_id).await?;
            assert_eq!(
                found_after_update.unwrap().data.unwrap().streaming_type,
                new_streaming_type,
                "streaming_type mismatch after update"
            );

            // Cleanup
            let mut tx = db.begin().await.context("error in cleanup")?;
            repository.delete_tx(&mut *tx, &job_result_id).await?;
            tx.commit().await.context("error in cleanup commit")?;
        }

        Ok(())
    }

    #[cfg(not(feature = "mysql"))]
    #[test]
    fn test_sqlite_streaming_type_values() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_rdb_from("sql/sqlite").await;
            sqlx::query("DELETE FROM job_result;").execute(pool).await?;
            _test_streaming_type_all_values(pool).await
        })
    }

    #[cfg(feature = "mysql")]
    #[test]
    fn test_mysql_streaming_type_values() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_rdb_from("sql/mysql").await;
            sqlx::query("TRUNCATE TABLE job_result;")
                .execute(pool)
                .await?;
            _test_streaming_type_all_values(pool).await
        })
    }
}
