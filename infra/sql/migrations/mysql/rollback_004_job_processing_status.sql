-- Rollback script for JobProcessingStatus RDB Index Table (MySQL)
-- Sprint 3: JobService拡張 + JobProcessingStatus RDBインデックス機能

-- Drop table (indexes are automatically dropped)
DROP TABLE IF EXISTS job_processing_status;
