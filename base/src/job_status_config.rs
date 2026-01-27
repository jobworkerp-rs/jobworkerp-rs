/// Configuration for JobProcessingStatus RDB indexing feature
///
/// This feature provides advanced search capabilities for job processing status
/// by indexing status information in RDB. It is optional and disabled by default
/// to avoid unnecessary complexity for hobby use cases.
///
/// # Use Cases
/// - Hobby/Small scale (default): `rdb_indexing_enabled=false` - No RDB indexing
/// - Enterprise/Large scale: `rdb_indexing_enabled=true` - RDB indexing for up to 1M jobs
///
/// # Environment Variables
/// - `JOB_STATUS_RDB_INDEXING`: Enable RDB indexing (default: false)
/// - `JOB_STATUS_CLEANUP_INTERVAL_HOURS`: Cleanup task interval in hours (default: 1)
/// - `JOB_STATUS_RETENTION_HOURS`: Retention period for deleted records in hours (default: 24)
#[derive(Clone, Debug)]
pub struct JobStatusConfig {
    /// Enable RDB indexing for job processing status
    pub rdb_indexing_enabled: bool,

    /// Cleanup task execution interval (hours)
    pub cleanup_interval_hours: u64,

    /// Retention period for logically deleted records (hours)
    pub retention_hours: u64,
}

impl Default for JobStatusConfig {
    fn default() -> Self {
        Self {
            rdb_indexing_enabled: false,
            cleanup_interval_hours: 1,
            retention_hours: 24,
        }
    }
}

impl JobStatusConfig {
    /// Load configuration from environment variables
    pub fn from_env() -> Self {
        Self {
            rdb_indexing_enabled: std::env::var("JOB_STATUS_RDB_INDEXING")
                .unwrap_or_else(|_| "false".to_string())
                .parse()
                .unwrap_or(false),

            cleanup_interval_hours: std::env::var("JOB_STATUS_CLEANUP_INTERVAL_HOURS")
                .unwrap_or_else(|_| "1".to_string())
                .parse()
                .unwrap_or(1),

            retention_hours: std::env::var("JOB_STATUS_RETENTION_HOURS")
                .unwrap_or_else(|_| "24".to_string())
                .parse()
                .unwrap_or(24),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = JobStatusConfig::default();
        assert!(!config.rdb_indexing_enabled);
        assert_eq!(config.cleanup_interval_hours, 1);
        assert_eq!(config.retention_hours, 24);
    }

    #[test]
    fn test_from_env_with_defaults() {
        // Clear environment variables to test defaults
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var("JOB_STATUS_RDB_INDEXING") };
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var("JOB_STATUS_CLEANUP_INTERVAL_HOURS") };
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var("JOB_STATUS_RETENTION_HOURS") };

        let config = JobStatusConfig::from_env();
        assert!(!config.rdb_indexing_enabled);
        assert_eq!(config.cleanup_interval_hours, 1);
        assert_eq!(config.retention_hours, 24);
    }

    #[test]
    fn test_from_env_with_custom_values() {
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::set_var("JOB_STATUS_RDB_INDEXING", "true") };
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::set_var("JOB_STATUS_CLEANUP_INTERVAL_HOURS", "2") };
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::set_var("JOB_STATUS_RETENTION_HOURS", "48") };

        let config = JobStatusConfig::from_env();
        assert!(config.rdb_indexing_enabled);
        assert_eq!(config.cleanup_interval_hours, 2);
        assert_eq!(config.retention_hours, 48);

        // Cleanup
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var("JOB_STATUS_RDB_INDEXING") };
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var("JOB_STATUS_CLEANUP_INTERVAL_HOURS") };
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var("JOB_STATUS_RETENTION_HOURS") };
    }
}
