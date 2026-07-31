use anyhow::Result;
use command_utils::util::datetime;
use infra::infra::worker_instance::WorkerInstanceRepository;
use jobworkerp_base::worker_instance_config::WorkerInstanceConfig;
use proto::jobworkerp::data::{ChannelConfig, StorageType, WorkerInstanceData, WorkerInstanceId};
use std::sync::Arc;
use tokio::sync::watch;
use tokio::time::{Duration, interval};

use super::instance_session::WorkerInstanceSessionHandle;

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
    session: Option<WorkerInstanceSessionHandle>,
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
                // This version is raised only by the recovery-aware registrar
                // after its atomic Registry protocol has been installed.
                rdb_status_index_recovery_version: 0,
            },
            repository,
            config,
            storage_type,
            session: None,
        }
    }

    pub fn with_session(mut self, session: WorkerInstanceSessionHandle) -> Self {
        self.session = Some(session);
        self
    }

    pub fn with_rdb_status_recovery_protocol(mut self) -> Self {
        self.instance_data.rdb_status_index_recovery_version = 1;
        self
    }

    /// Register instance on startup
    pub async fn register(&self) -> Result<()> {
        if !self.config.enabled {
            tracing::info!("Worker instance registration is disabled");
            return Ok(());
        }

        let existed = self
            .repository
            .upsert(&self.instance_id, &self.instance_data)
            .await?;

        if existed && self.session.is_some() {
            anyhow::bail!(
                "worker instance ID collision: recovery-enabled registration must not reuse {}",
                self.instance_id.value
            );
        }

        if let Some(session) = &self.session {
            session.record_heartbeat_success();
        }

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
        let mut consecutive_failures = 0_u32;
        let mut first_failure_at = None;

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let heartbeat_result = if should_timeout_heartbeat_request(self.session.is_some()) {
                        tokio::time::timeout(
                            Duration::from_secs(
                                self.config
                                    .rdb_status_recovery
                                    .heartbeat_request_timeout_sec,
                            ),
                            self.repository.update_heartbeat(&self.instance_id),
                        ).await
                    } else {
                        Ok(self.repository.update_heartbeat(&self.instance_id).await)
                    };
                    match heartbeat_result {
                        Ok(Ok(true)) => {
                            consecutive_failures = 0;
                            first_failure_at = None;
                            if let Some(session) = &self.session {
                                session.record_heartbeat_success();
                            }
                            tracing::debug!("Heartbeat updated: id={}", self.instance_id.value);
                        }
                        Ok(Ok(false)) => {
                            if let Some(session) = &self.session {
                                session.begin_isolation();
                                tracing::error!(
                                    "worker instance disappeared or was claimed for recovery; \
                                     old instance ID will not re-register"
                                );
                                break;
                            }
                            tracing::warn!("Instance not found, re-registering: id={}", self.instance_id.value);
                            if let Err(e) = self.register().await {
                                tracing::error!("Failed to re-register: {}", e);
                            }
                        }
                        Ok(Err(e)) => {
                            consecutive_failures += 1;
                            let started = first_failure_at.get_or_insert_with(std::time::Instant::now);
                            let should_isolate = consecutive_failures
                                >= self.config.rdb_status_recovery.heartbeat_failure_threshold
                                || started.elapsed()
                                    >= Duration::from_secs(
                                        self.config
                                            .rdb_status_recovery
                                            .heartbeat_failure_timeout_sec,
                                    );
                            if should_stop_heartbeat_loop(should_isolate, self.session.is_some())
                                && let Some(session) = &self.session
                            {
                                session.begin_isolation();
                                tracing::error!("Heartbeat update failed until isolation: {e}");
                                break;
                            }
                            if should_isolate {
                                tracing::error!(
                                    "Heartbeat update failed beyond recovery threshold; continuing without recovery session: {e}"
                                );
                            }
                            tracing::warn!("Heartbeat update failed ({consecutive_failures}): {e}");
                        }
                        Err(_) => {
                            consecutive_failures += 1;
                            let started = first_failure_at.get_or_insert_with(std::time::Instant::now);
                            let should_isolate = consecutive_failures
                                >= self.config.rdb_status_recovery.heartbeat_failure_threshold
                                || started.elapsed()
                                    >= Duration::from_secs(
                                        self.config
                                            .rdb_status_recovery
                                            .heartbeat_failure_timeout_sec,
                                    );
                            if should_stop_heartbeat_loop(should_isolate, self.session.is_some())
                                && let Some(session) = &self.session
                            {
                                session.begin_isolation();
                                tracing::error!("Heartbeat request timed out until isolation");
                                break;
                            }
                            if should_isolate {
                                tracing::error!(
                                    "Heartbeat request timed out beyond recovery threshold; continuing without recovery session"
                                );
                            }
                            tracing::warn!("Heartbeat request timed out ({consecutive_failures})");
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

fn should_stop_heartbeat_loop(
    recovery_threshold_reached: bool,
    has_recovery_session: bool,
) -> bool {
    recovery_threshold_reached && has_recovery_session
}

fn should_timeout_heartbeat_request(has_recovery_session: bool) -> bool {
    has_recovery_session
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

    #[test]
    fn only_recovery_enabled_workers_stop_after_heartbeat_failures() {
        assert!(should_stop_heartbeat_loop(true, true));
        assert!(!should_stop_heartbeat_loop(true, false));
        assert!(!should_stop_heartbeat_loop(false, true));
    }

    #[test]
    fn only_recovery_sessions_apply_a_heartbeat_request_timeout() {
        assert!(should_timeout_heartbeat_request(true));
        assert!(!should_timeout_heartbeat_request(false));
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

    #[tokio::test]
    async fn recovery_enabled_registration_rejects_an_existing_instance_id() {
        let repo = Arc::new(MemoryWorkerInstanceRepository::new());
        let config = WorkerInstanceConfig::default();
        let first = WorkerInstanceRegistrar::new(
            87654,
            "192.168.1.100".to_string(),
            None,
            vec![],
            repo.clone(),
            config.clone(),
            StorageType::Scalable,
        );
        first.register().await.unwrap();
        let second = WorkerInstanceRegistrar::new(
            87654,
            "192.168.1.101".to_string(),
            None,
            vec![],
            repo,
            config,
            StorageType::Scalable,
        )
        .with_session(WorkerInstanceSessionHandle::new(
            87654,
            Duration::from_secs(10),
            Duration::from_secs(1),
        ));
        assert!(second.register().await.is_err());
    }
}
