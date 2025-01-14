use super::super::{StorageConfig, UseStorageConfig};
use super::{WorkerApp, WorkerAppCacheHelper};
use crate::app::runner::redis::RedisRunnerAppImpl;
use crate::app::runner::{
    RunnerApp, RunnerDataWithDescriptor, UseRunnerApp, UseRunnerAppParserWithCache,
    UseRunnerParserWithCache,
};
use anyhow::Result;
use async_trait::async_trait;
use command_utils::util::option::FlatMap;
use command_utils::util::result::TapErr;
use infra::error::JobWorkerError;
use infra::infra::module::redis::{RedisRepositoryModule, UseRedisRepositoryModule};
use infra::infra::worker::event::UseWorkerPublish;
use infra::infra::worker::redis::{RedisWorkerRepository, UseRedisWorkerRepository};
use infra::infra::{IdGeneratorWrapper, UseIdGenerator};
use infra_utils::infra::memory::{MemoryCacheImpl, UseMemoryCache};
use proto::jobworkerp::data::{Worker, WorkerData, WorkerId};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct RedisWorkerAppImpl {
    storage_config: Arc<StorageConfig>,
    id_generator: Arc<IdGeneratorWrapper>,
    memory_cache: MemoryCacheImpl<Arc<String>, Worker>,
    list_memory_cache: MemoryCacheImpl<Arc<String>, Vec<Worker>>,
    repositories: Arc<RedisRepositoryModule>,
    descriptor_cache: Arc<MemoryCacheImpl<Arc<String>, RunnerDataWithDescriptor>>,
    runner_app: Arc<RedisRunnerAppImpl>,
}

impl RedisWorkerAppImpl {
    pub fn new(
        storage_config: Arc<StorageConfig>,
        id_generator: Arc<IdGeneratorWrapper>,
        memory_cache: MemoryCacheImpl<Arc<String>, Worker>,
        list_memory_cache: MemoryCacheImpl<Arc<String>, Vec<Worker>>,
        repositories: Arc<RedisRepositoryModule>,
        descriptor_cache: Arc<MemoryCacheImpl<Arc<String>, RunnerDataWithDescriptor>>,
        runner_app: Arc<RedisRunnerAppImpl>,
    ) -> Self {
        Self {
            storage_config,
            id_generator,
            memory_cache,
            list_memory_cache,
            repositories,
            descriptor_cache,
            runner_app,
        }
    }
}
// TODO now, hybrid repository (or redis?) version only
#[async_trait]
impl WorkerApp for RedisWorkerAppImpl {
    async fn create(&self, worker: &WorkerData) -> Result<WorkerId> {
        let id = self.id_generator().generate_id()?;
        let wid = WorkerId { value: id };
        let w = Worker {
            id: Some(wid),
            data: Some(worker.clone()),
        };
        let wsid = worker
            .runner_id
            .ok_or_else(|| JobWorkerError::InvalidParameter("runner_id is required".to_string()))?;
        self.validate_runner_settings_data(&wsid, worker.runner_settings.as_slice())
            .await?;
        self.redis_worker_repository().upsert(&w).await?;
        // clear cache
        let _ = self.clear_cache_by_name(&worker.name).await; // ignore error
        let _ = self
            .redis_worker_repository()
            .publish_worker_changed(&wid, worker)
            .await;
        Ok(wid)
    }

    async fn update(&self, id: &WorkerId, worker: &Option<WorkerData>) -> Result<bool> {
        if let Some(w) = worker {
            let wsid = w.runner_id.ok_or_else(|| {
                JobWorkerError::InvalidParameter("runner_id is required".to_string())
            })?;
            self.validate_runner_settings_data(&wsid, w.runner_settings.as_slice())
                .await?;

            let wk = Worker {
                id: Some(*id),
                data: Some(w.clone()),
            };
            self.redis_worker_repository().upsert(&wk).await?;
            // clear memory cache (XXX without limit offset cache)
            self.clear_cache(id).await;
            let _ = self
                .redis_worker_repository()
                .publish_worker_changed(id, w)
                .await;
            Ok(true)
        } else {
            // empty data, delete
            self.delete(id).await
        }
    }

    async fn delete(&self, id: &WorkerId) -> Result<bool> {
        let res = self.redis_worker_repository().delete(id).await?;
        if let Some(Worker {
            id: _,
            data: Some(wd),
        }) = self.find(id).await?
        {
            self.clear_cache_by_name(&wd.name).await;
        };
        self.clear_cache(id).await;
        let _ = self
            .redis_worker_repository()
            .publish_worker_deleted(id)
            .await;
        Ok(res)
    }

