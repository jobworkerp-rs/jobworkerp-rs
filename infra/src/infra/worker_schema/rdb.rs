use std::sync::Arc;

use super::rows::WorkerSchemaRow;
use crate::error::JobWorkerError;
use crate::infra::runner::factory::RunnerFactory;
use crate::infra::runner::factory::UseRunnerFactory;
use crate::infra::IdGeneratorWrapper;
use crate::infra::UseIdGenerator;
use anyhow::{Context, Result};
use async_trait::async_trait;
use infra_utils::infra::rdb::Rdb;
use infra_utils::infra::rdb::RdbPool;
use infra_utils::infra::rdb::UseRdbPool;
use proto::jobworkerp::data::RunnerType;
use proto::jobworkerp::data::{WorkerSchema, WorkerSchemaId};
use sqlx::{Executor, Pool};

#[async_trait]
pub trait WorkerSchemaRepository:
    UseRdbPool + UseRunnerFactory + UseIdGenerator + Sync + Send
{
    async fn add_from_plugins(&self) -> Result<()> {
        let names = self
            .runner_factory()
            .load_plugins()
            .await
            .context("error in add_from_plugins")?;
        for (name, fname) in names.iter() {
            let schema = WorkerSchemaRow {
                id: self.id_generator().generate_id()?,
                name: name.clone(),
                file_name: fname.clone(),
                r#type: RunnerType::Plugin as i32, // PLUGIN
            };
            let db = self.db_pool();
            let mut tx = db.begin().await.map_err(JobWorkerError::DBError)?;
            match self.create_tx(&mut *tx, &schema, false).await {
                Ok(_) => {
                    tx.commit().await.map_err(JobWorkerError::DBError)?;
                }
                Err(e) => {
                    tracing::warn!("error in create_worker_schema: {:?}", e);
                    tx.rollback().await.map_err(JobWorkerError::DBError)?;
                }
            }
        }
        Ok(())
    }

    async fn create(&self, schema_row: &WorkerSchemaRow) -> Result<bool> {
        self.create_tx(self.db_pool(), schema_row, false).await
    }
    async fn create_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        schema_row: &WorkerSchemaRow,
        ignore: bool,
    ) -> Result<bool> {
        let res = sqlx::query::<Rdb>(
            if ignore {
                "INSERT IGNORE INTO `worker_schema` (
                `id`,
                `name`,
                `file_name`,
                `type`
                ) VALUES (?,?,?,?)"
            } else if cfg!(feature = "mysql") {
                "INSERT INTO `worker_schema` (
                `id`,
                `name`,
                `file_name`,
                `type`
                ) VALUES (?,?,?,?) ON DUPLICATE KEY UPDATE `name` = VALUES(`name`), `file_name` = VALUES(`file_name`)"
            } else { // XXX sqlite does not support ON DUPLICATE KEY UPDATE
                "INSERT OR IGNORE INTO `worker_schema` (
                `id`,
                `name`,
                `file_name`,
                `type`
                ) VALUES (?,?,?,?)"
            }
        )
        .bind(schema_row.id)
        .bind(&schema_row.name)
        .bind(&schema_row.file_name)
        .bind(schema_row.r#type)
        .execute(tx)
        .await
        .map_err(JobWorkerError::DBError)?;
        Ok(res.rows_affected() > 0)
    }

    async fn delete(&self, id: &WorkerSchemaId) -> Result<bool> {
        let db_pool = self.db_pool().clone();
        self.delete_tx(&db_pool, id).await
    }

    async fn delete_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        id: &WorkerSchemaId,
    ) -> Result<bool> {
        // TODO transaction
        let rem = self.find_row_tx(self.db_pool(), id).await.unwrap_or(None);
        let del = sqlx::query::<Rdb>("DELETE FROM `worker_schema` WHERE `id` = ?;")
            .bind(id.value)
            .execute(tx)
            .await
            .map(|r| r.rows_affected() > 0)
            .map_err(JobWorkerError::DBError)?;
        if let Some(rem) = rem {
            if let Err(e) = self.runner_factory().unload_plugins(&rem.name).await {
                tracing::warn!("Failed to remove runner: {:?}", e);
            }
        }
        Ok(del)
    }

    async fn find(&self, id: &WorkerSchemaId) -> Result<Option<WorkerSchema>> {
        let row = self.find_row_tx(self.db_pool(), id).await?;
        if let Some(r2) = row {
            if let Some(r3) = self.runner_factory().create_by_name(&r2.name, false).await {
                Ok(Some(r2.to_proto(r3)))
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
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
        let rows = self.find_row_list_tx(self.db_pool(), limit, offset).await?;
        let mut results = Vec::new();
        for row in rows {
            if let Some(r) = self.runner_factory().create_by_name(&row.name, false).await {
                results.push(row.to_proto(r));
            }
        }
        Ok(results)
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
    pub runner_factory: Arc<RunnerFactory>,
    id_generator: Arc<IdGeneratorWrapper>,
}

pub trait UseWorkerSchemaRepository {
    fn worker_schema_repository(&self) -> &RdbWorkerSchemaRepositoryImpl;
}

impl RdbWorkerSchemaRepositoryImpl {
    pub fn new(
        pool: &'static RdbPool,
        runner_factory: Arc<RunnerFactory>,
        id_generator: Arc<IdGeneratorWrapper>,
    ) -> Self {
        Self {
            pool,
            runner_factory,
            id_generator,
        }
    }
}

impl UseRdbPool for RdbWorkerSchemaRepositoryImpl {
    fn db_pool(&self) -> &Pool<Rdb> {
        self.pool
    }
}
impl UseRunnerFactory for RdbWorkerSchemaRepositoryImpl {
    fn runner_factory(&self) -> &RunnerFactory {
        &self.runner_factory
    }
}
impl UseIdGenerator for RdbWorkerSchemaRepositoryImpl {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        &self.id_generator
    }
}
impl WorkerSchemaRepository for RdbWorkerSchemaRepositoryImpl {}

mod test {
    use super::RdbWorkerSchemaRepositoryImpl;
    use super::WorkerSchemaRepository;
    use crate::infra::runner::factory::RunnerFactory;
    use crate::infra::worker_schema::rows::WorkerSchemaRow;
    use anyhow::Context;
    use anyhow::Result;
    use infra_utils::infra::rdb::RdbPool;
    use infra_utils::infra::rdb::UseRdbPool;
    use proto::jobworkerp::data::RunnerType;
    use proto::jobworkerp::data::WorkerSchema;
    use proto::jobworkerp::data::WorkerSchemaData;
    use proto::jobworkerp::data::WorkerSchemaId;
    use std::sync::Arc;

    async fn _test_repository(pool: &'static RdbPool) -> Result<()> {
        let p = RunnerFactory::new();
        std::env::set_var("PLUGINS_RUNNER_DIR", "../target/debug");
        p.load_plugins().await.context("error in test")?;
        let id_generator = Arc::new(crate::infra::IdGeneratorWrapper::new());
        let repository = RdbWorkerSchemaRepositoryImpl::new(pool, Arc::new(p), id_generator);
        let db = repository.db_pool();
        let row = Some(WorkerSchemaRow {
            id: 12345, // XXX generated
            name: "HelloPlugin".to_string(),
            file_name: "libplugin_runner_hello.so".to_string(),
            r#type: RunnerType::Plugin as i32,
        });
        let data = Some(WorkerSchemaData {
            name: row.clone().unwrap().name.clone(),
            operation_proto: include_str!(
                "../../../../plugins/hello_runner/protobuf/hello_operation.proto"
            )
            .to_string(),
            job_arg_proto: include_str!(
                "../../../../plugins/hello_runner/protobuf/hello_job_args.proto"
            )
            .to_string(),
            runner_type: 0,
        });

        let org_count = repository.count_list_tx(repository.db_pool()).await?;

        let mut tx = db.begin().await.context("error in test")?;
        let updated = repository
            .create_tx(&mut *tx, &row.clone().unwrap(), false)
            .await?;
        assert!(updated);
        tx.commit().await.context("error in test delete commit")?;

        let id1 = WorkerSchemaId {
            value: row.clone().unwrap().id,
        };
        let expect = WorkerSchema {
            id: Some(id1),
            data,
        };

        // find
        let found = repository.find(&id1).await?;
        assert_eq!(Some(&expect), found.as_ref());

        let count = repository.count_list_tx(repository.db_pool()).await?;
        assert_eq!(1, count - org_count);

        // add from plugins (no additional record, no error)
        repository.add_from_plugins().await?;

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
                // delete only not built-in records
                sqlx::query("DELETE FROM worker_schema WHERE id > 100;")
                    .execute(pool)
                    .await?;
                pool
            } else {
                let pool = setup_test_rdb_from("sql/sqlite").await;
                // delete only not built-in records
                sqlx::query("DELETE FROM worker_schema WHERE id > 100;")
                    .execute(pool)
                    .await?;
                pool
            };
            _test_repository(rdb_pool).await
        })
    }
}
