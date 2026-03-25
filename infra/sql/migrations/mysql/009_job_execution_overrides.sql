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
