//! E2E tests for individual Runner implementations
//!
//! This module tests actual execution of different runners in real environments,
//! verifying both normal execution and cancellation behavior.
//!
//! E2E test files are located in the same directory:
//! - command_e2e.rs
//! - http_request_e2e.rs
//! - mcp_e2e.rs

use std::collections::HashMap;
use std::time::Duration;

/// Common test utilities for E2E tests
pub mod common {
    use super::*;

    /// Standard timeout for quick operations
    pub const QUICK_TIMEOUT: Duration = Duration::from_millis(100);

    /// Standard timeout for slow operations
    pub const SLOW_TIMEOUT: Duration = Duration::from_secs(5);

    /// Standard timeout for very slow operations
    pub const VERY_SLOW_TIMEOUT: Duration = Duration::from_secs(30);

    /// Create basic metadata map for tests
    pub fn create_test_metadata() -> HashMap<String, String> {
        let mut metadata = HashMap::new();
        metadata.insert("test_id".to_string(), "e2e_test".to_string());
        metadata.insert(
            "test_timestamp".to_string(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                .to_string(),
        );
        metadata
    }

    /// Assert that execution completed within expected time
    pub fn assert_timing(elapsed: Duration, expected_max: Duration, operation: &str) {
        assert!(
            elapsed <= expected_max,
            "{operation} took too long: {elapsed:?} > {expected_max:?}",
        );
    }

    /// Assert that operation was cancelled quickly
    pub fn assert_quick_cancellation(elapsed: Duration, operation: &str) {
        assert_timing(elapsed, QUICK_TIMEOUT, &format!("{operation} cancellation"));
    }
}
