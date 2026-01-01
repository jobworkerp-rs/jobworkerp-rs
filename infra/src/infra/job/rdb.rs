use std::{collections::HashSet, sync::Arc};

use super::{
    queue::{chan::UseChanQueueBuffer, rdb::RdbJobQueueRepository},
    rows::{JobRow, UseJobqueueAndCodec},
};
use crate::infra::{JobQueueConfig, UseJobQueueConfig};
use anyhow::{Context, Result};
use async_trait::async_trait;
use command_utils::util::datetime;
use infra_utils::infra::rdb::{Rdb, RdbPool, UseRdbPool};
use itertools::Itertools;
use jobworkerp_base::{codec::UseProstCodec, error::JobWorkerError};
use memory_utils::chan::{
    mpmc::{Chan, UseChanBuffer},
    ChanBuffer, ChanBufferItem,
};
use proto::jobworkerp::data::{Job, JobData, JobId, JobProcessingStatus};
use sqlx::Executor;

#[async_trait]
pub trait RdbJobRepository:
    RdbJobQueueRepository + UseJobqueueAndCodec + UseRdbPool + Sync + Send
{
    async fn create(&self, job: &Job) -> Result<bool> {
        self.create_tx(self.db_pool(), job).await
    }
    #[inline]
    async fn create_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        job: &Job,
    ) -> Result<bool> {
        if let (Some(id), Some(data)) = (job.id.as_ref(), job.data.as_ref()) {
            let res = sqlx::query::<Rdb>(
                "INSERT INTO job (
                  id,
                  worker_id,
                  args,
                  uniq_key,
                  enqueue_time,
                  grabbed_until_time,
                  run_after_time,
                  retried,
                  priority,
                  timeout,
                  request_streaming,
                  `using`
                ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
            )
            .bind(id.value)
            .bind(data.worker_id.as_ref().unwrap().value) // XXX unwrap
            .bind(&data.args)
            .bind(&data.uniq_key)
            .bind(data.enqueue_time)
            .bind(data.grabbed_until_time.unwrap_or(0))
            .bind(data.run_after_time)
            .bind(data.retried as i64)
            .bind(data.priority)
            .bind(data.timeout as i32)
            .bind(data.streaming_type) // DB column "request_streaming" stores StreamingType value
            .bind(&data.using)
            .execute(tx)
            .await
            .map_err(JobWorkerError::DBError)?;
            Ok(res.rows_affected() > 0)
        } else {
            Err(JobWorkerError::RuntimeError(format!("Cannot insert empty job: {job:?}")).into())
        }
    }

    async fn upsert(&self, id: &JobId, job: &JobData) -> Result<bool> {
        self.upsert_tx(self.db_pool(), id, job).await
    }
    // filepath: [rdb.rs](http://_vscodecontentref_/0)
    #[inline]
    async fn upsert_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        id: &JobId,
        job: &JobData,
    ) -> Result<bool> {
        #[cfg(feature = "mysql")]
        {
            // MySQL specific implementation with ON DUPLICATE KEY UPDATE
            sqlx::query(
                "INSERT INTO job (
                  id,
                  worker_id,
                  args,
                  uniq_key,
                  enqueue_time,
                  grabbed_until_time,
                  run_after_time,
                  retried,
                  priority,
                  timeout,
                  request_streaming,
                  `using`
                ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)
                  ON DUPLICATE KEY UPDATE
                    worker_id = VALUES(worker_id),
                    args = VALUES(args),
                    uniq_key = VALUES(uniq_key),
                    enqueue_time = VALUES(enqueue_time),
                    grabbed_until_time = VALUES(grabbed_until_time),
                    run_after_time = VALUES(run_after_time),
                    retried = VALUES(retried),
                    priority = VALUES(priority),
                    timeout = VALUES(timeout),
                    request_streaming = VALUES(request_streaming),
                    `using` = VALUES(`using`);",
            )
            .bind(id.value)
            .bind(job.worker_id.as_ref().unwrap().value) // XXX unwrap
            .bind(&job.args)
            .bind(&job.uniq_key)
            .bind(job.enqueue_time)
            .bind(job.grabbed_until_time.unwrap_or(0))
            .bind(job.run_after_time)
            .bind(job.retried as i64)
            .bind(job.priority)
            .bind(job.timeout as i64)
            .bind(job.streaming_type) // DB column "request_streaming" stores StreamingType value
            .bind(&job.using)
            .execute(tx)
            .await
            .map(|r| r.rows_affected() > 0)
            .map_err(JobWorkerError::DBError)
            .context(format!("error in upsert (MySQL): id = {}", id.value))
        }

        #[cfg(not(feature = "mysql"))]
        {
            // SQLite implementation using INSERT OR REPLACE
            sqlx::query(
                "INSERT OR REPLACE INTO job (
              id,
              worker_id,
              args,
              uniq_key,
              enqueue_time,
              grabbed_until_time,
              run_after_time,
              retried,
              priority,
              timeout,
              request_streaming,
              `using`
            ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?);",
            )
            .bind(id.value)
            .bind(job.worker_id.as_ref().unwrap().value) // XXX unwrap
            .bind(&job.args)
            .bind(&job.uniq_key)
            .bind(job.enqueue_time)
            .bind(job.grabbed_until_time.unwrap_or(0))
            .bind(job.run_after_time)
            .bind(job.retried as i64)
            .bind(job.priority)
            .bind(job.timeout as i64)
            .bind(job.streaming_type) // DB column "request_streaming" stores StreamingType value
            .bind(&job.using)
            .execute(tx)
            .await
            .map(|r| r.rows_affected() > 0)
            .map_err(JobWorkerError::DBError)
            .context(format!("error in upsert (SQLite): id = {}", id.value))
        }
    }
    async fn update(&self, id: &JobId, job: &JobData) -> Result<bool> {
        self.update_tx(self.db_pool(), id, job).await
    }
    #[inline]
    async fn update_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        id: &JobId,
        job: &JobData,
    ) -> Result<bool> {
        sqlx::query(
            "UPDATE job SET
            worker_id = ?,
            args = ?,
            uniq_key = ?,
            enqueue_time = ?,
            grabbed_until_time = ?,
            run_after_time = ?,
            retried = ?,
            priority = ?,
            timeout = ?,
            request_streaming = ?,
            `using` = ?
            WHERE id = ?;",
        )
        .bind(job.worker_id.as_ref().unwrap().value) // XXX unwrap
        .bind(&job.args)
        .bind(&job.uniq_key)
        .bind(job.enqueue_time)
        .bind(job.grabbed_until_time.unwrap_or(0))
        .bind(job.run_after_time)
        .bind(job.retried as i64)
        .bind(job.priority)
        .bind(job.timeout as i64)
        .bind(job.streaming_type) // DB column "request_streaming" stores StreamingType value
        .bind(&job.using)
        .bind(id.value)
        .execute(tx)
        .await
        .map(|r| r.rows_affected() > 0)
        .map_err(JobWorkerError::DBError)
        .context(format!("error in update: id = {}", id.value))
    }

    async fn delete(&self, id: &JobId) -> Result<bool> {
        self.delete_tx(self.db_pool(), id).await
    }
    #[inline]
    async fn delete_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        id: &JobId,
    ) -> Result<bool> {
        sqlx::query::<Rdb>("DELETE FROM job WHERE id = ?;")
            .bind(id.value)
            .execute(tx)
            .await
            .map(|r| r.rows_affected() > 0)
            .map_err(|e| JobWorkerError::DBError(e).into())
    }

    async fn find(&self, id: &JobId) -> Result<Option<Job>> {
        self.find_row_tx(self.db_pool(), id)
            .await
            .map(|r| r.map(|r2| r2.to_proto()))
    }
    #[inline]
    async fn find_row_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        id: &JobId,
    ) -> Result<Option<JobRow>> {
        sqlx::query_as::<Rdb, JobRow>("SELECT * FROM job WHERE id = ?;")
            .bind(id.value)
            .fetch_optional(tx)
            .await
            .map_err(JobWorkerError::DBError)
            .context(format!("error in find: id = {}", id.value))
    }

    async fn find_status(&self, id: &JobId) -> Result<Option<JobProcessingStatus>> {
        self.find_row_tx(self.db_pool(), id).await.map(|r| {
            r.map(|r2| {
                // status WaitResult cannot be determined from rdb
                if r2.grabbed_until_time.unwrap_or(0) == 0 {
                    JobProcessingStatus::Pending
                } else {
                    JobProcessingStatus::Running
                }
            })
        })
    }

    async fn find_all_status(&self) -> Result<Vec<(JobId, JobProcessingStatus)>> {
        self.find_row_list_tx(self.db_pool(), None, None)
            .await
            .map(|r| {
                r.into_iter()
                    .map(|r2| {
                        // status WaitResult cannot be determined from rdb
                        if r2.grabbed_until_time.unwrap_or(0) == 0 {
                            (JobId { value: r2.id }, JobProcessingStatus::Pending)
                        } else {
                            (JobId { value: r2.id }, JobProcessingStatus::Running)
                        }
                    })
                    .collect_vec()
            })
    }

    async fn find_list(&self, limit: Option<&i32>, offset: Option<&i64>) -> Result<Vec<Job>> {
        self.find_row_list_tx(self.db_pool(), limit, offset)
            .await
            .map(|r| r.iter().map(|r2| r2.to_proto()).collect_vec())
    }

    // XXX id is primitive (use only for restore)
    async fn find_list_in(&self, ids: &[&i64]) -> Result<Vec<Job>> {
        let params = format!("?{}", ", ?".repeat(ids.len() - 1));
        let query_str = format!("SELECT * FROM job WHERE id IN ( {params} )");
        let mut query = sqlx::query_as::<_, JobRow>(query_str.as_str());
        for i in ids.iter() {
            query = query.bind(i);
        }
        query
            .fetch_all(self.db_pool())
            .await
            .map_err(JobWorkerError::DBError)
            .context(format!("error in find_list_in: ({ids:?})"))
            .map(|r| r.iter().map(|r2| r2.to_proto()).collect_vec())
    }

    /// find instant job id set
    /// find job ids for restore to redis in startup (also fetch jobs in progress by redis queue)
    /// (TODO not fetch jobs in progress (set grabbed_until_time in db when grabbed job from redis?))
    // XXX return id set as primitive (use only for restore)
    async fn find_id_set_in_instant(
        &self,
        include_grabbed: bool,
        limit: Option<&i32>,
        offset: Option<&i64>,
    ) -> Result<HashSet<i64>> {
        let r = if let Some(l) = limit {
                if include_grabbed {
            sqlx::query_as::<_, (i64,)>(
                    "SELECT id FROM job WHERE run_after_time = 0 ORDER BY id DESC LIMIT ? OFFSET ?;"
            )
            .bind(l)
            .bind(offset.unwrap_or(&0i64))
            .fetch_all(self.db_pool())

                } else {
            sqlx::query_as::<_, (i64,)>(
                    "SELECT id FROM job WHERE run_after_time = 0 AND (grabbed_until_time = 0 OR grabbed_until_time < ?) ORDER BY id DESC LIMIT ? OFFSET ?;"
            )
            .bind(datetime::now_millis())
            .bind(l)
            .bind(offset.unwrap_or(&0i64))
            .fetch_all(self.db_pool())
                }
        } else if include_grabbed { // fetch all!
            sqlx::query_as::<_, (i64,)>("SELECT id FROM job WHERE run_after_time = 0 ORDER BY id DESC;").fetch_all(self.db_pool())
        }else {
            sqlx::query_as::<_, (i64,)>("SELECT id FROM job WHERE run_after_time = 0 AND (grabbed_until_time = 0 OR grabbed_until_time < ?) ORDER BY id DESC;")
                 .bind(datetime::now_millis()).fetch_all(self.db_pool())
        }
        .await
        .map_err(JobWorkerError::DBError)?;
        tracing::debug!("find_list_in_instant: {:?}", r.len());
        Ok(r.iter().map(|t| t.0).collect::<HashSet<i64>>())
    }

    #[inline]
    async fn find_row_list_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        limit: Option<&i32>,
        offset: Option<&i64>,
    ) -> Result<Vec<JobRow>> {
        if let Some(l) = limit {
            sqlx::query_as::<_, JobRow>("SELECT * FROM job ORDER BY id DESC LIMIT ? OFFSET ?;")
                .bind(l)
                .bind(offset.unwrap_or(&0i64))
                .fetch_all(tx)
        } else {
            // fetch all!
            sqlx::query_as::<_, JobRow>("SELECT * FROM job ORDER BY id DESC;").fetch_all(tx)
        }
        .await
        .map_err(JobWorkerError::DBError)
        .context(format!("error in find_list: ({limit:?}, {offset:?})"))
    }

    #[inline]
    async fn count_list_tx<'c, E: Executor<'c, Database = Rdb>>(&self, tx: E) -> Result<i64> {
        sqlx::query_scalar("SELECT count(*) as count FROM job;")
            .fetch_one(tx)
            .await
            .map_err(JobWorkerError::DBError)
            .context("error in count_list".to_string())
    }
}

