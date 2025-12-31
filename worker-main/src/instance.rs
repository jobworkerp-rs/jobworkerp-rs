use anyhow::Result;
use app::module::AppModule;
use infra::infra::worker_instance::UseWorkerInstanceRepository;
use infra::infra::worker_instance::WorkerInstanceRepository;
use jobworkerp_base::WORKER_INSTANCE_CONFIG;
use proto::jobworkerp::data::StorageType;
use std::sync::Arc;
use tokio::sync::watch;
use tokio::time::{interval, Duration};
use worker_app::worker::instance_registrar::WorkerInstanceRegistrar;

/// Generate a unique instance ID from IP address and process ID
///
/// The ID combines:
/// - Upper 32 bits: IPv4 address (all 4 octets)
/// - Lower 32 bits: Process ID
///
/// This ensures uniqueness even when multiple worker processes run on the same host.
///
/// # ID Structure
/// ```text
/// |<-- upper 32 bits (IP) -->|<-- lower 32 bits (PID) -->|
/// |   192.168.1.100          |        12345              |
/// ```
///
/// # Notes
/// - Process restart results in a new ID (different PID)
/// - In k8s, pod restart naturally gets a new ID
/// - IP and PID parts can be extracted for debugging
fn generate_instance_id() -> i64 {
    let ip_u32 = command_utils::util::id_generator::iputil::resolve_host_ipv4()
        .map(|addr| {
            let octets = addr.ip().octets();
            u32::from_be_bytes(octets)
        })
        .unwrap_or(0);
    let pid = std::process::id();

    // Upper 32 bits: IP, Lower 32 bits: PID
    ((ip_u32 as u64) << 32 | (pid as u64 & 0xFFFFFFFF)) as i64
}

/// Get IP address as string
fn get_ip_address() -> String {
    command_utils::util::id_generator::iputil::resolve_host_ipv4()
        .map(|ip| ip.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Get hostname if available
fn get_hostname() -> Option<String> {
    hostname::get().ok().and_then(|h| h.into_string().ok())
}

/// Cleanup task for expired worker instances
///
/// Only runs in Scalable configuration. In Standalone mode, this is a no-op.
struct InstanceCleanupTask {
    repository: Arc<dyn WorkerInstanceRepository>,
    storage_type: StorageType,
}

impl InstanceCleanupTask {
    fn new(repository: Arc<dyn WorkerInstanceRepository>, storage_type: StorageType) -> Self {
        Self {
            repository,
            storage_type,
        }
    }

    /// Start the cleanup loop
    ///
    /// For Scalable configuration, periodically deletes expired instances.
    /// For Standalone configuration, returns immediately.
    async fn start_cleanup_loop(self, mut shutdown_rx: watch::Receiver<bool>) {
        // Standalone mode does not need cleanup
        if self.storage_type == StorageType::Standalone {
            tracing::debug!("Standalone mode: skipping instance cleanup task");
            return;
        }

        let config = &*WORKER_INSTANCE_CONFIG;
        if !config.enabled {
            tracing::info!("Worker instance registry disabled: skipping cleanup task");
            return;
        }

        let cleanup_interval = Duration::from_secs(config.cleanup_interval_sec);
        let timeout_millis = config.timeout_millis();

        tracing::info!(
            "Starting instance cleanup task: interval={}s, timeout={}s",
            config.cleanup_interval_sec,
            config.timeout_sec
        );

        let mut cleanup_interval = interval(cleanup_interval);

        loop {
            tokio::select! {
                _ = cleanup_interval.tick() => {
                    match self.repository.delete_expired(timeout_millis).await {
                        Ok(deleted) if deleted > 0 => {
                            tracing::info!("Cleaned up {} expired worker instances", deleted);
                        }
                        Ok(_) => {
                            tracing::debug!("No expired worker instances to clean up");
                        }
                        Err(e) => {
                            tracing::warn!("Failed to clean up expired instances: {}", e);
                        }
                    }
                }
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        tracing::info!("Instance cleanup task shutting down");
                        break;
                    }
                }
            }
        }
    }
}

/// Manager for worker instance registration lifecycle
///
/// Handles:
/// - Instance registration on startup
/// - Heartbeat loop execution
/// - Cleanup task for expired instances (Scalable only)
/// - Instance unregistration on shutdown
pub struct WorkerInstanceManager {
    registrar: Arc<WorkerInstanceRegistrar>,
    heartbeat_handle: Option<tokio::task::JoinHandle<Result<()>>>,
    cleanup_handle: Option<tokio::task::JoinHandle<()>>,
}

