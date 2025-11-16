-- Migration 005: Add JobResult search indexes (SQLite)
-- Purpose: Add indexes for JobResultService FindListBy, CountBy, and DeleteBulk operations
-- Date: 2025-11-14
-- Related: Sprint 4 - JobResultService extension

-- ステータスフィルタ用インデックス
-- Used by: FindListBy with status filter, CountBy with status filter
CREATE INDEX IF NOT EXISTS idx_job_result_status ON job_result(status);

-- 時刻範囲検索用インデックス（開始時刻）
-- Used by: FindListBy with start_time_from/to, CountBy with start_time_from/to
CREATE INDEX IF NOT EXISTS idx_job_result_start_time ON job_result(start_time);

-- 時刻範囲検索用インデックス（終了時刻）
-- Used by: FindListBy with end_time_from/to, CountBy with end_time_from/to, DeleteBulk with end_time_before
CREATE INDEX IF NOT EXISTS idx_job_result_end_time ON job_result(end_time);

-- 一括削除用複合インデックス（終了時刻 + ステータス）
-- Used by: DeleteBulk with end_time_before AND status filter (e.g., delete old SUCCESS results)
-- Rationale: Composite index for efficient bulk deletion with time and status conditions
CREATE INDEX IF NOT EXISTS idx_job_result_end_status ON job_result(end_time, status);

-- Worker別時刻順複合インデックス（Worker ID + 終了時刻 降順）
-- Used by: FindListBy with worker_id filter sorted by end_time DESC (most recent first)
-- Rationale: Composite index for efficient per-worker queries with time-based sorting
-- Note: SQLite supports DESC in index definition
CREATE INDEX IF NOT EXISTS idx_job_result_worker_end ON job_result(worker_id, end_time DESC);

-- SQLite does not support ANALYZE TABLE syntax, using ANALYZE instead
ANALYZE;
