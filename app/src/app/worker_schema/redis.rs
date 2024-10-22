use anyhow::Result;
use async_trait::async_trait;
use command_utils::util::result::TapErr;
use debug_stub_derive::DebugStub;
use infra::infra::module::redis::{RedisRepositoryModule, UseRedisRepositoryModule};
use infra::infra::worker_schema::redis::{
    RedisWorkerSchemaRepository, UseRedisWorkerSchemaRepository,
};
use infra::infra::{IdGeneratorWrapper, UseIdGenerator};
use infra_utils::infra::lock::RwLockWithKey;
use infra_utils::infra::memory::{self, MemoryCacheConfig, MemoryCacheImpl, UseMemoryCache};
use proto::jobworkerp::data::{WorkerSchema, WorkerSchemaId};
use std::sync::Arc;
use std::time::Duration;
use stretto::AsyncCache;

use super::super::{StorageConfig, UseStorageConfig};
use super::{
    UseWorkerSchemaParserWithCache, WorkerSchemaApp, WorkerSchemaCacheHelper,
    WorkerSchemaWithDescriptor,
};

// TODO cache control
#[derive(DebugStub)]
pub struct RedisWorkerSchemaAppImpl {
    storage_config: Arc<StorageConfig>,
    id_generator: Arc<IdGeneratorWrapper>,
    #[debug_stub = "AsyncCache<Arc<String>, Vec<WorkerSchema>>"]
    async_cache: AsyncCache<Arc<String>, Vec<WorkerSchema>>,
    descriptor_cache: Arc<MemoryCacheImpl<Arc<String>, WorkerSchemaWithDescriptor>>,
    repositories: Arc<RedisRepositoryModule>,
    key_lock: RwLockWithKey<Arc<String>>,
}

impl RedisWorkerSchemaAppImpl {
    pub fn new(
        storage_config: Arc<StorageConfig>,
        id_generator: Arc<IdGeneratorWrapper>,
        memory_cache_config: &MemoryCacheConfig,
        repositories: Arc<RedisRepositoryModule>,
        descriptor_cache: Arc<MemoryCacheImpl<Arc<String>, WorkerSchemaWithDescriptor>>,
    ) -> Self {
        Self {
            storage_config,
            id_generator,
            async_cache: memory::new_memory_cache(memory_cache_config),
            descriptor_cache,
            repositories,
            key_lock: RwLockWithKey::default(),
        }
    }

    // async fn create_worker_schema(
    //     &self,
    //     worker_schema: WorkerSchemaData,
    // ) -> Result<WorkerSchemaId> {
    //     let schema = self.validate_and_get_worker_schema(worker_schema)?;
    //     let id = self.id_generator().generate_id()?;
    //     let rid = WorkerSchemaId { value: id };
    //     self.redis_worker_schema_repository()
    //         .upsert(&rid, &schema.schema)
    //         .await?;
    //     // clear list cache
    //     let _ = self
    //         .memory_cache
    //         .delete_cache_locked(&Self::find_all_list_cache_key())
    //         .await; // ignore error

    //     // let _ = self
    //     //     .redis_worker_schema_repository()
    //     //     .publish_worker_schema_changed(&rid, worker_schema)
    //     //     .await;

