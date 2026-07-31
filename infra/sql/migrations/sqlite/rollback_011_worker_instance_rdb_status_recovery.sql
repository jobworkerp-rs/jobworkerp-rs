DROP INDEX IF EXISTS idx_jps_recovery_instance_running;
ALTER TABLE job_processing_status DROP COLUMN worker_instance_id;