impl WorkerInstanceManager {
    /// Initialize and register the worker instance
    ///
    /// This should be called early in the boot process, after AppModule is created.
    pub async fn initialize(
        app_module: &Arc<AppModule>,
        shutdown_rx: watch::Receiver<bool>,
    ) -> Result<Self> {
        let config = WORKER_INSTANCE_CONFIG.clone();

        if !config.enabled {
            tracing::info!("Worker instance registration is disabled");
            return Ok(Self {
                registrar: Arc::new(WorkerInstanceRegistrar::new(
                    0,
                    String::new(),
                    None,
                    vec![],
                    app_module.repositories.worker_instance_repository(),
                    config,
                    StorageType::Standalone,
                )),
                heartbeat_handle: None,
                cleanup_handle: None,
            });
        }

        let storage_type = app_module.config_module.storage_type();

        // Get repository from app module
        let repository: Arc<dyn WorkerInstanceRepository> =
            app_module.repositories.worker_instance_repository();

        // Generate instance ID (IP + PID)
        let instance_id = generate_instance_id();

        // Get IP/hostname
        let ip_address = get_ip_address();
        let hostname = get_hostname();

        // Get channel configuration
        let channels = app_module
            .config_module
            .worker_config
            .channel_concurrency_pair();

        // Create registrar
        let registrar = Arc::new(WorkerInstanceRegistrar::new(
            instance_id,
            ip_address,
            hostname,
            channels,
            repository.clone(),
            config,
            storage_type,
        ));

        // Register on startup
        registrar.register().await?;

        // Start heartbeat loop (runs in both Standalone and Scalable)
        let registrar_clone = registrar.clone();
        let heartbeat_shutdown_rx = shutdown_rx.clone();
        let heartbeat_handle = Some(tokio::spawn(async move {
            registrar_clone
                .start_heartbeat_loop(heartbeat_shutdown_rx)
                .await
        }));

        // Start cleanup task (only runs in Scalable mode)
        let cleanup_task = InstanceCleanupTask::new(repository, storage_type);
        let cleanup_handle = Some(tokio::spawn(async move {
            cleanup_task.start_cleanup_loop(shutdown_rx).await
        }));

        Ok(Self {
            registrar,
            heartbeat_handle,
            cleanup_handle,
        })
    }

    /// Shutdown and unregister the worker instance
    ///
    /// This should be called during graceful shutdown, after other services have stopped.
    pub async fn shutdown(self) -> Result<()> {
        // Wait for heartbeat loop to finish
        if let Some(handle) = self.heartbeat_handle {
            let _ = handle.await;
        }

        // Wait for cleanup task to finish
        if let Some(handle) = self.cleanup_handle {
            let _ = handle.await;
        }

        // Unregister instance
        self.registrar.unregister().await?;

        Ok(())
    }

    /// Get reference to the registrar
    #[allow(dead_code)]
    pub fn registrar(&self) -> &Arc<WorkerInstanceRegistrar> {
        &self.registrar
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_instance_id_format() {
        let id = generate_instance_id();

        // Extract PID from lower 32 bits
        let extracted_pid = (id as u64 & 0xFFFFFFFF) as u32;
        assert_eq!(extracted_pid, std::process::id());
    }

    #[test]
    fn test_extract_ip_from_instance_id() {
        let id = generate_instance_id();

        // Extract IP from upper 32 bits
        let extracted_ip_u32 = ((id as u64) >> 32) as u32;

        // Verify it matches the resolved IP (if available)
        let expected_ip_u32 = command_utils::util::id_generator::iputil::resolve_host_ipv4()
            .map(|addr| {
                let octets = addr.ip().octets();
                u32::from_be_bytes(octets)
            })
            .unwrap_or(0);

        assert_eq!(extracted_ip_u32, expected_ip_u32);
    }

    #[test]
    fn test_instance_id_consistency() {
        // Multiple calls should return the same ID (same process, same IP)
        let id1 = generate_instance_id();
        let id2 = generate_instance_id();
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_get_ip_address_returns_valid_format() {
        let ip = get_ip_address();
        // Should be either "unknown" or a valid IP format
        if ip != "unknown" {
            // Check it's a valid IPv4 format (4 octets separated by dots)
            let parts: Vec<&str> = ip.split('.').collect();
            assert_eq!(parts.len(), 4);
            for part in parts {
                // Just verify parsing succeeds (u8 range is already 0-255)
                let _num: u8 = part.parse().expect("Should be valid octet");
            }
        }
    }

    #[test]
    fn test_get_hostname_returns_option() {
        let hostname = get_hostname();
        // Just verify it doesn't panic and returns Some or None
        if let Some(name) = hostname {
            assert!(!name.is_empty());
        }
    }
}
