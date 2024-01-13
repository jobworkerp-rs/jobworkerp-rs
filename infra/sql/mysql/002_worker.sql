DROP TABLE IF EXISTS worker;
CREATE TABLE `worker` (
  `id` BIGINT(10) PRIMARY KEY AUTO_INCREMENT,
  `name` VARCHAR(128) NOT NULL,
  `type` INT(10) NOT NULL, -- enum: command, request, k8s
  `operation` MEDIUMTEXT NOT NULL,
  -- retry and timeout setting
  `retry_type` INT(10) NOT NULL,             -- using as enum: exponential, constant or exponential (backoff) or none
  `interval` INT(10) NOT NULL DEFAULT 0,     -- millisecond for retry interval
  `max_interval` INT(10) NOT NULL DEFAULT 0, -- millisecond for max retry interval
  `max_retry`  INT(10) NOT NULL DEFAULT 0,   -- max count for retry until (0: unlimited)
  `basis`  FLOAT(10) NOT NULL DEFAULT 2.0,   -- basis for exponential backoff
  -- periodic setting
  `periodic_interval` INT(10) NOT NULL DEFAULT 0, -- 0 means not periodic, 0 >: millisecond
  -- execution setting
  `channel` VARCHAR(32) DEfAULT NULL,             -- queue channel (null means using default channel)
  `queue_type` INT(10) NOT NULL DEFAULT 0,   -- job queue type (redis or db or hybrid)
  `response_type` INT(10) NOT NULL DEFAULT 0,     -- job response type (none, direct, listen after) (for fast response worker)
  -- logging
  `store_success` TINYINT(1) NOT NULL DEFAULT 0, -- store result to db in success
  `store_failure` TINYINT(1) NOT NULL DEFAULT 0, -- store result to db in failure
  -- etc
  `next_workers` TEXT NOT NULL DEFAULT '',       -- next worker list to process
  `use_static` TINYINT(1) NOT NULL DEFAULT 0, -- use runner as static 
  UNIQUE KEY `name` (`name`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- optional table for db jobqueue
-- TODO create by new channel
DROP TABLE IF EXISTS job;
CREATE TABLE `job` (
  `id` BIGINT(20) PRIMARY KEY, -- the type of snowflake id is i64.
  `worker_id` BIGINT(20) NOT NULL,
  `arg` MEDIUMBLOB,
  `uniq_key` VARCHAR(128) DEFAULT NULL,
  `enqueue_time` BIGINT(20) NOT NULL,
  `grabbed_until_time` BIGINT(20) NOT NULL DEFAULT '0',
  `run_after_time` BIGINT(20) NOT NULL DEFAULT '0',  -- run job after this timestamp (milliseconds).
  `retried` INT(10) NOT NULL DEFAULT '0',
  `priority` INT(10) NOT NULL DEFAULT '0',
  `timeout` BIGINT(20) NOT NULL DEfAULT 0,
  KEY `worker_id_key` (`worker_id`),
  KEY `find_job_key` (`run_after_time`, `grabbed_until_time`, `worker_id`, `priority`),
  KEY `find_job_key2` (`run_after_time`, `grabbed_until_time`, `priority`),
  UNIQUE KEY `uniq_key_idx` (`uniq_key`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

DROP TABLE IF EXISTS job_result;
CREATE TABLE `job_result` (
  `id` BIGINT(20) PRIMARY KEY, -- the type of snowflake id is i64.
  `job_id` BIGINT(20) NOT NULL,
  `worker_id` BIGINT(20) NOT NULL,
  `arg` MEDIUMBLOB NOT NULL,
  `uniq_key` VARCHAR(128) DEFAULT NULL,
  `status` INT(10) DEFAULT NULL,
  `output` MEDIUMBLOB NOT NULL,
  `retried` INT(10) NOT NULL DEFAULT '0',
  `priority` INT(10) NOT NULL DEFAULT '0',
  `enqueue_time` BIGINT(20) NOT NULL,
  `run_after_time` BIGINT(20) NOT NULL,
  `start_time` BIGINT(20) NOT NULL,
  `end_time` BIGINT(20) NOT NULL,
  `timeout` BIGINT(20) NOT NULL DEfAULT 0,
  KEY `job_id_key` (`job_id`, `end_time`),
  KEY `worker_id_key` (`worker_id`, `job_id`),
  KEY `uniq_key_idx` (`uniq_key`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
