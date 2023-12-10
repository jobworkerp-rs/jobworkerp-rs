use super::rows::WorkerRow;
use crate::{error::JobWorkerError, infra::job::rows::UseJobqueueAndCodec};
use anyhow::{Context, Result};
use async_trait::async_trait;
use infra_utils::infra::rdb::UseRdbPool;
use itertools::Itertools;
use proto::jobworkerp::data::{Worker, WorkerData, WorkerId};
use sqlx::{Any, Executor, Pool};

#[async_trait]
pub trait RdbWorkerRepository: UseRdbPool + UseJobqueueAndCodec + Sync + Send {
    async fn create<'c, E: Executor<'c, Database = Any>>(
        &self,
        tx: E,
        worker: &WorkerData,
    ) -> Result<WorkerId> {
        let rp = worker.retry_policy.as_ref();
        let res = sqlx::query::<Any>(
            "INSERT INTO worker (
            `name`,
            `type`,
            `use_static`,
            `operation`,
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
            `next_workers`
            ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        )
        .bind(&worker.name)
        .bind(worker.r#type)
        .bind(worker.use_static)
        .bind(&worker.operation)
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
        .bind(WorkerRow::serialize_worker_ids(&worker.next_workers))
        .execute(tx)
        .await
        .map_err(JobWorkerError::DBError)?
        .last_insert_id();
        if let Some(id) = res {
            Ok(WorkerId { value: id })
        } else {
            // no record?
            Err(JobWorkerError::RuntimeError(format!(
                "Cannot insert worker (logic error?): {:?}",
                worker
            ))
            .into())
        }
    }

    async fn update<'c, E: Executor<'c, Database = Any>>(
        &self,
        tx: E,
        id: &WorkerId,
        worker: &WorkerData,
    ) -> Result<bool> {
        let rp = worker.retry_policy.as_ref();
        sqlx::query(
            "UPDATE `worker` SET
            `name` = ?,
            `type` = ?,
            `use_static` = ?,
            `operation` = ?,
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
            `next_workers` = ?
            WHERE `id` = ?;",
        )
        .bind(&worker.name)
        .bind(worker.r#type)
        .bind(worker.use_static)
        .bind(&worker.operation)
        .bind(rp.map(|p| p.r#type))
        .bind(rp.map(|p| p.interval as i64))
        .bind(rp.map(|p| p.max_interval as i64))
        .bind(rp.map(|p| p.max_retry as i64))
        .bind(rp.map(|p| p.basis))
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
        .bind(WorkerRow::serialize_worker_ids(&worker.next_workers))
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

    async fn delete_tx<'c, E: Executor<'c, Database = Any>>(
        &self,
        tx: E,
        id: &WorkerId,
    ) -> Result<bool> {
        sqlx::query::<Any>("DELETE FROM worker WHERE id = ?;")
            .bind(id.value)
            .execute(tx)
            .await
            .map(|r| r.rows_affected() > 0)
            .map_err(|e| JobWorkerError::DBError(e).into())
    }

    // for test
    async fn delete_all(&self) -> Result<bool> {
        sqlx::query::<Any>("DELETE FROM worker;")
            .execute(self.db_pool())
            .await
            .map(|r| r.rows_affected() > 0)
            .map_err(|e| JobWorkerError::DBError(e).into())
    }

    async fn find_by_name(&self, name: &str) -> Result<Option<Worker>> {
        sqlx::query_as::<Any, WorkerRow>("SELECT * FROM worker WHERE name = ?;")
            .bind(name)
            .fetch_optional(self.db_pool())
            .await
            .map_err(JobWorkerError::DBError)
            .context(format!("error in find: name = {}", name))
            .map(|r| r.map(|r2| r2.to_proto()))
    }

    async fn find(&self, id: &WorkerId) -> Result<Option<Worker>> {
        self.find_row_tx(self.db_pool(), id)
            .await
            .map(|r| r.map(|r2| r2.to_proto()))
    }

    async fn find_row_tx<'c, E: Executor<'c, Database = Any>>(
        &self,
        tx: E,
        id: &WorkerId,
    ) -> Result<Option<WorkerRow>> {
        sqlx::query_as::<Any, WorkerRow>("SELECT * FROM worker WHERE id = ?;")
            .bind(id.value)
            .fetch_optional(tx)
            .await
            .map_err(JobWorkerError::DBError)
            .context(format!("error in find: id = {}", id.value))
    }

    async fn find_list(&self, limit: Option<i32>, offset: Option<i64>) -> Result<Vec<Worker>> {
        self.find_row_list_tx(self.db_pool(), limit, offset)
            .await
            .map(|r| r.iter().map(|r2| r2.to_proto()).collect_vec())
    }

    async fn find_row_list_tx<'c, E: Executor<'c, Database = Any>>(
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
    async fn count_list_tx<'c, E: Executor<'c, Database = Any>>(&self, tx: E) -> Result<i64> {
        sqlx::query_scalar("SELECT count(*) as count FROM worker;")
            .fetch_one(tx)
            .await
            .map_err(JobWorkerError::DBError)
            .context("error in count_list".to_string())
    }
}

#[derive(Clone, Debug)]
pub struct RdbWorkerRepositoryImpl {
    pool: &'static Pool<Any>,
}

pub trait UseRdbWorkerRepository {
    fn rdb_worker_repository(&self) -> &RdbWorkerRepositoryImpl;
}

impl RdbWorkerRepositoryImpl {
    pub fn new(pool: &'static Pool<Any>) -> Self {
        Self { pool }
    }
}

impl UseRdbPool for RdbWorkerRepositoryImpl {
    fn db_pool(&self) -> &Pool<Any> {
        self.pool
    }
}
impl UseJobqueueAndCodec for RdbWorkerRepositoryImpl {}

impl RdbWorkerRepository for RdbWorkerRepositoryImpl {}

mod test {
    use super::RdbWorkerRepository;
    use super::RdbWorkerRepositoryImpl;
    use anyhow::Context;
    use anyhow::Result;
    use infra_utils::infra::rdb::UseRdbPool;
    use proto::jobworkerp::data::QueueType;
    use proto::jobworkerp::data::ResponseType;
    use proto::jobworkerp::data::RetryPolicy;
    use proto::jobworkerp::data::Worker;
    use proto::jobworkerp::data::WorkerData;
    use proto::jobworkerp::data::WorkerId;
    use sqlx::{Any, Pool};

    async fn _test_repository(pool: &'static Pool<Any>) -> Result<()> {
        let repository = RdbWorkerRepositoryImpl::new(pool);
        let db = repository.db_pool();
        let data = Some(WorkerData {
            name: "hoge1".to_string(),
            r#type: 3,
            operation: "hoge3".to_string(),
            retry_policy: Some(RetryPolicy {
                r#type: 5,
                interval: 6,
                max_interval: 7,
                max_retry: 8,
                basis: 9.0,
            }),
            periodic_interval: 11,
            channel: Some("hoge10".to_string()),
            queue_type: QueueType::Rdb as i32,
            response_type: ResponseType::NoResult as i32,
            store_success: true,
            store_failure: true,
            next_workers: vec![
                WorkerId { value: 1 },
                WorkerId { value: 2 },
                WorkerId { value: 3 },
            ],
            use_static: false,
        });

        let mut tx = db.begin().await.context("error in test")?;
        let id = repository.create(&mut *tx, &data.clone().unwrap()).await?;
        assert!(id.value > 0);
        tx.commit().await.context("error in test delete commit")?;

        let id1 = id;
        let expect = Worker {
            id: Some(id1.clone()),
            data,
        };

        // find
        let found = repository.find(&id1).await?;
        assert_eq!(Some(&expect), found.as_ref());

        // update
        tx = db.begin().await.context("error in test")?;
        let update = WorkerData {
            name: "fuga1".to_string(),
            r#type: 4,
            operation: "fuga3".to_string(),
            retry_policy: Some(RetryPolicy {
                r#type: 6,
                interval: 7,
                max_interval: 8,
                max_retry: 9,
                basis: 10.0,
            }),
            periodic_interval: 12,
            channel: Some("hoge11".to_string()),
            queue_type: QueueType::Redis as i32,
            response_type: ResponseType::ListenAfter as i32,
            store_success: false,
            store_failure: false,
            next_workers: vec![],
            use_static: false,
        };
        let updated = repository
            .update(&mut *tx, &expect.id.clone().unwrap(), &update)
            .await?;
        assert!(updated);
        tx.commit().await.context("error in test delete commit")?;

        // find
        let found = repository.find(&expect.id.clone().unwrap()).await?;
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

    #[sqlx::test]
    async fn test_sqlite() -> Result<()> {
        use infra_utils::infra::test::setup_test_sqlite;
        let sqlite_pool = setup_test_sqlite("sql/sqlite").await;
        sqlx::query("DELETE FROM worker;")
            .execute(sqlite_pool)
            .await?;
        _test_repository(sqlite_pool).await
    }

    #[sqlx::test]
    async fn test_mysql() -> Result<()> {
        use infra_utils::infra::test::setup_test_mysql;
        let mysql_pool = setup_test_mysql("sql/mysql").await;
        sqlx::query("TRUNCATE TABLE worker;")
            .execute(mysql_pool)
            .await?;
        _test_repository(mysql_pool).await
    }
}
