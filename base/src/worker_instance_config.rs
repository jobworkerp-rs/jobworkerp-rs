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

    /// Settings for the optional RDB status-index based recovery path.
    pub rdb_status_recovery: RdbStatusRecoveryConfig,
}

/// Settings that only affect recovery of jobs owned by a lost worker instance.
///
/// They deliberately live under the worker-instance configuration because the
/// feature is driven by the instance registry, not by the job data plane.
#[derive(Clone, Debug)]
pub struct RdbStatusRecoveryConfig {
    pub enabled: bool,
    pub lock_ttl_sec: u64,
    /// Extra time after a timed runner expires before recovery may retry it.
    pub execution_completion_reserve_sec: u64,
    /// Maximum time to wait after an instance expires before recovering an unbounded runner.
    pub unbounded_execution_recovery_timeout_sec: u64,
    pub start_permit_timeout_sec: u64,
    pub heartbeat_request_timeout_sec: u64,
    pub heartbeat_failure_threshold: u32,
    pub heartbeat_failure_timeout_sec: u64,
    pub isolation_check_interval_sec: u64,
}

impl Default for RdbStatusRecoveryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            lock_ttl_sec: 300,
            execution_completion_reserve_sec: 5,
            unbounded_execution_recovery_timeout_sec: 86_400,
            start_permit_timeout_sec: 10,
            heartbeat_request_timeout_sec: 10,
            heartbeat_failure_threshold: 2,
            heartbeat_failure_timeout_sec: 30,
            isolation_check_interval_sec: 5,
        }
    }
}

impl Default for WorkerInstanceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            heartbeat_interval_sec: 30,
            timeout_sec: 90,
            cleanup_interval_sec: 300,
            rdb_status_recovery: RdbStatusRecoveryConfig::default(),
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

            rdb_status_recovery: RdbStatusRecoveryConfig {
                enabled: env_bool("WORKER_INSTANCE_RDB_STATUS_RECOVERY_ENABLED", false),
                lock_ttl_sec: env_u64("WORKER_INSTANCE_RDB_STATUS_RECOVERY_LOCK_TTL_SEC", 300),
                execution_completion_reserve_sec: env_u64(
                    "WORKER_INSTANCE_RDB_STATUS_RECOVERY_EXECUTION_COMPLETION_RESERVE_SEC",
                    5,
                ),
                unbounded_execution_recovery_timeout_sec: env_u64(
                    "WORKER_INSTANCE_RDB_STATUS_RECOVERY_UNBOUNDED_EXECUTION_TIMEOUT_SEC",
                    86_400,
                ),
                start_permit_timeout_sec: env_u64("WORKER_INSTANCE_START_PERMIT_TIMEOUT_SEC", 10),
                heartbeat_request_timeout_sec: env_u64(
                    "WORKER_INSTANCE_HEARTBEAT_REQUEST_TIMEOUT_SEC",
                    10,
                ),
                heartbeat_failure_threshold: env_u64(
                    "WORKER_INSTANCE_HEARTBEAT_FAILURE_THRESHOLD",
                    2,
                ) as u32,
                heartbeat_failure_timeout_sec: env_u64(
                    "WORKER_INSTANCE_HEARTBEAT_FAILURE_TIMEOUT_SEC",
                    30,
                ),
                isolation_check_interval_sec: env_u64(
                    "WORKER_INSTANCE_ISOLATION_CHECK_INTERVAL_SEC",
                    5,
                ),
            },
        }
    }

    /// Get timeout in milliseconds
    pub fn timeout_millis(&self) -> i64 {
        self.timeout_sec.saturating_mul(1000).min(i64::MAX as u64) as i64
    }

    /// Validate only the relationships needed by the optional recovery loop.
    /// Callers log and disable recovery on an error while keeping registration alive.
    pub fn validate_rdb_status_recovery(&self) -> Result<(), String> {
        let recovery = &self.rdb_status_recovery;
        if !recovery.enabled {
            return Ok(());
        }
        if self.heartbeat_interval_sec == 0 || self.cleanup_interval_sec == 0 {
            return Err("heartbeat and cleanup intervals must be non-zero".to_string());
        }
        if recovery.start_permit_timeout_sec == 0
            || recovery.start_permit_timeout_sec >= self.timeout_sec
        {
            return Err(
                "start permit timeout must be non-zero and below instance timeout".to_string(),
            );
        }
        if recovery.heartbeat_request_timeout_sec == 0
            || recovery.heartbeat_request_timeout_sec >= self.timeout_sec
            || recovery.heartbeat_failure_threshold == 0
            || recovery.heartbeat_failure_timeout_sec == 0
            || recovery.heartbeat_failure_timeout_sec >= self.timeout_sec
            || recovery.isolation_check_interval_sec == 0
        {
            return Err("invalid heartbeat isolation configuration".to_string());
        }
        if recovery.lock_ttl_sec == 0
            || recovery.execution_completion_reserve_sec == 0
            || recovery.unbounded_execution_recovery_timeout_sec == 0
        {
            return Err("invalid recovery timeout".to_string());
        }
        Ok(())
    }
}

