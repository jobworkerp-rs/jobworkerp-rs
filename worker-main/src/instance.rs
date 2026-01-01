use anyhow::Result;
use app::app::worker_instance::InstanceCleanupTask;
use app::module::AppModule;
use infra::infra::worker_instance::UseWorkerInstanceRepository;
use infra::infra::worker_instance::WorkerInstanceRepository;
use jobworkerp_base::WORKER_INSTANCE_CONFIG;
use std::sync::Arc;
use tokio::sync::watch;
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

/// Configuration for WorkerInstanceManager initialization
#[derive(Debug, Clone, Copy, Default)]
pub struct WorkerInstanceManagerConfig {
    /// Enable instance registration and heartbeat (for worker processes)
    pub enable_registration: bool,
    /// Enable cleanup task for expired instances (for grpc-front processes)
    pub enable_cleanup: bool,
}

impl WorkerInstanceManagerConfig {
    /// Configuration for worker-only mode (distributed worker process)
    pub fn worker_only() -> Self {
        Self {
            enable_registration: true,
            enable_cleanup: false,
        }
    }

    /// Configuration for cleanup-only mode (distributed grpc-front process)
    pub fn cleanup_only() -> Self {
        Self {
            enable_registration: false,
            enable_cleanup: true,
        }
    }

    /// Configuration for all-in-one mode (both worker and grpc-front in same process)
    pub fn all_in_one() -> Self {
        Self {
            enable_registration: true,
            enable_cleanup: true,
        }
    }
}

/// Manager for worker instance registration lifecycle
///
/// Handles:
/// - Instance registration on startup (worker mode)
/// - Heartbeat loop execution (worker mode)
/// - Cleanup task for expired instances (grpc-front mode, Scalable only)
/// - Instance unregistration on shutdown (worker mode)
///
/// # Distributed Environment
/// - **worker binary**: Use `WorkerInstanceManagerConfig::worker_only()`
/// - **grpc-front binary**: Use `WorkerInstanceManagerConfig::cleanup_only()`
/// - **all-in-one binary**: Use `WorkerInstanceManagerConfig::all_in_one()`
pub struct WorkerInstanceManager {
    registrar: Option<Arc<WorkerInstanceRegistrar>>,
    heartbeat_handle: Option<tokio::task::JoinHandle<Result<()>>>,
    cleanup_handle: Option<tokio::task::JoinHandle<()>>,
}

impl WorkerInstanceManager {
    /// Initialize and register the worker instance
    ///
    /// This should be called early in the boot process, after AppModule is created.
    /// For all-in-one mode, use `WorkerInstanceManagerConfig::all_in_one()`.
    pub async fn initialize(
        app_module: &Arc<AppModule>,
        shutdown_rx: watch::Receiver<bool>,
    ) -> Result<Self> {
        Self::initialize_with_config(
            app_module,
            shutdown_rx,
            WorkerInstanceManagerConfig::all_in_one(),
        )
        .await
    }

    /// Initialize with specific configuration for distributed environment
    ///
    /// # Arguments
    /// * `app_module` - Application module
    /// * `shutdown_rx` - Shutdown signal receiver
    /// * `manager_config` - Configuration specifying which features to enable
    pub async fn initialize_with_config(
        app_module: &Arc<AppModule>,
        shutdown_rx: watch::Receiver<bool>,
        manager_config: WorkerInstanceManagerConfig,
    ) -> Result<Self> {
        let config = WORKER_INSTANCE_CONFIG.clone();

        if !config.enabled {
            tracing::info!("Worker instance registration is disabled");
            return Ok(Self {
                registrar: None,
                heartbeat_handle: None,
                cleanup_handle: None,
            });
        }

        let storage_type = app_module.config_module.storage_type();
        let repository: Arc<dyn WorkerInstanceRepository> =
            app_module.repositories.worker_instance_repository();

        let mut registrar = None;
        let mut heartbeat_handle = None;
        let mut cleanup_handle = None;

        // Registration and heartbeat (for worker processes)
        if manager_config.enable_registration {
            let instance_id = generate_instance_id();
            let ip_address = get_ip_address();
            let hostname = get_hostname();
            let channels = app_module
                .config_module
                .worker_config
                .channel_concurrency_pair();

            let reg = Arc::new(WorkerInstanceRegistrar::new(
                instance_id,
                ip_address,
                hostname,
                channels,
                repository.clone(),
                config,
                storage_type,
            ));

            // Register on startup
            reg.register().await?;

            // Start heartbeat loop
            let registrar_clone = reg.clone();
            let heartbeat_shutdown_rx = shutdown_rx.clone();
            heartbeat_handle = Some(tokio::spawn(async move {
                registrar_clone
                    .start_heartbeat_loop(heartbeat_shutdown_rx)
                    .await
            }));

            registrar = Some(reg);
        }

        // Cleanup task (for grpc-front processes, only runs in Scalable mode)
        if manager_config.enable_cleanup {
            let cleanup_task = InstanceCleanupTask::new(repository, storage_type);
            cleanup_handle = Some(tokio::spawn(async move {
                cleanup_task.start_cleanup_loop(shutdown_rx).await
            }));
        }

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

        // Unregister instance (only if registration was enabled)
        if let Some(registrar) = &self.registrar {
            registrar.unregister().await?;
        }

        Ok(())
    }

    /// Get reference to the registrar (if registration is enabled)
    #[allow(dead_code)]
    pub fn registrar(&self) -> Option<&Arc<WorkerInstanceRegistrar>> {
        self.registrar.as_ref()
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
