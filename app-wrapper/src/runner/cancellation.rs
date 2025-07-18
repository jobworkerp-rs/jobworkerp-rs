//! Job cancellation monitoring module for workers
//!
//! This module provides cancellation monitoring capabilities integrated with AppModule.
//! It solves the lock contention problem by leveraging infra layer repository integration.

use anyhow::Result;
use infra::infra::job::queue::JobQueueCancellationRepository;
use proto::jobworkerp::data::JobId;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// Mixin trait for accessing RunnerCancellationManager
pub trait UseRunnerCancellationManager {
    fn runner_cancellation_manager(&self) -> &RunnerCancellationManager;
    fn runner_cancellation_manager_mut(&mut self) -> &mut RunnerCancellationManager;
}

// Import for CancellationHelper integration
use jobworkerp_runner::runner::common::cancellation_helper::CancellationHelper;
use jobworkerp_runner::runner::cancellation::RunnerCancellationManager as RunnerCancellationManagerTrait;

/// キャンセル監視のセットアップ結果
#[derive(Debug)]
pub enum CancellationSetupResult {
    /// 正常にセットアップ完了（監視開始）
    MonitoringStarted,
    /// 既にキャンセル済み（即座にキャンセル処理が必要）
    AlreadyCancelled,
}

/// cleanup結果の詳細情報
#[derive(Debug)]
pub enum CleanupResult {
    Success,
    TaskError(tokio::task::JoinError),
    Timeout, // インフラレイヤーのタイムアウトで自動解決
}

/// pubsubを使ったキャンセル監視のマネージャー（AppModule統合版）
/// worker-app層でのAppModuleからの依存注入により、infra層への完全統合を実現
#[derive(Debug)]
pub struct RunnerCancellationManager {
    cancellation_repository: Option<Arc<dyn JobQueueCancellationRepository>>,
    job_id: Option<JobId>,
    pubsub_task_handle: Option<tokio::task::JoinHandle<()>>,
    cleanup_sender: Option<oneshot::Sender<()>>,
}

impl Default for RunnerCancellationManager {
    fn default() -> Self {
        Self::new()
    }
}

impl RunnerCancellationManager {
    /// 空のマネージャーを作成（後でリポジトリを設定）
    pub fn new() -> Self {
        Self {
            cancellation_repository: None,
            job_id: None,
            pubsub_task_handle: None,
            cleanup_sender: None,
        }
    }

    /// AppModuleからリポジトリを注入
    pub fn new_with_repository(
        cancellation_repository: Arc<dyn JobQueueCancellationRepository>,
    ) -> Self {
        Self {
            cancellation_repository: Some(cancellation_repository),
            job_id: None,
            pubsub_task_handle: None,
            cleanup_sender: None,
        }
    }

    /// リポジトリを設定
    pub fn set_repository(&mut self, repository: Arc<dyn JobQueueCancellationRepository>) {
        self.cancellation_repository = Some(repository);
    }

    /// 指定ジョブのキャンセル監視を開始（完全なpubsub統合版）
    pub async fn setup_monitoring(
        &mut self,
        job_id: &JobId,
        job_timeout_ms: u64,
        cancellation_helper: &mut CancellationHelper,
    ) -> Result<CancellationSetupResult> {
        // リポジトリが設定されているかチェック
        if self.cancellation_repository.is_none() {
            tracing::warn!("RunnerCancellationManager has no repository set, skipping monitoring");
            return Ok(CancellationSetupResult::MonitoringStarted); // Changed from AlreadyCancelled
        }

        self.job_id = Some(*job_id);

        // CancellationHelperからトークンを取得
        let execution_token = match cancellation_helper.setup_execution_token() {
            Ok(token) => token,
            Err(_) => {
                tracing::info!(
                    "CancellationHelper is already cancelled for job {}, skipping monitoring",
                    job_id.value
                );
                return Ok(CancellationSetupResult::AlreadyCancelled);
            }
        };

        let (cleanup_sender, cleanup_receiver) = oneshot::channel();
        self.cleanup_sender = Some(cleanup_sender);

        // 完全なpubsub統合実装
        let handle = self
            .start_pubsub_listener(job_id, job_timeout_ms, execution_token, cleanup_receiver)
            .await;
        self.pubsub_task_handle = Some(handle);

        tracing::debug!("Started cancellation monitoring for job {}", job_id.value);
        Ok(CancellationSetupResult::MonitoringStarted)
    }

