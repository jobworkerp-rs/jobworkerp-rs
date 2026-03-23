CREATE TABLE IF NOT EXISTS `job_execution_overrides` (
  `job_id` INTEGER PRIMARY KEY,
  `response_type` INT DEFAULT NULL,
  `store_success` BOOLEAN DEFAULT NULL,
  `store_failure` BOOLEAN DEFAULT NULL,
  `broadcast_results` BOOLEAN DEFAULT NULL,
  `retry_type` INT DEFAULT NULL,
  `retry_interval` INT DEFAULT NULL,
  `retry_max_interval` INT DEFAULT NULL,
  `retry_max_retry` INT DEFAULT NULL,
  `retry_basis` REAL DEFAULT NULL
);

-- Index for orphan cleanup NOT EXISTS queries in delete_bulk/delete
CREATE INDEX IF NOT EXISTS idx_job_result_job_id ON job_result(job_id);
