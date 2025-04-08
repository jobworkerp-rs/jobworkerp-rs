use super::rows::JobResultRow;
use crate::infra::job::rows::UseJobqueueAndCodec;
use anyhow::{Context, Result};
use async_trait::async_trait;
use infra_utils::infra::rdb::{Rdb, RdbPool, UseRdbPool};
use itertools::Itertools;
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{JobId, JobResult, JobResultData, JobResultId};
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
                end_time
            ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
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
                .unwrap_or_default(),
        )
        .bind(job_result.retried as i64)
        .bind(job_result.priority)
        .bind(job_result.timeout as i64)
        .bind(job_result.request_streaming)
        .bind(job_result.enqueue_time)
        .bind(job_result.run_after_time)
        .bind(job_result.start_time)
        .bind(job_result.end_time)
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
            end_time = ?
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
                .unwrap_or_default(),
        )
        .bind(job_result.retried as i64)
        .bind(job_result.priority)
        .bind(job_result.timeout as i64)
        .bind(job_result.request_streaming)
        .bind(job_result.enqueue_time)
        .bind(job_result.run_after_time)
        .bind(job_result.start_time)
        .bind(job_result.end_time)
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
        .context(format!("error in find_list: ({:?}, {:?})", limit, offset))
    }

    async fn count_list_tx<'c, E: Executor<'c, Database = Rdb>>(&self, tx: E) -> Result<i64> {
        sqlx::query_scalar("SELECT count(*) as count FROM job_result;")
            .fetch_one(tx)
            .await
            .map_err(JobWorkerError::DBError)
            .context("error in count_list".to_string())
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

impl UseJobqueueAndCodec for RdbJobResultRepositoryImpl {}

#[async_trait]
impl RdbJobResultRepository for RdbJobResultRepositoryImpl {}

mod test {
    use super::RdbJobResultRepository;
    use super::RdbJobResultRepositoryImpl;
    use crate::infra::job::rows::UseJobqueueAndCodec;
    use anyhow::Context;
    use anyhow::Result;
    use infra_utils::infra::rdb::RdbPool;
    use infra_utils::infra::rdb::UseRdbPool;
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
        let data = Some(JobResultData {
            job_id: Some(JobId { value: 1 }),
            worker_id: Some(WorkerId { value: 2 }),
            worker_name: "".to_string(),
            args: RdbJobResultRepositoryImpl::serialize_message(&args),
            uniq_key: Some("hoge4".to_string()),
            status: ResultStatus::ErrorAndRetry as i32,
            output: Some(ResultOutput {
                items: vec!["hoge6".as_bytes().to_vec()],
            }),
            retried: 8,
            max_retry: 0, // fixed
            priority: 1,
            timeout: 1000,
            request_streaming: false,
            enqueue_time: 9,
            run_after_time: 10,
            start_time: 11,
            end_time: 12,
            response_type: 0,     // fixed
            store_success: false, // fixed
            store_failure: false, // fixed
        });

        let id = JobResultId { value: 111 };
        assert!(repository.create(&id, &data.clone().unwrap()).await?);

        let id1 = id;
        let expect = JobResult {
            id: Some(id1),
            data,
        };

        // find
        let found = repository.find(&id1).await?;
        assert_eq!(Some(&expect), found.as_ref());

        // update
        let mut tx = db.begin().await.context("error in test")?;
        let args = TestArgs {
            args: vec!["fuga".to_string()],
        };
        let update = JobResultData {
            job_id: Some(JobId { value: 2 }),
            worker_id: Some(WorkerId { value: 3 }),
            worker_name: "".to_string(), // fixed
            args: RdbJobResultRepositoryImpl::serialize_message(&args),
            uniq_key: Some("fuga4".to_string()),
            status: ResultStatus::FatalError as i32,
            output: Some(ResultOutput {
                items: vec!["fuga6".as_bytes().to_vec()],
            }),
            retried: 1,
            max_retry: 0, // fixed
            priority: -1,
            timeout: 2000,
            request_streaming: true,
            enqueue_time: 10,
            run_after_time: 11,
            start_time: 12,
            end_time: 13,
            response_type: 0, // fixed
            store_success: false,
            store_failure: false,
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
}