    /// キャンセル監視のクリーンアップ
    pub async fn cleanup_monitoring(&mut self) -> Result<()> {
        // cleanup信号を送信してsubscribe loopを停止
        if let Some(sender) = self.cleanup_sender.take() {
            if sender.send(()).is_err() {
                tracing::warn!("Failed to send cleanup signal - receiver may have been dropped");
            }
        }

        // タスクの完了を待機
        if let Some(handle) = self.pubsub_task_handle.take() {
            Self::safe_cleanup_task(handle, "Cancellation monitoring").await;
        }

        self.job_id = None;

        tracing::debug!("Cleaned up cancellation monitoring");
        Ok(())
    }

    /// 完全なpubsub統合実装
    async fn start_pubsub_listener(
        &self,
        job_id: &JobId,
        job_timeout_ms: u64,
        execution_token: CancellationToken,
        cleanup_receiver: oneshot::Receiver<()>,
    ) -> tokio::task::JoinHandle<()> {
        let repository = self.cancellation_repository.clone().unwrap();
        let target_job_id = *job_id;

        tokio::spawn(async move {
            tracing::debug!(
                "Started pubsub listener task for job {}",
                target_job_id.value
            );
            
            tracing::debug!("About to call subscribe_job_cancellation_with_timeout for job {}", target_job_id.value);

            // 実際のpubsub統合
            let result = repository
                .subscribe_job_cancellation_with_timeout(
                    Box::new(move |received_job_id| {
                        let token = execution_token.clone();
                        Box::pin(async move {
                            tracing::debug!(
                                "Received pubsub cancellation notification for job {} (target: {})",
                                received_job_id.value, target_job_id.value
                            );
                            if received_job_id == target_job_id {
                                tracing::info!(
                                    "Received cancellation request for job {} in Runner - CANCELLING TOKEN",
                                    received_job_id.value
                                );
                                token.cancel(); // CancellationHelperのトークンをキャンセル
                            } else {
                                tracing::debug!(
                                    "Received cancellation for different job {} (target: {})",
                                    received_job_id.value, target_job_id.value
                                );
                            }
                            Ok(())
                        })
                    }),
                    job_timeout_ms,
                    cleanup_receiver,
                )
                .await;

            if let Err(e) = result {
                tracing::error!("Error in cancellation subscription: {:?}", e);
            }

            tracing::debug!(
                "Pubsub listener task terminated for job {}",
                target_job_id.value
            );
        })
    }

    /// cleanup処理を安全に実行
    async fn safe_cleanup_task(
        task_handle: tokio::task::JoinHandle<()>,
        task_name: &str,
    ) -> CleanupResult {
        match tokio::time::timeout(Duration::from_secs(5), task_handle).await {
            Ok(Ok(_)) => {
                tracing::debug!("{} task completed successfully", task_name);
                CleanupResult::Success
            }
            Ok(Err(join_error)) => {
                tracing::error!("{} task failed: {:?}", task_name, join_error);
                CleanupResult::TaskError(join_error)
            }
            Err(_timeout_error) => {
                tracing::warn!("{} task cleanup timed out - connection will auto-timeout via infrastructure timeout", task_name);
                CleanupResult::Timeout
            }
        }
    }
}

// RunnerCancellationManagerTrait implementation for compatibility with runner layer
#[async_trait::async_trait]
impl RunnerCancellationManagerTrait for RunnerCancellationManager {
    async fn setup_monitoring(
        &mut self,
        job_id: &JobId,
        job_data: &proto::jobworkerp::data::JobData,
        cancellation_helper: &mut CancellationHelper,
    ) -> Result<jobworkerp_runner::runner::cancellation::CancellationSetupResult> {
        let result = self.setup_monitoring(job_id, job_data.timeout, cancellation_helper).await?;
        
        match result {
            CancellationSetupResult::MonitoringStarted => {
                Ok(jobworkerp_runner::runner::cancellation::CancellationSetupResult::MonitoringStarted)
            }
            CancellationSetupResult::AlreadyCancelled => {
                Ok(jobworkerp_runner::runner::cancellation::CancellationSetupResult::AlreadyCancelled)
            }
        }
    }

    async fn cleanup_monitoring(&mut self) -> Result<()> {
        self.cleanup_monitoring().await
    }
}
