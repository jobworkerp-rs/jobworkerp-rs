use anyhow::anyhow;
use anyhow::Result;
use async_trait::async_trait;
use deadpool_redis::redis::AsyncCommands as PoolAsyncCommands;
use deadpool_redis::{Config, Connection, Pool, Runtime};
use debug_stub_derive::DebugStub;
use redis::aio::Connection as RedisConnection;
use redis::aio::PubSub;
use redis::Client;
use serde::Deserialize;
use std::time::Duration;

#[derive(Deserialize, Clone, DebugStub)]
pub struct RedisConfig {
    #[debug_stub = "[USER]"]
    pub username: Option<String>,
    #[debug_stub = "[PASSWORD]"]
    pub password: Option<String>,
    pub url: String,
    pub pool_create_timeout_msec: Option<u32>,
    pub pool_wait_timeout_msec: Option<u32>,
    pub pool_recycle_timeout_msec: Option<u32>,
    pub pool_size: usize,
}

pub type RedisPool = Pool;
pub type RedisClient = redis::Client;

#[async_trait]
pub trait UseRedisClient {
    fn redis_client(&self) -> &RedisClient;

    async fn subscribe(&self, channel: &str) -> Result<PubSub> {
        let mut pubsub = self
            .redis_client()
            .get_async_connection()
            .await?
            .into_pubsub();
        pubsub.subscribe(channel).await?;
        Ok(pubsub)
    }

    async fn psubscribe(&self, pchannel: &String) -> Result<PubSub> {
        let mut pubsub = self
            .redis_client()
            .get_async_connection()
            .await?
            .into_pubsub();
        pubsub.psubscribe(pchannel).await?;
        Ok(pubsub)
    }

    async fn publish(&self, channel: &str, message: &Vec<u8>) -> Result<()> {
        let mut conn = self.redis_client().get_async_connection().await?;
        conn.publish(channel, message).await?;
        Ok(())
    }
}

#[async_trait]
pub trait UseRedisConnection {
    fn redis_connection(&self) -> &RedisConnection;
}

#[async_trait]
pub trait UseRedisPool {
    fn redis_pool(&self) -> &Pool;

    async fn connection(&self) -> Result<Connection> {
        self.redis_pool()
            .get()
            .await
            .map_err(|e| anyhow!("{:?}", e))
    }
}

// for normal use (single connection)
pub fn new_redis_client(config: RedisConfig) -> Result<redis::Client> {
    tracing::info!("Connecting to {:?}", config.url);
    Client::open(config.url).map_err(|e| e.into())
}
pub async fn new_redis_connection(config: RedisConfig) -> Result<RedisConnection> {
    tracing::info!("Connecting to {:?}", config);

    let client = Client::open(config.url)?;
    client
        .get_async_connection()
        .await
        .map_err(|e| anyhow!("redis init error: {:?}", e))
}

// pooling for multiple blocking connections
pub async fn new_redis_pool(config: RedisConfig) -> Result<RedisPool> {
    let conf = Config::from_url(config.url);
    conf.builder()
        .map(|b| {
            b.max_size(config.pool_size)
                .create_timeout(
                    config
                        .pool_create_timeout_msec
                        .map(|s| Duration::from_millis(s as u64)),
                )
                .wait_timeout(
                    // same timeout for create
                    config
                        .pool_wait_timeout_msec
                        .map(|s| Duration::from_millis(s as u64)),
                )
                .recycle_timeout(
                    // same timeout for create
                    config
                        .pool_recycle_timeout_msec
                        .map(|s| Duration::from_millis(s as u64)),
                )
                .runtime(Runtime::Tokio1)
                .build()
                .unwrap()
        })
        .map_err(|e| anyhow!(format!("redis pool init error:{:?}", e)))
}

#[async_trait]
pub trait UseRedisLock: UseRedisPool {
    async fn lock(&self, key: impl Into<String> + Send + Sync, expire_sec: i32) -> Result<()> {
        let mut con = self.redis_pool().get().await?;
        // use redis set cmd with nx and ex option to lock
        let k = key.into();
        match redis::cmd("SET")
            .arg(&k)
            .arg(Self::lock_value())
            .arg("NX")
            .arg("EX")
            .arg(expire_sec)
            .query_async::<redis::aio::Connection, String>(con.as_mut())
            .await
        {
            Ok(lock) => {
                if Self::is_ok(lock) {
                    Ok(())
                } else {
                    // TODO log
                    tracing::debug!("failed to lock:{:?}", &k);
                    Err(anyhow!("failed to lock:{:?}", &k))
                }
            }
            Err(e) => {
                // unlock if error? (comment out for pesimistic lock but may make process slow (locked until expire time, so set expiretime not too long))
                // let _ = self.unlock(key).await;
                Err(e.into())
            }
        }
    }

    async fn unlock(&self, key: impl Into<String> + Send + Sync) -> Result<()> {
        let mut redis = self.redis_pool().get().await?;
        let k = key.into();
        redis.del(k).await?;
        Ok(())
    }

    #[inline]
    fn lock_value() -> &'static str {
        "1"
    }

    #[inline]
    fn is_ok(str: String) -> bool {
        str == "OK" // for redis OK response
    }

    #[inline]
    fn is_not_ok(str: String) -> bool {
        str != "OK" // for redis OK response
    }
}

