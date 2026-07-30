ALTER TABLE job_processing_status ADD COLUMN worker_instance_id BIGINT NULL;
CREATE INDEX idx_jps_recovery_instance_running
    ON job_processing_status(worker_instance_id, status, deleted_at, job_id);
