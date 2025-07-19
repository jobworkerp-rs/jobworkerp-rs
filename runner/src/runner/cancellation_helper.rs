//! CancelMonitoring共通実装ヘルパー
//!
//! DI（Dependency Injection）パターンによる重複実装解消を目的とした
//! キャンセル監視機能の統一実装を提供します。

use super::cancellation::{CancellationSetupResult, RunnerCancellationManager};
use anyhow::Result;
use proto::jobworkerp::data::{JobData, JobId, JobResult};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// キャンセル監視の共通実装（Manager必須版）
/// 3つのRunnerで重複していたロジックを統一
#[derive(Clone, Debug)]
pub struct CancelMonitoringHelper {
    // Manager必須 - Option削除で型安全性向上
    cancellation_manager: Arc<Mutex<Box<dyn RunnerCancellationManager>>>,
}

impl CancelMonitoringHelper {
    /// Manager必須constructor
    pub fn new(manager: Box<dyn RunnerCancellationManager>) -> Self {
        Self {
            cancellation_manager: Arc::new(Mutex::new(manager)),
        }
    }

    /// 統一された監視セットアップ実装（fallback不要）
    pub async fn setup_monitoring_impl(
        &mut self,
        job_id: JobId,
        job_data: &JobData,
    ) -> Result<Option<JobResult>> {
        tracing::debug!(
            "Setting up cancellation monitoring for job {}",
            job_id.value
        );

        // Manager存在保証 - Option分岐不要
        let mut manager = self.cancellation_manager.lock().await;
        let result = manager.setup_monitoring(&job_id, job_data).await?;

        match result {
            CancellationSetupResult::MonitoringStarted => {
                tracing::trace!("Cancellation monitoring started for job {}", job_id.value);
                Ok(None)
            }
            CancellationSetupResult::AlreadyCancelled => {
                tracing::info!(
                    "Job {} was already cancelled before execution",
                    job_id.value
                );
                Ok(None)
            }
        }
    }

    /// 統一されたクリーンアップ実装
    pub async fn cleanup_monitoring_impl(&mut self) -> Result<()> {
        tracing::trace!("Cleaning up cancellation monitoring");

        let mut manager = self.cancellation_manager.lock().await;
        manager.cleanup_monitoring().await?;
        Ok(())
    }

    /// 統一されたtoken取得（fallback不要）
    pub async fn get_cancellation_token(&self) -> CancellationToken {
        let manager = self.cancellation_manager.lock().await;
        manager.get_token().await
    }

    /// Pool環境でのリセット
    pub async fn reset_for_pooling_impl(&mut self) -> Result<()> {
        self.cleanup_monitoring_impl().await?;
        tracing::debug!("CancelMonitoringHelper reset for pooling");
        Ok(())
    }
}

/// CancelMonitoringHelperへのアクセストレイト
/// AppModuleでのDI統合用（Option対応）
pub trait UseCancelMonitoringHelper {
    fn cancel_monitoring_helper(&self) -> Option<&CancelMonitoringHelper>;
    fn cancel_monitoring_helper_mut(&mut self) -> Option<&mut CancelMonitoringHelper>;
}