    async fn delete_all(&self) -> Result<bool> {
        let res = self.redis_worker_repository().delete_all().await?;
        self.clear_cache_all().await;
        self.redis_worker_repository()
            .publish_worker_all_deleted()
            .await?;
        Ok(res)
    }
    async fn find_data_by_name(&self, name: &str) -> Result<Option<WorkerData>>
    where
        Self: Send + 'static,
    {
        self.find_by_name(name)
            .await
            .map(|w| w.flat_map(|wd| wd.data))
    }

    async fn find_by_name(&self, name: &str) -> Result<Option<Worker>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_name_cache_key(name));
        self.memory_cache
            .with_cache_if_some(&k, None, || async {
                self.redis_worker_repository()
                    .find_by_name(name)
                    .await
                    .tap_err(|err| {
                        tracing::warn!("cannot access redis and rdb in finding worker: {}", err)
                    })
            })
            .await
    }

    async fn find(&self, id: &WorkerId) -> Result<Option<Worker>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_cache_key(id));
        self.memory_cache
            .with_cache_if_some(&k, None, || async {
                self.redis_worker_repository()
                    .find(id)
                    .await
                    .tap_err(|err| {
                        tracing::warn!("cannot access redis and rdb in finding worker: {}", err)
                    })
            })
            .await
    }

    async fn find_list(&self, limit: Option<i32>, offset: Option<i64>) -> Result<Vec<Worker>>
    where
        Self: Send + 'static,
    {
        // let k = Arc::new(Self::find_list_cache_key(limit, offset));
        // self.list_memory_cache
        //     .with_cache(&k, None, || async {
        self.redis_worker_repository().find_all().await.map(|v| {
            // soft paging
            let start = offset.unwrap_or(0);
            if let Some(l) = limit {
                v.into_iter()
                    .skip(start as usize)
                    .take(l as usize)
                    .collect()
            } else {
                v.into_iter().skip(start as usize).collect()
            }
        })
        // })
        // .await
    }

    async fn find_all_worker_list(&self) -> Result<Vec<Worker>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_all_list_cache_key());
        self.list_memory_cache
            .with_cache(&k, None, || async {
                // not use rdb in normal case
                self.redis_worker_repository().find_all().await
            })
            .await
    }
    async fn count(&self) -> Result<i64>
    where
        Self: Send + 'static,
    {
        // TODO cache
        // find from redis first
        let cnt = self.redis_worker_repository().count().await?;
        Ok(cnt)
    }
    // for pubsub (XXX common logic...)
    async fn clear_cache_by(&self, id: Option<&WorkerId>, name: Option<&String>) -> Result<()> {
        if let Some(i) = id {
            self.clear_cache(i).await;
        }
        if let Some(n) = name {
            self.clear_cache_by_name(n).await;
        }
        if id.is_none() && name.is_none() {
            self.clear_cache_all().await;
        }
        Ok(())
    }
}

impl UseStorageConfig for RedisWorkerAppImpl {
    fn storage_config(&self) -> &StorageConfig {
        &self.storage_config
    }
}
impl UseIdGenerator for RedisWorkerAppImpl {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        &self.id_generator
    }
}

impl UseRedisRepositoryModule for RedisWorkerAppImpl {
    fn redis_repository_module(&self) -> &RedisRepositoryModule {
        &self.repositories
    }
}

impl WorkerAppCacheHelper for RedisWorkerAppImpl {
    fn memory_cache(&self) -> &MemoryCacheImpl<Arc<String>, Worker> {
        &self.memory_cache
    }
    fn list_memory_cache(&self) -> &MemoryCacheImpl<Arc<String>, Vec<Worker>> {
        &self.list_memory_cache
    }
}
impl UseRunnerApp for RedisWorkerAppImpl {
    fn runner_app(&self) -> Arc<dyn RunnerApp> {
        self.runner_app.clone()
    }
}
impl UseRunnerParserWithCache for RedisWorkerAppImpl {
    fn descriptor_cache(&self) -> &MemoryCacheImpl<Arc<String>, RunnerDataWithDescriptor> {
        &self.descriptor_cache
    }
}
impl UseRunnerAppParserWithCache for RedisWorkerAppImpl {}
