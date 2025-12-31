pub mod memory;
pub mod redis;

use anyhow::Result;
use async_trait::async_trait;
use proto::jobworkerp::data::{WorkerInstance, WorkerInstanceData, WorkerInstanceId};
use std::sync::Arc;

/// Repository trait for Worker Instance information
///
/// This trait provides CRUD operations for worker instance registry.
/// Different implementations are used for Standalone (Memory) and Scalable (Redis) configurations.
#[async_trait]
pub trait WorkerInstanceRepository: Send + Sync + std::fmt::Debug + 'static {
    /// Register or update instance information
    ///
    /// # Returns
    /// - `Ok(true)` if updated (already existed)
    /// - `Ok(false)` if created (new)
    async fn upsert(&self, id: &WorkerInstanceId, data: &WorkerInstanceData) -> Result<bool>;

    /// Update heartbeat timestamp
    ///
    /// # Returns
    /// - `Ok(true)` if updated successfully
    /// - `Ok(false)` if instance not found
    async fn update_heartbeat(&self, id: &WorkerInstanceId) -> Result<bool>;

    /// Delete instance information
    ///
    /// # Returns
    /// - `Ok(true)` if deleted
    /// - `Ok(false)` if not found
    async fn delete(&self, id: &WorkerInstanceId) -> Result<bool>;

    /// Find a specific instance by ID
    async fn find(&self, id: &WorkerInstanceId) -> Result<Option<WorkerInstance>>;

    /// Find all registered instances
    async fn find_all(&self) -> Result<Vec<WorkerInstance>>;

    /// Find all active instances (heartbeat within timeout)
    ///
    /// # Arguments
    /// - `timeout_millis`: Maximum age of last_heartbeat in milliseconds
    ///
    /// # Note
    /// - In Standalone configuration, this returns all instances without timeout filtering
    /// - In Scalable configuration, this filters by heartbeat timeout
    async fn find_all_active(&self, timeout_millis: i64) -> Result<Vec<WorkerInstance>>;

    /// Delete expired instances (heartbeat older than timeout)
    ///
    /// # Arguments
    /// - `timeout_millis`: Maximum age of last_heartbeat in milliseconds
    ///
    /// # Returns
    /// - Number of deleted instances
    ///
    /// # Note
    /// - In Standalone configuration, this is a no-op (returns 0)
    /// - In Scalable configuration, this deletes expired instances
    async fn delete_expired(&self, timeout_millis: i64) -> Result<u32>;
}

/// Trait for dependency injection of WorkerInstanceRepository
pub trait UseWorkerInstanceRepository {
    fn worker_instance_repository(&self) -> Arc<dyn WorkerInstanceRepository>;
}