#[derive(Clone, Debug)]
pub struct RdbChanJobRepositoryImpl {
    job_queue_config: Arc<JobQueueConfig>,
    pool: &'static RdbPool,
    chan_buf: ChanBuffer<Vec<u8>, Chan<ChanBufferItem<Vec<u8>>>>,
    shared_buffer: Arc<tokio::sync::Mutex<std::collections::HashMap<String, Vec<Job>>>>,
}

pub trait UseRdbChanJobRepository {
    fn rdb_job_repository(&self) -> &RdbChanJobRepositoryImpl;
}

pub trait UseRdbChanJobRepositoryOptional {
    fn rdb_job_repository_opt(&self) -> Option<&RdbChanJobRepositoryImpl>;
}

impl RdbChanJobRepositoryImpl {
    const DEFAULT_CAPACITY: usize = 100_000;
    pub fn new(job_queue_config: Arc<JobQueueConfig>, pool: &'static RdbPool) -> Self {
        Self {
            job_queue_config,
            pool,
            chan_buf: ChanBuffer::new(None, Self::DEFAULT_CAPACITY),
            shared_buffer: Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::with_capacity(Self::DEFAULT_CAPACITY),
            )),
        }
    }
}

impl UseRdbPool for RdbChanJobRepositoryImpl {
    fn db_pool(&self) -> &RdbPool {
        self.pool
    }
}

