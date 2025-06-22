use crate::workflow::execute::checkpoint::{repository::CheckPointRepository, CheckPointContext};
use anyhow::Result;
use async_trait::async_trait;
use deadpool_redis::redis::AsyncCommands;
use debug_stub_derive::DebugStub;
use infra_utils::infra::redis::{RedisPool, UseRedisPool};
use jobworkerp_base::codec::UseProstCodec;

pub trait RedisCheckPointRepository:
    CheckPointRepository + UseRedisPool + UseProstCodec + Sync + 'static
where
    Self: Send + 'static,
{
    const CHECKPOINT_KEY_PREFIX: &'static str = "checkpoint:";

    fn expire_sec(&self) -> Option<usize>;

    fn save_checkpoint_redis(
        &self,
        key: &str,
        checkpoint: &CheckPointContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        async move {
            let redis_key = format!("{}{}", Self::CHECKPOINT_KEY_PREFIX, key);
            let serialized = serde_json::to_vec(checkpoint)?;

            let mut conn = self.redis_pool().get().await?;

            if let Some(expire) = self.expire_sec() {
                conn.set_ex::<String, Vec<u8>, ()>(redis_key, serialized, expire as u64)
                    .await
                    .map_err(|e| anyhow::anyhow!("Redis set_ex error: {}", e))?;
            } else {
                conn.set::<String, Vec<u8>, ()>(redis_key, serialized)
                    .await
                    .map_err(|e| anyhow::anyhow!("Redis set error: {}", e))?;
            }

            Ok(())
        }
    }
    fn get_checkpoint_redis(
        &self,
        key: &str,
    ) -> impl std::future::Future<Output = Result<Option<CheckPointContext>>> + Send {
        async move {
            let redis_key = format!("{}{}", Self::CHECKPOINT_KEY_PREFIX, key);
            let mut conn = self.redis_pool().get().await?;

            let result: Option<Vec<u8>> = conn
                .get(redis_key)
                .await
                .map_err(|e| anyhow::anyhow!("Redis get error: {}", e))?;

            match result {
                Some(data) => {
                    let checkpoint = serde_json::from_slice(&data)?;
                    Ok(Some(checkpoint))
                }
                None => Ok(None),
            }
        }
    }

    fn delete_checkpoint_redis(
        &self,
        key: &str,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        async move {
            let redis_key = format!("{}{}", Self::CHECKPOINT_KEY_PREFIX, key);
            let mut conn = self.redis_pool().get().await?;

            conn.del::<String, ()>(redis_key)
                .await
                .map_err(|e| anyhow::anyhow!("Redis del error: {}", e))?;

            Ok(())
        }
    }
}

#[derive(Clone, DebugStub)]
pub struct RedisCheckPointRepositoryImpl {
    #[debug_stub = "&'static RedisPool"]
    pub redis_pool: &'static RedisPool,
    pub expire_sec: Option<usize>,
}

impl RedisCheckPointRepositoryImpl {
    pub fn new(redis_pool: &'static RedisPool, expire_sec: Option<usize>) -> Self {
        Self {
            redis_pool,
            expire_sec,
        }
    }
}

impl UseRedisPool for RedisCheckPointRepositoryImpl {
    fn redis_pool(&self) -> &'static RedisPool {
        self.redis_pool
    }
}

impl UseProstCodec for RedisCheckPointRepositoryImpl {}

#[async_trait]
impl CheckPointRepository for RedisCheckPointRepositoryImpl {
    async fn save_checkpoint(&self, key: &str, checkpoint: &CheckPointContext) -> Result<()> {
        self.save_checkpoint_redis(key, checkpoint).await
    }

    async fn get_checkpoint(&self, key: &str) -> Result<Option<CheckPointContext>> {
        self.get_checkpoint_redis(key).await
    }

    async fn delete_checkpoint(&self, key: &str) -> Result<()> {
        self.delete_checkpoint_redis(key).await
    }
}

impl RedisCheckPointRepository for RedisCheckPointRepositoryImpl {
    fn expire_sec(&self) -> Option<usize> {
        self.expire_sec
    }
}

pub trait UseRedisCheckPointRepository {
    fn redis_checkpoint_repository(&self) -> &RedisCheckPointRepositoryImpl;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::execute::{
        checkpoint::{TaskCheckPointContext, WorkflowCheckPointContext},
        context::WorkflowPosition,
    };
    use command_utils::util::stack::StackWithHistory;
    use std::sync::Arc;
    use uuid::Uuid;

    #[tokio::test]
    async fn test_redis_checkpoint_create_get_delete() -> Result<()> {
        let pool = infra_utils::infra::test::setup_test_redis_pool().await;
        let repo = RedisCheckPointRepositoryImpl::new(pool, Some(300)); // 5 minutes expiration

        let checkpoint = create_test_checkpoint_context();
        let key = "test_checkpoint_create_get_delete";

        // Test: Create checkpoint
        repo.save_checkpoint(key, &checkpoint).await?;

        // Test: Get checkpoint and verify data integrity
        let retrieved = repo.get_checkpoint(key).await?;
        assert!(retrieved.is_some());
        let retrieved_checkpoint = retrieved.unwrap();

        // Verify workflow context
        assert_eq!(checkpoint.workflow.name, retrieved_checkpoint.workflow.name);

        // Verify task context
        assert_eq!(
            checkpoint.task.flow_directive,
            retrieved_checkpoint.task.flow_directive
        );

        // Verify position
        assert_eq!(
            checkpoint.position.path.len(),
            retrieved_checkpoint.position.path.len()
        );

        // Test: Delete checkpoint
        repo.delete_checkpoint(key).await?;

        // Verify checkpoint is deleted
        let deleted_result = repo.get_checkpoint(key).await?;
        assert!(deleted_result.is_none());

        Ok(())
    }

