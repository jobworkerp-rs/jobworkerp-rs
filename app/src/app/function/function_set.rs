use anyhow::Result;
use async_trait::async_trait;
use core::fmt;
use infra::infra::function_set::rdb::{
    FunctionSetRepository, FunctionSetRepositoryImpl, UseFunctionSetRepository,
};
use infra_utils::infra::rdb::UseRdbPool;
use jobworkerp_base::error::JobWorkerError;
use memory_utils::cache::moka::{MokaCache, MokaCacheImpl, UseMokaCache};
use proto::jobworkerp::function::data::{
    FunctionSet, FunctionSetData, FunctionSetId, FunctionSpecs,
};
use std::{sync::Arc, time::Duration};

// Import for find_functions_by_set
use super::FunctionSpecConverter;
use crate::app::runner::{RunnerApp, UseRunnerApp};
use crate::app::worker::{UseWorkerApp, WorkerApp};

#[async_trait]
pub trait FunctionSetApp: // XXX 1 impl
    UseFunctionSetRepository
    + UseMokaCache<Arc<String>, FunctionSet>
    + UseRunnerApp
    + UseWorkerApp
    + FunctionSpecConverter
    + fmt::Debug
    + Send
    + Sync
    + 'static
{
    async fn create_function_set(&self, function_set: &FunctionSetData) -> Result<FunctionSetId> {
        // transaction example
        let db = self.function_set_repository().db_pool();
        let mut tx = db.begin().await.map_err(JobWorkerError::DBError)?;
        let id = self
            .function_set_repository()
            .create(&mut tx, function_set)
            .await?;
        tx.commit().await.map_err(JobWorkerError::DBError)?;
        Ok(id)
    }

    // only cache single instance
    async fn update_function_set(
        &self,
        id: &FunctionSetId,
        function_set: &Option<FunctionSetData>,
    ) -> Result<bool> {
        if let Some(w) = function_set {
            let pool = self.function_set_repository().db_pool();
            let mut tx = pool.begin().await.map_err(JobWorkerError::DBError)?;
            self.function_set_repository()
                .update(&mut tx, id, w)
                .await?;
            tx.commit().await.map_err(JobWorkerError::DBError)?;
            // clear memory cache
            let k = Arc::new(self.find_cache_key(&id.value));
            let _ = self.delete_cache(&k).await;
            Ok(true)
        } else {
            // all empty, no update
            Ok(false)
        }
    }

    async fn delete_function_set(&self, id: &FunctionSetId) -> Result<bool> {
        let r = self.function_set_repository().delete(id).await;
        let k = Arc::new(self.find_cache_key(&id.value));
        let _ = self.delete_cache(&k).await;
        r
    }

    fn find_cache_key(&self, id: &i64) -> String {
        ["function_set_id:", &id.to_string()].join("")
    }

    fn find_by_name_cache_key(&self, name: &str) -> String {
        ["function_set_id_name:", name].join("")
    }

    async fn find_function_set(
        &self,
        id: &FunctionSetId,
    ) -> Result<Option<FunctionSet>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(self.find_cache_key(&id.value));
        self.with_cache_if_some(&k, || async {
            self.function_set_repository().find(id).await
        })
        .await
    }

    async fn find_function_set_by_name(
        &self,
        name: &str,
    ) -> Result<Option<FunctionSet>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(self.find_by_name_cache_key(name));
        self.with_cache_if_some(&k, || async {
            self.function_set_repository().find_by_name(name).await
        })
        .await
    }

    async fn find_function_set_list(
        &self,
        limit: Option<&i32>,
        offset: Option<&i64>,
        _ttl: Option<&Duration>,
    ) -> Result<Vec<FunctionSet>>
    where
        Self: Send + 'static,
    {
        // TODO list cache
        self.function_set_repository()
            .find_list(limit, offset)
            .await
    }

    async fn find_function_set_all_list(&self, _ttl: Option<&Duration>) -> Result<Vec<FunctionSet>>
    where
        Self: Send + 'static,
    {
        // TODO list cache
        self.function_set_repository().find_list(None, None).await
    }

    async fn count(&self) -> Result<i64>
    where
        Self: Send + 'static,
    {
        // TODO cache
        self.function_set_repository()
            .count_list_tx(self.function_set_repository().db_pool())
            .await
    }

    async fn find_functions_by_set(&self, set_name: &str) -> Result<Vec<FunctionSpecs>> {
        // Get function set by name
        let function_set = self.find_function_set_by_name(set_name).await?;

        if let Some(set) = function_set {
            if let Some(data) = set.data {
                let mut functions = Vec::new();

                // Process each target in the function set
                for target in &data.targets {
                    if let Some(id) = &target.id {
                        match id {
                            // Runner type target
                            proto::jobworkerp::function::data::function_id::Id::RunnerId(
                                runner_id,
                            ) => {
                                // Find runner
                                if let Some(runner) =
                                    self.runner_app().find_runner(runner_id).await?
                                {
                                    functions.push(Self::convert_runner_to_function_specs(runner));
                                } else {
                                    tracing::warn!(
                                        "Runner not found for id: {} in FunctionSet: {}. Skipping.",
                                        runner_id.value,
                                        set_name
                                    );
                                }
                            }
                            // Worker type target
                            proto::jobworkerp::function::data::function_id::Id::WorkerId(
                                worker_id,
                            ) => {
                                // Find worker
                                if let Some(worker) = self.worker_app().find(worker_id).await? {
                                    if let Some(wid) = worker.id {
                                        if let Some(worker_data) = worker.data {
                                            if let Some(rid) = worker_data.runner_id {
                                                if let Some(runner) =
                                                    self.runner_app().find_runner(&rid).await?
                                                {
                                                    if let Ok(specs) =
                                                        Self::convert_worker_to_function_specs(
                                                            wid,
                                                            worker_data,
                                                            runner,
                                                        )
                                                    {
                                                        functions.push(specs);
                                                    } else {
                                                        tracing::warn!(
                                                        "Failed to convert worker to function specs for worker_id: {} in FunctionSet: {}",
                                                        worker_id.value,
                                                        set_name
                                                    );
                                                    }
                                                } else {
                                                    tracing::warn!(
                                                    "Runner not found for worker_id: {} in FunctionSet: {}. Skipping.",
                                                    worker_id.value,
                                                    set_name
                                                );
                                                }
                                            }
                                        }
                                    }
                                } else {
                                    tracing::warn!(
                                        "Worker not found for id: {} in FunctionSet: {}. Skipping.",
                                        worker_id.value,
                                        set_name
                                    );
                                }
                            }
                        }
                    } else {
                        tracing::warn!(
                            "FunctionId has no id set in FunctionSet: {}. Skipping this target.",
                            set_name
                        );
                        continue;
                    }
                }

                Ok(functions)
            } else {
                Ok(Vec::new())
            }
        } else {
            Ok(Vec::new())
        }
    }

}

