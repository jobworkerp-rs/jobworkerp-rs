use anyhow::Result;
use async_trait::async_trait;
use futures::Future;
use serde::Deserialize;
use std::fmt::Debug;
use std::{hash::Hash, time::Duration};
use stretto::AsyncCache;

#[derive(Deserialize, Debug, Clone, Copy)]
pub struct MemoryCacheConfig {
    pub num_counters: usize,
    /// max number of items (this cache is used with cost as fixed value 1 in inserting)
    pub max_cost: i64,
    pub use_metrics: bool,
}
impl Default for MemoryCacheConfig {
    fn default() -> Self {
        tracing::info!("Use default memoryCacheConfig.");
        // TODO consider of machine memory size?
        Self {
            num_counters: 12960,
            max_cost: 1296,
            use_metrics: true,
        }
    }
}

// TODO use ByteKey as key ?
// TODO cost
#[async_trait]
pub trait UseMemoryCache<KEY, VAL>
where
    KEY: Hash + Eq + Sync + Send + Debug + Clone + 'static,
    VAL: Debug + Send + Sync + Clone + 'static,
{
    // type KEY: Hash + Eq + Sync + Send + Debug + Clone;
    // type VAL: Debug + Send + Sync + Clone + 'static;

    /// default cache ttl
    const CACHE_TTL: Option<Duration>;

    fn cache(&self) -> &AsyncCache<KEY, VAL>;

    async fn set_cache(&self, key: KEY, value: VAL, ttl: Option<Duration>) -> bool
    where
        Self: Send + Sync,
    {
        match ttl.or(Self::CACHE_TTL) {
            Some(t) => self.cache().insert_with_ttl(key, value, 1i64, t).await,
            None => self.cache().insert(key, value, 1i64).await,
        }
    }

    async fn wait_cache(&self) {
        self.cache()
            .wait()
            .await
            .map_err(move |e| {
                tracing::warn!("cache wait error?: err: {}", e);
            })
            .unwrap_or(()); // XXX ignore error
    }

    async fn with_cache<R, F>(
        &self,
        key: &KEY,
        ttl: Option<Duration>,
        not_found_case: F,
    ) -> Result<VAL>
    where
        Self: Send + Sync,
        R: Future<Output = Result<VAL>> + Send,
        F: FnOnce() -> R + Send,
    {
        match self.find_cache(key).await {
            Some(r) => Ok(r),
            None => {
                let v = not_found_case().await;
                match v {
                    Ok(r) => {
                        self.set_cache((*key).clone(), r.clone(), ttl.or(Self::CACHE_TTL))
                            .await;
                        self.cache()
                            .wait()
                            .await
                            .map_err(move |e| {
                                tracing::warn!("cache error: key={:?}, err: {}", key, e);
                            })
                            .unwrap_or(()); // XXX ignore error
                        Ok(r)
                    }
                    Err(e) => {
                        tracing::warn!("cache error: key={:?}, err: {:?}", key, e);
                        Err(e)
                    }
                }
            }
        }
    }

    async fn find_cache(&self, key: &KEY) -> Option<VAL>
    where
        Self: Send + Sync,
    {
        self.cache().get(key).await.map(|v| {
            let res = v.value().clone();
            tracing::debug!("memory cache found: {:?}", key);
            v.release();
            res
        })
    }

    async fn delete_cache(&self, key: &KEY)
    where
        Self: Send + Sync,
    {
        self.cache().remove(key).await
    }
}

pub fn new_memory_cache<K: Hash + Eq, V: Send + Sync + 'static>(
    config: &MemoryCacheConfig,
) -> AsyncCache<K, V> {
    AsyncCache::<K, V>::builder(config.num_counters, config.max_cost)
        .set_metrics(config.use_metrics)
        .finalize(tokio::spawn)
        .unwrap()
}

