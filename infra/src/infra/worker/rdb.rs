use super::rows::WorkerRow;
use crate::infra::job::rows::UseJobqueueAndCodec;
use anyhow::{Context, Result};
use async_trait::async_trait;
use infra_utils::infra::rdb::{query_result, Rdb, RdbPool, UseRdbPool};
use itertools::Itertools;
use jobworkerp_base::{codec::UseProstCodec, error::JobWorkerError};
use proto::jobworkerp::data::{Worker, WorkerData, WorkerId};
use sqlx::Executor;

#[async_trait]
pub trait RdbWorkerRepository: UseRdbPool + UseJobqueueAndCodec + Sync + Send {
    async fn create<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        worker: &WorkerData,
    ) -> Result<WorkerId> {
        let rp = worker.retry_policy.as_ref();
        let res = sqlx::query::<Rdb>(
            "INSERT INTO worker (
            `name`,
            `description`,
            `runner_id`,
            `use_static`,
            `runner_settings`,
            `retry_type`,
            `interval`,
            `max_interval`,
            `max_retry`,
            `basis`,
            `periodic_interval`,
            `channel`,
            `queue_type`,
            `response_type`,
            `store_success`,
            `store_failure`,
            `broadcast_results`
            ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        )
        .bind(&worker.name)
        .bind(&worker.description)
        .bind(worker.runner_id.as_ref().map(|s| s.value).unwrap_or(0))
        .bind(worker.use_static)
        .bind(&worker.runner_settings)
        .bind(rp.map(|p| p.r#type).unwrap_or(0))
        .bind(rp.map(|p| p.interval as i64).unwrap_or(0))
        .bind(rp.map(|p| p.max_interval as i64).unwrap_or(0))
        .bind(rp.map(|p| p.max_retry as i64).unwrap_or(0))
        .bind(rp.map(|p| p.basis).unwrap_or(2.0))
        .bind(worker.periodic_interval as i64)
        .bind(
            worker
                .channel
                .as_ref()
                .unwrap_or(&Self::DEFAULT_CHANNEL_NAME.to_string()),
        )
        .bind(worker.queue_type)
        .bind(worker.response_type)
        .bind(worker.store_success)
        .bind(worker.store_failure)
        .bind(worker.broadcast_results)
        .execute(tx)
        .await
        .map_err(JobWorkerError::DBError)?;
        Ok(WorkerId {
            value: query_result::last_insert_id(res),
        })
    }

    async fn update<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        id: &WorkerId,
        worker: &WorkerData,
    ) -> Result<bool> {
        let rp = worker.retry_policy.as_ref();
        sqlx::query(
            "UPDATE `worker` SET
            `name` = ?,
            `description` = ?,
            `runner_id` = ?,
            `use_static` = ?,
            `runner_settings` = ?,
            `retry_type` = ?,
            `interval` = ?,
            `max_interval` = ?,
            `max_retry` = ?,
            `basis` = ?,
            `periodic_interval` = ?,
            `channel` = ?,
            `queue_type` = ?,
            `response_type` = ?,
            `store_success` = ?,
            `store_failure` = ?,
            `broadcast_results` = ?
            WHERE `id` = ?;",
        )
        .bind(&worker.name)
        .bind(&worker.description)
        .bind(worker.runner_id.map(|s| s.value).unwrap_or(0))
        .bind(worker.use_static)
        .bind(&worker.runner_settings)
        .bind(rp.map(|p| p.r#type).unwrap_or(0))
        .bind(rp.map(|p| p.interval as i64).unwrap_or(0))
        .bind(rp.map(|p| p.max_interval as i64).unwrap_or(0))
        .bind(rp.map(|p| p.max_retry as i64).unwrap_or(0))
        .bind(rp.map(|p| p.basis).unwrap_or(2.0))
        .bind(worker.periodic_interval as i64)
        .bind(
            worker
                .channel
                .as_ref()
                .unwrap_or(&Self::DEFAULT_CHANNEL_NAME.to_string()),
        )
        .bind(worker.queue_type)
        .bind(worker.response_type)
        .bind(worker.store_success)
        .bind(worker.store_failure)
        .bind(worker.broadcast_results)
        .bind(id.value)
        .execute(tx)
        .await
        .map(|r| r.rows_affected() > 0)
        .map_err(JobWorkerError::DBError)
        .context(format!("error in update: id = {}", id.value))
    }

    async fn delete(&self, id: &WorkerId) -> Result<bool> {
        self.delete_tx(self.db_pool(), id).await
    }

    async fn delete_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        id: &WorkerId,
    ) -> Result<bool> {
        sqlx::query::<Rdb>("DELETE FROM worker WHERE id = ?;")
            .bind(id.value)
            .execute(tx)
            .await
            .map(|r| r.rows_affected() > 0)
            .map_err(|e| JobWorkerError::DBError(e).into())
    }

    // for test
    async fn delete_all(&self) -> Result<bool> {
        sqlx::query::<Rdb>("DELETE FROM worker;")
            .execute(self.db_pool())
            .await
            .map(|r| r.rows_affected() > 0)
            .map_err(|e| JobWorkerError::DBError(e).into())
    }

    async fn find_by_name(&self, name: &str) -> Result<Option<Worker>> {
        sqlx::query_as::<Rdb, WorkerRow>("SELECT * FROM worker WHERE name = ?;")
            .bind(name)
            .fetch_optional(self.db_pool())
            .await
            .map_err(JobWorkerError::DBError)
            .context(format!("error in find: name = {}", name))
            .map(|r| r.and_then(|r2| r2.to_proto().ok()))
    }

    async fn find(&self, id: &WorkerId) -> Result<Option<Worker>> {
        self.find_row_tx(self.db_pool(), id)
            .await
            .map(|r| r.and_then(|r2| r2.to_proto().ok()))
    }

    async fn find_row_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        id: &WorkerId,
    ) -> Result<Option<WorkerRow>> {
        sqlx::query_as::<Rdb, WorkerRow>("SELECT * FROM worker WHERE id = ?;")
            .bind(id.value)
            .fetch_optional(tx)
            .await
            .map_err(JobWorkerError::DBError)
            .context(format!("error in find: id = {}", id.value))
    }

    async fn find_list(&self, limit: Option<i32>, offset: Option<i64>) -> Result<Vec<Worker>> {
        self.find_row_list_tx(self.db_pool(), limit, offset)
            .await
            .map(|r| r.iter().flat_map(|r2| r2.to_proto().ok()).collect_vec())
    }

    async fn find_row_list_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        limit: Option<i32>,
        offset: Option<i64>,
    ) -> Result<Vec<WorkerRow>> {
        if let Some(l) = limit {
            sqlx::query_as::<_, WorkerRow>(
                "SELECT * FROM worker ORDER BY id DESC LIMIT ? OFFSET ?;",
            )
            .bind(l)
            .bind(offset.unwrap_or(0i64))
            .fetch_all(tx)
        } else {
            // fetch all!
            sqlx::query_as::<_, WorkerRow>("SELECT * FROM worker ORDER BY id DESC;").fetch_all(tx)
        }
        .await
        .map_err(JobWorkerError::DBError)
        .context(format!("error in find_list: ({:?}, {:?})", limit, offset))
    }

    async fn count(&self) -> Result<i64> {
        self.count_list_tx(self.db_pool()).await
    }
    async fn count_list_tx<'c, E: Executor<'c, Database = Rdb>>(&self, tx: E) -> Result<i64> {
        sqlx::query_scalar("SELECT count(*) as count FROM worker;")
            .fetch_one(tx)
            .await
            .map_err(JobWorkerError::DBError)
            .context("error in count_list".to_string())
    }
}

