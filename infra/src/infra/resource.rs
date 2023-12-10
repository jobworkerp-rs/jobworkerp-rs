use crate::error::JobWorkerError;
use anyhow::Result;
use common::infra::{
    rdb::RDBConfig,
    redis::{RedisConfig, RedisPool},
};
use sqlx::{Any, Pool};

const SQLITE_SCHEMA: &str = include_str!("../../sql/sqlite/001_schema.sql");

static RDB_POOL: tokio::sync::OnceCell<Pool<Any>> = tokio::sync::OnceCell::const_new();

pub async fn setup_rdb_by_env() -> &'static Pool<Any> {
    let conf = load_db_config_from_env().unwrap_or_default();
    setup_rdb(&conf).await
}

// new rdb pool and store as static
// (if failed initializing, panic!)
pub async fn setup_rdb(db_config: &RDBConfig) -> &'static Pool<Any> {
    RDB_POOL
        .get_or_init(|| async {
            common::infra::rdb::new_rdb_pool(db_config, Some(&SQLITE_SCHEMA.to_string()))
                .await
                .unwrap()
        })
        .await
}

pub fn load_db_config_from_env() -> Result<RDBConfig> {
    // sqlite の設定優先
    envy::prefixed("SQLITE_")
        .from_env::<RDBConfig>()
        .or_else(|_| envy::prefixed("MYSQL_").from_env::<RDBConfig>())
        .map_err(|e| {
            JobWorkerError::RuntimeError(format!("cannot read redis config from env: {:?}", e))
                .into()
        })
}

// TODO
static _REDIS: tokio::sync::OnceCell<RedisPool> = tokio::sync::OnceCell::const_new();
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
            common::infra::redis::new_redis_pool(config)
                .await
                .expect("msg")
        })
        .await
}
pub fn load_redis_config_from_env() -> Result<RedisConfig> {
    envy::prefixed("REDIS_")
        .from_env::<RedisConfig>()
        .map_err(|e| {
            JobWorkerError::RuntimeError(format!("cannot read redis config from env: {:?}", e))
                .into()
        })
}
