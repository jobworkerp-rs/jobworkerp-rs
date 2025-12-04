//! Cancellation token management

use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

/// State for tracking cancellation
#[derive(Default)]
pub struct CancelState {
    pub cancelled: bool,
    pub cancel_token: Option<CancellationToken>,
}

impl CancelState {
    /// Reset cancellation state and create a new token
    pub fn reset(&mut self) -> CancellationToken {
        self.cancelled = false;
        let token = CancellationToken::new();
        self.cancel_token = Some(token.clone());
        token
    }

    /// Cancel the current operation
    pub fn cancel(&mut self) -> bool {
        if let Some(token) = &self.cancel_token {
            token.cancel();
            self.cancelled = true;
            true
        } else {
            false
        }
    }

    /// Check if cancelled
    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    /// Get the current cancellation token if any
    pub fn token(&self) -> Option<CancellationToken> {
        self.cancel_token.clone()
    }
}

/// Thread-safe cancel state wrapper
pub type SharedCancelState = Arc<RwLock<CancelState>>;

/// Create a new shared cancel state
pub fn new_shared_cancel_state() -> SharedCancelState {
    Arc::new(RwLock::new(CancelState::default()))
}
