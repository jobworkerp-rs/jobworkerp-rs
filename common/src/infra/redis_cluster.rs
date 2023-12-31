use anyhow::anyhow;
use anyhow::Result;
use async_trait::async_trait;
use deadpool::managed::PoolConfig;
use deadpool_redis::cluster::{Config, Connection, Pool};
use deadpool_redis::redis::cmd;
use deadpool_redis::redis::AsyncCommands as PoolAsyncCommands;
use deadpool_redis::Timeouts;
use debug_stub_derive::DebugStub;
use redis::cluster::ClusterClient;
use redis::cluster_async::ClusterConnection;
use serde::Deserialize;
use std::time::Duration;

#[derive(Deserialize, Clone, DebugStub)]
pub struct RedisClusterConfig {
    #[debug_stub = "[USER]"]
    pub username: Option<String>,
    #[debug_stub = "[PASSWORD]"]
    pub password: Option<String>,
    pub urls: Vec<String>,
    pub pool_create_timeout_msec: Option<u64>,
    pub pool_wait_timeout_msec: Option<u64>,
    pub pool_recycle_timeout_msec: Option<u64>,
    pub pool_size: usize,
}
impl From<RedisClusterConfig> for Config {
    fn from(val: RedisClusterConfig) -> Self {
        Config {
            urls: Some(val.urls),
            connections: None,
            pool: Some(PoolConfig {
                max_size: val.pool_size,
                timeouts: Timeouts {
                    wait: val.pool_wait_timeout_msec.map(Duration::from_millis),
                    create: val.pool_create_timeout_msec.map(Duration::from_millis),
                    recycle: val.pool_recycle_timeout_msec.map(Duration::from_millis),
                },
                queue_mode: deadpool::managed::QueueMode::Fifo,
            }),
        }
    }
}

pub type RedisClusterPool = Pool;

// TODO looking forward to resolving issue: https://github.com/redis-rs/redis-rs/issues/492
//
// use redis::aio::Connection as RedisConnection;
// use redis::aio::PubSub;
// pub type RedisClient = redis::cluster::ClusterClient;
//#[async_trait]
//pub trait UseRedisClusterClient {
//    fn redis_cluster_client(&self) -> &RedisClient;
//
//    async fn subscribe(&self, channel: &str) -> Result<PubSub> {
//        let mut pubsub = self
//            .redis_cluster_client()
//            .get_async_connection()
//            .await?
//            .into_pubsub();
//        pubsub.subscribe(channel).await?;
//        Ok(pubsub)
//    }
//
//    async fn psubscribe(&self, pchannel: &String) -> Result<PubSub> {
//        let mut pubsub = self
//            .redis_cluster_client()
//            .get_async_connection()
//            .await?
//            .into_pubsub();
//        pubsub.psubscribe(pchannel).await?;
//        Ok(pubsub)
//    }
//
//    async fn publish(&self, channel: &str, message: &Vec<u8>) -> Result<()> {
//        let mut conn = self.redis_cluster_client().get_async_connection().await?;
//        conn.publish(channel, message).await?;
//        Ok(())
//    }
//}

#[async_trait]
pub trait UseRedisClusterConnection {
    fn redis_cluster_connection(&self) -> &ClusterConnection;
}

#[async_trait]
pub trait UseRedisClusterPool {
    fn redis_cluster_pool(&self) -> &Pool;

    async fn connection(&self) -> Result<Connection> {
        self.redis_cluster_pool()
            .get()
            .await
            .map_err(|e| anyhow!("{:?}", e))
    }
}

// for normal use (single connection)
pub fn new_redis_cluster_client(conf: RedisClusterConfig) -> Result<ClusterClient> {
    tracing::info!("Connecting to {:?}", conf.urls);
    ClusterClient::new(conf.urls).map_err(|e| e.into())
}
pub async fn new_redis_cluster_connection(config: RedisClusterConfig) -> Result<ClusterConnection> {
    tracing::info!("Connecting to {:?}", config);

    let client = new_redis_cluster_client(config)?;
    client
        .get_async_connection()
        .await
        .map_err(|e| anyhow!("redis_cluster init error: {:?}", e))
}

// pooling for multiple blocking connections
pub async fn new_redis_cluster_pool(conf: RedisClusterConfig) -> Result<RedisClusterPool> {
    tracing::info!("Connecting to cluster with pool{:?}", conf.urls);
    let cconf: Config = conf.into();
    cconf.builder()?.build().map_err(|e| {
        anyhow!(format!(
            "redis cluster pool init error:{:?}, config={:?}",
            e, cconf
        ))
    })
}

#[async_trait]
pub trait UseRedisClusterLock: UseRedisClusterPool {
    async fn lock(&self, key: impl Into<String> + Send + Sync, expire_sec: i32) -> Result<()> {
        let mut con = self.redis_cluster_pool().get().await?;
        // use redis_cluster set cmd with nx and ex option to lock
        let k = key.into();
        match cmd("SET")
            .arg(&k)
            .arg(Self::lock_value())
            .arg("NX")
            .arg("EX")
            .arg(expire_sec)
            .query_async::<_, String>(con.as_mut())
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
        let mut redis_cluster = self.redis_cluster_pool().get().await?;
        let k = key.into();
        redis_cluster.del(k).await?;
        Ok(())
    }

