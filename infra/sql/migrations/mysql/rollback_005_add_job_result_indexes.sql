-- Rollback Migration 005: Remove JobResult search indexes
-- Purpose: Rollback indexes added for JobResultService FindListBy, CountBy, and DeleteBulk operations
-- Date: 2025-11-14
-- Related: Sprint 4 - JobResultService extension rollback

-- Drop all indexes added in migration 005
DROP INDEX IF EXISTS idx_job_result_status ON job_result;
DROP INDEX IF EXISTS idx_job_result_start_time ON job_result;
DROP INDEX IF EXISTS idx_job_result_end_time ON job_result;
DROP INDEX IF EXISTS idx_job_result_end_status ON job_result;
DROP INDEX IF EXISTS idx_job_result_worker_end ON job_result;

-- テーブル統計更新
ANALYZE TABLE job_result;
