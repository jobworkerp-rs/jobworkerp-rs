-- Rollback migration 003: Remove created_at columns and triggers from runner and worker tables
-- WARNING: This will remove created_at data. Backup your database before running this script.

BEGIN TRANSACTION;

-- Drop triggers first
DROP TRIGGER IF EXISTS set_runner_created_at;
DROP TRIGGER IF EXISTS set_worker_created_at;

-- Drop indexes
DROP INDEX IF EXISTS idx_runner_created_at;
DROP INDEX IF EXISTS idx_worker_created_at;
DROP INDEX IF EXISTS idx_runner_type;
DROP INDEX IF EXISTS idx_worker_runner_id;
DROP INDEX IF EXISTS idx_worker_channel;
DROP INDEX IF EXISTS idx_worker_periodic_interval;

-- SQLite does not support DROP COLUMN directly in older versions
-- For SQLite 3.35.0+, you can use ALTER TABLE DROP COLUMN
-- For older versions, you need to recreate the table

-- Check SQLite version: SELECT sqlite_version();
-- If version >= 3.35.0, uncomment the following:

-- ALTER TABLE runner DROP COLUMN created_at;
-- ALTER TABLE worker DROP COLUMN created_at;

-- For older SQLite versions, use the following table recreation approach:

-- Runner table recreation
CREATE TABLE runner_backup (
  id BIGINT(10) PRIMARY KEY,
  name VARCHAR(128) NOT NULL,
  description TEXT NOT NULL,
  definition TEXT NOT NULL,
  type INT(10) NOT NULL,
  UNIQUE(name)
);

INSERT INTO runner_backup (id, name, description, definition, type)
SELECT id, name, description, definition, type FROM runner;

DROP TABLE runner;

ALTER TABLE runner_backup RENAME TO runner;

-- Worker table recreation
CREATE TABLE worker_backup (
  id BIGINT(10) PRIMARY KEY AUTOINCREMENT,
  name VARCHAR(128) NOT NULL,
  description TEXT NOT NULL,
  runner_id BIGINT(10) NOT NULL,
  runner_settings MEDIUMBLOB NOT NULL,
  retry_type INT(10) NOT NULL,
  interval INT(10) NOT NULL DEFAULT 0,
  max_interval INT(10) NOT NULL DEFAULT 0,
  max_retry INT(10) NOT NULL DEFAULT 0,
  basis FLOAT(10) NOT NULL DEFAULT 2.0,
  periodic_interval INT(10) NOT NULL DEFAULT 0,
  channel VARCHAR(32) DEFAULT NULL,
  queue_type INT(10) NOT NULL DEFAULT 0,
  response_type INT(10) NOT NULL DEFAULT 0,
  store_success TINYINT(1) NOT NULL DEFAULT 0,
  store_failure TINYINT(1) NOT NULL DEFAULT 0,
  use_static TINYINT(1) NOT NULL DEFAULT 0,
  broadcast_results TINYINT(1) NOT NULL DEFAULT 0,
  UNIQUE(name)
);

INSERT INTO worker_backup (
  id, name, description, runner_id, runner_settings, retry_type,
  interval, max_interval, max_retry, basis, periodic_interval,
  channel, queue_type, response_type, store_success, store_failure,
  use_static, broadcast_results
)
SELECT
  id, name, description, runner_id, runner_settings, retry_type,
  interval, max_interval, max_retry, basis, periodic_interval,
  channel, queue_type, response_type, store_success, store_failure,
  use_static, broadcast_results
FROM worker;

DROP TABLE worker;

ALTER TABLE worker_backup RENAME TO worker;

COMMIT;
