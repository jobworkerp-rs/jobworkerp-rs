use std::sync::Arc;

use crate::workflow::execute::{
    checkpoint::{repository::memory::MemoryCheckPointRepositoryImpl, CheckPointContext},
    context::WorkflowPosition,
    task::ExecutionId,
};
use async_trait::async_trait;
use infra_utils::infra::redis::RedisPool;
use memory_utils::cache::moka::MokaCacheConfig;

pub mod memory;
pub mod redis;

#[async_trait]
pub trait CheckPointRepositoryWithId:
    UseCheckPointRepository + std::fmt::Debug + Sync + 'static
where
    Self: Send + 'static,
{
    async fn save_checkpoint_with_id(
        &self,
        execution_id: &ExecutionId,
        workflow_name: &str,
        checkpoint: &CheckPointContext,
    ) -> anyhow::Result<()> {
        let key = self.generate_key(
            execution_id,
            workflow_name,
            &checkpoint.position.as_json_pointer(),
        );
        self.checkpoint_repository()
            .save_checkpoint(&key, checkpoint)
            .await
    }

    async fn get_checkpoint_with_id(
        &self,
        execution_id: &ExecutionId,
        workflow_name: &str,
        position: &str,
    ) -> anyhow::Result<Option<CheckPointContext>> {
        let key = self.generate_key(execution_id, workflow_name, position);
        self.checkpoint_repository().get_checkpoint(&key).await
    }

    async fn delete_checkpoint_with_id(
        &self,
        execution_id: &ExecutionId,
        workflow_name: &str,
        position: &str,
    ) -> anyhow::Result<()> {
        let key = self.generate_key(execution_id, workflow_name, position);
        self.checkpoint_repository().delete_checkpoint(&key).await
    }

    // generate a unique key for the checkpoint
    fn generate_key(
        &self,
        execution_id: &ExecutionId,
        workflow_name: &str,
        position: &str,
    ) -> String {
        format!("{}:{}:{}", workflow_name, execution_id.value, position)
    }
}

#[async_trait]
pub trait CheckPointRepository: Sync + 'static
where
    Self: Send + 'static,
{
    async fn save_checkpoint(
        &self,
        key: &str,
        checkpoint: &CheckPointContext,
    ) -> anyhow::Result<()>;

    async fn get_checkpoint(&self, key: &str) -> anyhow::Result<Option<CheckPointContext>>;

    async fn delete_checkpoint(&self, key: &str) -> anyhow::Result<()>;
}

impl std::fmt::Debug for dyn CheckPointRepository + Sync + 'static {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CheckPointRepository")
            .finish_non_exhaustive()
    }
}

pub trait UseCheckPointRepository: Sync + 'static {
    fn checkpoint_repository(&self) -> &dyn CheckPointRepository;
}

#[derive(Clone, Debug)]
pub struct CheckPointRepositoryWithIdImpl {
    repository: Arc<dyn CheckPointRepository + Sync + 'static>,
}
impl CheckPointRepositoryWithIdImpl {
    pub fn new_memory(config: &MokaCacheConfig) -> Self {
        Self {
            repository: Arc::new(MemoryCheckPointRepositoryImpl::new(config)),
        }
    }
    pub fn new_redis(redis_pool: &'static RedisPool, expire_sec: Option<usize>) -> Self {
        Self {
            repository: Arc::new(redis::RedisCheckPointRepositoryImpl::new(
                redis_pool, expire_sec,
            )),
        }
    }
}

impl UseCheckPointRepository for CheckPointRepositoryWithIdImpl {
    fn checkpoint_repository(&self) -> &dyn CheckPointRepository {
        self.repository.as_ref()
    }
}
impl CheckPointRepositoryWithId for CheckPointRepositoryWithIdImpl {}

pub struct CheckPointId {
    pub value: String,
}

impl CheckPointId {
    pub fn new(workflow_id: &str, position: &WorkflowPosition) -> Self {
        Self {
            value: format!("{}:{}", workflow_id, position.as_json_pointer()),
        }
    }
}
impl std::str::FromStr for CheckPointId {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self {
            value: value.to_string(),
        })
    }
}
