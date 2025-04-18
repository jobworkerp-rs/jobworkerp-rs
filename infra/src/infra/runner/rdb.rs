use std::sync::Arc;

use super::rows::RunnerRow;
use super::rows::RunnerWithSchema;
use crate::infra::IdGeneratorWrapper;
use crate::infra::UseIdGenerator;
use anyhow::{Context, Result};
use async_trait::async_trait;
use infra_utils::infra::rdb::Rdb;
use infra_utils::infra::rdb::RdbPool;
use infra_utils::infra::rdb::UseRdbPool;
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::runner::factory::RunnerSpecFactory;
use jobworkerp_runner::runner::factory::UseRunnerSpecFactory;
use proto::jobworkerp::data::RunnerId;
use proto::jobworkerp::data::RunnerType;
use sqlx::{Executor, Pool};

#[async_trait]
pub trait RunnerRepository:
    UseRdbPool + UseRunnerSpecFactory + UseIdGenerator + Sync + Send
{
    async fn add_from_plugins_from(&self, dir: &str) -> Result<()> {
        let metas = self.plugin_runner_factory().load_plugins_from(dir).await;
        for meta in metas.iter() {
            let data = RunnerRow {
                id: self.id_generator().generate_id()?,
                name: meta.name.clone(),
                description: meta.description.clone(),
                file_name: meta.filename.clone(),
                r#type: RunnerType::Plugin as i32, // PLUGIN
            };
            self.create(&data).await.inspect_err(|e| {
                tracing::error!(
                    "Failed to create runner for plugins {}: {:?}",
                    &data.name,
                    e
                );
            })?;
        }
        Ok(())
    }

    async fn create(&self, runner_row: &RunnerRow) -> Result<bool> {
        self.create_tx(self.db_pool(), runner_row, false).await
    }
    async fn create_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        runner_row: &RunnerRow,
        ignore: bool,
    ) -> Result<bool> {
        let res = sqlx::query::<Rdb>(
            if ignore {
                "INSERT IGNORE INTO `runner` (
                `id`,
                `name`,
                `description`,
                `file_name`,
                `type`
                ) VALUES (?,?,?,?,?)"
            } else if cfg!(feature = "mysql") {
                "INSERT INTO `runner` (
                `id`,
                `name`,
                `description`,
                `file_name`,
                `type`
                ) VALUES (?,?,?,?,?) ON DUPLICATE KEY UPDATE
                  `name` = VALUES(`name`), `description` = VALUES(`description`), `file_name` = VALUES(`file_name`)"
            } else { // XXX sqlite does not support ON DUPLICATE KEY UPDATE
                "INSERT OR IGNORE INTO `runner` (
                `id`,
                `name`,
                `description`,
                `file_name`,
                `type`
                ) VALUES (?,?,?,?,?)"
            }
        )
        .bind(runner_row.id)
        .bind(&runner_row.name)
        .bind(&runner_row.description)
        .bind(&runner_row.file_name)
        .bind(runner_row.r#type)
        .execute(tx)
        .await
        .map_err(JobWorkerError::DBError)?;
        Ok(res.rows_affected() > 0)
    }

    async fn delete(&self, id: &RunnerId) -> Result<bool> {
        let db_pool = self.db_pool().clone();
        self.delete_tx(&db_pool, id).await
    }

    async fn delete_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        id: &RunnerId,
    ) -> Result<bool> {
        // TODO transaction
        let rem = self.find_row_tx(self.db_pool(), id).await.unwrap_or(None);
        let del = sqlx::query::<Rdb>("DELETE FROM `runner` WHERE `id` = ?;")
            .bind(id.value)
            .execute(tx)
            .await
            .map(|r| r.rows_affected() > 0)
            .map_err(JobWorkerError::DBError)?;
        if let Some(rem) = rem {
            if let Err(e) = self.plugin_runner_factory().unload_plugins(&rem.name).await {
                tracing::warn!("Failed to remove runner: {:?}", e);
            }
        }
        Ok(del)
    }

    async fn find(&self, id: &RunnerId) -> Result<Option<RunnerWithSchema>> {
        let row = self.find_row_tx(self.db_pool(), id).await?;
        if let Some(r2) = row {
            if let Some(r3) = self
                .plugin_runner_factory()
                .create_plugin_by_name(&r2.name, false)
                .await
            {
                Ok(Some(r2.to_runner_with_schema(r3)))
            } else {
                tracing::debug!("runner not found from runners: {:?}", &id);
                Ok(None)
            }
        } else {
            tracing::debug!("runner not found from db: {:?}", &id);
            Ok(None)
        }
    }

    async fn find_row_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        id: &RunnerId,
    ) -> Result<Option<RunnerRow>> {
        sqlx::query_as::<Rdb, RunnerRow>("SELECT * FROM `runner` WHERE `id` = ?;")
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
    ) -> Result<Vec<RunnerWithSchema>> {
        let rows = self.find_row_list_tx(self.db_pool(), limit, offset).await?;
        let mut results = Vec::new();
        for row in rows {
            if let Some(r) = self
                .plugin_runner_factory()
                .create_plugin_by_name(&row.name, false)
                .await
            {
                results.push(row.to_runner_with_schema(r));
            }
        }
        Ok(results)
    }

    async fn find_row_list_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        limit: Option<&i32>,
        offset: Option<&i64>,
    ) -> Result<Vec<RunnerRow>> {
        if let Some(l) = limit {
            sqlx::query_as::<_, RunnerRow>(
                "SELECT * FROM `runner` ORDER BY `id` DESC LIMIT ? OFFSET ?;",
            )
            .bind(l)
            .bind(offset.unwrap_or(&0i64))
            .fetch_all(tx)
        } else {
            // fetch all!
            sqlx::query_as::<_, RunnerRow>("SELECT * FROM `runner` ORDER BY `id` DESC;")
                .fetch_all(tx)
        }
        .await
        .map_err(JobWorkerError::DBError)
        .context(format!("error in find_list: ({:?}, {:?})", limit, offset))
    }

    async fn count_list_tx<'c, E: Executor<'c, Database = Rdb>>(&self, tx: E) -> Result<i64> {
        sqlx::query_scalar("SELECT count(*) as count FROM `runner`;")
            .fetch_one(tx)
            .await
            .map_err(JobWorkerError::DBError)
            .context("error in count_list".to_string())
    }
}

