ALTER TABLE job_execution_overrides
    ADD COLUMN expected_runner_id BIGINT(20) DEFAULT NULL;