#[derive(Debug)]
pub struct FunctionSetAppImpl {
    function_set_repository: Arc<FunctionSetRepositoryImpl>,
    memory_cache: MokaCacheImpl<Arc<String>, FunctionSet>,
    runner_app: Arc<dyn RunnerApp + 'static>,
    worker_app: Arc<dyn WorkerApp + 'static>,
}

impl FunctionSetAppImpl {
    pub fn new(
        function_set_repository: Arc<FunctionSetRepositoryImpl>,
        mc_config: &memory_utils::cache::moka::MokaCacheConfig,
        runner_app: Arc<dyn RunnerApp + 'static>,
        worker_app: Arc<dyn WorkerApp + 'static>,
    ) -> Self {
        let memory_cache = MokaCacheImpl::new(mc_config);
        Self {
            function_set_repository,
            memory_cache,
            runner_app,
            worker_app,
        }
    }
}

impl UseFunctionSetRepository for FunctionSetAppImpl {
    fn function_set_repository(&self) -> &FunctionSetRepositoryImpl {
        &self.function_set_repository
    }
}

impl UseRunnerApp for FunctionSetAppImpl {
    fn runner_app(&self) -> Arc<dyn RunnerApp + 'static> {
        self.runner_app.clone()
    }
}

impl UseWorkerApp for FunctionSetAppImpl {
    fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static> {
        &self.worker_app
    }
}

impl FunctionSpecConverter for FunctionSetAppImpl {}

impl FunctionSetApp for FunctionSetAppImpl {}

impl UseMokaCache<Arc<String>, FunctionSet> for FunctionSetAppImpl {
    fn cache(&self) -> &MokaCache<Arc<String>, FunctionSet> {
        self.memory_cache.cache()
    }
}

pub trait UseFunctionSetApp {
    fn function_set_app(&self) -> &FunctionSetAppImpl;
}