    // #[tokio::test]
    // async fn test_redis_checkpoint_expiration() -> Result<()> {
    //     let pool = infra_utils::infra::test::setup_test_redis_pool().await;
    //     let repo = RedisCheckPointRepositoryImpl::new(pool, Some(1)); // 1 second expiration

    //     let checkpoint = create_test_checkpoint_context();
    //     let key = "test_checkpoint_expiration";

    //     // Save checkpoint
    //     repo.save_checkpoint(key, &checkpoint).await?;

    //     // Verify checkpoint exists immediately
    //     let result = repo.get_checkpoint(key).await?;
    //     assert!(result.is_some());

    //     // Wait for expiration (2 seconds to be safe)
    //     tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    //     // Verify checkpoint has expired
    //     let expired_result = repo.get_checkpoint(key).await?;
    //     assert!(expired_result.is_none());

    //     Ok(())
    // }

    #[tokio::test]
    async fn test_redis_checkpoint_nonexistent_key() -> Result<()> {
        let pool = infra_utils::infra::test::setup_test_redis_pool().await;
        let repo = RedisCheckPointRepositoryImpl::new(pool, None);

        // Try to get non-existent checkpoint
        let result = repo.get_checkpoint("nonexistent_checkpoint_key").await?;
        assert!(result.is_none());

        // Try to delete non-existent checkpoint (should not error)
        repo.delete_checkpoint("nonexistent_checkpoint_key").await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_redis_checkpoint_update_existing() -> Result<()> {
        let pool = infra_utils::infra::test::setup_test_redis_pool().await;
        let repo = RedisCheckPointRepositoryImpl::new(pool, Some(300));

        let key = "test_checkpoint_update";

        // Create and save first checkpoint
        let checkpoint1 = create_test_checkpoint_context();
        repo.save_checkpoint(key, &checkpoint1).await?;

        // Create second checkpoint with different workflow ID
        let mut checkpoint2 = create_test_checkpoint_context();
        checkpoint2.workflow.name = Uuid::new_v4().to_string();
        repo.save_checkpoint(key, &checkpoint2).await?;

        // Verify the checkpoint was updated (overwritten)
        let retrieved = repo.get_checkpoint(key).await?;
        assert!(retrieved.is_some());
        let retrieved_checkpoint = retrieved.unwrap();
        assert_eq!(
            checkpoint2.workflow.name,
            retrieved_checkpoint.workflow.name
        );
        assert_ne!(
            checkpoint1.workflow.name,
            retrieved_checkpoint.workflow.name
        );

        // Cleanup
        repo.delete_checkpoint(key).await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_redis_checkpoint_multiple_keys() -> Result<()> {
        let pool = infra_utils::infra::test::setup_test_redis_pool().await;
        let repo = RedisCheckPointRepositoryImpl::new(pool, Some(300));

        let checkpoint1 = create_test_checkpoint_context();
        let checkpoint2 = create_test_checkpoint_context();

        let key1 = "test_checkpoint_multi_1";
        let key2 = "test_checkpoint_multi_2";

        // Save multiple checkpoints
        repo.save_checkpoint(key1, &checkpoint1).await?;
        repo.save_checkpoint(key2, &checkpoint2).await?;

        // Verify both checkpoints exist independently
        let retrieved1 = repo.get_checkpoint(key1).await?;
        let retrieved2 = repo.get_checkpoint(key2).await?;

        assert!(retrieved1.is_some());
        assert!(retrieved2.is_some());

        // Delete one checkpoint
        repo.delete_checkpoint(key1).await?;

        // Verify only one is deleted
        let deleted_result1 = repo.get_checkpoint(key1).await?;
        let still_exists_result2 = repo.get_checkpoint(key2).await?;

        assert!(deleted_result1.is_none());
        assert!(still_exists_result2.is_some());

        // Cleanup remaining checkpoint
        repo.delete_checkpoint(key2).await?;

        Ok(())
    }

    fn create_test_checkpoint_context() -> CheckPointContext {
        let workflow_name = Uuid::new_v4();
        let input = Arc::new(serde_json::json!({"test": "input"}));
        let context_variables = Arc::new(
            [("var1".to_string(), serde_json::json!("value1"))]
                .iter()
                .cloned()
                .collect::<serde_json::Map<String, serde_json::Value>>(),
        );

        // Create WorkflowCheckPointContext directly with minimal fields
        let workflow_checkpoint = WorkflowCheckPointContext {
            name: workflow_name.to_string(),
            input: input.clone(),
            context_variables: context_variables.clone(),
        };

        // Create TaskCheckPointContext directly
        let task_checkpoint = TaskCheckPointContext {
            input: input.clone(),
            output: Arc::new(serde_json::json!({"processed": "output"})),
            context_variables: context_variables.clone(),
            flow_directive: "exit".to_string(),
        };

        // Create WorkflowPosition directly
        let position = WorkflowPosition {
            path: StackWithHistory::new(),
        };

        CheckPointContext {
            workflow: workflow_checkpoint,
            task: task_checkpoint,
            position,
        }
    }
}