#[derive(Clone, Debug)]
pub struct RdbWorkerRepositoryImpl {
    pool: &'static RdbPool,
}

pub trait UseRdbWorkerRepository {
    fn rdb_worker_repository(&self) -> &RdbWorkerRepositoryImpl;
}

impl RdbWorkerRepositoryImpl {
    pub fn new(pool: &'static RdbPool) -> Self {
        Self { pool }
    }
}

impl UseRdbPool for RdbWorkerRepositoryImpl {
    fn db_pool(&self) -> &RdbPool {
        self.pool
    }
}
impl UseProstCodec for RdbWorkerRepositoryImpl {}
impl UseJobqueueAndCodec for RdbWorkerRepositoryImpl {}

impl RdbWorkerRepository for RdbWorkerRepositoryImpl {}

mod test {
    use crate::infra::job::rows::JobqueueAndCodec;
    use crate::infra::job::rows::UseJobqueueAndCodec;

    use super::RdbWorkerRepository;
    use super::RdbWorkerRepositoryImpl;
    use anyhow::Context;
    use anyhow::Result;
    use infra_utils::infra::rdb::RdbPool;
    use infra_utils::infra::rdb::UseRdbPool;
    use proto::jobworkerp::data::QueueType;
    use proto::jobworkerp::data::ResponseType;
    use proto::jobworkerp::data::RetryPolicy;
    use proto::jobworkerp::data::RunnerId;
    use proto::jobworkerp::data::Worker;
    use proto::jobworkerp::data::WorkerData;
    use proto::TestRunnerSettings;

