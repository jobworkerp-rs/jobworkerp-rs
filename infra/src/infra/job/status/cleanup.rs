use super::rdb::RdbJobProcessingStatusIndexRepository;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::interval;

/// JobProcessingStatus cleanup task
///
/// Periodically removes logically deleted records from job_processing_status table.
/// Only runs when JOB_STATUS_RDB_INDEXING=true.
/// Gracefully stops on SIGINT (Ctrl+C) via shutdown receiver.
pub struct JobStatusCleanupTask {
    index_repository: Arc<RdbJobProcessingStatusIndexRepository>,
    cleanup_interval: Duration,
}

impl JobStatusCleanupTask {
    pub fn new(
        index_repository: Arc<RdbJobProcessingStatusIndexRepository>,
        cleanup_interval_hours: u64,
    ) -> Self {
        Self {
            index_repository,
            cleanup_interval: Duration::from_secs(cleanup_interval_hours * 3600),
        }
    }

    /// Spawn cleanup task in background with graceful shutdown support
    ///
    /// # Arguments
    /// * `shutdown_recv` - Shutdown signal receiver (shared with other worker tasks)
    ///
    /// # Returns
    /// JoinHandle that completes when shutdown signal is received
    pub fn spawn(
        self,
        shutdown_recv: tokio::sync::watch::Receiver<bool>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move { self.run(shutdown_recv).await })
    }

    async fn run(self, mut shutdown_recv: tokio::sync::watch::Receiver<bool>) {
        let mut interval = interval(self.cleanup_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        tracing::info!(
            interval_hours = self.cleanup_interval.as_secs() / 3600,
            "JobStatusCleanupTask started"
        );

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    match self.index_repository.cleanup_deleted_records().await {
                        Ok(deleted_count) => {
                            if deleted_count > 0 {
                                tracing::info!(
                                    deleted_count,
                                    "JobStatusCleanupTask: deleted old job_processing_status records"
                                );
                            } else {
                                tracing::debug!("JobStatusCleanupTask: no records to delete");
                            }
                        }
                        Err(e) => {
                            tracing::error!(
                                error = ?e,
                                "JobStatusCleanupTask: failed to cleanup job_processing_status"
                            );
                        }
                    }
                }
                _ = shutdown_recv.changed() => {
                    tracing::info!("JobStatusCleanupTask: shutdown signal received, stopping cleanup task");
                    break;
                }
            }
        }

        tracing::info!("JobStatusCleanupTask stopped gracefully");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use infra_utils::infra::test::{setup_test_rdb_from, TEST_RUNTIME};
    use jobworkerp_base::job_status_config::JobStatusConfig;

    #[test]
    fn test_cleanup_interval_calculation() {
        let interval_hours = 1u64;
        let expected_secs = interval_hours * 3600;
        let duration = Duration::from_secs(expected_secs);

        assert_eq!(duration.as_secs(), 3600);
    }

    #[test]
    fn test_cleanup_task_creation() {
        TEST_RUNTIME.block_on(async {
            let pool = if cfg!(feature = "mysql") {
                setup_test_rdb_from("sql/mysql").await
            } else {
                setup_test_rdb_from("sql/sqlite").await
            };

            let config = JobStatusConfig {
                rdb_indexing_enabled: true,
                cleanup_interval_hours: 2,
                retention_hours: 48,
            };

            let repo = Arc::new(RdbJobProcessingStatusIndexRepository::new(
                Arc::new(pool.clone()),
                Arc::new(config),
            ));

            let task = JobStatusCleanupTask::new(repo, 2);

            assert_eq!(task.cleanup_interval, Duration::from_secs(7200)); // 2 * 3600
        })
    }

    #[test]
    fn test_cleanup_task_graceful_shutdown() {
        TEST_RUNTIME.block_on(async {
            let pool = if cfg!(feature = "mysql") {
                setup_test_rdb_from("sql/mysql").await
            } else {
                setup_test_rdb_from("sql/sqlite").await
            };

            let config = JobStatusConfig {
                rdb_indexing_enabled: true,
                cleanup_interval_hours: 24, // 24 hours (won't trigger in test)
                retention_hours: 48,
            };

            let repo = Arc::new(RdbJobProcessingStatusIndexRepository::new(
                Arc::new(pool.clone()),
                Arc::new(config),
            ));

            let task = JobStatusCleanupTask::new(repo, 24);

            let (shutdown_send, shutdown_recv) = tokio::sync::watch::channel(false);

            // Spawn cleanup task
            let handle = task.spawn(shutdown_recv);

            // Wait a bit to ensure task is running
            tokio::time::sleep(Duration::from_millis(100)).await;

            // Send shutdown signal
            shutdown_send.send(true).unwrap();

            // Wait for task to complete (should stop gracefully)
            let result = tokio::time::timeout(Duration::from_secs(5), handle).await;

            assert!(result.is_ok(), "Cleanup task should stop within 5 seconds");
        })
    }
}