    //     Ok(rid)
    // }
}
// TODO now, hybrid repository (or redis?) version only
#[async_trait]
impl WorkerSchemaApp for RedisWorkerSchemaAppImpl {
    async fn load_worker_schema(&self) -> Result<bool> {
        self.redis_worker_schema_repository()
            .add_from_plugins()
            .await?;
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key())
            .await;
        Ok(true)
    }

    async fn delete_worker_schema(&self, id: &WorkerSchemaId) -> Result<bool> {
        let res = self.redis_worker_schema_repository().delete(id).await?;
        let _ = self
            .delete_cache_locked(&Self::find_cache_key(&id.value))
            .await;
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key())
            .await;
        // TODO
        // let _ = self
        //     .redis_worker_schema_repository()
        //     .publish_worker_schema_deleted(id)
        //     .await;
        Ok(res)
    }

    async fn find_worker_schema(
        &self,
        id: &WorkerSchemaId,
        ttl: Option<&Duration>,
    ) -> Result<Option<WorkerSchema>>
    where
        Self: Send + 'static,
    {
        let k = Self::find_cache_key(&id.value);
        self.with_cache_locked(&k, ttl, || async {
            self.redis_worker_schema_repository()
                .find(id)
                .await
                .tap_err(|err| {
                    tracing::warn!(
                        "cannot access redis and rdb in finding worker_schema: {}",
                        err
                    )
                })
                .map(|r| r.map(|o| vec![o]).unwrap_or_default()) // XXX cache type: vector
        })
        .await
        .map(|r| r.first().map(|o| (*o).clone()))
    }

    async fn find_worker_schema_list(
        &self,
        limit: Option<&i32>,
        offset: Option<&i64>,
        _ttl: Option<&Duration>,
    ) -> Result<Vec<WorkerSchema>>
    where
        Self: Send + 'static,
    {
        // let k = Arc::new(Self::find_list_cache_key(limit, offset));
        // self.memory_cache
        //     .with_cache(&k, None, || async {
        self.redis_worker_schema_repository()
            .find_all()
            .await
            .map(|v| {
                // soft paging
                let start = offset.unwrap_or(&0);
                if let Some(l) = limit {
                    v.into_iter()
                        .skip(*start as usize)
                        .take(*l as usize)
                        .collect()
                } else {
                    v.into_iter().skip(*start as usize).collect()
                }
            })
        // })
        // .await
    }

    async fn find_worker_schema_all_list(&self, ttl: Option<&Duration>) -> Result<Vec<WorkerSchema>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_all_list_cache_key());
        self.with_cache(&k, ttl, || async {
            // not use rdb in normal case
            self.redis_worker_schema_repository().find_all().await
        })
        .await
    }
    async fn count(&self) -> Result<i64>
    where
        Self: Send + 'static,
    {
        // TODO cache
        // find from redis first
        let cnt = self.redis_worker_schema_repository().count().await?;
        Ok(cnt)
    }
    // for test
    #[cfg(test)]
    async fn create_test_schema(
        &self,
        schema_id: &WorkerSchemaId,
        name: &str,
    ) -> Result<WorkerSchemaWithDescriptor> {
        use super::test::test_worker_schema_with_descriptor;

        let schema = test_worker_schema_with_descriptor(name);
        let _res = self
            .redis_worker_schema_repository()
            .upsert(schema_id, &schema.schema)
            .await?;
        self.store_proto_cache(schema_id, &schema).await;
        // clear memory cache
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key())
            .await;
        Ok(schema)
    }
}

impl UseStorageConfig for RedisWorkerSchemaAppImpl {
    fn storage_config(&self) -> &StorageConfig {
        &self.storage_config
    }
}
impl UseIdGenerator for RedisWorkerSchemaAppImpl {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        &self.id_generator
    }
}

impl UseRedisRepositoryModule for RedisWorkerSchemaAppImpl {
    fn redis_repository_module(&self) -> &RedisRepositoryModule {
        &self.repositories
    }
}
impl UseMemoryCache<Arc<String>, Vec<WorkerSchema>> for RedisWorkerSchemaAppImpl {
    fn cache(&self) -> &AsyncCache<Arc<String>, Vec<WorkerSchema>> {
        &self.async_cache
    }

    #[doc = " default cache ttl"]
    fn default_ttl(&self) -> Option<&Duration> {
        None
    }

    fn key_lock(&self) -> &RwLockWithKey<Arc<String>> {
        &self.key_lock
    }
}
impl WorkerSchemaCacheHelper for RedisWorkerSchemaAppImpl {}

impl UseWorkerSchemaParserWithCache for RedisWorkerSchemaAppImpl {
    fn descriptor_cache(&self) -> &MemoryCacheImpl<Arc<String>, WorkerSchemaWithDescriptor> {
        &self.descriptor_cache
    }
}
