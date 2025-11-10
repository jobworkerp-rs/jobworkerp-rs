-- Add created_at columns to runner and worker tables
-- SQLite does not support DEFAULT with function expressions, so we use 0 as default
-- and update existing records with current timestamp

BEGIN TRANSACTION;

-- Add created_at column to runner table
ALTER TABLE runner ADD COLUMN created_at BIGINT NOT NULL DEFAULT 0;

-- Add created_at column to worker table
ALTER TABLE worker ADD COLUMN created_at BIGINT NOT NULL DEFAULT 0;

-- Update existing records with migration execution timestamp (milliseconds)
-- SQLite: strftime('%s', 'now') returns seconds, multiply by 1000 for milliseconds
-- Note: New records should set created_at explicitly in application code
UPDATE runner SET created_at = CAST(strftime('%s', 'now') AS INTEGER) * 1000 WHERE created_at = 0;
UPDATE worker SET created_at = CAST(strftime('%s', 'now') AS INTEGER) * 1000 WHERE created_at = 0;

-- Create indexes for created_at columns
CREATE INDEX IF NOT EXISTS idx_runner_created_at ON runner(created_at);
CREATE INDEX IF NOT EXISTS idx_worker_created_at ON worker(created_at);

COMMIT;
