use anyhow::Result;
use infra_utils::infra::{
    rdb::{RdbConfig, RdbConfigImpl, RdbPool, RdbUrlConfigImpl},
    redis::{RedisConfig, RedisPool},
};
use jobworkerp_base::error::JobWorkerError;

const SQLITE_SCHEMA: &str = include_str!("../../sql/sqlite/002_schema.sql");

static RDB_POOL: tokio::sync::OnceCell<RdbPool> = tokio::sync::OnceCell::const_new();

pub async fn setup_rdb_by_env() -> &'static RdbPool {
    let conf = load_db_config_from_env().unwrap_or(
        load_db_url_config_from_env().unwrap_or(RdbConfig::Separate(RdbConfigImpl::default())),
    );
    setup_rdb(&conf).await
}

// new rdb pool and store as static
// (if failed initializing, panic!)
pub async fn setup_rdb(db_config: &RdbConfig) -> &'static RdbPool {
    sqlx::any::install_default_drivers();
    RDB_POOL
        .get_or_init(|| async {
            infra_utils::infra::rdb::new_rdb_pool(db_config, Some(&SQLITE_SCHEMA.to_string()))
                .await
                .unwrap()
        })
        .await
}

pub fn load_db_url_config_from_env() -> Result<RdbConfig> {
    // sqlite first
    envy::prefixed("SQLITE_")
        .from_env::<RdbUrlConfigImpl>()
        .map(RdbConfig::Url)
        .or_else(|_| {
            envy::prefixed("MYSQL_")
                .from_env::<RdbUrlConfigImpl>()
                .map(RdbConfig::Url)
        })
        .map_err(|e| {
            JobWorkerError::RuntimeError(format!("cannot read redis config from env: {e:?}")).into()
        })
}

pub fn load_db_config_from_env() -> Result<RdbConfig> {
    // sqlite first
    envy::prefixed("SQLITE_")
        .from_env::<RdbConfigImpl>()
        .map(RdbConfig::Separate)
        .or_else(|_| {
            envy::prefixed("MYSQL_")
                .from_env::<RdbConfigImpl>()
                .map(RdbConfig::Separate)
        })
        .map_err(|e| {
            JobWorkerError::RuntimeError(format!("cannot read redis config from env: {e:?}")).into()
        })
}

static _REDIS: tokio::sync::OnceCell<RedisPool> = tokio::sync::OnceCell::const_new();
static _REDIS_BLOCKING: tokio::sync::OnceCell<RedisPool> = tokio::sync::OnceCell::const_new();

pub async fn setup_redis_client(config: RedisConfig) -> redis::Client {
    redis::Client::open(config.url.clone())
        .unwrap_or_else(|_| panic!("cannot open redis client: config={:?}", &config))
}

pub async fn setup_redis_client_by_env() -> redis::Client {
    let conf = load_redis_config_from_env().unwrap();
    redis::Client::open(conf.url.clone())
        .unwrap_or_else(|_| panic!("cannot open redis client: config={:?}", &conf))
}

// static _REDIS_CON: tokio::sync::OnceCell<redis::aio::Connection> =
//     tokio::sync::OnceCell::const_new();
// pub async fn setup_redis_connection_by_env() -> &'static redis::aio::Connection {
//     let conf = _load_redis_config_from_env().unwrap();
//     setup_redis_connection(conf).await
// }
// pub async fn setup_redis_connection(config: RedisConfig) -> &'static redis::aio::Connection {
//     _REDIS_CON
//         .get_or_init(|| async {
//             common::infra::redis::new_redis_connection(config.clone())
//                 .await
//                 .expect(
//                     format!("cannot initiailize redis connection: config={:?}", config).as_str(),
//                 )
//         })
//         .await
// }

pub async fn setup_redis_pool_by_env() -> &'static RedisPool {
    let conf = load_redis_config_from_env().unwrap();
    setup_redis_pool(conf).await
}

pub async fn setup_redis_pool(config: RedisConfig) -> &'static RedisPool {
    _REDIS
        .get_or_init(|| async {
            infra_utils::infra::redis::new_redis_pool(config)
                .await
                .expect("failed to initialize redis pool")
        })
        .await
}

/// Setup a Redis pool for blocking operations like BLPOP.
/// This pool has response_timeout disabled to allow indefinite waiting.
pub async fn setup_redis_blocking_pool_by_env() -> &'static RedisPool {
    let mut conf = load_redis_config_from_env().unwrap();
    conf.blocking = true;
    setup_redis_blocking_pool(conf).await
}

/// Setup a Redis pool for blocking operations like BLPOP.
/// This pool has response_timeout disabled to allow indefinite waiting.
pub async fn setup_redis_blocking_pool(config: RedisConfig) -> &'static RedisPool {
    _REDIS_BLOCKING
        .get_or_init(|| async {
            let mut blocking_config = config;
            blocking_config.blocking = true;
            infra_utils::infra::redis::new_redis_pool(blocking_config)
                .await
                .expect("failed to initialize redis blocking pool")
        })
        .await
}

pub fn load_redis_config_from_env() -> Result<RedisConfig> {
    envy::prefixed("REDIS_")
        .from_env::<RedisConfig>()
        .map_err(|e| {
            JobWorkerError::RuntimeError(format!("cannot read redis config from env: {e:?}")).into()
        })
}
