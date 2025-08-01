use crate::infra::job::rows::JobRow;
use anyhow::{Context, Result};
use async_trait::async_trait;
use command_utils::util::datetime;
use infra_utils::infra::rdb::{Rdb, RdbArguments, UseRdbPool};
use itertools::Itertools;
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{Job, JobId, WorkerId};
use sqlx::{Arguments, FromRow};

pub const GRAB_MERGIN_MILLISEC: i64 = 10000;

#[async_trait]
pub trait RdbJobQueueRepository: UseRdbPool + Sync + Send {
    async fn fetch_jobs_to_process(
        &self,
        offset: i64,
        limit: u32,
        worker_ids: Vec<WorkerId>,
        mergin_msec: u32, //for early get for running job in exact time: run_after_time <= now + mergin_msec
        future_only_mode: bool, // use only run_after_time > 0
    ) -> Result<Vec<Job>> {
        let now = datetime::now_millis();
        let future_query = if future_only_mode {
            "AND run_after_time > 0"
        } else {
            ""
        };
        let query = if worker_ids.is_empty() {
            format!(
                r#"
                SELECT * FROM job
                WHERE run_after_time <= ? {future_query} AND grabbed_until_time <= ?
                ORDER BY run_after_time, priority DESC
                LIMIT ? OFFSET ?
            "#
            )
        } else {
            let in_clause = worker_ids
                .iter()
                .map(|_| "?")
                .collect::<Vec<&str>>()
                .join(",");
            format!(
                r#"
                SELECT * FROM job
                WHERE run_after_time <= ? {future_query} AND grabbed_until_time <= ? AND worker_id IN ({in_clause})
                ORDER BY run_after_time, priority DESC
                LIMIT ? OFFSET ?
            "#
            )
        };
        let mut args = RdbArguments::default();
        args.add(now + mergin_msec as i64).map_err(|e| {
            JobWorkerError::OtherError(format!("sql args err:{now:?}, error:{e:?}"))
        })?;
        args.add(now).map_err(|e| {
            JobWorkerError::OtherError(format!("sql args err:{now:?}, error:{e:?}"))
        })?;
        for id in &worker_ids {
            args.add(id.value).map_err(|e| {
                JobWorkerError::OtherError(format!("sql args err:{now:?}, error:{e:?}"))
            })?;
        }
        args.add(limit as i64).map_err(|e| {
            JobWorkerError::OtherError(format!("sql args err:{now:?}, error:{e:?}"))
        })?;
        args.add(offset).map_err(|e| {
            JobWorkerError::OtherError(format!("sql args err:{now:?}, error:{e:?}"))
        })?;
        let mut rows = sqlx::query_with::<Rdb, _>(&query, args)
            .fetch_all(self.db_pool())
            .await
            .map_err(JobWorkerError::DBError)
            .context("failed to find_job query")?;
        let mut jobs = Vec::new();
        for row in rows.drain(..) {
            match JobRow::from_row(&row) {
                Ok(r) => jobs.push(r.to_proto()),
                Err(e) => {
                    // skip invalid row
                    tracing::error!("failed to parse row: {:?}", e);
                    continue;
                }
            }
        }
        Ok(jobs)
    }
    /// fetch timeouted jobs for recovery to redis queue in hybrid storage
    /// (from backuped records)
    async fn fetch_timeouted_backup_jobs(&self, limit: u32, offset: i64) -> Result<Vec<Job>> {
        let now = datetime::now_millis();
        sqlx::query_as::<Rdb, JobRow>(
            r#"
            SELECT * FROM job
            WHERE grabbed_until_time > 0 AND grabbed_until_time <= ? AND run_after_time = 0
            ORDER BY run_after_time, priority DESC
            LIMIT ? OFFSET ?"#,
        )
        .bind(now)
        .bind(limit as i32)
        .bind(offset)
        .fetch_all(self.db_pool())
        .await
        .map(|r| r.into_iter().map(|r2| r2.to_proto()).collect_vec())
        .map_err(JobWorkerError::DBError)
        .context("failed to find_job query")
    }
    /// grab(lock) job to prevent other worker to process the job
    ///
    /// - `timeout`: timeout to process the job
    /// - `original_grabbed_until_time`: grabbed_until_time of the job before grab
    /// - `grabbed_until_time`: grabbed_until_time to set (None means default value(with timeout+mergin))
    async fn grab_job(
        &self,
        job_id: &JobId,
        timeout: Option<u64>,
        original_grabbed_until_time: i64,
    ) -> Result<bool> {
        // time millis to re-execute if the job does not disappear from queue (row) after a while after timeout(GRAB_MERGIN_MILLISEC)
        let grabbed_until_time = Self::grabbed_until_time(timeout, datetime::now_millis());

        let query = r#"
            UPDATE job
            SET grabbed_until_time = ?
            WHERE id = ? AND grabbed_until_time = ?
        "#;
        let res = sqlx::query(query)
            .bind(grabbed_until_time)
            .bind(job_id.value) // XXX unwrap
            .bind(original_grabbed_until_time) // XXX unwrap
            .execute(self.db_pool())
            .await
            .map_err(JobWorkerError::DBError)
            .context("failed to execute query")?;
        Ok(res.rows_affected() > 0)
    }
    /// unix time (millis) to re-execute if the job does not finish after a while (timeout + GRAB_MERGIN_MILLISEC)
    fn grabbed_until_time(timeout: Option<u64>, now: i64) -> i64 {
        let mut timeout: i64 = timeout.unwrap_or(0) as i64;
        if timeout == 0 {
            timeout = 1000 * 60 * 60 * 24 * 365 * 100; // XXX 100 years
        }
        now + timeout + GRAB_MERGIN_MILLISEC
    }

