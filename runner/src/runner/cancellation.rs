//! Job cancellation monitoring module for runners
//!
//! This module provides cancellation monitoring capabilities for runners through pubsub.
//! It solves the lock contention problem in job cancellation by allowing runners to monitor
//! cancellation requests internally rather than relying on external cancel() calls.

use anyhow::Result;
use async_trait::async_trait;
use proto::jobworkerp::data::{JobData, JobId, JobResult};

use crate::runner::cancellation_helper::CancelMonitoringHelper;
use crate::runner::RunnerTrait;

/// Result of cancellation monitoring setup
#[derive(Debug)]
pub enum CancellationSetupResult {
    /// Successfully set up monitoring and started monitoring
    MonitoringStarted,
    /// Job was already cancelled, immediate cancellation processing required
    AlreadyCancelled,
}

/// RunnerCancellationManager trait for DI integration
/// This trait provides an interface for cancellation monitoring without dependencies
#[async_trait]
pub trait RunnerCancellationManager: Send + Sync + std::fmt::Debug {
    /// Setup cancellation monitoring for a specific job
    async fn setup_monitoring(
        &mut self,
        job_id: &JobId,
        job_data: &JobData,
    ) -> Result<CancellationSetupResult>;

    /// Cleanup cancellation monitoring
    async fn cleanup_monitoring(&mut self) -> Result<()>;

    /// Get cancellation token directly from Manager
    /// Returns pubsub-integrated token to enable distributed cancellation monitoring
    async fn get_token(&self) -> tokio_util::sync::CancellationToken;

    /// Check if token is cancelled
    /// Provides quick cancellation state check without async overhead
    fn is_cancelled(&self) -> bool;
}

/// Simple mixin trait for adding cancellation monitoring to runners
///
/// This is a simplified version that avoids complex dependencies and circular references.
/// The actual cancellation monitoring implementation will be handled at the JobRunner level.
#[async_trait]
pub trait CancelMonitoring: Send + Sync {
    /// Initialize cancellation monitoring for specific job
    /// Returns Some(JobResult) if the job should be cancelled immediately, None to continue execution
    async fn setup_cancellation_monitoring(
        &mut self,
        job_id: JobId,
        job_data: &JobData,
    ) -> Result<Option<JobResult>>;

    /// Cleanup cancellation monitoring
    async fn cleanup_cancellation_monitoring(&mut self) -> Result<()>;

    /// Complete state reset for pool recycling
    /// Fully resets CancellationManager state when returning to pool to prevent state contamination for next job
    async fn reset_for_pooling(&mut self) -> Result<()> {
        // Default implementation: cleanup_cancellation_monitoring() + logging
        self.cleanup_cancellation_monitoring().await?;
        tracing::debug!("CancelMonitoring reset for pooling (default implementation)");
        Ok(())
    }
}

/// Type-safe integration trait for Runners  
/// Avoids the dangers of downcast_mut()
/// Renamed from CancelMonitoringCapable to provide unified interface
pub trait CancellableRunner: RunnerTrait + CancelMonitoring {
    /// Type-safe access to cancel monitoring functionality
    fn as_cancel_monitoring(&mut self) -> &mut dyn CancelMonitoring;

    /// Clone cancel helper for streaming operations
    /// Used for use_static=false cases where stream lifetime needs extension
    fn clone_cancel_helper_for_stream(&self) -> Option<CancelMonitoringHelper> {
        None // Default implementation: no cancellation monitoring
    }
}

// Provide blanket implementation with streaming support
use crate::runner::cancellation_helper::UseCancelMonitoringHelper;

impl<T> CancellableRunner for T
where
    T: RunnerTrait + CancelMonitoring + UseCancelMonitoringHelper,
{
    fn as_cancel_monitoring(&mut self) -> &mut dyn CancelMonitoring {
        self
    }

    /// Clone cancel helper for streaming operations
    /// This implementation leverages UseCancelMonitoringHelper for type-safe access
    fn clone_cancel_helper_for_stream(&self) -> Option<CancelMonitoringHelper> {
        self.cancel_monitoring_helper().cloned()
    }
}
