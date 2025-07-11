use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::JobId;
use jobworkerp_runner::runner::RunnerTrait;
use app::app::job::constants::cancellation::{
    RUNNING_JOB_DEFAULT_TTL_SECONDS, RUNNING_JOB_GRACE_PERIOD_MS, 
    RUNNING_JOB_CLEANUP_INTERVAL_SECONDS
};

/// TTL付き実行中ジョブエントリ（メモリリーク防止）
#[derive(Clone)]
pub struct RunningJobEntry {
    pub runner: Arc<Mutex<Box<dyn RunnerTrait + Send + Sync>>>,
    pub registered_at: Instant,
    pub job_timeout_ms: u64, // ジョブ固有のタイムアウト
    pub is_static: bool, // static runnerかどうか
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
    
    /// エントリが期限切れかチェック（Job個別timeout対応）
    pub fn is_expired(&self) -> bool {
        let ttl = if self.job_timeout_ms > 0 {
            // Job個別タイムアウト + 余裕時間を使用
            Duration::from_millis(self.job_timeout_ms + RUNNING_JOB_GRACE_PERIOD_MS)
        } else {
            // timeout未指定（無制限）の場合はデフォルトTTL使用
            Duration::from_secs(RUNNING_JOB_DEFAULT_TTL_SECONDS)
        };
        
        let elapsed = self.registered_at.elapsed();
        
        // デバッグ情報：期限切れ判定の詳細ログ
        if elapsed > ttl {
            tracing::debug!(
                "Job entry expired: elapsed={}ms, ttl={}ms (job_timeout={}ms + grace={}ms)", 
                elapsed.as_millis(), 
                ttl.as_millis(),
                self.job_timeout_ms,
                RUNNING_JOB_GRACE_PERIOD_MS
            );
            true
        } else {
            false
        }
    }
    
    /// 期限までの残り時間を取得（デバッグ用）
    pub fn time_to_expire(&self) -> Option<Duration> {
        let ttl = if self.job_timeout_ms > 0 {
            Duration::from_millis(self.job_timeout_ms + RUNNING_JOB_GRACE_PERIOD_MS)
        } else {
            Duration::from_secs(RUNNING_JOB_DEFAULT_TTL_SECONDS)
        };
        
        let elapsed = self.registered_at.elapsed();
        if elapsed < ttl {
            Some(ttl - elapsed)
        } else {
            None
        }
    }
}

/// 実行中ジョブ管理（Redis/Memory共通）
pub struct RunningJobManager {
    running_jobs: Arc<RwLock<HashMap<i64, RunningJobEntry>>>,
    cleanup_started: Arc<AtomicBool>,
}

impl RunningJobManager {
    pub fn new() -> Self {
        Self {
            running_jobs: Arc::new(RwLock::new(HashMap::new())),
            cleanup_started: Arc::new(AtomicBool::new(false)),
        }
    }
    
    /// 実行中ジョブの登録
    pub async fn register_running_job(
        &self,
        job_id: &JobId,
        runner: Arc<Mutex<Box<dyn RunnerTrait + Send + Sync>>>,
        job_timeout_ms: u64,
        is_static: bool,
    ) {
        let entry = RunningJobEntry::new(runner, job_timeout_ms, is_static);
        self.running_jobs.write().await.insert(job_id.value, entry);
        
        tracing::debug!("Registered running job {} (timeout={}ms, static={})", 
                       job_id.value, job_timeout_ms, is_static);
    }

    /// 実行中ジョブの登録解除
    pub async fn unregister_running_job(&self, job_id: &JobId) -> bool {
        let removed = self.running_jobs.write().await.remove(&job_id.value).is_some();
        if removed {
            tracing::debug!("Unregistered running job {}", job_id.value);
        }
        removed
    }

    /// 指定ジョブのキャンセル実行
    pub async fn cancel_running_job(&self, job_id: &JobId) -> Result<bool, JobWorkerError> {
        if let Some(entry) = self.running_jobs.read().await.get(&job_id.value) {
            let runner = entry.runner.clone();
            let job_id_clone = job_id.clone();
            let running_jobs = self.running_jobs.clone();
            
            // Runner::cancel()を呼び出し + 管理対象から除去
            tokio::spawn(async move {
                runner.lock().await.cancel().await;
                tracing::info!("Successfully cancelled running job {}", job_id_clone.value);
                
                // キャンセル処理後、管理対象から除去
                if running_jobs.write().await.remove(&job_id_clone.value).is_some() {
                    tracing::debug!("Removed cancelled job {} from running job manager", job_id_clone.value);
                }
            });
            
            Ok(true)
        } else {
            tracing::debug!("Job {} not found in running jobs, may have completed", job_id.value);
            Ok(false)
        }
    }

    /// 実行中ジョブ数の取得（監視・デバッグ用）
    pub async fn get_running_job_count(&self) -> usize {
        self.running_jobs.read().await.len()
    }

    /// バックグラウンドクリーンアップタスク開始
    pub fn start_cleanup_task(&self) {
        // 既にクリーンアップタスクが開始されている場合は何もしない
        if self.cleanup_started.compare_exchange(
            false, 
            true, 
            Ordering::Acquire,
            Ordering::Relaxed
        ).is_err() {
            tracing::debug!("Cleanup task already started");
            return;
        }

        let running_jobs = self.running_jobs.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(
                Duration::from_secs(RUNNING_JOB_CLEANUP_INTERVAL_SECONDS)
            );
            
            loop {
                interval.tick().await;
                
                let mut jobs = running_jobs.write().await;
                let initial_count = jobs.len();
                
                jobs.retain(|job_id, entry| {
                    if entry.is_expired() {
                        tracing::warn!("Background cleanup: expired running job {} (registered: {:?}, timeout: {}ms)", 
                                      job_id, entry.registered_at, entry.job_timeout_ms);
                        false
                    } else {
                        true
                    }
                });
                
                let cleaned_count = initial_count - jobs.len();
                if cleaned_count > 0 {
                    tracing::info!("Background cleanup: removed {} expired running job entries", cleaned_count);
                }
                
                drop(jobs); // 明示的にlockを解放
            }
        });
        
        tracing::info!("Started background cleanup task for running jobs");
    }
}

/// RunningJobManagerへのアクセストレイト
pub trait UseRunningJobManager {
    fn running_job_manager(&self) -> &RunningJobManager;
}