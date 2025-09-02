//! E2E tests for cancellation functionality across all runners
//!
//! Tests external cancellation, pubsub mechanisms, and integration scenarios
//!
//! Cancellation test files are located in the same directory:
//! - external_cancellation_e2e.rs
//! - pubsub_cancellation_e2e.rs
//! - streaming_cancellation_e2e.rs

use std::collections::HashMap;
use std::time::Duration;

/// Common test utilities for cancellation E2E tests
pub mod common {
    use super::*;

    /// Timeout for operations that should be cancelled quickly
    pub const CANCELLATION_TIMEOUT: Duration = Duration::from_millis(200);

    /// Timeout for slow operations before cancellation
    pub const SLOW_OPERATION_TIMEOUT: Duration = Duration::from_secs(10);

    /// Standard delay before triggering cancellation
    pub const CANCELLATION_DELAY: Duration = Duration::from_millis(500);

    /// Create test metadata with cancellation tracking
    pub fn create_cancellation_metadata() -> HashMap<String, String> {
        let mut metadata = HashMap::new();
        metadata.insert("test_type".to_string(), "cancellation_e2e".to_string());
        metadata.insert(
            "test_id".to_string(),
            format!(
                "cancel_test_{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ),
        );
        metadata
    }

    /// Assert that cancellation happened within reasonable time
    pub fn assert_cancellation_timing(elapsed: Duration, operation: &str) {
        assert!(
            elapsed < SLOW_OPERATION_TIMEOUT,
            "{operation} should be cancelled before full timeout, took {elapsed:?}",
        );

        // If it completed very quickly, it was likely cancelled
        if elapsed < Duration::from_secs(2) {
            eprintln!("✓ {operation} was cancelled quickly in {elapsed:?}");
        } else {
            eprintln!("⚠️ {operation} took {elapsed:?}, may have completed before cancellation",);
        }
    }

    /// Check if error message indicates cancellation
    pub fn is_cancellation_error(error_msg: &str) -> bool {
        let cancellation_indicators = ["cancelled", "canceled", "abort", "interrupt", "timeout"];

        let lower_msg = error_msg.to_lowercase();
        cancellation_indicators
            .iter()
            .any(|&indicator| lower_msg.contains(indicator))
    }

    /// Test runner types that support full cancellation
    pub enum CancellationCapableRunner {
        Command,
        HttpRequest,
        PythonCommand,
        Docker,
        LlmCompletion,
        LlmChat,
    }

    impl CancellationCapableRunner {
        pub fn name(&self) -> &'static str {
            match self {
                Self::Command => "COMMAND",
                Self::HttpRequest => "HTTP_REQUEST",
                Self::PythonCommand => "PYTHON_COMMAND",
                Self::Docker => "DOCKER",
                Self::LlmCompletion => "LLM_COMPLETION",
                Self::LlmChat => "LLM_CHAT",
            }
        }

        pub fn all() -> Vec<Self> {
            vec![
                Self::Command,
                Self::HttpRequest,
                Self::PythonCommand,
                Self::Docker,
                Self::LlmCompletion,
                Self::LlmChat,
            ]
        }
    }

    /// Test runner types with limited cancellation support
    pub enum LimitedCancellationRunner {
        Mcp,
        GrpcUnary,
        Slack,
    }

    impl LimitedCancellationRunner {
        pub fn name(&self) -> &'static str {
            match self {
                Self::Mcp => "MCP",
                Self::GrpcUnary => "GRPC_UNARY",
                Self::Slack => "SLACK",
            }
        }

        pub fn all() -> Vec<Self> {
            vec![Self::Mcp, Self::GrpcUnary, Self::Slack]
        }
    }
}