    // reset grabbed_until_time of specified job to 0
    async fn reset_grabbed_until_time(
        &self,
        job_id: &JobId,
        old_grabbed_until_time: i64,
        run_after_time: Option<i64>,
    ) -> Result<bool> {
        let res = if let Some(rat) = run_after_time {
            sqlx::query(
                r#"
            UPDATE job
            SET grabbed_until_time = 0, run_after_time = ?
            WHERE id = ? AND grabbed_until_time = ?
        "#,
            )
            .bind(rat)
            .bind(job_id.value)
            .bind(old_grabbed_until_time)
        } else {
            sqlx::query(
                r#"
            UPDATE job
            SET grabbed_until_time = 0
            WHERE id = ? AND grabbed_until_time = ?
        "#,
            )
            .bind(job_id.value)
            .bind(old_grabbed_until_time)
        }
        .execute(self.db_pool())
        .await
        .map_err(JobWorkerError::DBError)
        .context("failed to execute query")?;
        Ok(res.rows_affected() > 0)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::infra::job::rdb::RdbChanJobRepositoryImpl;
    use crate::infra::job::rdb::RdbJobRepository;
    use crate::infra::job::rows::JobqueueAndCodec;
    use crate::infra::job::rows::UseJobqueueAndCodec;
    use crate::infra::JobQueueConfig;
    use anyhow::Result;
    use command_utils::util::datetime;
    use infra_utils::infra::rdb::RdbPool;
    use infra_utils::infra::test::TEST_RUNTIME;
    use proto::jobworkerp::data::Job;
    use proto::jobworkerp::data::JobData;
    use proto::jobworkerp::data::WorkerId;
    use std::collections::HashMap;
    use std::sync::Arc;

    async fn _test_job_queue_repository(pool: &'static RdbPool) -> Result<()> {
        let repo = RdbChanJobRepositoryImpl::new(Arc::new(JobQueueConfig::default()), pool);
        let worker_id = WorkerId { value: 11 };
        let worker_id2 = WorkerId { value: 21 };

        let jid = JobId { value: 1 };
        let jargs = JobqueueAndCodec::serialize_message(&proto::TestArgs {
            args: vec!["GET".to_string(), "/".to_string()],
        });
        let instant_job_data = JobData {
            worker_id: Some(worker_id),
            args: jargs.clone(),
            grabbed_until_time: Some(0),
            run_after_time: 0,
            ..Default::default()
        };
        let metadata = HashMap::new();
        let job0 = Job {
            id: Some(jid),
            data: Some(instant_job_data.clone()),
            metadata: metadata.clone(),
        };
        assert!(repo.create(&job0).await?);
        let current_job_data = JobData {
            worker_id: Some(worker_id2),
            args: jargs.clone(),
            grabbed_until_time: Some(0),
            run_after_time: datetime::now_millis(),
            ..Default::default()
        };
        let jid1 = JobId { value: 2 };
        let job1 = Job {
            id: Some(jid1),
            data: Some(current_job_data.clone()),
            metadata: metadata.clone(),
        };
        assert!(repo.create(&job1).await?);

        let future_job_data = JobData {
            worker_id: Some(worker_id),
            args: jargs.clone(),
            grabbed_until_time: Some(0),
            run_after_time: datetime::now_millis() + 10000,
            ..Default::default()
        };
        let jid2 = JobId { value: 3 };
        let job2 = Job {
            id: Some(jid2),
            data: Some(future_job_data.clone()),
            metadata: metadata.clone(),
        };

        assert!(repo.create(&job2).await?);
        // specify worker_id
        let jobs0 = repo
            .fetch_jobs_to_process(0, 5, vec![worker_id], 1000, false)
            .await?;
        assert_eq!(jobs0.len(), 1);

        // all jobs to process
        let jobs1 = repo
            .fetch_jobs_to_process(0, 5, vec![], 1000, false)
            .await?;
        println!("{jobs1:?}");
        assert_eq!(jobs1.len(), 2);

        // future only
        let jobs = repo.fetch_jobs_to_process(0, 5, vec![], 1000, true).await?;
        assert_eq!(jobs.len(), 1);
        let job = &jobs[0];
        assert_eq!(job, &job1);

        // grab job twice but only first one is success
        let grabbed = repo
            .grab_job(
                job.id.as_ref().unwrap(),
                job.data.as_ref().map(|d| d.timeout),
                job.data
                    .as_ref()
                    .and_then(|d| d.grabbed_until_time)
                    .unwrap_or(0),
            )
            .await?;
        assert!(grabbed);
        let grabbed = repo
            .grab_job(
                job.id.as_ref().unwrap(),
                job.data.as_ref().map(|d| d.timeout),
                job.data
                    .as_ref()
                    .and_then(|d| d.grabbed_until_time)
                    .unwrap_or(0),
            )
            .await?;
        assert!(!grabbed);
        let jobs2 = repo.fetch_jobs_to_process(0, 5, vec![], 1000, true).await?;
        assert_eq!(jobs2.len(), 0);

        // not future only
        let jobs3 = repo
            .fetch_jobs_to_process(0, 5, vec![], 1000, false)
            .await?;
        assert_eq!(jobs3.len(), 1);
        let grabbed2 = repo
            .grab_job(
                jobs3[0].id.as_ref().unwrap(),
                jobs3[0].data.as_ref().map(|d| d.timeout),
                jobs3[0]
                    .data
                    .as_ref()
                    .and_then(|d| d.grabbed_until_time)
                    .unwrap_or(0),
            )
            .await?;
        assert!(grabbed2);
        let del = repo.delete(jobs3[0].id.as_ref().unwrap()).await?;
        assert!(del);
        let jobs4 = repo
            .fetch_jobs_to_process(0, 5, vec![], 1000, false)
            .await?;
        assert_eq!(jobs4.len(), 0);

        Ok(())
    }

    #[cfg(not(feature = "mysql"))]
    #[test]
    fn test_sqlite() -> Result<()> {
        use infra_utils::infra::test::{setup_test_rdb_from, truncate_tables};
        TEST_RUNTIME.block_on(async {
            let sqlite_pool = setup_test_rdb_from("sql/sqlite").await;
            truncate_tables(sqlite_pool, vec!["job", "worker", "job_result"]).await;
            sqlx::query("DELETE FROM job;").execute(sqlite_pool).await?;
            _test_job_queue_repository(sqlite_pool).await
        })
    }

    #[cfg(feature = "mysql")]
    #[test]
    fn test_mysql() -> Result<()> {
        use infra_utils::infra::test::{setup_test_rdb_from, truncate_tables};
        TEST_RUNTIME.block_on(async {
            let mysql_pool = setup_test_rdb_from("sql/mysql").await;
            truncate_tables(mysql_pool, vec!["job", "worker", "job_result"]).await;
            _test_job_queue_repository(mysql_pool).await
        })
    }

    #[test]
    fn test_fetch_timeouted_backup_jobs() -> Result<()> {
        use infra_utils::infra::test::{setup_test_rdb, truncate_tables};
        use proto::jobworkerp::data::JobData;
        use proto::jobworkerp::data::WorkerId;
        TEST_RUNTIME.block_on(async {
            let rdb_pool = setup_test_rdb().await;
            truncate_tables(rdb_pool, vec!["job", "worker", "job_result"]).await;
            let repo = RdbChanJobRepositoryImpl::new(Arc::new(JobQueueConfig::default()), rdb_pool);
            let worker_id = WorkerId { value: 11 };
            let worker_id2 = WorkerId { value: 21 };
            let jid0 = JobId { value: 1 };
            let jargs = JobqueueAndCodec::serialize_message(&proto::TestArgs {
                args: vec!["GET".to_string(), "/".to_string()],
            });
            let metadata = HashMap::new();
            let now_millis = datetime::now_millis();

            // for redis job: run_after_time:0, not timeouted (grabbed)
            let instant_job_data_for_redis = JobData {
                worker_id: Some(worker_id),
                args: jargs.clone(),
                grabbed_until_time: Some(now_millis + 10000),
                run_after_time: 0,
                ..Default::default()
            };
            let job0 = Job {
                id: Some(jid0),
                data: Some(instant_job_data_for_redis.clone()),
                metadata: metadata.clone(),
            };
            assert!(repo.create(&job0).await?);

            // for redis job: run_after_time:0, timeouted
            let timeouted_job_data_for_redis = JobData {
                worker_id: Some(worker_id),
                args: jargs.clone(),
                grabbed_until_time: Some(now_millis - 1000),
                run_after_time: 0,
                ..Default::default()
            };
            let jid1 = JobId { value: 11 };
            let job1 = Job {
                id: Some(jid1),
                data: Some(timeouted_job_data_for_redis.clone()),
                metadata: metadata.clone(),
            };
            assert!(repo.create(&job1).await?);

            // for rdb job, timeouted
            let current_job_data_for_rdb = JobData {
                worker_id: Some(worker_id2),
                args: jargs.clone(),
                grabbed_until_time: Some(now_millis - 1000),
                run_after_time: datetime::now_millis(),
                ..Default::default()
            };
            let jid2 = JobId { value: 22 };
            let job2 = Job {
                id: Some(jid2),
                data: Some(current_job_data_for_rdb.clone()),
                metadata: metadata.clone(),
            };
            assert!(repo.create(&job2).await?);

            // for rdb future job
            let future_job_data = JobData {
                worker_id: Some(worker_id),
                args: jargs.clone(),
                grabbed_until_time: Some(0),
                run_after_time: datetime::now_millis() + 10000,
                ..Default::default()
            };
            let jid3 = JobId { value: 33 };
            let job3 = Job {
                id: Some(jid3),
                data: Some(future_job_data.clone()),
                metadata: metadata.clone(),
            };
            assert!(repo.create(&job3).await?);

            let timeouted_backup_jobs = repo.fetch_timeouted_backup_jobs(100, 0).await.unwrap();
            assert_eq!(timeouted_backup_jobs.len(), 1);
            if let Job {
                id: Some(jid),
                data: Some(data),
                metadata: _,
            } = &timeouted_backup_jobs[0]
            {
                assert_eq!(jid, &jid1);
                assert_eq!(data.worker_id.as_ref().unwrap(), &worker_id);
            } else {
                panic!("invalid job: {:?}", timeouted_backup_jobs[0]);
            }
            Ok(())
        })
    }
}
