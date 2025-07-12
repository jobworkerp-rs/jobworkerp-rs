use app::app::job::constants::cancellation::{
    RUNNING_JOB_CLEANUP_INTERVAL_SECONDS, RUNNING_JOB_GRACE_PERIOD_MS,
};
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::runner::RunnerTrait;
use proto::jobworkerp::data::JobId;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};

/// Running job entry with TTL (prevents memory leak)
#[derive(Clone)]
pub struct RunningJobEntry {
    pub runner: Arc<Mutex<Box<dyn RunnerTrait + Send + Sync>>>,
    pub registered_at: Instant,
    pub job_timeout_ms: u64, // Job-specific timeout
    pub is_static: bool,     // Whether this is a static runner
}

impl RunningJobEntry {
    pub fn new(
        runner: Arc<Mutex<Box<dyn RunnerTrait + Send + Sync>>>,
        job_timeout_ms: u64,
        is_static: bool,
    ) -> Self {
        Self {
            runner,
            registered_at: Instant::now(),
            job_timeout_ms,
            is_static,
        }
    }

    /// Check if the entry is expired (supports per-job timeout)
    pub fn is_expired(&self) -> bool {
        let ttl = if self.job_timeout_ms > 0 {
            // Use job-specific timeout + grace period
            Some(Duration::from_millis(
                self.job_timeout_ms + RUNNING_JOB_GRACE_PERIOD_MS,
            ))
        } else {
            None
        };

        let elapsed = self.registered_at.elapsed();

        // Debug info: detailed log for expiration check
        if ttl.is_some_and(|t| elapsed > t) {
            tracing::debug!(
                "Job entry expired: elapsed={}ms, ttl={}ms (job_timeout={}ms + grace={}ms)",
                elapsed.as_millis(),
                ttl.map(|t| t.as_millis()).unwrap_or_default(),
                self.job_timeout_ms,
                RUNNING_JOB_GRACE_PERIOD_MS
            );
            true
        } else {
            false
        }
    }

    /// Get remaining time until expiration (for debugging)
    pub fn time_to_expire(&self) -> Option<Duration> {
        let ttl = if self.job_timeout_ms > 0 {
            Some(Duration::from_millis(
                self.job_timeout_ms + RUNNING_JOB_GRACE_PERIOD_MS,
            ))
        } else {
            None
        };

        let elapsed = self.registered_at.elapsed();
        ttl.map(|t| t - elapsed)
    }
}

/// Running job manager (shared for Redis/Memory)
pub struct RunningJobManager {
    running_jobs: Arc<RwLock<HashMap<i64, RunningJobEntry>>>,
    cleanup_started: Arc<AtomicBool>,
}

impl Default for RunningJobManager {
    fn default() -> Self {
        Self::new()
    }
}

impl RunningJobManager {
    pub fn new() -> Self {
        Self {
            running_jobs: Arc::new(RwLock::new(HashMap::new())),
            cleanup_started: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Register a running job
    pub async fn register_running_job(
        &self,
        job_id: &JobId,
        runner: Arc<Mutex<Box<dyn RunnerTrait + Send + Sync>>>,
        job_timeout_ms: u64,
        is_static: bool,
    ) {
        let entry = RunningJobEntry::new(runner, job_timeout_ms, is_static);
        self.running_jobs.write().await.insert(job_id.value, entry);

        tracing::debug!(
            "Registered running job {} (timeout={}ms, static={})",
            job_id.value,
            job_timeout_ms,
            is_static
        );
    }

    /// Unregister a running job
    pub async fn unregister_running_job(&self, job_id: &JobId) -> bool {
        let removed = self
            .running_jobs
            .write()
            .await
            .remove(&job_id.value)
            .is_some();
        if removed {
            tracing::debug!("Unregistered running job {}", job_id.value);
        }
        removed
    }

    /// Execute cancellation for the specified job
    pub async fn cancel_running_job(&self, job_id: &JobId) -> Result<bool, JobWorkerError> {
        if let Some(entry) = self.running_jobs.read().await.get(&job_id.value) {
            let runner = entry.runner.clone();
            let job_id_clone = *job_id;
            let running_jobs = self.running_jobs.clone();

            // Call Runner::cancel() and remove from management
            tokio::spawn(async move {
                runner.lock().await.cancel().await;
                tracing::info!("Successfully cancelled running job {}", job_id_clone.value);

                // After cancellation, remove from management
                if running_jobs
                    .write()
                    .await
                    .remove(&job_id_clone.value)
                    .is_some()
                {
                    tracing::debug!(
                        "Removed cancelled job {} from running job manager",
                        job_id_clone.value
                    );
                }
            });

            Ok(true)
        } else {
            tracing::debug!(
                "Job {} not found in running jobs, may have completed",
                job_id.value
            );
            Ok(false)
        }
    }

    /// Get the number of running jobs (for monitoring/debugging)
    pub async fn get_running_job_count(&self) -> usize {
        self.running_jobs.read().await.len()
    }

    /// Start background cleanup task
    pub fn start_cleanup_task(&self) {
        // Do nothing if cleanup task has already started
        if self
            .cleanup_started
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            tracing::debug!("Cleanup task already started");
            return;
        }

        let running_jobs = self.running_jobs.clone();
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(Duration::from_secs(RUNNING_JOB_CLEANUP_INTERVAL_SECONDS));

            loop {
                interval.tick().await;

                let mut jobs = running_jobs.write().await;
                let initial_count = jobs.len();

                jobs.retain(|job_id, entry| {
                    if entry.is_expired() {
                        tracing::warn!(
                            "Background cleanup: expired running job {} (registered: {:?}, timeout: {}ms)", 
                            job_id, entry.registered_at, entry.job_timeout_ms
                        );
                        false
                    } else {
                        true
                    }
                });

                let cleaned_count = initial_count - jobs.len();
                if cleaned_count > 0 {
                    tracing::info!(
                        "Background cleanup: removed {} expired running job entries",
                        cleaned_count
                    );
                }

                drop(jobs); // Explicitly release the lock
            }
        });

        tracing::info!("Started background cleanup task for running jobs");
    }
}

/// Trait for accessing RunningJobManager
pub trait UseRunningJobManager {
    fn running_job_manager(&self) -> &RunningJobManager;
}
