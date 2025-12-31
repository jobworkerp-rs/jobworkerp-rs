use anyhow::Result;
use async_trait::async_trait;
use command_utils::util::datetime;
use dashmap::DashMap;
use proto::jobworkerp::data::{WorkerInstance, WorkerInstanceData, WorkerInstanceId};
use std::sync::Arc;

use super::WorkerInstanceRepository;

/// Memory-based implementation for Standalone configuration
///
/// # Behavior in Standalone Configuration
/// - Heartbeat runs to update `last_heartbeat` for freeze detection
/// - Timeout deletion is NOT performed (returns 0 from `delete_expired`)
/// - `find_all_active()` returns all instances without timeout filtering
/// - External monitoring can check `last_heartbeat` to detect freezes
/// - Freeze recovery is handled by process restart (supervisord, etc.)
#[derive(Clone, Debug)]
pub struct MemoryWorkerInstanceRepository {
    instances: Arc<DashMap<i64, WorkerInstance>>,
}

impl MemoryWorkerInstanceRepository {
    pub fn new() -> Self {
        Self {
            instances: Arc::new(DashMap::new()),
        }
    }
}

impl Default for MemoryWorkerInstanceRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl WorkerInstanceRepository for MemoryWorkerInstanceRepository {
    async fn upsert(&self, id: &WorkerInstanceId, data: &WorkerInstanceData) -> Result<bool> {
        let instance = WorkerInstance {
            id: Some(*id),
            data: Some(data.clone()),
        };
        let existed = self.instances.insert(id.value, instance).is_some();
        tracing::debug!(
            "upsert worker instance to memory: id={}, existed={}",
            id.value,
            existed
        );
        Ok(existed)
    }

    async fn update_heartbeat(&self, id: &WorkerInstanceId) -> Result<bool> {
        if let Some(mut entry) = self.instances.get_mut(&id.value) {
            if let Some(ref mut data) = entry.value_mut().data {
                data.last_heartbeat = datetime::now_millis();
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn delete(&self, id: &WorkerInstanceId) -> Result<bool> {
        let removed = self.instances.remove(&id.value).is_some();
        tracing::debug!(
            "delete worker instance from memory: id={}, removed={}",
            id.value,
            removed
        );
        Ok(removed)
    }

    async fn find(&self, id: &WorkerInstanceId) -> Result<Option<WorkerInstance>> {
        Ok(self.instances.get(&id.value).map(|entry| entry.clone()))
    }

    async fn find_all(&self) -> Result<Vec<WorkerInstance>> {
        Ok(self
            .instances
            .iter()
            .map(|entry| entry.value().clone())
            .collect())
    }

    async fn find_all_active(&self, _timeout_millis: i64) -> Result<Vec<WorkerInstance>> {
        // Standalone configuration: return all instances without timeout filtering
        // last_heartbeat is still useful for external monitoring to detect freezes
        self.find_all().await
    }

    async fn delete_expired(&self, _timeout_millis: i64) -> Result<u32> {
        // Standalone configuration: no automatic deletion
        // Freeze detection and recovery is handled by external monitoring + process restart
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::jobworkerp::data::ChannelConfig;

    fn create_test_data() -> WorkerInstanceData {
        WorkerInstanceData {
            ip_address: "192.168.1.100".to_string(),
            hostname: Some("test-worker".to_string()),
            channels: vec![
                ChannelConfig {
                    name: "".to_string(),
                    concurrency: 4,
                },
                ChannelConfig {
                    name: "priority".to_string(),
                    concurrency: 2,
                },
            ],
            registered_at: datetime::now_millis(),
            last_heartbeat: datetime::now_millis(),
        }
    }

    #[tokio::test]
    async fn test_upsert_and_find() {
        let repo = MemoryWorkerInstanceRepository::new();
        let id = WorkerInstanceId { value: 12345 };
        let data = create_test_data();

        // Insert new
        let existed = repo.upsert(&id, &data).await.unwrap();
        assert!(!existed);

        // Find
        let found = repo.find(&id).await.unwrap();
        assert!(found.is_some());
        let instance = found.unwrap();
        assert_eq!(instance.id.as_ref().unwrap().value, 12345);
        assert_eq!(instance.data.as_ref().unwrap().ip_address, "192.168.1.100");

        // Update existing
        let existed = repo.upsert(&id, &data).await.unwrap();
        assert!(existed);
    }

    #[tokio::test]
    async fn test_update_heartbeat() {
        let repo = MemoryWorkerInstanceRepository::new();
        let id = WorkerInstanceId { value: 12345 };
        let mut data = create_test_data();
        let old_heartbeat = data.last_heartbeat - 10000; // 10 seconds ago
        data.last_heartbeat = old_heartbeat;

        repo.upsert(&id, &data).await.unwrap();

        // Update heartbeat
        let updated = repo.update_heartbeat(&id).await.unwrap();
        assert!(updated);

        // Verify heartbeat was updated
        let found = repo.find(&id).await.unwrap().unwrap();
        assert!(found.data.as_ref().unwrap().last_heartbeat > old_heartbeat);
    }

    #[tokio::test]
    async fn test_update_heartbeat_not_found() {
        let repo = MemoryWorkerInstanceRepository::new();
        let id = WorkerInstanceId { value: 99999 };

        let updated = repo.update_heartbeat(&id).await.unwrap();
        assert!(!updated);
    }

    #[tokio::test]
    async fn test_delete() {
        let repo = MemoryWorkerInstanceRepository::new();
        let id = WorkerInstanceId { value: 12345 };
        let data = create_test_data();

        repo.upsert(&id, &data).await.unwrap();

        // Delete
        let deleted = repo.delete(&id).await.unwrap();
        assert!(deleted);

        // Verify deleted
        let found = repo.find(&id).await.unwrap();
        assert!(found.is_none());

        // Delete again should return false
        let deleted = repo.delete(&id).await.unwrap();
        assert!(!deleted);
    }

    #[tokio::test]
    async fn test_find_all() {
        let repo = MemoryWorkerInstanceRepository::new();

        for i in 1..=3 {
            let id = WorkerInstanceId { value: i };
            let mut data = create_test_data();
            data.ip_address = format!("192.168.1.{}", i);
            repo.upsert(&id, &data).await.unwrap();
        }

        let all = repo.find_all().await.unwrap();
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn test_find_all_active_returns_all_in_standalone() {
        let repo = MemoryWorkerInstanceRepository::new();

        // Add instance with old heartbeat
        let id = WorkerInstanceId { value: 1 };
        let mut data = create_test_data();
        data.last_heartbeat = datetime::now_millis() - 1000000; // Very old
        repo.upsert(&id, &data).await.unwrap();

        // In Standalone, find_all_active should return all regardless of timeout
        let active = repo.find_all_active(90000).await.unwrap();
        assert_eq!(active.len(), 1);
    }

    #[tokio::test]
    async fn test_delete_expired_noop_in_standalone() {
        let repo = MemoryWorkerInstanceRepository::new();

        // Add instance with old heartbeat
        let id = WorkerInstanceId { value: 1 };
        let mut data = create_test_data();
        data.last_heartbeat = datetime::now_millis() - 1000000; // Very old
        repo.upsert(&id, &data).await.unwrap();

        // In Standalone, delete_expired should do nothing
        let deleted = repo.delete_expired(90000).await.unwrap();
        assert_eq!(deleted, 0);

        // Instance should still exist
        let found = repo.find(&id).await.unwrap();
        assert!(found.is_some());
    }
}
