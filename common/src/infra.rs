pub mod memory;
pub mod rdb;
pub mod redis;
pub mod redis_cluster;

use std::borrow::Cow;

// for test only
pub mod test {
    use crate::util::result::TapErr;

    use super::{
        rdb::RDBConfig,
        redis::{RedisClient, RedisConfig, RedisPool},
    };
    use anyhow::Result;
    use once_cell::sync::Lazy;
    use sqlx::{migrate::Migrator, Any, Pool};
    use tokio::sync::OnceCell;

    // migrate once in initialization
    static MYSQL_INIT: OnceCell<Pool<Any>> = OnceCell::const_new();
    static SQLITE_INIT: OnceCell<Pool<Any>> = OnceCell::const_new();

    pub static SQLITE_CONFIG: Lazy<RDBConfig> = Lazy::new(|| RDBConfig {
        host: "".to_string(),
        port: "".to_string(),
        user: "".to_string(),
        password: "".to_string(),
        dbname: "./test_db.sqlite3".to_string(),
        max_connections: 20,
    });

    pub static MYSQL_CONFIG: Lazy<RDBConfig> = Lazy::new(|| {
        let host = std::env::var("TEST_MYSQL_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        RDBConfig {
            host,
            port: "3306".to_string(),
            user: "mysql".to_string(),
            password: "mysql".to_string(),
            dbname: "test".to_string(),
            max_connections: 20,
        }
    });

    pub async fn setup_test_sqlite<T: Into<String>>(dir: T) -> &'static Pool<Any> {
        SQLITE_INIT
            .get_or_init(|| async {
                _setup_sqlite_internal(dir)
                    .await
                    .tap_err(|e| tracing::error!("error: {:?}", e))
                    .unwrap()
            })
            .await
    }

    async fn _setup_sqlite_internal<T: Into<String>>(dir: T) -> Result<Pool<Any>> {
        let pool = crate::infra::rdb::new_rdb_pool(&SQLITE_CONFIG, None).await?;
        Migrator::new(std::path::Path::new(&dir.into()))
            .await?
            .run(&pool)
            .await?;
        Ok(pool)
    }

    pub async fn setup_test_mysql<T: Into<String>>(dir: T) -> &'static Pool<Any> {
        MYSQL_INIT
            .get_or_init(|| async {
                let pool = crate::infra::rdb::new_rdb_pool(&MYSQL_CONFIG, None)
                    .await
                    .unwrap();
                Migrator::new(std::path::Path::new(&dir.into()))
                    .await
                    .unwrap()
                    .run(&pool)
                    .await
                    .unwrap();
                // sqlx::migrate!(dir.into()).run(&pool).await.unwrap();
                pool
            })
            .await
    }

    pub static REDIS_CONFIG: Lazy<RedisConfig> = Lazy::new(|| {
        let url = std::env::var("TEST_REDIS_HOST")
            .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
        RedisConfig {
            username: None,
            password: None,
            //url: "redis://redis:6379".to_string(),
            url,
            pool_create_timeout_msec: None,
            pool_wait_timeout_msec: None,
            pool_recycle_timeout_msec: None,
            pool_size: 10,
        }
    });

    static REDIS: tokio::sync::OnceCell<RedisPool> = tokio::sync::OnceCell::const_new();

    pub async fn setup_test_redis_pool() -> &'static RedisPool {
        setup_redis_pool(REDIS_CONFIG.clone()).await
    }
    pub fn setup_test_redis_client() -> Result<RedisClient> {
        crate::infra::redis::new_redis_client(REDIS_CONFIG.clone())
    }
    pub async fn setup_redis_pool(config: RedisConfig) -> &'static RedisPool {
        REDIS
            .get_or_init(|| async {
                crate::infra::redis::new_redis_pool(config)
                    .await
                    .expect("msg")
            })
            .await
    }
}

/// A key in bytes.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ByteKey {
    key: Vec<u8>,
}

impl ByteKey {
    /// Create a new key from anything that can be read as bytes.
    pub fn new<S>(key: S) -> ByteKey
    where
        S: Into<Vec<u8>>,
    {
        ByteKey { key: key.into() }
    }

    /// Read the key as a str slice if it can be parsed as a UTF8 string.
    pub fn as_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.key).ok()
    }

    /// Read the key as a byte slice.
    pub fn as_bytes(&self) -> &[u8] {
        &self.key
    }

    /// Read the key as a lossy UTF8 string with `String::from_utf8_lossy`.
    pub fn as_str_lossy(&self) -> Cow<str> {
        String::from_utf8_lossy(&self.key)
    }

    /// Convert the key to a UTF8 string, if possible.
    pub fn into_string(self) -> Option<String> {
        String::from_utf8(self.key).ok()
    }

    /// Read the inner bytes making up the key.
    pub fn into_bytes(self) -> Vec<u8> {
        self.key
    }

    /// Replace this key with an empty string, returning the bytes from the original key.
    pub fn take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.key)
    }
}

impl From<String> for ByteKey {
    fn from(s: String) -> ByteKey {
        ByteKey {
            key: s.into_bytes(),
        }
    }
}

impl<'a> From<&'a str> for ByteKey {
    fn from(s: &'a str) -> ByteKey {
        ByteKey {
            key: s.as_bytes().to_vec(),
        }
    }
}

impl<'a> From<&'a String> for ByteKey {
    fn from(s: &'a String) -> ByteKey {
        ByteKey {
            key: s.as_bytes().to_vec(),
        }
    }
}

impl<'a> From<&'a ByteKey> for ByteKey {
    fn from(k: &'a ByteKey) -> ByteKey {
        k.clone()
    }
}

impl<'a> From<&'a [u8]> for ByteKey {
    fn from(k: &'a [u8]) -> Self {
        ByteKey { key: k.to_vec() }
    }
}
