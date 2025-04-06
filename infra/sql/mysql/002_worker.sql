DROP TABLE IF EXISTS worker;
CREATE TABLE `worker` (
  `id` BIGINT(10) PRIMARY KEY AUTO_INCREMENT,
  `name` VARCHAR(128) NOT NULL,
  `description` TEXT NOT NULL,
  `runner_id` BIGINT(10) NOT NULL,
  `runner_settings` MEDIUMBLOB NOT NULL,
  -- retry and timeout setting
  `retry_type` INT(10) NOT NULL,             -- using as enum: constant, linear, exponential (backoff) or none
  `interval` INT(10) NOT NULL DEFAULT 0,     -- millisecond for retry interval
  `max_interval` INT(10) NOT NULL DEFAULT 0, -- millisecond for max retry interval
  `max_retry`  INT(10) NOT NULL DEFAULT 0,   -- max count for retry until (0: unlimited)
  `basis`  FLOAT(10) NOT NULL DEFAULT 2.0,   -- basis for exponential backoff
  -- periodic setting
  `periodic_interval` INT(10) NOT NULL DEFAULT 0, -- 0 means not periodic, >0: interval in milliseconds
  -- execution setting
  `channel` VARCHAR(32) DEfAULT NULL,             -- queue channel (null means using default channel)
  `queue_type` INT(10) NOT NULL DEFAULT 0,   -- job queue type (redis or db or hybrid)
  `response_type` INT(10) NOT NULL DEFAULT 0,     -- job response type (none, direct, listen after) (for fast response worker)
  -- logging
  `store_success` TINYINT(1) NOT NULL DEFAULT 0, -- store result to db in success
  `store_failure` TINYINT(1) NOT NULL DEFAULT 0, -- store result to db in failure
  -- etc
  `use_static` TINYINT(1) NOT NULL DEFAULT 0, -- use runner as static 
  `broadcast_results` TINYINT(1) NOT NULL DEFAULT 0, -- broadcast results to all listeners
  UNIQUE KEY `name` (`name`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- optional table for db jobqueue
-- TODO create by new channel
DROP TABLE IF EXISTS job;
CREATE TABLE `job` (
  `id` BIGINT(20) PRIMARY KEY, -- the type of snowflake id is i64.
  `worker_id` BIGINT(20) NOT NULL,
  `args` MEDIUMBLOB,
  `uniq_key` VARCHAR(128) DEFAULT NULL,
  `enqueue_time` BIGINT(20) NOT NULL,
  `grabbed_until_time` BIGINT(20) NOT NULL DEFAULT '0',
  `run_after_time` BIGINT(20) NOT NULL DEFAULT '0',  -- run job after this timestamp (milliseconds).
  `retried` INT(10) NOT NULL DEFAULT '0',
  `priority` INT(10) NOT NULL DEFAULT '0',
  `timeout` BIGINT(20) NOT NULL DEfAULT 0,
  `request_streaming` TINYINT(1) NOT NULL DEFAULT 0, -- request streaming (available if supported by runner)
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
  `args` MEDIUMBLOB NOT NULL,
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
  `request_streaming` TINYINT(1) NOT NULL DEFAULT 0,
  KEY `job_id_key` (`job_id`, `end_time`),
  KEY `worker_id_key` (`worker_id`, `job_id`),
  KEY `uniq_key_idx` (`uniq_key`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- runner definition
DROP TABLE IF EXISTS runner;
CREATE TABLE `runner` (
  `id` BIGINT(10) PRIMARY KEY,
  `name` VARCHAR(128) NOT NULL, -- name for identification
  `description` TEXT NOT NULL, -- runner description
  `file_name` VARCHAR(512) NOT NULL, -- file name of the runner dynamic library
  `type` INT(10) NOT NULL, -- runner type. enum: command, request, grpc_unary, plugin
  UNIQUE KEY `name` (`name`),
  UNIQUE KEY `file_name` (`file_name`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- builtin runner definitions (runner.type != 0 cannot edit or delete)
-- (file_name is not real file name(built-in runner), but just a name for identification)
INSERT IGNORE INTO runner (id, name, description, file_name, type) VALUES (
  1, 'COMMAND', 
  'Executes shell commands with specified arguments in the operating system.',
  'builtin1', 1
), (
  2, 'HTTP_REQUEST',
  'Sends HTTP requests to specified URLs with configured method, headers, and body.',
  'builtin2', 2
), (
  3, 'GRPC_UNARY',
  'Makes gRPC unary calls to specified services with configured method, metadata, and request message.',
  'builtin3', 3
), (
  4, 'DOCKER',
  'Runs Docker containers with specified images, environment variables, and command arguments.',
  'builtin4', 4
), (
  5, 'SLACK_POST_MESSAGE',
  'Posts messages to Slack channels using specified workspace tokens and message content.',
  'builtin5', 5
), (
  6, 'PYTHON_COMMAND',
  'Executes Python scripts or commands with specified arguments and environment.',
  'builtin6', 6
), (
  65535, 'INLINE_WORKFLOW',
  'Executes a workflow defined directly within the job request. Workflow steps are run sequentially and the definition is not stored for future reuse.',
  'builtin7', 65535
), (
  -1, 'REUSABLE_WORKFLOW',
  'Allows users to define and save workflow definitions that can be executed multiple times using their ID reference. Stored workflows can be reused across different job requests.',
  'builtin8', -1
);
