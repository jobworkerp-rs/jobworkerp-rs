ALTER TABLE job_processing_status ADD COLUMN worker_instance_id BIGINT;

-- Supports recovery pagination for one expired instance without scanning all RUNNING rows.
CREATE INDEX idx_jps_recovery_instance_running
    ON job_processing_status(worker_instance_id, status, job_id)
    WHERE deleted_at IS NULL;
