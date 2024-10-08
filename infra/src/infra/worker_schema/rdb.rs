use super::rows::WorkerSchemaRow;
use crate::error::JobWorkerError;
use anyhow::{Context, Result};
use async_trait::async_trait;
use infra_utils::infra::rdb::Rdb;
use infra_utils::infra::rdb::RdbPool;
use infra_utils::infra::rdb::UseRdbPool;
use itertools::Itertools;
use proto::jobworkerp::data::{WorkerSchema, WorkerSchemaData, WorkerSchemaId};
use sqlx::{Executor, Pool};

#[async_trait]
pub trait WorkerSchemaRepository: UseRdbPool + Sync + Send {
    async fn create<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        id: WorkerSchemaId,
        worker_schema: &WorkerSchemaData,
    ) -> Result<WorkerSchemaId> {
        let res = sqlx::query::<Rdb>(
            "INSERT INTO `worker_schema` (
            `id`,
            `name`,
            `operation_type`,
            `job_arg_proto`
            ) VALUES (?,?,?,?)",
        )
        .bind(id.value)
        .bind(&worker_schema.name)
        .bind(worker_schema.operation_type)
        .bind(&worker_schema.job_arg_proto)
        .execute(tx)
        .await
        .map_err(JobWorkerError::DBError)?;
        if res.rows_affected() > 0 {
            Ok(id)
        } else {
            // no record?
            Err(JobWorkerError::RuntimeError(format!(
                "Cannot insert worker_schema (logic error?): {:?}",
                worker_schema
            ))
            .into())
        }
    }

    async fn update<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        id: &WorkerSchemaId,
        worker_schema: &WorkerSchemaData,
    ) -> Result<bool> {
        sqlx::query(
            "UPDATE `worker_schema` SET
            `name` = ?,
            `operation_type` = ?,
            `job_arg_proto` = ?
            WHERE `id` = ?;",
        )
        .bind(&worker_schema.name)
        .bind(worker_schema.operation_type)
        .bind(&worker_schema.job_arg_proto)
        .bind(id.value)
        .execute(tx)
        .await
        .map(|r| r.rows_affected() > 0)
        .map_err(JobWorkerError::DBError)
        .context(format!("error in update: id = {}", id.value))
    }

    async fn delete(&self, id: &WorkerSchemaId) -> Result<bool> {
        self.delete_tx(self.db_pool(), id).await
    }

    async fn delete_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        id: &WorkerSchemaId,
    ) -> Result<bool> {
        let del = sqlx::query::<Rdb>("DELETE FROM `worker_schema` WHERE `id` = ?;")
            .bind(id.value)
            .execute(tx)
            .await
            .map(|r| r.rows_affected() > 0)
            .map_err(JobWorkerError::DBError)?;
        Ok(del)
    }

    async fn find(&self, id: &WorkerSchemaId) -> Result<Option<WorkerSchema>> {
        self.find_row_tx(self.db_pool(), id)
            .await
            .map(|r| r.map(|r2| r2.to_proto()))
    }

    async fn find_row_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        id: &WorkerSchemaId,
    ) -> Result<Option<WorkerSchemaRow>> {
        sqlx::query_as::<Rdb, WorkerSchemaRow>("SELECT * FROM `worker_schema` WHERE `id` = ?;")
            .bind(id.value)
            .fetch_optional(tx)
            .await
            .map_err(JobWorkerError::DBError)
            .context(format!("error in find: id = {}", id.value))
    }

    async fn find_list(
        &self,
        limit: Option<&i32>,
        offset: Option<&i64>,
    ) -> Result<Vec<WorkerSchema>> {
        self.find_row_list_tx(self.db_pool(), limit, offset)
            .await
            .map(|r| r.iter().map(|r2| r2.to_proto()).collect_vec())
    }

    async fn find_row_list_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        limit: Option<&i32>,
        offset: Option<&i64>,
    ) -> Result<Vec<WorkerSchemaRow>> {
        if let Some(l) = limit {
            sqlx::query_as::<_, WorkerSchemaRow>(
                "SELECT * FROM `worker_schema` ORDER BY `id` DESC LIMIT ? OFFSET ?;",
            )
            .bind(l)
            .bind(offset.unwrap_or(&0i64))
            .fetch_all(tx)
        } else {
            // fetch all!
            sqlx::query_as::<_, WorkerSchemaRow>(
                "SELECT * FROM `worker_schema` ORDER BY `id` DESC;",
            )
            .fetch_all(tx)
        }
        .await
        .map_err(JobWorkerError::DBError)
        .context(format!("error in find_list: ({:?}, {:?})", limit, offset))
    }

    async fn count_list_tx<'c, E: Executor<'c, Database = Rdb>>(&self, tx: E) -> Result<i64> {
        sqlx::query_scalar("SELECT count(*) as count FROM `worker_schema`;")
            .fetch_one(tx)
            .await
            .map_err(JobWorkerError::DBError)
            .context("error in count_list".to_string())
    }
}

#[derive(Debug, Clone)]
pub struct RdbWorkerSchemaRepositoryImpl {
    pool: &'static RdbPool,
}

pub trait UseWorkerSchemaRepository {
    fn worker_schema_repository(&self) -> &RdbWorkerSchemaRepositoryImpl;
}

impl RdbWorkerSchemaRepositoryImpl {
    pub fn new(pool: &'static RdbPool) -> Self {
        Self { pool }
    }
}

impl UseRdbPool for RdbWorkerSchemaRepositoryImpl {
    fn db_pool(&self) -> &Pool<Rdb> {
        self.pool
    }
}

impl WorkerSchemaRepository for RdbWorkerSchemaRepositoryImpl {}

mod test {
    use super::RdbWorkerSchemaRepositoryImpl;
    use super::WorkerSchemaRepository;
    use anyhow::Context;
    use anyhow::Result;
    use infra_utils::infra::rdb::RdbPool;
    use infra_utils::infra::rdb::UseRdbPool;
    use proto::jobworkerp::data::WorkerSchema;
    use proto::jobworkerp::data::WorkerSchemaData;
    use proto::jobworkerp::data::WorkerSchemaId;

    async fn _test_repository(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbWorkerSchemaRepositoryImpl::new(pool);
        let db = repository.db_pool();
        let data = Some(WorkerSchemaData {
            name: "hoge1".to_string(),
            operation_type: 3,
            job_arg_proto: "hoge5".to_string(),
        });

        let mut tx = db.begin().await.context("error in test")?;
        let id = repository
            .create(
                &mut *tx,
                WorkerSchemaId { value: 3232 },
                &data.clone().unwrap(),
            )
            .await?;
        assert!(id.value > 0);
        tx.commit().await.context("error in test delete commit")?;

        let id1 = id;
        let expect = WorkerSchema {
            id: Some(id1),
            data,
        };

        // find
        let found = repository.find(&id1).await?;
        assert_eq!(Some(&expect), found.as_ref());

        // update
        tx = db.begin().await.context("error in test")?;
        let update = WorkerSchemaData {
            name: "fuga1".to_string(),
            operation_type: 4,
            job_arg_proto: "fuga5".to_string(),
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
                sqlx::query("TRUNCATE TABLE worker_schema;")
                    .execute(pool)
                    .await?;
                pool
            } else {
                let pool = setup_test_rdb_from("sql/sqlite").await;
                sqlx::query("DELETE FROM worker_schema;")
                    .execute(pool)
                    .await?;
                pool
            };
            _test_repository(rdb_pool).await
        })
    }
}
