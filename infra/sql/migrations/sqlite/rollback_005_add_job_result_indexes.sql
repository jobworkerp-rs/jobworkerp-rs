-- Rollback Migration 005: Remove JobResult search indexes (SQLite)
-- Purpose: Rollback indexes added for JobResultService FindListBy, CountBy, and DeleteBulk operations
-- Date: 2025-11-14
-- Related: Sprint 4 - JobResultService extension rollback

-- Drop all indexes added in migration 005
DROP INDEX IF EXISTS idx_job_result_status;
DROP INDEX IF EXISTS idx_job_result_start_time;
DROP INDEX IF EXISTS idx_job_result_end_time;
DROP INDEX IF EXISTS idx_job_result_end_status;
DROP INDEX IF EXISTS idx_job_result_worker_end;

-- Update statistics
ANALYZE;
