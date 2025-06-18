use crate::workflow::{
    execute::checkpoint::repository::{CheckPointRepositoryWithId, CheckPointRepositoryWithIdImpl},
    WorkflowConfig,
};
use infra_utils::infra::{cache::MokaCacheConfig, redis::RedisPool};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct AppWrapperConfigModule {
    pub workflow_config: Arc<WorkflowConfig>,
}
impl AppWrapperConfigModule {
    pub fn new_by_env() -> Self {
        let workflow_config = Arc::new(WorkflowConfig::new_by_envy());
        Self { workflow_config }
    }
    pub fn new(workflow_config: Arc<WorkflowConfig>) -> Self {
        Self { workflow_config }
    }
}
#[derive(Clone, Debug)]
pub struct AppWrapperRepositoryModule {
    pub memory_checkpoint_repository: Arc<
        dyn crate::workflow::execute::checkpoint::repository::CheckPointRepositoryWithId
            + Send
            + Sync
            + 'static,
    >,
    pub redis_checkpoint_repository: Option<
        Arc<
            dyn crate::workflow::execute::checkpoint::repository::CheckPointRepositoryWithId
                + 'static,
        >,
    >,
}
impl AppWrapperRepositoryModule {
    const DEFAULT_MEMORY_CHECKPOINT_MAX_COUNT: u32 = 10000;
    pub fn new(
        config_module: Arc<AppWrapperConfigModule>,
        redis_pool: Option<&'static RedisPool>,
    ) -> Self {
        let workflow_config = config_module.workflow_config.clone();
        let redis_checkpoint_repository = redis_pool.map(|rp| {
            Arc::new(CheckPointRepositoryWithIdImpl::new_redis(
                rp,
                workflow_config
                    .checkpoint_expire_sec
                    .map(|seconds| seconds as usize), // Convert seconds to milliseconds
            )) as Arc<dyn CheckPointRepositoryWithId>
        });

        let memory_checkpoint_repository = Arc::new(CheckPointRepositoryWithIdImpl::new_memory(
            &MokaCacheConfig {
                num_counters: workflow_config
                    .checkpoint_max_count
                    .unwrap_or(Self::DEFAULT_MEMORY_CHECKPOINT_MAX_COUNT)
                    as usize,
                ttl: workflow_config
                    .checkpoint_expire_sec
                    .map(std::time::Duration::from_secs), // Convert seconds to milliseconds
            },
        )) as Arc<dyn CheckPointRepositoryWithId>;
        Self {
            memory_checkpoint_repository,
            redis_checkpoint_repository,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AppWrapperModule {
    pub config_module: Arc<AppWrapperConfigModule>,
    pub repositories: Arc<AppWrapperRepositoryModule>,
}
impl AppWrapperModule {
    pub fn new_by_env(redis_pool: Option<&'static RedisPool>) -> Self {
        let config_module = Arc::new(AppWrapperConfigModule::new_by_env());
        let repositories = Arc::new(AppWrapperRepositoryModule::new(
            config_module.clone(),
            redis_pool,
        ));
        Self {
            config_module,
            repositories,
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub mod test {
    use app::module::AppModule;

    use super::*;

    pub fn create_test_app_wrapper_module(app_module: Arc<AppModule>) -> AppWrapperModule {
        let workflow_config = Arc::new(WorkflowConfig::new(
            Some(120),                           // task_default_timeout
            Some("test-user-agent".to_string()), // http_user_agent
            Some(60),                            // http_timeout_sec
            Some(120),                           // checkpoint_expire_sec
            Some(1000),                          // checkpoint_max_count
        ));
        let config_module = Arc::new(AppWrapperConfigModule::new(workflow_config));
        let repositories = Arc::new(AppWrapperRepositoryModule::new(
            config_module.clone(),
            app_module
                .repositories
                .redis_module
                .as_ref()
                .map(|r| r.redis_pool),
        ));
        AppWrapperModule {
            config_module,
            repositories,
        }
    }
}
