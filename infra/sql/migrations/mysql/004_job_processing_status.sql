-- MySQL Migration: Add job_processing_status table for RDB indexing feature
-- This feature is disabled by default (JOB_STATUS_RDB_INDEXING=false)
-- Enable for large-scale environments with 1M+ pending jobs

-- Create job_processing_status table
CREATE TABLE IF NOT EXISTS `job_processing_status` (
    -- Basic information
    `job_id` BIGINT PRIMARY KEY,
    `status` INT NOT NULL COMMENT 'PENDING=1, RUNNING=2, WAIT_RESULT=3, CANCELLING=4',

    -- Job attributes (for search)
    `worker_id` BIGINT NOT NULL,
    `channel` VARCHAR(255) NOT NULL,
    `priority` INT NOT NULL,
    `enqueue_time` BIGINT NOT NULL,

    -- Timestamp information
    `pending_time` BIGINT COMMENT 'Timestamp when job entered PENDING state (milliseconds)',
    `start_time` BIGINT COMMENT 'Timestamp when job entered RUNNING state (milliseconds)',

    -- Real-time output capability flags
    `is_streamable` BOOLEAN NOT NULL DEFAULT 0 COMMENT 'Whether job was enqueued via EnqueueForStream',
    `broadcast_results` BOOLEAN NOT NULL DEFAULT 0 COMMENT 'Worker broadcast_results setting',

    -- Metadata
    `version` BIGINT NOT NULL COMMENT 'Optimistic lock version number',
    `deleted_at` BIGINT COMMENT 'Logical deletion timestamp (NULL: active, NOT NULL: deleted)',
    `updated_at` BIGINT NOT NULL COMMENT 'Last update timestamp (for detecting sync delays)'
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Index design for fast search
-- Note: MySQL/MariaDB uses composite indexes including deleted_at column
-- This approach is different from SQLite's partial indexes (WHERE clause)
-- Both achieve the same performance goal: filtering out deleted records efficiently

-- Status-based search (WHERE status = ? AND deleted_at IS NULL)
-- Composite index: (status, deleted_at) allows efficient filtering
CREATE INDEX idx_jps_status_active ON job_processing_status(status, deleted_at);

-- Worker-based search (WHERE worker_id = ? AND deleted_at IS NULL)
CREATE INDEX idx_jps_worker_id_active ON job_processing_status(worker_id, deleted_at);

-- Channel-based search (WHERE channel = ? AND deleted_at IS NULL)
CREATE INDEX idx_jps_channel_active ON job_processing_status(channel, deleted_at);

-- Start time sorting (WHERE deleted_at IS NULL ORDER BY start_time DESC)
-- Index order: (start_time DESC, deleted_at) for efficient sorting
CREATE INDEX idx_jps_start_time_active ON job_processing_status(start_time DESC, deleted_at);

-- Composite index for status + start_time queries
-- (WHERE status = ? AND deleted_at IS NULL ORDER BY start_time DESC)
CREATE INDEX idx_jps_status_start ON job_processing_status(status, start_time DESC, deleted_at);

-- Update table statistics
ANALYZE TABLE job_processing_status;
