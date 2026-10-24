use crate::app::function::{FunctionApp, UseFunctionApp};
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
use super::FunctionAppImpl;

#[async_trait]
pub trait FunctionSetApp: // XXX 1 impl
    UseFunctionSetRepository
    + UseMokaCache<Arc<String>, FunctionSet>
    + UseFunctionApp
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
            // ID and name lookups cache targets; renames must retire the old name too.
            self.clear().await;
            Ok(true)
        } else {
            // all empty, no update
            Ok(false)
        }
    }

    async fn delete_function_set(&self, id: &FunctionSetId) -> Result<bool> {
        let deleted = self.function_set_repository().delete(id).await?;
        self.clear().await;
        Ok(deleted)
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
        let function_set = self.find_function_set_by_name(set_name).await?;

        if let Some(set) = function_set {
            if let Some(data) = set.data {
                self.function_app()
                    .convert_function_usings_to_specs(&data.targets, set_name)
                    .await
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
    function_app: Arc<FunctionAppImpl>,
}

impl FunctionSetAppImpl {
    pub fn new(
        function_set_repository: Arc<FunctionSetRepositoryImpl>,
        mc_config: &memory_utils::cache::moka::MokaCacheConfig,
        function_app: Arc<FunctionAppImpl>,
    ) -> Self {
        let memory_cache = MokaCacheImpl::new(mc_config);
        Self {
            function_set_repository,
            memory_cache,
            function_app,
        }
    }
}

impl UseFunctionSetRepository for FunctionSetAppImpl {
    fn function_set_repository(&self) -> &FunctionSetRepositoryImpl {
        &self.function_set_repository
    }
}

impl UseFunctionApp for FunctionSetAppImpl {
    fn function_app(&self) -> &FunctionAppImpl {
        &self.function_app
    }
}

impl FunctionSetApp for FunctionSetAppImpl {}

impl UseMokaCache<Arc<String>, FunctionSet> for FunctionSetAppImpl {
    fn cache(&self) -> &MokaCache<Arc<String>, FunctionSet> {
        self.memory_cache.cache()
    }
}

pub trait UseFunctionSetApp {
    fn function_set_app(&self) -> &FunctionSetAppImpl;
}

#[cfg(test)]
mod cache_invalidation_tests {
    use super::*;
    use infra_utils::infra::test::TEST_RUNTIME;
    use proto::jobworkerp::data::RunnerId;
    use proto::jobworkerp::function::data::{FunctionId, FunctionUsing, function_id};

    async fn setup() -> FunctionSetAppImpl {
        let module = crate::module::test::create_rdb_chan_test_app(false, false)
            .await
            .unwrap();
        FunctionSetAppImpl::new(
            Arc::new(module.function_set_app.function_set_repository().clone()),
            &memory_utils::cache::moka::MokaCacheConfig {
                num_counters: 1000,
                ttl: None,
            },
            module.function_app.clone(),
        )
    }

    fn data(name: &str, runner: i64) -> FunctionSetData {
        FunctionSetData {
            name: name.into(),
            targets: vec![FunctionUsing {
                function_id: Some(FunctionId {
                    id: Some(function_id::Id::RunnerId(RunnerId { value: runner })),
                }),
                using: None,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn update_refreshes_id_name_and_resolved_tools() {
        TEST_RUNTIME.block_on(async {
            let app = setup().await;
            let original = data("cached-tools", 1);
            let id = app.create_function_set(&original).await.unwrap();
            assert_eq!(
                app.find_function_set(&id).await.unwrap().unwrap().data,
                Some(original.clone())
            );
            assert_eq!(
                app.find_function_set_by_name(&original.name)
                    .await
                    .unwrap()
                    .unwrap()
                    .data,
                Some(original.clone())
            );
            assert_eq!(
                app.find_functions_by_set(&original.name).await.unwrap()[0].runner_id,
                Some(RunnerId { value: 1 })
            );
            let updated = data(&original.name, 2);
            assert!(
                app.update_function_set(&id, &Some(updated.clone()))
                    .await
                    .unwrap()
            );
            assert_eq!(
                app.find_function_set(&id).await.unwrap().unwrap().data,
                Some(updated.clone())
            );
            assert_eq!(
                app.find_function_set_by_name(&original.name)
                    .await
                    .unwrap()
                    .unwrap()
                    .data,
                Some(updated)
            );
            assert_eq!(
                app.find_functions_by_set(&original.name).await.unwrap()[0].runner_id,
                Some(RunnerId { value: 2 })
            );
        });
    }

    #[test]
    fn rename_retires_old_name_and_refreshes_new_name_and_id() {
        TEST_RUNTIME.block_on(async {
            let app = setup().await;
            let original = data("old-tools", 1);
            let id = app.create_function_set(&original).await.unwrap();
            assert!(app.find_function_set(&id).await.unwrap().is_some());
            assert!(
                app.find_function_set_by_name(&original.name)
                    .await
                    .unwrap()
                    .is_some()
            );
            assert!(
                app.find_function_set_by_name("new-tools")
                    .await
                    .unwrap()
                    .is_none()
            );
            let renamed = data("new-tools", 2);
            app.update_function_set(&id, &Some(renamed.clone()))
                .await
                .unwrap();
            assert!(
                app.find_function_set_by_name(&original.name)
                    .await
                    .unwrap()
                    .is_none()
            );
            let found = app
                .find_function_set_by_name(&renamed.name)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(found.id, Some(id));
            assert_eq!(found.data, Some(renamed.clone()));
            assert_eq!(
                app.find_function_set(&id).await.unwrap().unwrap().data,
                Some(renamed)
            );
            assert!(
                app.find_functions_by_set(&original.name)
                    .await
                    .unwrap()
                    .is_empty()
            );
        });
    }

    #[test]
    fn deletion_retires_cached_tools_and_same_name_recreation_uses_new_targets() {
        TEST_RUNTIME.block_on(async {
            let app = setup().await;
            let original = data("recreated-tools", 1);
            let id = app.create_function_set(&original).await.unwrap();
            assert!(app.find_function_set(&id).await.unwrap().is_some());
            assert!(
                app.find_function_set_by_name(&original.name)
                    .await
                    .unwrap()
                    .is_some()
            );
            assert_eq!(
                app.find_functions_by_set(&original.name)
                    .await
                    .unwrap()
                    .len(),
                1
            );
            assert!(app.delete_function_set(&id).await.unwrap());
            assert!(app.find_function_set(&id).await.unwrap().is_none());
            assert!(
                app.find_function_set_by_name(&original.name)
                    .await
                    .unwrap()
                    .is_none()
            );
            assert!(
                app.find_functions_by_set(&original.name)
                    .await
                    .unwrap()
                    .is_empty()
            );
            let replacement = data(&original.name, 2);
            let new_id = app.create_function_set(&replacement).await.unwrap();
            assert_ne!(new_id, id);
            let found = app
                .find_function_set_by_name(&original.name)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(found.id, Some(new_id));
            assert_eq!(found.data, Some(replacement));
            assert_eq!(
                app.find_functions_by_set(&original.name).await.unwrap()[0].runner_id,
                Some(RunnerId { value: 2 })
            );
        });
    }

    #[test]
    fn rejected_and_empty_updates_preserve_the_existing_set() {
        TEST_RUNTIME.block_on(async {
            let app = setup().await;
            let original = data("original-tools", 1);
            let id = app.create_function_set(&original).await.unwrap();
            app.create_function_set(&data("occupied-tools", 2))
                .await
                .unwrap();
            assert!(app.find_function_set(&id).await.unwrap().is_some());
            assert!(
                app.find_function_set_by_name(&original.name)
                    .await
                    .unwrap()
                    .is_some()
            );
            assert!(
                app.update_function_set(&id, &Some(data("occupied-tools", 2)))
                    .await
                    .is_err()
            );
            assert!(!app.update_function_set(&id, &None).await.unwrap());
            assert_eq!(
                app.find_function_set(&id).await.unwrap().unwrap().data,
                Some(original.clone())
            );
            assert_eq!(
                app.find_function_set_by_name(&original.name)
                    .await
                    .unwrap()
                    .unwrap()
                    .data,
                Some(original)
            );
        });
    }
}
