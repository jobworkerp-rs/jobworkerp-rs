use std::sync::Arc;

use super::rows::WorkerSchemaRow;
use crate::error::JobWorkerError;
use crate::infra::plugins::PluginLoader;
use crate::infra::plugins::Plugins;
use crate::infra::plugins::UsePlugins;
use anyhow::{Context, Result};
use async_trait::async_trait;
use infra_utils::infra::rdb::Rdb;
use infra_utils::infra::rdb::RdbPool;
use infra_utils::infra::rdb::UseRdbPool;
use proto::jobworkerp::data::{WorkerSchema, WorkerSchemaId};
use sqlx::{Executor, Pool};

#[async_trait]
pub trait WorkerSchemaRepository: UseRdbPool + UsePlugins + Sync + Send {
    async fn add_from_plugins(&self) -> Result<()> {
        let names = self
            .plugins()
            .load_plugin_files_from_env()
            .await
            .context("error in add_from_plugins")?;
        for (name, fname) in names.iter() {
            let schema = WorkerSchemaRow {
                id: 0,
                name: name.clone(),
                file_name: fname.clone(),
            };
            let db = self.db_pool();
            let mut tx = db.begin().await.map_err(JobWorkerError::DBError)?;
            match self.create(&mut *tx, &schema).await {
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

    async fn create<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        schema_row: &WorkerSchemaRow,
    ) -> Result<WorkerSchemaId> {
        let res = sqlx::query::<Rdb>(
            "INSERT INTO `worker_schema` (
            `name`,
            `file_name`
            ) VALUES (?,?)",
        )
        .bind(&schema_row.name)
        .bind(&schema_row.file_name)
        .execute(tx)
        .await
        .map_err(JobWorkerError::DBError)?;
        if res.rows_affected() > 0 {
            Ok(WorkerSchemaId {
                value: schema_row.id,
            })
        } else {
            // no record?
            Err(JobWorkerError::RuntimeError(format!(
                "Cannot insert worker_schema (logic error?): {:?}",
                schema_row
            ))
            .into())
        }
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
        if let Err(e) = self
            .plugins()
            .runner_plugins()
            .write()
            .await
            .unload(&rem.unwrap().name)
        {
            tracing::warn!("Failed to remove runner: {:?}", e);
        }
        Ok(del)
    }

    async fn find(&self, id: &WorkerSchemaId) -> Result<Option<WorkerSchema>> {
        let row = self.find_row_tx(self.db_pool(), id).await?;
        if let Some(r2) = row {
            if let Some(r3) = self
                .plugins()
                .runner_plugins()
                .write()
                .await
                .find_plugin_runner_by_name(&r2.name)
            {
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
        for r2 in rows {
            if let Some(r3) = self
                .plugins()
                .runner_plugins()
                .write()
                .await
                .find_plugin_runner_by_name(&r2.name)
            {
                results.push(r2.to_proto(r3));
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
    pub plugins: Arc<Plugins>,
}

pub trait UseWorkerSchemaRepository {
    fn worker_schema_repository(&self) -> &RdbWorkerSchemaRepositoryImpl;
}

impl RdbWorkerSchemaRepositoryImpl {
    pub fn new(pool: &'static RdbPool, plugins: Arc<Plugins>) -> Self {
        Self { pool, plugins }
    }
}

impl UseRdbPool for RdbWorkerSchemaRepositoryImpl {
    fn db_pool(&self) -> &Pool<Rdb> {
        self.pool
    }
}
impl UsePlugins for RdbWorkerSchemaRepositoryImpl {
    fn plugins(&self) -> &Plugins {
        &self.plugins
    }
}

impl WorkerSchemaRepository for RdbWorkerSchemaRepositoryImpl {}

mod test {
    use super::RdbWorkerSchemaRepositoryImpl;
    use super::WorkerSchemaRepository;
    use crate::infra::plugins::Plugins;
    use crate::infra::worker_schema::rows::WorkerSchemaRow;
    use anyhow::Context;
    use anyhow::Result;
    use infra_utils::infra::rdb::RdbPool;
    use infra_utils::infra::rdb::UseRdbPool;
    use proto::jobworkerp::data::WorkerSchema;
    use proto::jobworkerp::data::WorkerSchemaData;
    use std::sync::Arc;

    async fn _test_repository(pool: &'static RdbPool) -> Result<()> {
        let p = Plugins::new();
        p.load_plugin_files_from_env().await.context("error in test")?;
        let repository = RdbWorkerSchemaRepositoryImpl::new(pool, Arc::new(p));
        let db = repository.db_pool();
        let row = Some(WorkerSchemaRow {
            id: 0,
            name: "hoge1".to_string(),
            file_name: "fuga2".to_string(),
        });
        let data = Some(WorkerSchemaData {
            name: row.clone().unwrap().name.clone(),
            operation_proto: "".to_string(), // TODO
            job_arg_proto: "hoge5".to_string(),
        });

        let mut tx = db.begin().await.context("error in test")?;
        let id = repository.create(&mut *tx, &row.clone().unwrap()).await?;
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