impl RdbJobQueueRepository for RdbChanJobRepositoryImpl {}

impl RdbJobRepository for RdbChanJobRepositoryImpl {}

impl UseProstCodec for RdbChanJobRepositoryImpl {}
impl UseJobqueueAndCodec for RdbChanJobRepositoryImpl {}

impl UseJobQueueConfig for RdbChanJobRepositoryImpl {
    fn job_queue_config(&self) -> &JobQueueConfig {
        &self.job_queue_config
    }
}
impl UseChanBuffer for RdbChanJobRepositoryImpl {
    type Item = Vec<u8>;
    fn chan_buf(&self) -> &ChanBuffer<Vec<u8>, Chan<ChanBufferItem<Vec<u8>>>> {
        &self.chan_buf
    }
}
impl UseChanQueueBuffer for RdbChanJobRepositoryImpl {
    fn queue_list_buffer(
        &self,
    ) -> &tokio::sync::Mutex<std::collections::HashMap<String, Vec<Job>>> {
        &self.shared_buffer
    }
}

#[cfg(test)]
mod test {
    use super::RdbChanJobRepositoryImpl;
    use super::RdbJobRepository;
    use crate::infra::JobQueueConfig;
    use anyhow::Result;
    use infra_utils::infra::rdb::RdbPool;
    use infra_utils::infra::rdb::UseRdbPool;
    use jobworkerp_base::codec::UseProstCodec;
    use proto::jobworkerp::data::Job;
    use proto::jobworkerp::data::JobData;
    use proto::jobworkerp::data::JobId;
    use proto::jobworkerp::data::WorkerId;
    use proto::TestArgs;
    use std::collections::HashMap;
    use std::sync::Arc;

