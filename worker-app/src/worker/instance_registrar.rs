use anyhow::Result;
use command_utils::util::datetime;
use infra::infra::worker_instance::WorkerInstanceRepository;
use jobworkerp_base::worker_instance_config::WorkerInstanceConfig;
use proto::jobworkerp::data::{ChannelConfig, StorageType, WorkerInstanceData, WorkerInstanceId};
use std::sync::Arc;
use tokio::sync::watch;
use tokio::time::{interval, Duration};

/// Worker instance registrar for managing instance lifecycle
///
/// Handles registration, heartbeat, and unregistration of worker instances.
/// Both Standalone and Scalable configurations use heartbeat for state tracking,
/// but only Scalable performs timeout-based deletion.
pub struct WorkerInstanceRegistrar {
    instance_id: WorkerInstanceId,
    instance_data: WorkerInstanceData,
    repository: Arc<dyn WorkerInstanceRepository>,
    config: WorkerInstanceConfig,
    storage_type: StorageType,
}

impl WorkerInstanceRegistrar {
    pub fn new(
        instance_id: i64,
        ip_address: String,
        hostname: Option<String>,
        channels: Vec<(String, u32)>,
        repository: Arc<dyn WorkerInstanceRepository>,
        config: WorkerInstanceConfig,
        storage_type: StorageType,
    ) -> Self {
        let now = datetime::now_millis();

        let channel_configs: Vec<ChannelConfig> = channels
            .into_iter()
            .map(|(name, concurrency)| ChannelConfig { name, concurrency })
            .collect();

        Self {
            instance_id: WorkerInstanceId { value: instance_id },
            instance_data: WorkerInstanceData {
                ip_address,
                hostname,
                channels: channel_configs,
                registered_at: now,
                last_heartbeat: now,
            },
            repository,
            config,
            storage_type,
        }
    }

    /// Register instance on startup
    pub async fn register(&self) -> Result<()> {
        if !self.config.enabled {
            tracing::info!("Worker instance registration is disabled");
            return Ok(());
        }

        self.repository
            .upsert(&self.instance_id, &self.instance_data)
            .await?;

        tracing::info!(
            "Registered worker instance: id={}, ip={}, channels={:?}, storage_type={:?}",
            self.instance_id.value,
            self.instance_data.ip_address,
            self.instance_data.channels,
            self.storage_type
        );

        Ok(())
    }

    /// Start heartbeat loop (runs in both Standalone and Scalable)
    ///
    /// In Standalone: Updates last_heartbeat for freeze detection by external monitoring
    /// In Scalable: Updates last_heartbeat for active instance tracking
    pub async fn start_heartbeat_loop(
        self: Arc<Self>,
        mut shutdown_rx: watch::Receiver<bool>,
    ) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        let mut interval = interval(Duration::from_secs(self.config.heartbeat_interval_sec));

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    match self.repository.update_heartbeat(&self.instance_id).await {
                        Ok(true) => {
                            tracing::debug!("Heartbeat updated: id={}", self.instance_id.value);
                        }
                        Ok(false) => {
                            tracing::warn!("Instance not found, re-registering: id={}", self.instance_id.value);
                            if let Err(e) = self.register().await {
                                tracing::error!("Failed to re-register: {}", e);
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Heartbeat update failed: {}", e);
                        }
                    }
                }
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        tracing::info!("Heartbeat loop shutting down");
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    /// Unregister instance on shutdown
    pub async fn unregister(&self) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        match self.repository.delete(&self.instance_id).await {
            Ok(true) => {
                tracing::info!(
                    "Unregistered worker instance: id={}",
                    self.instance_id.value
                );
            }
            Ok(false) => {
                tracing::warn!(
                    "Worker instance was already removed: id={}",
                    self.instance_id.value
                );
            }
            Err(e) => {
                tracing::error!("Failed to unregister worker instance: {}", e);
            }
        }

        Ok(())
    }

    /// Get instance ID
    pub fn instance_id(&self) -> &WorkerInstanceId {
        &self.instance_id
    }

    /// Get storage type
    pub fn storage_type(&self) -> StorageType {
        self.storage_type
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use infra::infra::worker_instance::memory::MemoryWorkerInstanceRepository;

    fn create_test_registrar() -> WorkerInstanceRegistrar {
        let repo = Arc::new(MemoryWorkerInstanceRepository::new());
        WorkerInstanceRegistrar::new(
            12345,
            "192.168.1.100".to_string(),
            Some("test-worker".to_string()),
            vec![("".to_string(), 4), ("priority".to_string(), 2)],
            repo,
            WorkerInstanceConfig::default(),
            StorageType::Standalone,
        )
    }

    #[tokio::test]
    async fn test_register_and_unregister() {
        let registrar = create_test_registrar();

        // Register
        registrar.register().await.unwrap();

        // Verify registration
        let found = registrar
            .repository
            .find(&registrar.instance_id)
            .await
            .unwrap();
        assert!(found.is_some());

        // Unregister
        registrar.unregister().await.unwrap();

        // Verify unregistration
        let found = registrar
            .repository
            .find(&registrar.instance_id)
            .await
            .unwrap();
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn test_register_disabled() {
        let repo = Arc::new(MemoryWorkerInstanceRepository::new());
        let config = WorkerInstanceConfig {
            enabled: false,
            ..Default::default()
        };

        let registrar = WorkerInstanceRegistrar::new(
            12345,
            "192.168.1.100".to_string(),
            None,
            vec![],
            repo.clone(),
            config,
            StorageType::Standalone,
        );

        // Should not register when disabled
        registrar.register().await.unwrap();

        let found = repo.find(&WorkerInstanceId { value: 12345 }).await.unwrap();
        assert!(found.is_none());
    }
}
