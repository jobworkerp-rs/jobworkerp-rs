CREATE TABLE IF NOT EXISTS `job_execution_overrides` (
  `job_id` BIGINT PRIMARY KEY,
  `response_type` INT DEFAULT NULL,
  `store_success` TINYINT(1) DEFAULT NULL,
  `store_failure` TINYINT(1) DEFAULT NULL,
  `broadcast_results` TINYINT(1) DEFAULT NULL,
  `retry_type` INT DEFAULT NULL,
  `retry_interval` INT UNSIGNED DEFAULT NULL,
  `retry_max_interval` INT UNSIGNED DEFAULT NULL,
  `retry_max_retry` INT UNSIGNED DEFAULT NULL,
  `retry_basis` FLOAT DEFAULT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Index for orphan cleanup NOT EXISTS queries in delete_bulk/delete
-- (base schema has job_id_key(job_id, end_time) which covers this,
--  but migration-only setups need an explicit index)
CREATE INDEX idx_job_result_job_id ON job_result(job_id);
