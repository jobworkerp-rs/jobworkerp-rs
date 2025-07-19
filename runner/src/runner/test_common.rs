#[cfg(any(test, feature = "test-utils"))]
pub mod mock {
    use super::super::cancellation::{CancellationSetupResult, RunnerCancellationManager};
    use anyhow::Result;
    use async_trait::async_trait;
    use proto::jobworkerp::data::{JobData, JobId};
    use tokio_util::sync::CancellationToken;

    /// テスト用の共通MockCancellationManager
    #[derive(Debug)]
    pub struct MockCancellationManager {
        token: Option<CancellationToken>,
    }

    impl Default for MockCancellationManager {
        fn default() -> Self {
            Self::new()
        }
    }

    impl MockCancellationManager {
        pub fn new_with_token(token: CancellationToken) -> Self {
            Self { token: Some(token) }
        }

        pub fn new() -> Self {
            Self { token: None }
        }
    }

    #[async_trait]
    impl RunnerCancellationManager for MockCancellationManager {
        async fn setup_monitoring(
            &mut self,
            _job_id: &JobId,
            _job_data: &JobData,
        ) -> Result<CancellationSetupResult> {
            Ok(CancellationSetupResult::MonitoringStarted)
        }

        async fn cleanup_monitoring(&mut self) -> Result<()> {
            Ok(())
        }

        async fn get_token(&self) -> CancellationToken {
            self.token.clone().unwrap_or_default()
        }

        fn is_cancelled(&self) -> bool {
            self.token.as_ref().is_some_and(|t| t.is_cancelled())
        }
    }
}
