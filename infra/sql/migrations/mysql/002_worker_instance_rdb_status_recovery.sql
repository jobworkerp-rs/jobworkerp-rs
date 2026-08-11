-- Add the worker instance ownership data used to recover abandoned RUNNING jobs.
ALTER TABLE job_processing_status
    ADD COLUMN worker_instance_id BIGINT NULL;

-- Supports recovery pagination for one expired instance without scanning all RUNNING rows.
CREATE INDEX idx_jps_recovery_instance_running
    ON job_processing_status(worker_instance_id, status, deleted_at, job_id);
