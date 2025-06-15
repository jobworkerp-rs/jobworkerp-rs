use anyhow::Result;
use async_trait::async_trait;
use debug_stub_derive::DebugStub;
use infra_utils::infra::cache::{MokaCacheConfig, MokaCacheImpl, UseMokaCache};

use crate::workflow::execute::checkpoint::{repository::CheckPointRepository, CheckPointContext};

pub trait MemoryCheckPointRepository:
    CheckPointRepository + UseMokaCache<String, CheckPointContext> + Sync + 'static
where
    Self: Send + 'static,
{
    fn save_checkpoint_memory(
        &self,
        key: &str,
        checkpoint: &CheckPointContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        async {
            self.set_cache(key.to_string(), checkpoint.clone()).await;
            Ok(())
        }
    }

    fn get_checkpoint_memory(
        &self,
        key: &str,
    ) -> impl std::future::Future<Output = Result<Option<CheckPointContext>>> + Send {
        async {
            let result = self.find_cache(&key.to_string()).await;
            Ok(result)
        }
    }

    fn delete_checkpoint_memory(
        &self,
        key: &str,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        async {
            self.delete_cache(&key.to_string()).await;
            Ok(())
        }
    }
}

#[derive(Clone, DebugStub)]
pub struct MemoryCheckPointRepositoryImpl {
    #[debug_stub = "MokaCacheImpl<String, CheckPointContext>"]
    pub cache: MokaCacheImpl<String, CheckPointContext>,
}

impl MemoryCheckPointRepositoryImpl {
    pub fn new(config: &MokaCacheConfig) -> Self {
        Self {
            cache: MokaCacheImpl::new(config),
        }
    }

    pub fn new_default() -> Self {
        Self::new(&MokaCacheConfig::default())
    }
}

impl UseMokaCache<String, CheckPointContext> for MemoryCheckPointRepositoryImpl {
    fn cache(&self) -> &infra_utils::infra::cache::MokaCache<String, CheckPointContext> {
        self.cache.cache()
    }
}

#[async_trait]
impl CheckPointRepository for MemoryCheckPointRepositoryImpl {
    async fn save_checkpoint(&self, key: &str, checkpoint: &CheckPointContext) -> Result<()> {
        self.save_checkpoint_memory(key, checkpoint).await
    }

    async fn get_checkpoint(&self, key: &str) -> Result<Option<CheckPointContext>> {
        self.get_checkpoint_memory(key).await
    }

    async fn delete_checkpoint(&self, key: &str) -> Result<()> {
        self.delete_checkpoint_memory(key).await
    }
}

impl MemoryCheckPointRepository for MemoryCheckPointRepositoryImpl {}

pub trait UseMemoryCheckPointRepository {
    fn memory_checkpoint_repository(&self) -> &MemoryCheckPointRepositoryImpl;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::{
        definition::workflow::{Input, WorkflowDsl, WorkflowSchema},
        execute::{
            checkpoint::{TaskCheckPointContext, WorkflowCheckPointContext},
            context::{Then, WorkflowPosition},
        },
    };
    use command_utils::util::stack::StackWithHistory;
    use std::{str::FromStr, sync::Arc, time::Duration};
    use uuid::Uuid;

