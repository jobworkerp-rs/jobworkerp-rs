-- Rollback script for JobProcessingStatus RDB Index Table
-- Sprint 3: JobService拡張 + JobProcessingStatus RDBインデックス機能

-- Drop all indexes
DROP INDEX IF EXISTS idx_jps_status_start;
DROP INDEX IF EXISTS idx_jps_start_time_active;
DROP INDEX IF EXISTS idx_jps_channel_active;
DROP INDEX IF EXISTS idx_jps_worker_id_active;
DROP INDEX IF EXISTS idx_jps_status_active;

-- Drop table
DROP TABLE IF EXISTS job_processing_status;
