//! CancelMonitoring shared implementation helper
//!
//! Provides unified cancellation monitoring implementation to eliminate
//! duplicate implementations using DI (Dependency Injection) pattern.

use super::cancellation::{CancellationSetupResult, RunnerCancellationManager};
use anyhow::Result;
use command_utils::util::datetime;
use proto::jobworkerp::data::{
    JobData, JobId, JobResult, JobResultData, ResultOutput, ResultStatus,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Output of a job whose cancellation was requested before it started.
pub const CANCELLED_BEFORE_EXECUTION: &str = "Job was cancelled before execution";

/// Shared cancellation monitoring implementation (Manager required version)
/// Unifies logic that was duplicated across 3 runners
#[derive(Clone, Debug)]
pub struct CancelMonitoringHelper {
    // Manager required - removed Option for improved type safety
    cancellation_manager: Arc<Mutex<Box<dyn RunnerCancellationManager>>>,
}

impl CancelMonitoringHelper {
    /// Constructor requiring Manager
    pub fn new(manager: Box<dyn RunnerCancellationManager>) -> Self {
        Self {
            cancellation_manager: Arc::new(Mutex::new(manager)),
        }
    }

    /// Unified monitoring setup implementation (no fallback needed)
    pub async fn setup_monitoring_impl(
        &mut self,
        job_id: JobId,
        job_data: &JobData,
    ) -> Result<Option<JobResult>> {
        tracing::debug!(
            "Setting up cancellation monitoring for job {}",
            job_id.value
        );

        // Manager existence guaranteed - no Option branching needed
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
                #[allow(deprecated)]
                Ok(Some(JobResult {
                    id: None,
                    data: Some(JobResultData {
                        error: Some(proto::stream_error::cancelled_by_worker(
                            CANCELLED_BEFORE_EXECUTION,
                        )),
                        job_id: Some(job_id),
                        status: ResultStatus::Cancelled as i32,
                        output: Some(ResultOutput {
                            items: CANCELLED_BEFORE_EXECUTION.as_bytes().to_vec(),
                        }),
                        start_time: datetime::now_millis(),
                        end_time: datetime::now_millis(),
                        worker_id: job_data.worker_id,
                        args: job_data.args.clone(),
                        uniq_key: job_data.uniq_key.clone(),
                        retried: job_data.retried,
                        max_retry: 0,
                        priority: job_data.priority,
                        timeout: job_data.timeout,
                        streaming_type: job_data.streaming_type,
                        enqueue_time: job_data.enqueue_time,
                        run_after_time: job_data.run_after_time,
                        response_type: 0,
                        store_success: false,
                        store_failure: false,
                        worker_name: String::new(),
                        using: job_data.using.clone(),
                        broadcast_results: false,
                        resolved_retry_policy: None,
                    }),
                    metadata: HashMap::new(),
                }))
            }
        }
    }

    /// Unified cleanup implementation
    pub async fn cleanup_monitoring_impl(&mut self) -> Result<()> {
        tracing::trace!("Cleaning up cancellation monitoring");

        let mut manager = self.cancellation_manager.lock().await;
        manager.cleanup_monitoring().await?;
        Ok(())
    }

    /// Unified token acquisition (no fallback needed)
    pub async fn get_cancellation_token(&self) -> CancellationToken {
        let manager = self.cancellation_manager.lock().await;
        manager.get_token().await
    }

    /// Reset for pool environment
    pub async fn reset_for_pooling_impl(&mut self) -> Result<()> {
        self.cleanup_monitoring_impl().await?;
        tracing::debug!("CancelMonitoringHelper reset for pooling");
        Ok(())
    }
}

/// Access trait for CancelMonitoringHelper
/// For DI integration with AppModule (Option support)
pub trait UseCancelMonitoringHelper {
    fn cancel_monitoring_helper(&self) -> Option<&CancelMonitoringHelper>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use proto::jobworkerp::data::StreamErrorCode;

    #[derive(Debug)]
    struct AlreadyCancelledManager;

    #[async_trait]
    impl RunnerCancellationManager for AlreadyCancelledManager {
        async fn setup_monitoring(
            &mut self,
            _job_id: &JobId,
            _job_data: &JobData,
        ) -> Result<CancellationSetupResult> {
            Ok(CancellationSetupResult::AlreadyCancelled)
        }

        async fn cleanup_monitoring(&mut self) -> Result<()> {
            Ok(())
        }

        async fn get_token(&self) -> CancellationToken {
            CancellationToken::new()
        }

        fn is_cancelled(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn already_cancelled_job_result_is_classified_as_cancelled() {
        let mut helper = CancelMonitoringHelper::new(Box::new(AlreadyCancelledManager));
        let result = helper
            .setup_monitoring_impl(JobId { value: 1 }, &JobData::default())
            .await
            .unwrap()
            .expect("cancelled result");
        let data = result.data.unwrap();
        assert_eq!(data.status, ResultStatus::Cancelled as i32);
        let error = data.error.expect("classification");
        assert_eq!(error.code, StreamErrorCode::Cancelled as i32);
        assert_eq!(error.origin, proto::stream_error::WORKER_ORIGIN);
    }
}
