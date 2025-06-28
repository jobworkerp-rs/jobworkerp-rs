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
use jobworkerp_runner::runner::mcp::proxy::McpServerProxy;
use jobworkerp_runner::runner::plugins::PluginMetadata;
use proto::jobworkerp::data::RunnerId;
use proto::jobworkerp::data::RunnerType;
use sqlx::{Executor, Pool};
use std::sync::Arc;

#[async_trait]
pub trait RunnerRepository:
    UseRdbPool + UseRunnerSpecFactory + UseIdGenerator + Sync + Send
{
    // delegate
    async fn load_plugin(
        &self,
        name: Option<&str>,
        file_path: &str,
        overwrite: bool,
    ) -> Result<PluginMetadata> {
        self.runner_spec_factory()
            .load_plugin(name, file_path, overwrite)
            .await
    }
    async fn load_mcp_server(
        &self,
        name: &str,
        description: &str,
        definition: &str, // transport
    ) -> Result<McpServerProxy> {
        self.runner_spec_factory()
            .load_mcp_server(name, description, definition)
            .await
    }

    // load mcp servers from config file
    async fn add_from_mcp_config_file(&self) -> Result<()> {
        for server in self
            .runner_spec_factory()
            .mcp_clients
            .find_all()
            .await
            .iter()
        {
            let data = RunnerRow {
                id: self.id_generator().generate_id()?,
                name: server.name.clone(),
                description: server.description.clone().unwrap_or_default(),
                definition: serde_json::to_string(&server.transport).unwrap(),
                r#type: RunnerType::McpServer as i32,
            };
            self._create(&data).await.inspect_err(|e| {
                tracing::error!(
                    "Failed to create runner for plugins {}: {:?}",
                    &data.name,
                    e
                );
            })?;
        }
        Ok(())
    }
    // auto-load plugins from plugin directory
    async fn add_from_plugins_from(&self, dir: &str) -> Result<()> {
        let metas = self.runner_spec_factory().load_plugins_from(dir).await;
        for meta in metas.iter() {
            let data = RunnerRow {
                id: self.id_generator().generate_id()?,
                name: meta.name.clone(),
                description: meta.description.clone(),
                definition: meta.filename.clone(),
                r#type: RunnerType::Plugin as i32, // PLUGIN
            };
            self._create(&data).await.inspect_err(|e| {
                tracing::error!(
                    "Failed to create runner for plugins {}: {:?}",
                    &data.name,
                    e
                );
            })?;
        }
        Ok(())
    }

    /// load and create plugin from path
    async fn create_plugin(
        &self,
        name: &str,
        description: &str,
        file_path: &str,
    ) -> Result<RunnerId> {
        let meta = self
            .runner_spec_factory()
            .load_plugin(Some(name), file_path, false)
            .await?;
        let id = RunnerId {
            value: self.id_generator().generate_id()?,
        };
        let data = RunnerRow {
            id: id.value,
            name: name.to_string(),
            description: description.to_string(),
            definition: meta.filename.clone(),
            r#type: RunnerType::Plugin as i32, // PLUGIN
        };
        if self._create(&data).await.inspect_err(|e| {
            tracing::error!(
                "Failed to create runner for plugins {}: {:?}",
                &data.name,
                e
            );
        })? {
            tracing::info!("Plugin runner {} created", name);
            Ok(id)
        } else {
            tracing::warn!("Plugin runner {} already exists", name);
            Err(
                JobWorkerError::AlreadyExists(format!("Plugin runner {name} already exists"))
                    .into(),
            )
        }
    }
    // test and create mcp server
    async fn create_mcp_server(
        &self,
        name: &str,
        description: &str,
        definition: &str, // transport
    ) -> Result<RunnerId> {
        // for test
        let _proxy = self
            .runner_spec_factory()
            .load_mcp_server(name, description, definition)
            .await?;
        let id = RunnerId {
            value: self.id_generator().generate_id()?,
        };
        // create runner
        let data = RunnerRow {
            id: id.value,
            name: name.to_string(),
            description: description.to_string(),
            definition: definition.to_string(),
            r#type: RunnerType::McpServer as i32,
        };
        if self._create(&data).await.inspect_err(|e| {
            tracing::error!(
                "Failed to create runner for mcp server {}: {:?}",
                &data.name,
                e
            );
        })? {
            tracing::info!("MCP server {} created", name);
            Ok(id)
        } else {
            tracing::warn!("MCP server {} already exists", name);
            Err(JobWorkerError::AlreadyExists(format!("MCP server {name} already exists")).into())
        }
    }

    async fn remove_by_name(&self, name: &str) -> Result<bool> {
        self.runner_spec_factory().unload_plugins(name).await?;
        self._delete_by_name_tx(self.db_pool(), name).await
    }
    async fn remove(&self, id: &RunnerId) -> Result<bool> {
        let mut tx = self.db_pool().begin().await?;
        let rem = self.find_row_tx(&mut *tx, id).await.unwrap_or(None);
        let del = self._delete_tx(&mut *tx, id).await?;
        if let Some(rem) = rem {
            if let Err(e) = self.runner_spec_factory().unload_plugins(&rem.name).await {
                tracing::warn!("Failed to remove runner: {:?}", e);
            }
            if let Err(e) = self
                .runner_spec_factory()
                .unload_mcp_server(&rem.name)
                .await
            {
                tracing::warn!("Failed to remove runner: {:?}", e);
            }
        }
        tx.commit().await?;
        Ok(del)
    }

    /////////
    // rdb operations
    ////////

    async fn _create(&self, runner_row: &RunnerRow) -> Result<bool> {
        self._create_tx(self.db_pool(), runner_row).await
    }
    async fn _create_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        runner_row: &RunnerRow,
    ) -> Result<bool> {
        let res = sqlx::query::<Rdb>(if cfg!(feature = "mysql") {
            "INSERT IGNORE INTO `runner` (
                `id`,
                `name`,
                `description`,
                `definition`,
                `type`
                ) VALUES (?,?,?,?,?)"
        } else {
            // sqlite
            "INSERT OR IGNORE INTO `runner` (
                `id`,
                `name`,
                `description`,
                `definition`,
                `type`
                ) VALUES (?,?,?,?,?)"
        })
        .bind(runner_row.id)
        .bind(&runner_row.name)
        .bind(&runner_row.description)
        .bind(&runner_row.definition)
        .bind(runner_row.r#type)
        .execute(tx)
        .await
        .map_err(JobWorkerError::DBError)?;
        Ok(res.rows_affected() > 0)
    }

    async fn _upsert(&self, runner_row: &RunnerRow) -> Result<bool> {
        self._upsert_tx(self.db_pool(), runner_row).await
    }

    async fn _upsert_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        runner_row: &RunnerRow,
    ) -> Result<bool> {
        let res = sqlx::query::<Rdb>(if cfg!(feature = "mysql") {
            "INSERT INTO `runner` (
                `id`,
                `name`,
                `description`,
                `definition`,
                `type`
                ) VALUES (?,?,?,?,?)
                 ON DUPLICATE KEY UPDATE 
                 `name` = VALUES(`name`),
                 `description` = VALUES(`description`),
                 `definition` = VALUES(`definition`),
                 `type` = VALUES(`type`)"
        } else {
            // sqlite
            "REPLACE INTO `runner` (
                `id`,
                `name`,
                `description`,
                `definition`,
                `type`
                ) VALUES (?,?,?,?,?)"
        })
        .bind(runner_row.id)
        .bind(&runner_row.name)
        .bind(&runner_row.description)
        .bind(&runner_row.definition)
        .bind(runner_row.r#type)
        .execute(tx)
        .await
        .map_err(JobWorkerError::DBError)?;
        Ok(res.rows_affected() > 0)
    }

    async fn _delete(&self, id: &RunnerId) -> Result<bool> {
        let db_pool = self.db_pool().clone();
        self._delete_tx(&db_pool, id).await
    }

    async fn _delete_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        id: &RunnerId,
    ) -> Result<bool> {
        // TODO transaction
        let del = sqlx::query::<Rdb>("DELETE FROM `runner` WHERE `id` = ?;")
            .bind(id.value)
            .execute(tx)
            .await
            .map(|r| r.rows_affected() > 0)
            .map_err(JobWorkerError::DBError)?;
        Ok(del)
    }

    async fn _delete_by_name_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        name: &str,
    ) -> Result<bool> {
        // TODO transaction
        let del = sqlx::query::<Rdb>("DELETE FROM `runner` WHERE `name` = ?;")
            .bind(name)
            .execute(tx)
            .await
            .map(|r| r.rows_affected() > 0)
            .map_err(JobWorkerError::DBError)?;
        Ok(del)
    }

    async fn find(&self, id: &RunnerId) -> Result<Option<RunnerWithSchema>> {
        let row = self.find_row_tx(self.db_pool(), id).await?;
        if let Some(r2) = row {
            if let Some(r3) = self
                .runner_spec_factory()
                .create_runner_spec_by_name(&r2.name, false)
                .await
            {
                Ok(Some(r2.to_runner_with_schema(r3).await))
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

    async fn find_by_name(&self, name: &str) -> Result<Option<RunnerWithSchema>> {
        let row = self.find_row_by_name_tx(self.db_pool(), name).await?;
        if let Some(r2) = row {
            if let Some(r3) = self
                .runner_spec_factory()
                .create_runner_spec_by_name(&r2.name, false)
                .await
            {
                Ok(Some(r2.to_runner_with_schema(r3).await))
            } else {
                tracing::debug!("runner not found from runners: name={:?}", name);
                Ok(None)
            }
        } else {
            tracing::debug!("runner not found from db: name={:?}", name);
            Ok(None)
        }
    }

    async fn find_row_by_name_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        name: &str,
    ) -> Result<Option<RunnerRow>> {
        sqlx::query_as::<Rdb, RunnerRow>("SELECT * FROM `runner` WHERE `name` = ?;")
            .bind(name)
            .fetch_optional(tx)
            .await
            .map_err(JobWorkerError::DBError)
            .context(format!("error in find: name = {name}"))
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
                .runner_spec_factory()
                .create_runner_spec_by_name(&row.name, false)
                .await
            {
                results.push(row.to_runner_with_schema(r).await);
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
        .context(format!("error in find_list: ({limit:?}, {offset:?})"))
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
    fn runner_spec_factory(&self) -> &RunnerSpecFactory {
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
    use jobworkerp_runner::runner::mcp::config::McpConfig;
    use jobworkerp_runner::runner::mcp::config::McpServerConfig;
    use jobworkerp_runner::runner::mcp::proxy::McpServerFactory;
    use jobworkerp_runner::runner::plugins::Plugins;
    use proto::jobworkerp::data::RunnerData;
    use proto::jobworkerp::data::RunnerId;
    use proto::jobworkerp::data::RunnerType;
    use proto::jobworkerp::data::StreamingOutputType;
    use std::sync::Arc;

    async fn _test_repository(pool: &'static RdbPool) -> Result<()> {
        let p = Arc::new(RunnerSpecFactory::new(
            Arc::new(Plugins::new()),
            Arc::new(McpServerFactory::default()),
        ));
        p.load_plugins_from(TEST_PLUGIN_DIR).await;
        let id_generator = Arc::new(crate::infra::IdGeneratorWrapper::new());
        let repository = RdbRunnerRepositoryImpl::new(pool, p.clone(), id_generator);
        let db = repository.db_pool();
        let row = Some(RunnerRow {
            id: 123456, // XXX generated
            name: "HelloPlugin".to_string(),
            description: "Hello! Plugin".to_string(),
            definition: "./target/debug/libplugin_runner_hello.dylib".to_string(),
            r#type: RunnerType::Plugin as i32,
        });
        let data = RunnerData {
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
            definition: "./target/debug/libplugin_runner_hello.dylib".to_string(),
        };
        let plugin = p
            .create_runner_spec_by_name(&data.name, false)
            .await
            .unwrap();

        let org_count = repository.count_list_tx(repository.db_pool()).await?;

        let mut tx = db.begin().await.context("error in test")?;
        let inserted = repository
            ._create_tx(&mut *tx, &row.clone().unwrap())
            .await?;
        assert!(inserted);
        let found = repository
            .find_row_tx(
                &mut *tx,
                &RunnerId {
                    value: row.clone().unwrap().id,
                },
            )
            .await?;
        assert_eq!(row, found);
        let row = RunnerRow {
            id: row.clone().unwrap().id,
            name: row.clone().unwrap().name,
            description: "Hello! Plugin2".to_string(),
            definition: row.clone().unwrap().definition,
            r#type: RunnerType::Plugin as i32,
        };
        let upserted = repository._upsert_tx(&mut *tx, &row).await?;
        assert!(upserted);
        let found = repository
            .find_row_tx(&mut *tx, &RunnerId { value: row.id })
            .await?;
        assert_eq!(row, found.unwrap());
        tx.commit().await.context("error in test delete commit")?;

        let id1 = RunnerId { value: row.id };
        let expect = RunnerWithSchema {
            id: Some(id1),
            data: Some(RunnerData {
                name: row.name.clone(),
                description: row.description.clone(),
                ..data
            }),
            settings_schema: plugin.settings_schema(),
            arguments_schema: plugin.arguments_schema(),
            output_schema: plugin.output_schema(),
            tools: Vec::default(),
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
        let del = repository.remove_by_name("HelloPlugin").await?;
        tx.commit().await.context("error in test delete commit")?;
        assert!(del, "delete error");
        assert!(repository.find(&id1).await?.is_none(), "record not deleted");
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

    #[test]
    fn test_add_from_mcp_server() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        use jobworkerp_runner::runner::mcp::integration_tests;

        TEST_RUNTIME.block_on(async {
            let rdb_pool = if cfg!(feature = "mysql") {
                let pool = setup_test_rdb_from("sql/mysql").await;
                // delete only not built-in records
                sqlx::query("DELETE FROM runner WHERE id > 10000 AND type = ?;")
                    .bind(RunnerType::McpServer as i32)
                    .execute(pool)
                    .await?;
                pool
            } else {
                let pool = setup_test_rdb_from("sql/sqlite").await;
                // delete only not built-in records
                sqlx::query("DELETE FROM runner WHERE id > 10000 AND type = ?;")
                    .bind(RunnerType::McpServer as i32)
                    .execute(pool)
                    .await?;
                pool
            };

            let transport = integration_tests::create_time_mcp_server_transport().await?;
            let server1 = McpServerConfig {
                name: "time".to_string(),
                description: Some("Test MCP Server1".to_string()),
                transport: transport.clone(),
            };
            let server2 = McpServerConfig {
                name: "time2".to_string(),
                description: None,
                transport,
            };
            // test Mcp clients
            let mcp_clients = McpServerFactory::new(McpConfig {
                server: vec![server1, server2],
            });
            let plugins = Arc::new(Plugins::new());
            let p = Arc::new(RunnerSpecFactory::new(plugins, Arc::new(mcp_clients)));

            let id_generator = Arc::new(crate::infra::IdGeneratorWrapper::new());
            let repository = RdbRunnerRepositoryImpl::new(rdb_pool, p.clone(), id_generator);

            let before_count = repository.count_list_tx(repository.db_pool()).await?;

            repository.add_from_mcp_config_file().await?;

            let after_count = repository.count_list_tx(repository.db_pool()).await?;
            assert_eq!(after_count - before_count, 2,);

            let rows = repository
                .find_row_list_tx(repository.db_pool(), None, None)
                .await?;
            let mcp_servers: Vec<&RunnerRow> = rows
                .iter()
                .filter(|row| row.r#type == RunnerType::McpServer as i32)
                .collect();
            println!("McpServer rows: {mcp_servers:?}");

            assert_eq!(mcp_servers.len(), 2);

            let server_names: Vec<&str> = mcp_servers.iter().map(|row| row.name.as_str()).collect();
            assert!(server_names.contains(&"time"),);
            assert!(server_names.contains(&"time2"),);

            for row in mcp_servers.iter() {
                match row.name.as_str() {
                    "time" => assert_eq!(row.description, "Test MCP Server1"),
                    "time2" => assert_eq!(row.description, ""),
                    _ => panic!("Unexpected server name: {}", row.name),
                }
            }

            for row in mcp_servers.iter() {
                let id = RunnerId { value: row.id };
                repository.remove(&id).await?;
                assert!(repository.find(&id).await?.is_none());
            }

            Ok(())
        })
    }

    #[test]
    fn test_create_plugin() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;

        TEST_RUNTIME.block_on(async {
            let rdb_pool = if cfg!(feature = "mysql") {
                let pool = setup_test_rdb_from("sql/mysql").await;
                // delete only not built-in records
                sqlx::query("DELETE FROM runner WHERE id > 10000 AND type = ?;")
                    .bind(RunnerType::Plugin as i32)
                    .execute(pool)
                    .await?;
                pool
            } else {
                let pool = setup_test_rdb_from("sql/sqlite").await;
                // delete only not built-in records
                sqlx::query("DELETE FROM runner WHERE id > 10000 AND type = ?;")
                    .bind(RunnerType::Plugin as i32)
                    .execute(pool)
                    .await?;
                pool
            };

            let plugins = Arc::new(Plugins::new());
            let mcp_clients = McpServerFactory::default();
            let p = Arc::new(RunnerSpecFactory::new(plugins, Arc::new(mcp_clients)));

            let id_generator = Arc::new(crate::infra::IdGeneratorWrapper::new());
            let repository = RdbRunnerRepositoryImpl::new(rdb_pool, p.clone(), id_generator);

            let before_count = repository.count_list_tx(repository.db_pool()).await?;

            let plugin_path = if cfg!(target_os = "macos") {
                "../target/debug/libplugin_runner_test.dylib"
            } else {
                "../target/debug/libplugin_runner_test.so"
            };

            repository
                .create_plugin("TestPlugin", "Test Plugin Description", plugin_path)
                .await?;

            let after_count = repository.count_list_tx(repository.db_pool()).await?;
            assert_eq!(after_count - before_count, 1, "Plugin should be created");

            let found = repository.find_by_name("TestPlugin").await?;
            assert!(found.is_some(), "Plugin should be found by name");

            let plugin = found.unwrap();
            assert_eq!(plugin.data.as_ref().unwrap().name, "TestPlugin");
            assert_eq!(
                plugin.data.as_ref().unwrap().description,
                "Test Plugin Description"
            );

            let runner_id = plugin.id.unwrap();
            let mut tx = repository
                .db_pool()
                .begin()
                .await
                .context("error in test cleanup")?;
            repository._delete_tx(&mut *tx, &runner_id).await?;
            tx.commit().await.context("error in test cleanup commit")?;

            Ok(())
        })
    }

    #[test]
    fn test_create_mcp_server() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        use serde_json::json;

        TEST_RUNTIME.block_on(async {
            let rdb_pool = if cfg!(feature = "mysql") {
                let pool = setup_test_rdb_from("sql/mysql").await;
                // delete only not built-in records
                sqlx::query("DELETE FROM runner WHERE id > 10000 AND type = ?;")
                    .bind(RunnerType::McpServer as i32)
                    .execute(pool)
                    .await?;
                pool
            } else {
                let pool = setup_test_rdb_from("sql/sqlite").await;
                // delete only not built-in records
                sqlx::query("DELETE FROM runner WHERE id > 10000 AND type = ?;")
                    .bind(RunnerType::McpServer as i32)
                    .execute(pool)
                    .await?;
                pool
            };

            let plugins = Arc::new(Plugins::new());
            let mcp_clients = McpServerFactory::default();
            let p = Arc::new(RunnerSpecFactory::new(plugins, Arc::new(mcp_clients)));

            let id_generator = Arc::new(crate::infra::IdGeneratorWrapper::new());
            let repository = RdbRunnerRepositoryImpl::new(rdb_pool, p.clone(), id_generator);

            let before_count = repository.count_list_tx(repository.db_pool()).await?;

            let definition = json!({
                "transport": "stdio",
                "command": "npx",
                "args": ["-y", "@modelcontextprotocol/server-everything"]
            })
            .to_string();

            let runner_id = repository
                .create_mcp_server("TestMcpServer", "Test MCP Server Description", &definition)
                .await?;

            assert!(runner_id.value > 0);

            let after_count = repository.count_list_tx(repository.db_pool()).await?;
            assert_eq!(
                after_count - before_count,
                1,
                "MCP Server should be created"
            );

            // find by name
            let found = repository.find_by_name("TestMcpServer").await?;
            assert!(found.is_some(), "MCP Server should be found by name");

            let server = found.unwrap();
            assert_eq!(server.data.as_ref().unwrap().name, "TestMcpServer");
            assert_eq!(
                server.data.as_ref().unwrap().description,
                "Test MCP Server Description"
            );
            assert_eq!(server.data.as_ref().unwrap().definition, definition);
            assert_eq!(
                server.data.as_ref().unwrap().runner_type,
                RunnerType::McpServer as i32
            );

            // already exists
            let result = repository
                .create_mcp_server("TestMcpServer", "Duplicate Server", &definition)
                .await;
            assert!(
                result.is_err(),
                "Should fail when creating a server with an existing name"
            );

            // clean up
            let runner_id = server.id.unwrap();
            repository
                .remove(&runner_id)
                .await
                .context("error in test cleanup")?;
            assert!(
                repository.find(&runner_id).await?.is_none(),
                "record not deleted"
            );

            Ok(())
        })
    }
}