#[tokio::test]
async fn with_cache_test() {
    struct MemCache {
        mcache: AsyncCache<&'static str, &'static str>,
    }
    // impl<K: Hash + Eq + 'static, V: Send + Sync + 'static> MemoryCache<'_, K, V> for MemCache {};
    impl UseMemoryCache<&'static str, &'static str> for MemCache {
        // type KEY = &'static str;
        // type VAL = &'static str;
        const CACHE_TTL: Option<Duration> = Some(Duration::from_secs(60)); // 1 min

        fn cache(&self) -> &AsyncCache<&'static str, &'static str> {
            &self.mcache
        }
    }
    let config = MemoryCacheConfig {
        num_counters: 10000,
        max_cost: 1e6 as i64,
        use_metrics: true,
    };
    let cache = MemCache {
        mcache: new_memory_cache::<&'static str, &'static str>(&config),
    };

    let key = "hoge";
    let value1 = "value!!";
    let value2 = "value2!!";
    let ttl = Some(Duration::from_secs_f32(0.2));
    // let ttl = None; // Some(Duration::from_secs(1));
    // resolve and store cache
    let val1: Result<&str> = cache
        .with_cache(&key, ttl, move || async move { Ok(value1) })
        .await;
    assert_eq!(val1.unwrap(), value1);
    // use cache
    let val2: Result<&str> = cache
        .with_cache(&key, ttl, move || async move { Ok(value2) })
        .await;
    assert_eq!(val2.unwrap(), value1);
    // cache expired
    tokio::time::sleep(Duration::from_millis(200)).await;
    let val3: Result<&str> = cache
        .with_cache(&key, ttl, move || async move { Ok(value2) })
        .await;
    assert_eq!(val3.unwrap(), value2);
    let val4: Option<&str> = cache.find_cache(&key).await;
    assert_eq!(val4.unwrap(), value2);
}

#[tokio::test]
async fn with_arc_key_cache_test() {
    use std::sync::Arc;
    struct MemCache {
        mcache: AsyncCache<Arc<String>, &'static str>,
    }
    impl UseMemoryCache<Arc<String>, &'static str> for MemCache {
        // type KEY = Arc<String>;
        // type VAL = &'static str;

        const CACHE_TTL: Option<Duration> = Some(Duration::from_secs(60)); // 1 min
        fn cache(&self) -> &AsyncCache<Arc<String>, &'static str> {
            &self.mcache
        }
    }
    let config = MemoryCacheConfig {
        num_counters: 10000,
        max_cost: 1e6 as i64,
        use_metrics: true,
    };
    let cache = MemCache {
        mcache: new_memory_cache::<Arc<String>, &'static str>(&config),
    };
    let key = &Arc::new(String::from("hoge"));
    let value1 = "value!!";
    let value2 = "value2!!";
    let ttl = Some(Duration::from_secs_f32(0.2));
    // resolve and store cache
    assert_eq!(None, cache.find_cache(&key.clone()).await);
    let val1: Result<&str> = cache
        .with_cache(key, ttl, move || async move { Ok(value1) })
        .await;
    assert_eq!(val1.unwrap(), value1);
    // use cache
    assert_eq!(Some(value1), cache.find_cache(&key.clone()).await);
    let val2: Result<&str> = cache
        .with_cache(key, ttl, move || async move { Ok(value2) })
        .await;
    assert_eq!(val2.unwrap(), value1);
    // cache expired
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(None, cache.find_cache(&key.clone()).await);
    let val3: Result<&str> = cache
        .with_cache(key, None, move || async move { Ok(value2) })
        .await;
    assert_eq!(val3.unwrap(), value2);
    assert_eq!(Some(value2), cache.find_cache(key).await);
}
#[tokio::test]
async fn set_find_cache_test() {
    struct MemCache {
        mcache: AsyncCache<&'static str, &'static str>,
    }
    // impl<K: Hash + Eq + 'static, V: Send + Sync + 'static> MemoryCache<'_, K, V> for MemCache {};
    impl UseMemoryCache<&'static str, &'static str> for MemCache {
        // type KEY = &'static str;
        // type VAL = &'static str;
        const CACHE_TTL: Option<Duration> = Some(Duration::from_secs(60)); // 1 min

        fn cache(&self) -> &AsyncCache<&'static str, &'static str> {
            &self.mcache
        }
    }
    let config = MemoryCacheConfig {
        num_counters: 10000,
        max_cost: 1e6 as i64,
        use_metrics: true,
    };
    let cache = MemCache {
        mcache: new_memory_cache::<&'static str, &'static str>(&config),
    };

    let key = "hoge";
    let value1 = "value!!";
    let value2 = "value2!!";
    let ttl = Some(Duration::from_secs_f32(0.2));
    // let ttl = None; // Some(Duration::from_secs(1));
    // resolve and store cache
    assert!(cache.set_cache(key, value1, ttl).await);
    cache.wait_cache().await;
    assert_eq!(cache.find_cache(&key).await.unwrap(), value1);
    // use cache
    assert!(cache.set_cache(key, value2, ttl).await);
    cache.wait_cache().await;
    assert_eq!(cache.find_cache(&key).await.unwrap(), value2);
    // cache expired
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(cache.find_cache(&key).await, None);
}
