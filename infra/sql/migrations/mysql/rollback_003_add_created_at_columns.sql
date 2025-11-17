-- Rollback migration 003: Remove created_at columns and triggers from runner and worker tables
-- WARNING: This will remove created_at data. Backup your database before running this script.

START TRANSACTION;

-- Drop indexes
-- Note: MySQL does not support IF EXISTS for DROP INDEX in older versions
-- If indexes don't exist, these commands will error (expected for partial rollback)
DROP INDEX idx_runner_created_at ON runner;
DROP INDEX idx_worker_created_at ON worker;
DROP INDEX idx_runner_type ON runner;
DROP INDEX idx_worker_runner_id ON worker;
DROP INDEX idx_worker_channel ON worker;
DROP INDEX idx_worker_periodic_interval ON worker;

-- Remove created_at columns
-- MySQL supports DROP COLUMN
ALTER TABLE runner DROP COLUMN created_at;
ALTER TABLE worker DROP COLUMN created_at;

-- Update table statistics
ANALYZE TABLE runner;
ANALYZE TABLE worker;

COMMIT;
