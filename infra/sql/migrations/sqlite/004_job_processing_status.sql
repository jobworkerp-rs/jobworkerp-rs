-- JobProcessingStatus RDB Index Table Creation
-- Sprint 3: JobService拡張 + JobProcessingStatus RDBインデックス機能
--
-- Purpose: Enable advanced job status search in large-scale environments (100万+ jobs)
-- Default: Disabled (JOB_STATUS_RDB_INDEXING=false for hobby use)

CREATE TABLE IF NOT EXISTS `job_processing_status` (
    -- Primary key
    `job_id` BIGINT PRIMARY KEY,

    -- Job status (PENDING=1, RUNNING=2, WAIT_RESULT=3, CANCELLING=4)
    `status` INT NOT NULL,

    -- Job attributes (for filtering)
    `worker_id` BIGINT NOT NULL,
    `channel` TEXT NOT NULL,
    `priority` INT NOT NULL,
    `enqueue_time` BIGINT NOT NULL,

    -- Timestamp information
    `pending_time` BIGINT,  -- Time when job entered PENDING state (milliseconds)
    `start_time` BIGINT,    -- Time when job entered RUNNING state (milliseconds)

    -- Real-time output availability
    `is_streamable` BOOLEAN NOT NULL DEFAULT 0,      -- Enqueued via EnqueueForStream
    `broadcast_results` BOOLEAN NOT NULL DEFAULT 0,  -- Worker's broadcast_results setting

    -- Metadata
    `version` BIGINT NOT NULL,  -- Optimistic locking version number
    `deleted_at` BIGINT,        -- Logical deletion timestamp (NULL: active, NOT NULL: deleted)
    `updated_at` BIGINT NOT NULL  -- Last update timestamp (for sync delay detection)
);

-- Indexes with partial WHERE clause for active records only
CREATE INDEX IF NOT EXISTS idx_jps_status_active
    ON job_processing_status(status)
    WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_jps_worker_id_active
    ON job_processing_status(worker_id)
    WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_jps_channel_active
    ON job_processing_status(channel)
    WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_jps_start_time_active
    ON job_processing_status(start_time)
    WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_jps_status_start
    ON job_processing_status(status, start_time DESC)
    WHERE deleted_at IS NULL;

-- No ANALYZE needed for SQLite