    async fn _test_repository(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbChanJobRepositoryImpl::new(Arc::new(JobQueueConfig::default()), pool);
        let id = JobId { value: 1 };
        let args = RdbChanJobRepositoryImpl::serialize_message(&TestArgs {
            args: vec!["hoge".to_string()],
        })?;
        let data = Some(JobData {
            worker_id: Some(WorkerId { value: 2 }),
            args,
            uniq_key: Some("hoge3".to_string()),
            enqueue_time: 5,
            grabbed_until_time: Some(6),
            run_after_time: 7,
            retried: 8,
            priority: 2,
            timeout: 10000,
            streaming_type: 0,
            using: Some("hoge".to_string()),
        });
        let job = Job {
            id: Some(id),
            data: data.clone(),
            metadata: HashMap::new(),
        };

        let res = repository.create(&job).await?;
        assert!(res, "create error");

        let id1 = id;
        let expect = job.clone();

        // find
        let found = repository.find(&id1).await?;
        assert_eq!(Some(&expect), found.as_ref());
        let args2 = RdbChanJobRepositoryImpl::serialize_message(&TestArgs {
            args: vec!["fuga3".to_string()],
        })?;

        // update
        let update = JobData {
            worker_id: Some(WorkerId { value: 3 }),
            args: args2,
            uniq_key: Some("fuga3".to_string()),
            enqueue_time: 6,
            grabbed_until_time: Some(7),
            run_after_time: 8,
            retried: 9,
            priority: 1,
            timeout: 10000,
            streaming_type: 1,
            using: Some("fuga".to_string()),
        };
        let updated = repository.upsert(&expect.id.unwrap(), &update).await?;
        assert!(updated);

        // find
        let found = repository.find(&expect.id.unwrap()).await?;
        assert_eq!(&update, &found.unwrap().data.unwrap());
        let count = repository.count_list_tx(repository.db_pool()).await?;
        assert_eq!(1, count);

        // delete record
        let del = repository.delete(&expect.id.unwrap()).await?;
        assert!(del, "delete error");
        Ok(())
    }
    async fn _test_find_id_set_in_instant(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbChanJobRepositoryImpl::new(Arc::new(JobQueueConfig::default()), pool);
        let args = RdbChanJobRepositoryImpl::serialize_message(&TestArgs {
            args: vec!["hoge1".to_string()],
        })?;
        let data = Some(JobData {
            worker_id: Some(WorkerId { value: 2 }),
            args,
            uniq_key: Some("fuga1".to_string()),
            enqueue_time: 5,
            grabbed_until_time: Some(6),
            run_after_time: 0,
            retried: 8,
            priority: 2,
            timeout: 10000,
            streaming_type: 0,
            using: None,
        });
        let job = Job {
            id: Some(JobId { value: 1 }),
            data: data.clone(),
            metadata: HashMap::new(),
        };
        repository.create(&job).await?;
        // future job
        let args2 = RdbChanJobRepositoryImpl::serialize_message(&TestArgs {
            args: vec!["hoge2".to_string()],
        })?;

        let data = Some(JobData {
            worker_id: Some(WorkerId { value: 2 }),
            args: args2,
            uniq_key: Some("fuga2".to_string()),
            enqueue_time: 5,
            grabbed_until_time: None,
            run_after_time: 10,
            retried: 8,
            priority: 2,
            timeout: 10000,
            streaming_type: 0,
            using: None,
        });
        let job = Job {
            id: Some(JobId { value: 2 }),
            data: data.clone(),
            metadata: HashMap::new(),
        };
        repository.create(&job).await?;
        // grabbed job
        let args3 = RdbChanJobRepositoryImpl::serialize_message(&TestArgs {
            args: vec!["hoge3".to_string()],
        })?;
        let data = Some(JobData {
            worker_id: Some(WorkerId { value: 2 }),
            args: args3,
            uniq_key: Some("fuga3".to_string()),
            enqueue_time: 5,
            grabbed_until_time: Some(command_utils::util::datetime::now_millis() + 10000), // grabbed until 10 sec later
            run_after_time: 0,
            retried: 8,
            priority: 2,
            timeout: 10000,
            streaming_type: 0,
            using: None,
        });
        let job = Job {
            id: Some(JobId { value: 3 }),
            data: data.clone(),
            metadata: HashMap::new(),
        };
        repository.create(&job).await?;
        let ids = repository.find_id_set_in_instant(true, None, None).await?;
        assert_eq!(2, ids.len());
        assert!(ids.contains(&1));
        assert!(ids.contains(&3)); // include grabbed
        let ids = repository.find_id_set_in_instant(false, None, None).await?;
        assert_eq!(1, ids.len());
        assert!(ids.contains(&1));
        Ok(())
    }

