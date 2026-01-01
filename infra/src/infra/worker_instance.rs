pub mod memory;
pub mod redis;

use anyhow::Result;
use async_trait::async_trait;
use proto::jobworkerp::data::{WorkerInstance, WorkerInstanceData, WorkerInstanceId};
use std::collections::HashMap;
use std::sync::Arc;

/// Aggregated channel information from active worker instances
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChannelAggregation {
    /// Total concurrency across all active instances for this channel
    pub total_concurrency: u32,
    /// Number of active instances processing this channel
    pub active_instances: usize,
}

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

    /// Get aggregated channel information from active instances
    ///
    /// # Arguments
    /// - `timeout_millis`: Maximum age of last_heartbeat in milliseconds
    ///
    /// # Returns
    /// - HashMap where key is channel name, value is ChannelAggregation
    ///
    /// # Note
    /// - Default implementation uses find_all_active and aggregates in memory
    /// - Implementations may override for better performance
    async fn get_channel_aggregation(
        &self,
        timeout_millis: i64,
    ) -> Result<HashMap<String, ChannelAggregation>> {
        let instances = self.find_all_active(timeout_millis).await?;
        Ok(aggregate_instance_channels(&instances))
    }
}

/// Aggregate worker instance channels into ChannelAggregation map
///
/// This is a pure function that aggregates channel information from instances.
pub fn aggregate_instance_channels(
    instances: &[WorkerInstance],
) -> HashMap<String, ChannelAggregation> {
    let mut result: HashMap<String, ChannelAggregation> = HashMap::new();

    for instance in instances {
        if let Some(data) = &instance.data {
            for channel in &data.channels {
                let entry = result.entry(channel.name.clone()).or_default();
                entry.total_concurrency += channel.concurrency;
                entry.active_instances += 1;
            }
        }
    }

    result
}

/// Trait for dependency injection of WorkerInstanceRepository
pub trait UseWorkerInstanceRepository {
    fn worker_instance_repository(&self) -> Arc<dyn WorkerInstanceRepository>;
}
