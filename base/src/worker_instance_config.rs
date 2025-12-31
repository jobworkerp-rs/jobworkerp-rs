/// Configuration for Worker Instance Registry feature
///
/// This feature enables worker instances to register themselves in a centralized
/// storage, allowing grpc-front to aggregate channel information from all active
/// worker instances.
///
/// # Standalone vs Scalable Configuration
/// - **Standalone**: Heartbeat runs for freeze detection via `last_heartbeat`.
///   No automatic timeout deletion. External monitoring can check `last_heartbeat`.
/// - **Scalable**: Heartbeat runs for active state tracking. Timeout deletion
///   is performed to handle crashed worker instances.
///
/// # Environment Variables
/// - `WORKER_INSTANCE_ENABLED`: Enable instance registration (default: true)
/// - `WORKER_INSTANCE_HEARTBEAT_INTERVAL_SEC`: Heartbeat interval in seconds (default: 30)
/// - `WORKER_INSTANCE_TIMEOUT_SEC`: Inactive timeout in seconds (default: 90, Scalable only)
/// - `WORKER_INSTANCE_CLEANUP_INTERVAL_SEC`: Expired cleanup interval in seconds (default: 300, Scalable only)
#[derive(Clone, Debug)]
pub struct WorkerInstanceConfig {
    /// Enable instance registration feature
    pub enabled: bool,

    /// Heartbeat send interval (seconds)
    pub heartbeat_interval_sec: u64,

    /// Inactive timeout (seconds) - Used in Scalable configuration only
    pub timeout_sec: u64,

    /// Expired instance cleanup interval (seconds) - Used in Scalable configuration only
    pub cleanup_interval_sec: u64,
}

impl Default for WorkerInstanceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            heartbeat_interval_sec: 30,
            timeout_sec: 90,
            cleanup_interval_sec: 300,
        }
    }
}

impl WorkerInstanceConfig {
    /// Load configuration from environment variables
    pub fn from_env() -> Self {
        Self {
            enabled: std::env::var("WORKER_INSTANCE_ENABLED")
                .unwrap_or_else(|_| "true".to_string())
                .parse()
                .unwrap_or(true),

            heartbeat_interval_sec: std::env::var("WORKER_INSTANCE_HEARTBEAT_INTERVAL_SEC")
                .unwrap_or_else(|_| "30".to_string())
                .parse()
                .unwrap_or(30),

            timeout_sec: std::env::var("WORKER_INSTANCE_TIMEOUT_SEC")
                .unwrap_or_else(|_| "90".to_string())
                .parse()
                .unwrap_or(90),

            cleanup_interval_sec: std::env::var("WORKER_INSTANCE_CLEANUP_INTERVAL_SEC")
                .unwrap_or_else(|_| "300".to_string())
                .parse()
                .unwrap_or(300),
        }
    }

    /// Get timeout in milliseconds
    pub fn timeout_millis(&self) -> i64 {
        (self.timeout_sec * 1000) as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = WorkerInstanceConfig::default();
        assert!(config.enabled);
        assert_eq!(config.heartbeat_interval_sec, 30);
        assert_eq!(config.timeout_sec, 90);
        assert_eq!(config.cleanup_interval_sec, 300);
    }

    #[test]
    fn test_timeout_millis() {
        let config = WorkerInstanceConfig::default();
        assert_eq!(config.timeout_millis(), 90_000);
    }

    #[test]
    fn test_from_env_with_defaults() {
        std::env::remove_var("WORKER_INSTANCE_ENABLED");
        std::env::remove_var("WORKER_INSTANCE_HEARTBEAT_INTERVAL_SEC");
        std::env::remove_var("WORKER_INSTANCE_TIMEOUT_SEC");
        std::env::remove_var("WORKER_INSTANCE_CLEANUP_INTERVAL_SEC");

        let config = WorkerInstanceConfig::from_env();
        assert!(config.enabled);
        assert_eq!(config.heartbeat_interval_sec, 30);
        assert_eq!(config.timeout_sec, 90);
        assert_eq!(config.cleanup_interval_sec, 300);
    }

    #[test]
    fn test_from_env_with_custom_values() {
        std::env::set_var("WORKER_INSTANCE_ENABLED", "false");
        std::env::set_var("WORKER_INSTANCE_HEARTBEAT_INTERVAL_SEC", "60");
        std::env::set_var("WORKER_INSTANCE_TIMEOUT_SEC", "180");
        std::env::set_var("WORKER_INSTANCE_CLEANUP_INTERVAL_SEC", "600");

        let config = WorkerInstanceConfig::from_env();
        assert!(!config.enabled);
        assert_eq!(config.heartbeat_interval_sec, 60);
        assert_eq!(config.timeout_sec, 180);
        assert_eq!(config.cleanup_interval_sec, 600);

        // Cleanup
        std::env::remove_var("WORKER_INSTANCE_ENABLED");
        std::env::remove_var("WORKER_INSTANCE_HEARTBEAT_INTERVAL_SEC");
        std::env::remove_var("WORKER_INSTANCE_TIMEOUT_SEC");
        std::env::remove_var("WORKER_INSTANCE_CLEANUP_INTERVAL_SEC");
    }
}
