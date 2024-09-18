use anyhow::Result;
use async_trait::async_trait;
use debug_stub_derive::DebugStub;
use infra::infra::module::rdb::RdbChanRepositoryModule;
use infra::infra::runner_schema::rdb::{RdbRunnerSchemaRepositoryImpl, UseRunnerSchemaRepository};
use infra::infra::{IdGeneratorWrapper, UseIdGenerator};
use infra::{error::JobWorkerError, infra::runner_schema::rdb::RunnerSchemaRepository};
use infra_utils::infra::lock::RwLockWithKey;
use infra_utils::infra::memory::{self, MemoryCacheConfig, MemoryCacheImpl, UseMemoryCache};
use infra_utils::infra::rdb::UseRdbPool;
use proto::jobworkerp::data::{RunnerSchema, RunnerSchemaData, RunnerSchemaId};
use std::{sync::Arc, time::Duration};
use stretto::AsyncCache;

use crate::app::StorageConfig;

use super::{
    RunnerSchemaApp, RunnerSchemaCacheHelper, RunnerSchemaWithDescriptor,
    UseRunnerSchemaParserWithCache,
};

#[derive(Clone, DebugStub)]
pub struct RdbRunnerSchemaAppImpl {
    storage_config: Arc<StorageConfig>,
    id_generator: Arc<IdGeneratorWrapper>,
    #[debug_stub = "AsyncCache<Arc<String>, Vec<RunnerSchema>>"]
    async_cache: AsyncCache<Arc<String>, Vec<RunnerSchema>>,
    memory_cache: MemoryCacheImpl<Arc<String>, RunnerSchemaWithDescriptor>,
    repositories: Arc<RdbChanRepositoryModule>,
}

impl RdbRunnerSchemaAppImpl {
    pub fn new(
        storage_config: Arc<StorageConfig>,
        id_generator: Arc<IdGeneratorWrapper>,
        memory_cache_config: &MemoryCacheConfig,
        repositories: Arc<RdbChanRepositoryModule>,
    ) -> Self {
        Self {
            storage_config,
            id_generator,
            async_cache: memory::new_memory_cache(memory_cache_config),
            memory_cache: MemoryCacheImpl::new(memory_cache_config, None),
            repositories,
        }
    }
}

impl UseIdGenerator for RdbRunnerSchemaAppImpl {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        &self.id_generator
    }
}
impl UseRunnerSchemaParserWithCache for RdbRunnerSchemaAppImpl {
    fn cache(&self) -> &MemoryCacheImpl<Arc<String>, RunnerSchemaWithDescriptor> {
        &self.memory_cache
    }
}

#[async_trait]
impl RunnerSchemaApp for RdbRunnerSchemaAppImpl {
    async fn create_runner_schema(
        &self,
        runner_schema: RunnerSchemaData,
    ) -> Result<RunnerSchemaId> {
        let id = RunnerSchemaId {
            value: self.id_generator().generate_id()?,
        };
        let db = self.runner_schema_repository().db_pool();
        let mut tx = db.begin().await.map_err(JobWorkerError::DBError)?;
        let id = self
            .runner_schema_repository()
            .create(&mut *tx, id, &runner_schema)
            .await?;
        tx.commit().await.map_err(JobWorkerError::DBError)?;
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key())
            .await;

        Ok(id)
    }

    // offset, limit 付きのリストキャッシュは更新されないので反映に時間かかる (ttl短かめにする)
    async fn update_runner_schema(
        &self,
        id: &RunnerSchemaId,
        runner_schema: &Option<RunnerSchemaData>,
    ) -> Result<bool> {
        if let Some(w) = runner_schema {
            let pool = self.runner_schema_repository().db_pool();
            let mut tx = pool.begin().await.map_err(JobWorkerError::DBError)?;
            self.runner_schema_repository()
                .update(&mut *tx, id, w)
                .await?;
            tx.commit().await.map_err(JobWorkerError::DBError)?;
            // clear memory cache
            let _ = self
                .delete_cache_locked(&Self::find_cache_key(&id.value))
                .await;
            let _ = self
                .delete_cache_locked(&Self::find_all_list_cache_key())
                .await;
            Ok(true)
        } else {
            // all empty, no update
            Ok(false)
        }
    }

    async fn delete_runner_schema(&self, id: &RunnerSchemaId) -> Result<bool> {
        let res = self.runner_schema_repository().delete(id).await?;
        // clear memory cache
        let _ = self
            .delete_cache_locked(&Self::find_cache_key(&id.value))
            .await;
        let _ = self
            .delete_cache_locked(&Self::find_all_list_cache_key())
            .await;
        Ok(res)
    }

    async fn find_runner_schema(
        &self,
        id: &RunnerSchemaId,
        ttl: Option<&Duration>,
    ) -> Result<Option<RunnerSchema>>
    where
        Self: Send + 'static,
    {
        let k = Self::find_cache_key(&id.value);
        self.with_cache_locked(&k, ttl, || async {
            let v = self.runner_schema_repository().find(id).await;
            match v {
                Ok(opt) => Ok(match opt {
                    Some(v) => vec![v],
                    None => Vec::new(),
                }),
                Err(e) => Err(e),
            }
        })
        .await
        .map(|r| r.first().map(|o| (*o).clone()))
    }

    // XXX no cache
    async fn find_runner_schema_list(
        &self,
        limit: Option<&i32>,
        offset: Option<&i64>,
        _ttl: Option<&Duration>,
    ) -> Result<Vec<RunnerSchema>>
    where
        Self: Send + 'static,
    {
        // TODO cache
        self.runner_schema_repository()
            .find_list(limit, offset)
            .await
    }

    async fn find_runner_schema_all_list(&self, ttl: Option<&Duration>) -> Result<Vec<RunnerSchema>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_all_list_cache_key());
        self.with_cache_locked(&k, ttl, || async {
            self.runner_schema_repository().find_list(None, None).await
        })
        .await
    }

    async fn count(&self) -> Result<i64>
    where
        Self: Send + 'static,
    {
        // TODO cache
        self.runner_schema_repository()
            .count_list_tx(self.runner_schema_repository().db_pool())
            .await
    }
}

impl UseRunnerSchemaRepository for RdbRunnerSchemaAppImpl {
    fn runner_schema_repository(&self) -> &RdbRunnerSchemaRepositoryImpl {
        &self.repositories.runner_schema_repository
    }
}

impl UseMemoryCache<Arc<String>, Vec<RunnerSchema>> for RdbRunnerSchemaAppImpl {
    fn cache(&self) -> &AsyncCache<Arc<String>, Vec<RunnerSchema>> {
        &self.async_cache
    }

    #[doc = " default cache ttl"]
    fn default_ttl(&self) -> Option<&Duration> {
        todo!()
    }

    fn key_lock(&self) -> &RwLockWithKey<Arc<String>> {
        todo!()
    }
}
impl RunnerSchemaCacheHelper for RdbRunnerSchemaAppImpl {}
