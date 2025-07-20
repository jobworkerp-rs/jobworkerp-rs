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
use jobworkerp_runner::runner::cancellation::RunnerCancellationManager as RunnerCancellationManagerTrait;

/// Cancellation monitoring setup result
#[derive(Debug)]
pub enum CancellationSetupResult {
    /// Setup completed successfully (monitoring started)
    MonitoringStarted,
    /// Already cancelled (requires immediate cancellation processing)
    AlreadyCancelled,
}

/// Cleanup result details
#[derive(Debug)]
pub enum CleanupResult {
    Success,
    TaskError(tokio::task::JoinError),
    Timeout, // Auto-resolved by infrastructure layer timeout
}

/// Cancellation monitoring manager using pubsub (AppModule integration version)
/// Achieves complete infra layer integration through AppModule dependency injection at worker-app layer
#[derive(Debug)]
pub struct RunnerCancellationManager {
    cancellation_repository: Option<Arc<dyn JobQueueCancellationRepository>>,
    job_id: Option<JobId>,
    pubsub_task_handle: Option<tokio::task::JoinHandle<()>>,
    cleanup_sender: Option<oneshot::Sender<()>>,

    // Integrate CancellationHelper functionality
    cancellation_token: Option<CancellationToken>,
}

impl Default for RunnerCancellationManager {
    fn default() -> Self {
        Self::new()
    }
}

impl RunnerCancellationManager {
    /// Create empty manager (repository to be set later)
    pub fn new() -> Self {
        Self {
            cancellation_repository: None,
            job_id: None,
            pubsub_task_handle: None,
            cleanup_sender: None,
            cancellation_token: None,
        }
    }

    /// Inject repository from AppModule
    pub fn new_with_repository(
        cancellation_repository: Arc<dyn JobQueueCancellationRepository>,
    ) -> Self {
        Self {
            cancellation_repository: Some(cancellation_repository),
            job_id: None,
            pubsub_task_handle: None,
            cleanup_sender: None,
            cancellation_token: None,
        }
    }

    /// Set repository
    pub fn set_repository(&mut self, repository: Arc<dyn JobQueueCancellationRepository>) {
        self.cancellation_repository = Some(repository);
    }

    /// Internal token management - integrates CancellationHelper functionality
    async fn setup_token(&mut self) -> Result<CancellationToken> {
        let token = if let Some(existing_token) = &self.cancellation_token {
            if existing_token.is_cancelled() {
                self.cancellation_token = None;
                return Err(anyhow::anyhow!("Token was cancelled before start"));
            }
            existing_token.clone()
        } else {
            let new_token = CancellationToken::new();
            self.cancellation_token = Some(new_token.clone());
            new_token
        };
        Ok(token)
    }

    /// Token setting method for testing (forced cancellation)
    #[cfg(any(test, feature = "test-utils"))]
    pub fn set_cancellation_token(&mut self, token: CancellationToken) {
        self.cancellation_token = Some(token);
    }

    /// Start cancellation monitoring for specified job (complete pubsub integration version)
    pub async fn setup_monitoring(
        &mut self,
        job_id: &JobId,
        job_timeout_ms: u64,
    ) -> Result<CancellationSetupResult> {
        // Check if repository is configured
        if self.cancellation_repository.is_none() {
            tracing::warn!("RunnerCancellationManager has no repository set, skipping monitoring");
            return Ok(CancellationSetupResult::MonitoringStarted); // Changed from AlreadyCancelled
        }

        self.job_id = Some(*job_id);

        // Use internal token management
        let execution_token = match self.setup_token().await {
            Ok(token) => token,
            Err(_) => {
                tracing::info!(
                    "Token is already cancelled for job {}, skipping monitoring",
                    job_id.value
                );
                return Ok(CancellationSetupResult::AlreadyCancelled);
            }
        };

        let (cleanup_sender, cleanup_receiver) = oneshot::channel();
        self.cleanup_sender = Some(cleanup_sender);

        // Complete pubsub integration implementation
        let handle = self
            .start_pubsub_listener(job_id, job_timeout_ms, execution_token, cleanup_receiver)
            .await;
        self.pubsub_task_handle = Some(handle);

        tracing::debug!("Started cancellation monitoring for job {}", job_id.value);
        Ok(CancellationSetupResult::MonitoringStarted)
    }

    /// Cleanup cancellation monitoring
    pub async fn cleanup_monitoring(&mut self) -> Result<()> {
        // Send cleanup signal to stop subscribe loop
        if let Some(sender) = self.cleanup_sender.take() {
            if sender.send(()).is_err() {
                tracing::warn!("Failed to send cleanup signal - receiver may have been dropped");
            }
        }

        // Wait for task completion
        if let Some(handle) = self.pubsub_task_handle.take() {
            Self::safe_cleanup_task(handle, "Cancellation monitoring").await;
        }

        self.job_id = None;
        self.cancellation_token = None; // Also clear token state

        tracing::debug!("Cleaned up cancellation monitoring");
        Ok(())
    }

    /// Complete pubsub integration implementation
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

            tracing::debug!(
                "About to call subscribe_job_cancellation_with_timeout for job {}",
                target_job_id.value
            );

            // Actual pubsub integration
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
                                token.cancel(); // Cancel CancellationHelper token
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

    /// Safely execute cleanup processing
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
    ) -> Result<jobworkerp_runner::runner::cancellation::CancellationSetupResult> {
        let result = self.setup_monitoring(job_id, job_data.timeout).await?;

        match result {
            CancellationSetupResult::MonitoringStarted => Ok(
                jobworkerp_runner::runner::cancellation::CancellationSetupResult::MonitoringStarted,
            ),
            CancellationSetupResult::AlreadyCancelled => Ok(
                jobworkerp_runner::runner::cancellation::CancellationSetupResult::AlreadyCancelled,
            ),
        }
    }

    async fn cleanup_monitoring(&mut self) -> Result<()> {
        self.cleanup_monitoring().await
    }

    async fn get_token(&self) -> tokio_util::sync::CancellationToken {
        self.cancellation_token.clone().unwrap_or_default()
    }

    fn is_cancelled(&self) -> bool {
        self.cancellation_token
            .as_ref()
            .is_some_and(|t| t.is_cancelled())
    }
}
