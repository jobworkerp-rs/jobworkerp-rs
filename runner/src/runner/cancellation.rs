//! Job cancellation monitoring module for runners
//!
//! This module provides cancellation monitoring capabilities for runners through pubsub.
//! It solves the lock contention problem in job cancellation by allowing runners to monitor
//! cancellation requests internally rather than relying on external cancel() calls.

use anyhow::Result;
use async_trait::async_trait;
use proto::jobworkerp::data::{JobData, JobId, JobResult};

use crate::runner::RunnerTrait;

// Import for CancellationHelper integration
use super::common::cancellation_helper::CancellationHelper;

/// キャンセル監視のセットアップ結果
#[derive(Debug)]
pub enum CancellationSetupResult {
    /// 正常にセットアップ完了（監視開始）
    MonitoringStarted,
    /// 既にキャンセル済み（即座にキャンセル処理が必要）
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
        cancellation_helper: &mut CancellationHelper,
    ) -> Result<CancellationSetupResult>;

    /// Cleanup cancellation monitoring
    async fn cleanup_monitoring(&mut self) -> Result<()>;
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

    /// Pool recycling時の完全状態リセット
    /// Pool返却時にCancellationManager状態を完全にリセットし、次回ジョブでの状態混入を防ぐ
    async fn reset_for_pooling(&mut self) -> Result<()> {
        // デフォルト実装: cleanup_cancellation_monitoring() + ログ出力
        self.cleanup_cancellation_monitoring().await?;
        tracing::debug!("CancelMonitoring reset for pooling (default implementation)");
        Ok(())
    }
}

/// Type-safe integration trait for Runners
/// Avoids the dangers of downcast_mut()
pub trait CancelMonitoringCapable: RunnerTrait + CancelMonitoring {
    fn as_cancel_monitoring(&mut self) -> &mut dyn CancelMonitoring;
}

// Old struct-based implementation removed - now using trait-based approach