fn env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
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
    fn rdb_status_recovery_defaults_are_disabled_and_valid() {
        let config = WorkerInstanceConfig::default();
        let recovery = &config.rdb_status_recovery;

        assert!(!recovery.enabled);
        assert_eq!(recovery.lock_ttl_sec, 300);
        assert_eq!(recovery.unbounded_execution_recovery_timeout_sec, 86_400);
        assert!(config.validate_rdb_status_recovery().is_ok());
    }

    #[test]
    fn rdb_status_recovery_rejects_invalid_deadlines() {
        let mut config = WorkerInstanceConfig::default();
        config.rdb_status_recovery.enabled = true;
        config.rdb_status_recovery.start_permit_timeout_sec = config.timeout_sec;

        assert!(config.validate_rdb_status_recovery().is_err());
    }

    #[test]
    fn rdb_status_recovery_requires_a_nonzero_completion_reserve() {
        let mut config = WorkerInstanceConfig::default();
        config.rdb_status_recovery.enabled = true;
        config.rdb_status_recovery.execution_completion_reserve_sec = 0;

        assert!(config.validate_rdb_status_recovery().is_err());
    }

    #[test]
    fn rdb_status_recovery_requires_a_nonzero_lock_ttl() {
        let mut config = WorkerInstanceConfig::default();
        config.rdb_status_recovery.enabled = true;
        config.rdb_status_recovery.lock_ttl_sec = 0;

        assert!(config.validate_rdb_status_recovery().is_err());
    }

    #[test]
    fn rdb_status_recovery_requires_a_nonzero_unbounded_execution_timeout() {
        let mut config = WorkerInstanceConfig::default();
        config.rdb_status_recovery.enabled = true;
        config
            .rdb_status_recovery
            .unbounded_execution_recovery_timeout_sec = 0;

        assert!(config.validate_rdb_status_recovery().is_err());
    }

    #[test]
    fn test_from_env_with_defaults() {
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var("WORKER_INSTANCE_ENABLED") };
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var("WORKER_INSTANCE_HEARTBEAT_INTERVAL_SEC") };
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var("WORKER_INSTANCE_TIMEOUT_SEC") };
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var("WORKER_INSTANCE_CLEANUP_INTERVAL_SEC") };

        let config = WorkerInstanceConfig::from_env();
        assert!(config.enabled);
        assert_eq!(config.heartbeat_interval_sec, 30);
        assert_eq!(config.timeout_sec, 90);
        assert_eq!(config.cleanup_interval_sec, 300);
    }

    #[test]
    fn test_from_env_with_custom_values() {
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::set_var("WORKER_INSTANCE_ENABLED", "false") };
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::set_var("WORKER_INSTANCE_HEARTBEAT_INTERVAL_SEC", "60") };
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::set_var("WORKER_INSTANCE_TIMEOUT_SEC", "180") };
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::set_var("WORKER_INSTANCE_CLEANUP_INTERVAL_SEC", "600") };

        let config = WorkerInstanceConfig::from_env();
        assert!(!config.enabled);
        assert_eq!(config.heartbeat_interval_sec, 60);
        assert_eq!(config.timeout_sec, 180);
        assert_eq!(config.cleanup_interval_sec, 600);

        // Cleanup
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var("WORKER_INSTANCE_ENABLED") };
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var("WORKER_INSTANCE_HEARTBEAT_INTERVAL_SEC") };
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var("WORKER_INSTANCE_TIMEOUT_SEC") };
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var("WORKER_INSTANCE_CLEANUP_INTERVAL_SEC") };
    }
}
