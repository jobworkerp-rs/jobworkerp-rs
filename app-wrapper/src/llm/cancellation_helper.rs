//! Cancellation helper module for LLM runners
//! Provides common functionality for managing cancellation tokens and avoiding code duplication

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use tokio_util::sync::CancellationToken;

/// Manages cancellation token lifecycle for LLM runners
/// This helper reduces code duplication between chat and completion implementations
#[derive(Debug, Clone)]
pub struct CancellationHelper {
    cancellation_token: Option<CancellationToken>,
}

impl CancellationHelper {
    /// Create a new cancellation helper
    pub fn new() -> Self {
        Self {
            cancellation_token: None,
        }
    }

    /// Set up cancellation token for execution
    /// Returns the token to use for the current execution, or an error if pre-cancelled
    pub fn setup_execution_token(&mut self) -> Result<CancellationToken> {
        let cancellation_token = if let Some(existing_token) = &self.cancellation_token {
            // If token already exists and is cancelled, return early
            if existing_token.is_cancelled() {
                self.cancellation_token = None; // Reset token on early cancellation
                return Err(anyhow!("LLM execution was cancelled before start"));
            }
            existing_token.clone()
        } else {
            let new_token = CancellationToken::new();
            self.cancellation_token = Some(new_token.clone());
            new_token
        };

        Ok(cancellation_token)
    }

    /// Clear cancellation token after execution completes
    pub fn clear_token(&mut self) {
        self.cancellation_token = None;
    }

    /// Get current cancellation token if exists
    pub fn get_token(&self) -> Option<&CancellationToken> {
        self.cancellation_token.as_ref()
    }

    /// Cancel current execution
    pub fn cancel(&self) {
        if let Some(token) = &self.cancellation_token {
            token.cancel();
            tracing::info!("LLM execution cancelled");
        } else {
            tracing::warn!("No active LLM execution to cancel");
        }
    }

    /// Set cancellation token (for testing)
    /// This method is package-private to avoid exposing it as a public API
    #[allow(dead_code)]
    pub(crate) fn set_cancellation_token(&mut self, token: CancellationToken) {
        self.cancellation_token = Some(token);
    }
}

impl Default for CancellationHelper {
    fn default() -> Self {
        Self::new()
    }
}

/// Execute a future with cancellation support
/// This macro reduces boilerplate for cancellable operations
#[macro_export]
macro_rules! execute_with_cancellation {
    ($future:expr, $token:expr, $operation_name:expr) => {
        tokio::select! {
            result = $future => result,
            _ = $token.cancelled() => {
                tracing::info!(concat!($operation_name, " was cancelled"));
                return Err(anyhow::anyhow!(concat!($operation_name, " was cancelled")));
            }
        }
    };
}

/// Helper function to handle cancellable execution with proper error handling
pub async fn execute_cancellable<F, T>(
    future: F,
    cancellation_token: &CancellationToken,
    operation_name: &str,
) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    tokio::select! {
        result = future => result,
        _ = cancellation_token.cancelled() => {
            tracing::info!("{} was cancelled", operation_name);
            Err(anyhow!("{} was cancelled", operation_name))
        }
    }
}

/// Handle the result tuple from run() method with cancellation token cleanup
pub fn handle_run_result<T>(
    helper: &mut CancellationHelper,
    result: Result<T>,
    metadata: HashMap<String, String>,
) -> (Result<T>, HashMap<String, String>) {
    // Clear cancellation token after execution
    helper.clear_token();
    (result, metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::sleep;

    #[tokio::test]
    async fn test_cancellation_helper_basic() {
        let mut helper = CancellationHelper::new();

        // Test setting up execution token
        let token = helper.setup_execution_token().unwrap();
        assert!(!token.is_cancelled());

        // Test cancellation
        helper.cancel();
        assert!(token.is_cancelled());

        // Test clearing token
        helper.clear_token();
        assert!(helper.get_token().is_none());
    }

    #[tokio::test]
    async fn test_pre_cancelled_token() {
        let mut helper = CancellationHelper::new();

        // Set a pre-cancelled token
        let cancelled_token = CancellationToken::new();
        cancelled_token.cancel();
        helper.set_cancellation_token(cancelled_token);

        // Should fail with pre-cancelled error
        let result = helper.setup_execution_token();
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("cancelled before start"));
    }

    #[tokio::test]
    async fn test_execute_cancellable() {
        let token = CancellationToken::new();

        // Test successful execution
        let result = execute_cancellable(async { Ok("success") }, &token, "test operation").await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "success");

        // Test cancelled execution
        token.cancel();
        let result = execute_cancellable(
            async {
                sleep(Duration::from_secs(1)).await;
                Ok("should not complete")
            },
            &token,
            "cancelled operation",
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cancelled"));
    }
}
