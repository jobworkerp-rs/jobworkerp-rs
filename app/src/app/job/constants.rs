//! Job cancellation related constants
//! 
//! This module defines constants used for job cancellation functionality,
//! including intervals, channel names, messages, and TTL settings.

use std::time::Duration;

/// Constants for job cancellation functionality
pub mod cancellation {
    use super::Duration;

    /// Interval for checking cancellation status (500ms)
    pub const CANCELLATION_CHECK_INTERVAL_MS: u64 = 500;
    pub const CANCELLATION_CHECK_INTERVAL: Duration = Duration::from_millis(CANCELLATION_CHECK_INTERVAL_MS);

    /// Redis Pub/Sub channel name for job cancellation notifications
    /// This constant must be used consistently across all workers to avoid channel name conflicts
    pub const JOB_CANCELLATION_CHANNEL: &str = "job_cancellation_channel";

    /// Standard cancellation messages for ResultOutput
    /// These messages are stored in job results and provide consistent user feedback
    pub const CANCEL_REASON_USER_REQUEST: &str = "Job cancelled by user request";
    pub const CANCEL_REASON_BEFORE_EXECUTION: &str = "Job cancelled before execution";

    /// TTL settings for running job management (memory leak prevention)
    /// These values ensure that job entries are automatically cleaned up
    
    /// Default TTL for jobs without explicit timeout (100 hours)
    pub const RUNNING_JOB_DEFAULT_TTL_SECONDS: u64 = 360000; // 100 hours
    
    /// Grace period added to job-specific timeout (5 minutes)
    /// This allows jobs to complete gracefully even after timeout
    pub const RUNNING_JOB_GRACE_PERIOD_MS: u64 = 300_000; // 5 minutes
    
    /// Interval for background cleanup of expired job entries (5 minutes)
    pub const RUNNING_JOB_CLEANUP_INTERVAL_SECONDS: u64 = 300; // 5 minutes
}

#[cfg(test)]
mod tests {
    use super::cancellation::*;

    #[test]
    fn test_cancellation_constants() {
        // Verify that constants have reasonable values
        assert_eq!(CANCELLATION_CHECK_INTERVAL_MS, 500);
        assert_eq!(CANCELLATION_CHECK_INTERVAL.as_millis(), 500);
        
        // Channel name should be non-empty
        assert!(!JOB_CANCELLATION_CHANNEL.is_empty());
        
        // Messages should be user-friendly
        assert!(CANCEL_REASON_USER_REQUEST.contains("user request"));
        assert!(CANCEL_REASON_BEFORE_EXECUTION.contains("before execution"));
        
        // TTL values should be reasonable
        assert!(RUNNING_JOB_DEFAULT_TTL_SECONDS > 0);
        assert!(RUNNING_JOB_GRACE_PERIOD_MS > 0);
        assert!(RUNNING_JOB_CLEANUP_INTERVAL_SECONDS > 0);
        
        // Grace period should be reasonable (less than 1 hour)
        assert!(RUNNING_JOB_GRACE_PERIOD_MS < 3_600_000);
    }

    #[test]
    fn test_ttl_relationships() {
        // Cleanup interval should be reasonable compared to grace period
        let cleanup_interval_ms = RUNNING_JOB_CLEANUP_INTERVAL_SECONDS * 1000;
        assert!(cleanup_interval_ms <= RUNNING_JOB_GRACE_PERIOD_MS * 2);
        
        // Default TTL should be much larger than grace period
        let default_ttl_ms = RUNNING_JOB_DEFAULT_TTL_SECONDS * 1000;
        assert!(default_ttl_ms > RUNNING_JOB_GRACE_PERIOD_MS * 10);
    }
}