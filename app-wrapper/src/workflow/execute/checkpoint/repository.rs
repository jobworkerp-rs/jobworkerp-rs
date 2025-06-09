use crate::workflow::execute::checkpoint::CheckPointContext;
use async_trait::async_trait;

pub mod memory;
pub mod redis;

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