#[derive(Debug, Clone)]
pub struct RdbRunnerRepositoryImpl {
    pool: &'static RdbPool,
    pub runner_factory: Arc<RunnerSpecFactory>,
    id_generator: Arc<IdGeneratorWrapper>,
}

pub trait UseRdbRunnerRepository {
    fn runner_repository(&self) -> &RdbRunnerRepositoryImpl;
}

impl RdbRunnerRepositoryImpl {
    pub fn new(
        pool: &'static RdbPool,
        runner_factory: Arc<RunnerSpecFactory>,
        id_generator: Arc<IdGeneratorWrapper>,
    ) -> Self {
        Self {
            pool,
            runner_factory,
            id_generator,
        }
    }
}

impl UseRdbPool for RdbRunnerRepositoryImpl {
    fn db_pool(&self) -> &Pool<Rdb> {
        self.pool
    }
}
impl UseRunnerSpecFactory for RdbRunnerRepositoryImpl {
    fn plugin_runner_factory(&self) -> &RunnerSpecFactory {
        &self.runner_factory
    }
}
impl UseIdGenerator for RdbRunnerRepositoryImpl {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        &self.id_generator
    }
}
impl RunnerRepository for RdbRunnerRepositoryImpl {}

#[cfg(test)]
mod test {
    use super::RdbRunnerRepositoryImpl;
    use super::RunnerRepository;
    use crate::infra::module::test::TEST_PLUGIN_DIR;
    use crate::infra::runner::rows::RunnerRow;
    use crate::infra::runner::rows::RunnerWithSchema;
    use anyhow::Context;
    use anyhow::Result;
    use infra_utils::infra::rdb::RdbPool;
    use infra_utils::infra::rdb::UseRdbPool;
    use jobworkerp_runner::runner::factory::RunnerSpecFactory;
    use jobworkerp_runner::runner::plugins::Plugins;
    use proto::jobworkerp::data::RunnerData;
    use proto::jobworkerp::data::RunnerId;
    use proto::jobworkerp::data::RunnerType;
    use proto::jobworkerp::data::StreamingOutputType;
    use std::sync::Arc;

    async fn _test_repository(pool: &'static RdbPool) -> Result<()> {
        let p = Arc::new(RunnerSpecFactory::new(Arc::new(Plugins::new())));
        p.load_plugins_from(TEST_PLUGIN_DIR).await;
        let id_generator = Arc::new(crate::infra::IdGeneratorWrapper::new());
        let repository = RdbRunnerRepositoryImpl::new(pool, p.clone(), id_generator);
        let db = repository.db_pool();
        let row = Some(RunnerRow {
            id: 123456, // XXX generated
            name: "HelloPlugin".to_string(),
            description: "Hello! Plugin".to_string(),
            file_name: "libplugin_runner_hello.dylib".to_string(),
            r#type: RunnerType::Plugin as i32,
        });
        let data = Some(RunnerData {
            name: row.clone().unwrap().name.clone(),
            description: row.clone().unwrap().description.clone(),
            runner_settings_proto: include_str!(
                "../../../../plugins/hello_runner/protobuf/hello_runner.proto"
            )
            .to_string(),
            job_args_proto: include_str!(
                "../../../../plugins/hello_runner/protobuf/hello_job_args.proto"
            )
            .to_string(),
            result_output_proto: Some(
                include_str!("../../../../plugins/hello_runner/protobuf/hello_result.proto")
                    .to_string(),
            ),
            runner_type: 0,
            output_type: StreamingOutputType::Both as i32, // hello
        });
        let plugin = p
            .create_plugin_by_name(&data.as_ref().unwrap().name, false)
            .await
            .unwrap();

        let org_count = repository.count_list_tx(repository.db_pool()).await?;

        let mut tx = db.begin().await.context("error in test")?;
        let updated = repository
            .create_tx(&mut *tx, &row.clone().unwrap(), false)
            .await?;
        assert!(updated);
        tx.commit().await.context("error in test delete commit")?;

        let id1 = RunnerId {
            value: row.clone().unwrap().id,
        };
        let expect = RunnerWithSchema {
            id: Some(id1),
            data,
            settings_schema: plugin.settings_schema(),
            arguments_schema: plugin.arguments_schema(),
            output_schema: plugin.output_schema(),
        };

        // find
        let found = repository.find(&id1).await?;
        assert_eq!(Some(&expect), found.as_ref());

        let count = repository.count_list_tx(repository.db_pool()).await?;
        assert_eq!(1, count - org_count);

        // add from plugins (no additional record, no error)
        repository.add_from_plugins_from(TEST_PLUGIN_DIR).await?;

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
                sqlx::query("DELETE FROM runner WHERE id > 100;")
                    .execute(pool)
                    .await?;
                pool
            } else {
                let pool = setup_test_rdb_from("sql/sqlite").await;
                // delete only not built-in records
                sqlx::query("DELETE FROM runner WHERE id > 100;")
                    .execute(pool)
                    .await?;
                pool
            };
            _test_repository(rdb_pool).await
        })
    }
}