    async fn _test_repository(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbWorkerRepositoryImpl::new(pool);
        let db = repository.db_pool();
        let data = Some(WorkerData {
            name: "hoge1".to_string(),
            description: "hoge2".to_string(),
            runner_id: Some(RunnerId { value: 323 }),
            runner_settings: JobqueueAndCodec::serialize_message(&TestRunnerSettings {
                name: "hoge".to_string(),
            }),
            retry_policy: Some(RetryPolicy {
                r#type: 5,
                interval: 6,
                max_interval: 7,
                max_retry: 8,
                basis: 9.0,
            }),
            periodic_interval: 11,
            channel: Some("hoge10".to_string()),
            queue_type: QueueType::ForcedRdb as i32,
            response_type: ResponseType::NoResult as i32,
            store_success: true,
            store_failure: true,
            use_static: false,
            broadcast_results: false,
        });

        let mut tx = db.begin().await.context("error in test")?;
        let id = repository.create(&mut *tx, &data.clone().unwrap()).await?;
        assert!(id.value > 0);
        tx.commit().await.context("error in test delete commit")?;

        let id1 = id;
        let expect = Worker {
            id: Some(id1),
            data,
        };

        // find
        let found = repository.find(&id1).await?;
        assert_eq!(Some(&expect), found.as_ref());

        // update
        tx = db.begin().await.context("error in test")?;
        let update = WorkerData {
            name: "fuga1".to_string(),
            description: "fuga2".to_string(),
            runner_id: Some(RunnerId { value: 324 }),
            runner_settings: RdbWorkerRepositoryImpl::serialize_message(&TestRunnerSettings {
                name: "fuga".to_string(),
            }),
            retry_policy: Some(RetryPolicy {
                r#type: 6,
                interval: 7,
                max_interval: 8,
                max_retry: 9,
                basis: 10.0,
            }),
            periodic_interval: 12,
            channel: Some("hoge11".to_string()),
            queue_type: QueueType::Normal as i32,
            response_type: ResponseType::Direct as i32,
            store_success: false,
            store_failure: false,
            use_static: false,
            broadcast_results: false,
        };
        let updated = repository
            .update(&mut *tx, &expect.id.unwrap(), &update)
            .await?;
        assert!(updated);
        tx.commit().await.context("error in test delete commit")?;

        // find
        let found = repository.find(&expect.id.unwrap()).await?;
        assert_eq!(&update, &found.unwrap().data.unwrap());
        let count = repository.count_list_tx(repository.db_pool()).await?;
        assert_eq!(1, count);

        // delete record
        tx = db.begin().await.context("error in test")?;
        let del = repository.delete_tx(&mut *tx, &expect.id.unwrap()).await?;
        tx.commit().await.context("error in test delete commit")?;
        assert!(del, "delete error");
        Ok(())
    }

    #[test]
    fn run_test() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let rdb_pool = if cfg!(feature = "mysql") {
                let pool = setup_test_rdb_from("sql/mysql").await;
                sqlx::query("TRUNCATE TABLE worker;").execute(pool).await?;
                pool
            } else {
                let pool = setup_test_rdb_from("sql/sqlite").await;
                sqlx::query("DELETE FROM worker;").execute(pool).await?;
                pool
            };
            _test_repository(rdb_pool).await
        })
    }
}
