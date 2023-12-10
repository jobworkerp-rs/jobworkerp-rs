use std::{path::Path, time::Duration};

use anyhow::{anyhow, Context, Result};
use debug_stub_derive::DebugStub;
use futures::StreamExt;
use log::LevelFilter;
use serde::Deserialize;
use sqlx::{
    any::AnyPoolOptions,
    migrate::MigrateDatabase,
    mysql::MySqlConnectOptions,
    sqlite::{SqliteAutoVacuum, SqliteConnectOptions},
    Any, ConnectOptions, Pool,
};

#[derive(Deserialize, Clone, DebugStub)]
pub struct RDBConfig {
    pub host: String,
    pub port: String,
    #[debug_stub = "[USER]"]
    pub user: String,
    #[debug_stub = "[PASSWORD]"]
    pub password: String,
    pub dbname: String,
    pub max_connections: u32,
}

impl RDBConfig {
    pub fn mysql_url(&self) -> String {
        format!(
            "mysql://{}:{}@{}:{}/{}",
            self.user, self.password, self.host, self.port, self.dbname
        )
    }
    // TODO not implemeted for pgsql
    pub fn pgsql_url(&self) -> String {
        format!(
            "pgsql://{}:{}@{}:{}/{}",
            self.user, self.password, self.host, self.port, self.dbname
        )
    }
    pub fn sqlite_url(&self) -> String {
        format!("sqlite://{}", self.dbname)
    }
}
impl Default for RDBConfig {
    fn default() -> Self {
        tracing::info!("Use default RDBConfig (sqlite3).");
        RDBConfig {
            host: "".to_string(),
            port: "".to_string(),
            user: "".to_string(),
            password: "".to_string(),
            dbname: "jobworkerp.sqlite3".to_string(),
            max_connections: 20,
        }
    }
}

#[test]
fn url_test() {
    let conf = RDBConfig {
        host: "127.0.0.1".to_string(),
        port: "1111".to_string(),
        user: "hoge_user".to_string(),
        password: "pass".to_string(),
        dbname: "db".to_string(),
        max_connections: 20,
    };
    assert_eq!(conf.mysql_url(), "mysql://hoge_user:pass@127.0.0.1:1111/db");
    assert_eq!(conf.sqlite_url(), "sqlite://db")
}

pub async fn new_sqlite_pool(
    config: &RDBConfig,
    init_schema: Option<&String>,
) -> Result<Pool<Any>> {
    // create db file if not exists
    if !sqlx::Sqlite::database_exists(&config.sqlite_url()).await? {
        sqlx::Sqlite::create_database(&config.sqlite_url()).await?;
    }
    // TODO config
    let mut options = SqliteConnectOptions::new()
        .auto_vacuum(SqliteAutoVacuum::Incremental)
        .filename(Path::new(&config.dbname));
    options.log_statements(LevelFilter::Debug);
    options.log_slow_statements(LevelFilter::Warn, Duration::from_secs(1));
    let pr = AnyPoolOptions::new()
        .max_connections(config.max_connections)
        //.min_connections(3)
        .connect_with(options.into())
        .await
        .context(format!(
            "cannot initialize sql connection. url:{:?}",
            config.sqlite_url()
        ));
    match pr {
        Ok(p) => setup_sqlite(&p, init_schema).await.map(|_| p),
        Err(e) => Err(anyhow!("setup error: {:?}", e)),
    }
}

async fn setup_sqlite(p: &Pool<Any>, init_schema: Option<&String>) -> Result<()> {
    sqlx::query::<Any>("PRAGMA journal_mode = WAL;")
        .execute(p)
        .await?;
    sqlx::query::<Any>("PRAGMA synchronous  = NORMAL;")
        .execute(p)
        .await?;
    if let Some(init_schema) = init_schema {
        let mut res = sqlx::query::<Any>(init_schema.as_str())
            .execute_many(p)
            .await;
        while let Some(result) = res.next().await {
            let _r = result?; // Handle each result individually
        }
    }
    Ok(())
}

pub async fn new_mysql_pool(config: &RDBConfig) -> Result<Pool<Any>> {
    let port = config.port.parse::<u16>()?;
    let mut options = MySqlConnectOptions::new()
        .host(&config.host)
        .port(port)
        .username(&config.user)
        .password(&config.password)
        .database(&config.dbname)
        .charset("utf8mb4")
        .statement_cache_capacity(2048); // TODO setting
    options.log_statements(LevelFilter::Debug);
    options.log_slow_statements(LevelFilter::Warn, Duration::from_secs(1));

    // TODO set from config
    AnyPoolOptions::new()
        .idle_timeout(Some(Duration::from_secs(10 * 60)))
        .max_lifetime(Some(Duration::from_secs(10 * 60))) // same as mariadb server wait_timeout
        .acquire_timeout(Duration::from_millis(3000))
        .test_before_acquire(true)
        .max_connections(config.max_connections)
        .min_connections(config.max_connections / 10 + 1)
        .connect_with(options.into())
        // .connect(&config.mysql_url())
        .await
        .context(format!(
            "cannot initialize mysql connection:{:?}",
            config.mysql_url()
        ))
}

pub async fn new_rdb_pool(
    db_config: &RDBConfig,
    sqlite_schema: Option<&String>,
) -> Result<Pool<Any>> {
    let p = new_mysql_pool(db_config).await;
    if p.is_err() {
        new_sqlite_pool(db_config, sqlite_schema).await
    } else {
        p
    }
}

pub trait UseRdbPool {
    fn db_pool(&self) -> &Pool<Any>;
}

pub trait UseRdbOption {
    fn db_pool(&self) -> Option<&Pool<Any>>;
}