    /// Test all streaming_type values (0=None, 1=Response, 2=Internal) are correctly
    /// stored and retrieved from DB
    async fn _test_streaming_type_all_values(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbChanJobRepositoryImpl::new(Arc::new(JobQueueConfig::default()), pool);

        // Test each streaming_type value: None(0), Response(1), Internal(2)
        let streaming_type_values = [(0i32, "None"), (1i32, "Response"), (2i32, "Internal")];

        for (streaming_type_value, type_name) in streaming_type_values {
            let job_id = JobId {
                value: 100 + streaming_type_value as i64,
            };
            let args = RdbChanJobRepositoryImpl::serialize_message(&TestArgs {
                args: vec![format!("streaming_type_{}", type_name)],
            })?;

            let job_data = JobData {
                worker_id: Some(WorkerId { value: 1 }),
                args,
                uniq_key: Some(format!("streaming_test_{}", type_name)),
                enqueue_time: 1000,
                grabbed_until_time: None,
                run_after_time: 0,
                retried: 0,
                priority: 0,
                timeout: 5000,
                streaming_type: streaming_type_value,
                using: None,
            };

            let job = Job {
                id: Some(job_id),
                data: Some(job_data.clone()),
                metadata: HashMap::new(),
            };

            // Create job
            let created = repository.create(&job).await?;
            assert!(
                created,
                "Failed to create job with streaming_type={}",
                type_name
            );

            // Find and verify streaming_type is preserved
            let found = repository.find(&job_id).await?;
            assert!(
                found.is_some(),
                "Job not found for streaming_type={}",
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
            let updated_data = JobData {
                streaming_type: new_streaming_type,
                ..job_data.clone()
            };
            let updated = repository.upsert(&job_id, &updated_data).await?;
            assert!(
                updated,
                "Failed to update job with streaming_type={}",
                type_name
            );

            let found_after_update = repository.find(&job_id).await?;
            assert_eq!(
                found_after_update.unwrap().data.unwrap().streaming_type,
                new_streaming_type,
                "streaming_type mismatch after update"
            );

            // Cleanup
            repository.delete(&job_id).await?;
        }

        Ok(())
    }

    #[cfg(not(feature = "mysql"))]
    #[test]
    fn test_sqlite() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let sqlite_pool = setup_test_rdb_from("sql/sqlite").await;
            sqlx::query("DELETE FROM job;").execute(sqlite_pool).await?;
            _test_repository(sqlite_pool).await?;
            _test_find_id_set_in_instant(sqlite_pool).await
        })
    }

    #[cfg(not(feature = "mysql"))]
    #[test]
    fn test_sqlite_streaming_type_values() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let sqlite_pool = setup_test_rdb_from("sql/sqlite").await;
            sqlx::query("DELETE FROM job;").execute(sqlite_pool).await?;
            _test_streaming_type_all_values(sqlite_pool).await
        })
    }

    #[cfg(feature = "mysql")]
    #[test]
    fn test_mysql() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let mysql_pool = setup_test_rdb_from("sql/mysql").await;
            sqlx::query("TRUNCATE TABLE job;")
                .execute(mysql_pool)
                .await?;
            _test_repository(mysql_pool).await?;
            sqlx::query("TRUNCATE TABLE job;")
                .execute(mysql_pool)
                .await?;
            _test_find_id_set_in_instant(mysql_pool).await
        })
    }

    #[cfg(feature = "mysql")]
    #[test]
    fn test_mysql_streaming_type_values() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let mysql_pool = setup_test_rdb_from("sql/mysql").await;
            sqlx::query("TRUNCATE TABLE job;")
                .execute(mysql_pool)
                .await?;
            _test_streaming_type_all_values(mysql_pool).await
        })
    }
}