#[tokio::test]
async fn single_test() {
    use redis::AsyncCommands;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct User {
        id: u64,
        name: String,
        mail: String,
    }
    let config = RedisConfig {
        username: None,
        password: None,
        url: "redis://127.0.0.1:6379".to_string(),
        pool_create_timeout_msec: None,
        pool_wait_timeout_msec: None,
        pool_recycle_timeout_msec: None,
        pool_size: 10,
    };
    let mut cli = new_redis_connection(config).await.unwrap();
    cli.del::<&str, u32>("foo").await.unwrap();
    let v: Option<String> = cli.get("foo").await.unwrap();

    // use the pool like any other RedisClient with the Deref trait
    assert_eq!(None, v);
    assert!(cli
        .set_ex::<&str, &str, bool>("foo", "bar", 30)
        .await
        .unwrap());
    assert_eq!(
        "bar".to_string(),
        cli.get::<&str, String>("foo").await.unwrap()
    );
    let user1 = User {
        id: 1,
        name: "Taro".to_string(),
        mail: "taro@example.com".to_string(),
    };
    // set serialized user1
    assert!(cli
        .set_ex::<&str, String, bool>("user1", serde_json::to_string(&user1).unwrap(), 30,)
        .await
        .unwrap());
    // get and deserialize user1 string
    if let Ok(st) = cli.get::<&str, Option<String>>("user1").await {
        assert_eq!(
            Some(user1),
            serde_json::from_str(st.unwrap().as_str()).unwrap()
        )
    } else {
        panic!("expected value not found for struct User");
    }
    // for end
}

#[tokio::test]
#[ignore = "need redis server (redis:6379) for test"]
async fn pool_test() -> Result<()> {
    use redis::AsyncCommands;
    // use std::time::Duration;

    // tracing_subscriber::fmt()
    //     .with_max_level(tracing::Level::DEBUG)
    //     .init();

    #[derive(Clone)]
    struct RedisPool {
        pool: Pool,
    }
    impl UseRedisPool for RedisPool {
        fn redis_pool(&self) -> &Pool {
            &self.pool
        }
    }
    let config = RedisConfig {
        username: None,
        password: None,
        url: "redis://127.0.0.1:6379".to_string(),
        pool_create_timeout_msec: None,
        pool_wait_timeout_msec: None,
        pool_recycle_timeout_msec: None,
        pool_size: 5,
    };
    let p = new_redis_pool(config).await.unwrap();
    let client = RedisPool { pool: p };
    let th_p = client.clone();
    // blocking (pop)
    let pop_jh = tokio::spawn(async move {
        while let Ok((k, v)) = th_p
            .connection()
            .await
            .unwrap()
            .blpop::<&str, (String, i64)>("foobl", 1.0)
            .await
        {
            println!("============ Blocking pop result on {}: {}", k, v);
        }
    });

    let key = "ffffoooo";
    // use the pool like any other RedisClient with the Deref trait
    assert_eq!(
        Ok(None),
        client
            .connection()
            .await
            .unwrap()
            .get::<&str, Option<String>>(key)
            .await
    );
    assert_eq!(
        Ok(true),
        client
            .connection()
            .await
            .unwrap()
            .set_ex(key, "bar", 10)
            .await
    );
    assert_eq!(
        "bar".to_string(),
        client
            .connection()
            .await
            .unwrap()
            .get::<&str, String>(key)
            .await
            .unwrap()
    );
    // push for blpop
    for idx in 0..50i64 {
        assert!(client
            .connection()
            .await
            .unwrap()
            .rpush::<&str, i64, bool>("foobl", idx)
            .await
            .is_ok());
        // tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(Ok(true), client.connection().await.unwrap().del(key).await);
    assert_eq!(
        Ok(None),
        client
            .connection()
            .await
            .unwrap()
            .get::<&str, Option<String>>(key)
            .await
    );
    // for end
    // wait for blpop timeout
    pop_jh.await?;
    // pop_jh.abort();
    Ok(())
}

// lock and unlock test with UseRedisLock
#[tokio::test]
async fn lock_unlock_test() -> Result<()> {
    #[derive(Clone)]
    struct RedisPool {
        pool: Pool,
    }
    impl UseRedisPool for RedisPool {
        fn redis_pool(&self) -> &Pool {
            &self.pool
        }
    }
    impl UseRedisLock for RedisPool {}

    let config = RedisConfig {
        username: None,
        password: None,
        url: "redis://127.0.0.1:6379".to_string(),
        pool_create_timeout_msec: None,
        pool_wait_timeout_msec: None,
        pool_recycle_timeout_msec: None,
        pool_size: 5,
    };
    let p = new_redis_pool(config).await.unwrap();
    let client = RedisPool { pool: p };
    let key = "lock_test";
    client.lock(key, 10).await?;
    // try lock
    assert!(client.lock(key, 10).await.is_err());
    // unlock
    client.unlock(key).await?;
    // try lock again
    assert!(client.lock(key, 10).await.is_ok());
    // for end
    Ok(())
}