    #[inline]
    fn lock_value() -> &'static str {
        "1"
    }

    #[inline]
    fn is_ok(str: String) -> bool {
        str == "OK" // for redis_cluster OK response
    }

    #[inline]
    fn is_not_ok(str: String) -> bool {
        str != "OK" // for redis_cluster OK response
    }
}

#[cfg(test)]
mod test {
    use crate::infra::redis_cluster::{UseRedisClusterLock, UseRedisClusterPool};
    use anyhow::Result;
    use deadpool_redis::cluster::Pool;
    use redis::AsyncCommands;
    use serde::Deserialize;

    #[tokio::test]
    #[ignore = "need redis_cluster server (redis-cluster:7000-7005) for test"]
    async fn single_test() {
        use serde::{Deserialize, Serialize};

        #[derive(Debug, Serialize, Deserialize, PartialEq)]
        struct User {
            id: u64,
            name: String,
            mail: String,
        }
        let config = super::RedisClusterConfig {
            username: None,
            password: None,
            urls: vec![
                "redis://127.0.0.1:7000".to_string(),
                "redis://127.0.0.1:7001".to_string(),
                "redis://127.0.0.1:7002".to_string(),
                "redis://127.0.0.1:7003".to_string(),
                "redis://127.0.0.1:7004".to_string(),
                "redis://127.0.0.1:7005".to_string(),
            ],
            pool_create_timeout_msec: None,
            pool_wait_timeout_msec: None,
            pool_recycle_timeout_msec: None,
            pool_size: 10,
        };
        let mut cli = super::new_redis_cluster_connection(config).await.unwrap();
        cli.del::<&str, u32>("foo").await.unwrap();
        let v: Option<String> = cli.get("foo").await.unwrap();

        // use the pool like any other RedisClusterClient with the Deref trait
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
    #[ignore = "need redis_cluster server (redis-cluster:7000-7005) for test"]
    async fn pool_test() -> Result<()> {
        // use std::time::Duration;

        // tracing_subscriber::fmt()
        //     .with_max_level(tracing::Level::DEBUG)
        //     .init();

        #[derive(Clone)]
        struct RedisClusterPool {
            pool: Pool,
        }
        impl super::UseRedisClusterPool for RedisClusterPool {
            fn redis_cluster_pool(&self) -> &Pool {
                &self.pool
            }
        }
        let config = super::RedisClusterConfig {
            username: None,
            password: None,
            urls: vec![
                "redis://127.0.0.1:7000".to_string(),
                "redis://127.0.0.1:7001".to_string(),
                "redis://127.0.0.1:7002".to_string(),
                "redis://127.0.0.1:7003".to_string(),
                "redis://127.0.0.1:7004".to_string(),
                "redis://127.0.0.1:7005".to_string(),
            ],
            pool_create_timeout_msec: None,
            pool_wait_timeout_msec: None,
            pool_recycle_timeout_msec: None,
            pool_size: 5,
        };
        let p = super::new_redis_cluster_pool(config).await.unwrap();
        let client = RedisClusterPool { pool: p };
        let th_p = client.clone();
        // blocking (pull)
        let pull_jh = tokio::spawn(async move {
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
        // use the pool like any other RedisClusterClient with the Deref trait
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
        pull_jh.await?;
        // pull_jh.abort();
        Ok(())
    }

    // lock and unlock test with UseRedisClusterLock
    #[tokio::test]
    #[ignore = "need redis_cluster server (redis-cluster:7000-7005) for test"]
    async fn lock_unlock_test() -> Result<()> {
        #[derive(Clone)]
        struct RedisClusterPool {
            pool: Pool,
        }
        impl UseRedisClusterPool for RedisClusterPool {
            fn redis_cluster_pool(&self) -> &Pool {
                &self.pool
            }
        }
        impl super::UseRedisClusterLock for RedisClusterPool {}

        let config = super::RedisClusterConfig {
            username: None,
            password: None,
            urls: vec![
                "redis://127.0.0.1:7000".to_string(),
                "redis://127.0.0.1:7001".to_string(),
                "redis://127.0.0.1:7002".to_string(),
                "redis://127.0.0.1:7003".to_string(),
                "redis://127.0.0.1:7004".to_string(),
                "redis://127.0.0.1:7005".to_string(),
            ],
            pool_create_timeout_msec: None,
            pool_wait_timeout_msec: None,
            pool_recycle_timeout_msec: None,
            pool_size: 5,
        };
        let p = super::new_redis_cluster_pool(config).await.unwrap();
        let client = RedisClusterPool { pool: p };
        let key = "lock_test";
        // get lock
        client.lock(key, 10).await?;
        // try lock -> failure
        assert!(client.lock(key, 10).await.is_err());
        // unlock
        client.unlock(key).await?;
        // try lock again
        assert!(client.lock(key, 10).await.is_ok());
        // for end
        Ok(())
    }
}