    #[tokio::test]
    async fn test_memory_checkpoint_create_get_delete() -> Result<()> {
        let config = MokaCacheConfig {
            num_counters: 10000,
            ttl: Some(Duration::from_secs(300)), // 5 minutes
        };
        let repo = MemoryCheckPointRepositoryImpl::new(&config);

        let checkpoint = create_test_checkpoint_context();
        let key = "test_checkpoint_create_get_delete";

        // Test: Create checkpoint
        repo.save_checkpoint(key, &checkpoint).await?;

        // Test: Get checkpoint and verify data integrity
        let retrieved = repo.get_checkpoint(key).await?;
        assert!(retrieved.is_some());
        let retrieved_checkpoint = retrieved.unwrap();

        // Verify workflow context
        assert_eq!(checkpoint.workflow.id, retrieved_checkpoint.workflow.id);

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
    // async fn test_memory_checkpoint_expiration() -> Result<()> {
    //     let config = MokaCacheConfig {
    //         num_counters: 10000,
    //         ttl: Some(Duration::from_millis(200)), // 200ms expiration
    //     };
    //     let repo = MemoryCheckPointRepositoryImpl::new(&config);

    //     let checkpoint = create_test_checkpoint_context();
    //     let key = "test_checkpoint_expiration";

    //     // Save checkpoint
    //     repo.save_checkpoint(key, &checkpoint).await?;

    //     // Verify checkpoint exists immediately
    //     let result = repo.get_checkpoint(key).await?;
    //     assert!(result.is_some());

    //     // Wait for expiration (400ms to be safe)
    //     tokio::time::sleep(Duration::from_millis(400)).await;

    //     // Verify checkpoint has expired
    //     let expired_result = repo.get_checkpoint(key).await?;
    //     assert!(expired_result.is_none());

    //     Ok(())
    // }

    #[tokio::test]
    async fn test_memory_checkpoint_nonexistent_key() -> Result<()> {
        let repo = MemoryCheckPointRepositoryImpl::new_default();

        // Try to get non-existent checkpoint
        let result = repo.get_checkpoint("nonexistent_checkpoint_key").await?;
        assert!(result.is_none());

        // Try to delete non-existent checkpoint (should not error)
        repo.delete_checkpoint("nonexistent_checkpoint_key").await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_memory_checkpoint_update_existing() -> Result<()> {
        let config = MokaCacheConfig {
            num_counters: 10000,
            ttl: Some(Duration::from_secs(300)),
        };
        let repo = MemoryCheckPointRepositoryImpl::new(&config);

        let key = "test_checkpoint_update";

        // Create and save first checkpoint
        let checkpoint1 = create_test_checkpoint_context();
        repo.save_checkpoint(key, &checkpoint1).await?;

        // Create second checkpoint with different workflow ID
        let mut checkpoint2 = create_test_checkpoint_context();
        checkpoint2.workflow.id = Uuid::new_v4();
        repo.save_checkpoint(key, &checkpoint2).await?;

        // Verify the checkpoint was updated (overwritten)
        let retrieved = repo.get_checkpoint(key).await?;
        assert!(retrieved.is_some());
        let retrieved_checkpoint = retrieved.unwrap();
        assert_eq!(checkpoint2.workflow.id, retrieved_checkpoint.workflow.id);
        assert_ne!(checkpoint1.workflow.id, retrieved_checkpoint.workflow.id);

        // Cleanup
        repo.delete_checkpoint(key).await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_memory_checkpoint_multiple_keys() -> Result<()> {
        let config = MokaCacheConfig {
            num_counters: 10000,
            ttl: Some(Duration::from_secs(300)),
        };
        let repo = MemoryCheckPointRepositoryImpl::new(&config);

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

    #[tokio::test]
    async fn test_memory_checkpoint_clear_all() -> Result<()> {
        let config = MokaCacheConfig {
            num_counters: 10000,
            ttl: Some(Duration::from_secs(300)),
        };
        let repo = MemoryCheckPointRepositoryImpl::new(&config);

        let checkpoint1 = create_test_checkpoint_context();
        let checkpoint2 = create_test_checkpoint_context();

        let key1 = "test_checkpoint_clear_1";
        let key2 = "test_checkpoint_clear_2";

        // Save multiple checkpoints
        repo.save_checkpoint(key1, &checkpoint1).await?;
        repo.save_checkpoint(key2, &checkpoint2).await?;

        // Verify both checkpoints exist
        assert!(repo.get_checkpoint(key1).await?.is_some());
        assert!(repo.get_checkpoint(key2).await?.is_some());

        // Clear all checkpoints
        repo.clear().await;

        // Verify all checkpoints are cleared
        assert!(repo.get_checkpoint(key1).await?.is_none());
        assert!(repo.get_checkpoint(key2).await?.is_none());

        Ok(())
    }

    fn create_test_checkpoint_context() -> CheckPointContext {
        let workflow_id = Uuid::new_v4();
        let input = Arc::new(serde_json::json!({"test": "input"}));
        let context_variables = Arc::new(
            [("var1".to_string(), serde_json::json!("value1"))]
                .iter()
                .cloned()
                .collect::<serde_json::Map<String, serde_json::Value>>(),
        );

        // Create WorkflowCheckPointContext directly with minimal fields
        let workflow_checkpoint = WorkflowCheckPointContext {
            id: workflow_id,
            input: input.clone(),
            context_variables: context_variables.clone(),
        };

        // Create TaskCheckPointContext directly
        let task_checkpoint = TaskCheckPointContext {
            input: input.clone(),
            output: Arc::new(serde_json::json!({"processed": "output"})),
            context_variables: context_variables.clone(),
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
