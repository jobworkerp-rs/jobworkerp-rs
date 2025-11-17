-- Add created_at columns to runner and worker tables
-- MySQL does not support DEFAULT with function expressions in older versions,
-- so we use 0 as default and update existing records with current timestamp

START TRANSACTION;

-- Add created_at column to runner table
ALTER TABLE runner ADD COLUMN created_at BIGINT(20) NOT NULL DEFAULT 0;

-- Add created_at column to worker table
ALTER TABLE worker ADD COLUMN created_at BIGINT(20) NOT NULL DEFAULT 0;

-- Update existing records with migration execution timestamp (milliseconds)
-- Note: New records should set created_at explicitly in application code
UPDATE runner SET created_at = UNIX_TIMESTAMP() * 1000 WHERE created_at = 0;
UPDATE worker SET created_at = UNIX_TIMESTAMP() * 1000 WHERE created_at = 0;

-- Create indexes for created_at columns
-- Note: MySQL does not support IF NOT EXISTS for CREATE INDEX (SQLite only)
-- This migration assumes indexes do not exist yet
CREATE INDEX idx_runner_created_at ON runner(created_at);
CREATE INDEX idx_worker_created_at ON worker(created_at);

-- Update table statistics for query optimizer
ANALYZE TABLE runner;
ANALYZE TABLE worker;

COMMIT;
