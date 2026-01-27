use infra::infra::worker_instance::WorkerInstanceRepository;
use jobworkerp_base::WORKER_INSTANCE_CONFIG;
use proto::jobworkerp::data::StorageType;
use rand::Rng;
use std::sync::Arc;
use tokio::sync::watch;
use tokio::time::{Duration, interval};

/// Cleanup task for expired worker instances
///
/// Only runs in Scalable configuration. In Standalone mode, this is a no-op.
///
/// When multiple grpc-front instances are running, each instance applies a random
/// initial delay (0 to cleanup_interval) before starting the cleanup loop.
/// This distributes cleanup execution across time, reducing redundant operations
/// when multiple instances try to clean up the same expired workers.
pub struct InstanceCleanupTask {
    repository: Arc<dyn WorkerInstanceRepository>,
    storage_type: StorageType,
}

impl InstanceCleanupTask {
    pub fn new(repository: Arc<dyn WorkerInstanceRepository>, storage_type: StorageType) -> Self {
        Self {
            repository,
            storage_type,
        }
    }

    /// Start the cleanup loop
    ///
    /// For Scalable configuration, periodically deletes expired instances.
    /// For Standalone configuration, returns immediately.
    ///
    /// Applies a random initial delay to distribute cleanup execution across
    /// multiple grpc-front instances.
    pub async fn start_cleanup_loop(self, mut shutdown_rx: watch::Receiver<bool>) {
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

        let cleanup_interval_secs = config.cleanup_interval_sec;
        let cleanup_interval = Duration::from_secs(cleanup_interval_secs);
        let timeout_millis = config.timeout_millis();

        // Apply random initial delay to distribute cleanup across multiple grpc-front instances
        let initial_delay_secs = rand::rng().random_range(0..cleanup_interval_secs);
        let initial_delay = Duration::from_secs(initial_delay_secs);

        tracing::info!(
            "Starting instance cleanup task: interval={}s, timeout={}s, initial_delay={}s",
            cleanup_interval_secs,
            config.timeout_sec,
            initial_delay_secs
        );

        // Wait for initial random delay (with shutdown check)
        tokio::select! {
            _ = tokio::time::sleep(initial_delay) => {}
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    tracing::info!("Instance cleanup task shutting down during initial delay");
                    return;
                }
            }
        }

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
