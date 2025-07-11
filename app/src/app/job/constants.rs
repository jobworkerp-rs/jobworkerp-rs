//! Job cancellation related constants
//!
//! This module defines constants used for job cancellation functionality,
//! including intervals, channel names, messages, and TTL settings.

/// Constants for job cancellation functionality
pub mod cancellation {
    use std::time::Duration;

    /// Interval for checking cancellation status (500ms)
    pub const CANCELLATION_CHECK_INTERVAL_MS: u64 = 500;
    pub const CANCELLATION_CHECK_INTERVAL: Duration =
        Duration::from_millis(CANCELLATION_CHECK_INTERVAL_MS);

    /// Redis Pub/Sub channel name for job cancellation notifications
    /// This constant must be used consistently across all workers to avoid channel name conflicts
    pub const JOB_CANCELLATION_CHANNEL: &str = "job_cancellation_channel";

    /// Standard cancellation messages for ResultOutput
    /// These messages are stored in job results and provide consistent user feedback
    pub const CANCEL_REASON_USER_REQUEST: &str = "Job cancelled by user request";
    pub const CANCEL_REASON_BEFORE_EXECUTION: &str = "Job cancelled before execution";

    /// Grace period added to job-specific timeout (5 minutes)
    /// This allows jobs to complete gracefully even after timeout
    pub const RUNNING_JOB_GRACE_PERIOD_MS: u64 = 300_000; // 5 minutes

    /// Interval for background cleanup of expired job entries (5 minutes)
    pub const RUNNING_JOB_CLEANUP_INTERVAL_SECONDS: u64 = 300; // 5 minutes
}
