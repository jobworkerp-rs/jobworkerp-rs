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
  `created_at` BIGINT(20) NOT NULL DEFAULT 0, -- record creation timestamp (milliseconds)
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
  `sub_method` VARCHAR(255) DEFAULT NULL, -- sub-method name for MCP/Plugin runners with multiple tools
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
  `sub_method` VARCHAR(255) DEFAULT NULL, -- sub-method name for MCP/Plugin runners (for retry/periodic re-execution)
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
  `definition` TEXT NOT NULL, -- runner definition (mcp definition or plugin file name)
  `type` INT(10) NOT NULL, -- runner type. enum: command, request, grpc_unary, plugin
  `created_at` BIGINT(20) NOT NULL DEFAULT 0, -- record creation timestamp (milliseconds)
  UNIQUE KEY `name` (`name`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;


-- builtin runner definitions (runner.type != 0 cannot edit or delete)
INSERT IGNORE INTO runner (id, name, description, definition, type) VALUES (
  1, 'COMMAND', 
  'Executes shell commands with specified arguments in the operating system environment.',
  'builtin1', 1
), (
  2, 'HTTP_REQUEST',
  'Sends HTTP requests to specified URLs with configured methods, headers, and body content.',
  'builtin2', 2
), (
  3, 'GRPC_UNARY',
  'Makes gRPC unary calls to specified services with configured methods, metadata, and request messages.',
  'builtin3', 3
), (
  4, 'DOCKER',
  'Runs Docker containers with specified images, environment variables, and command arguments.',
  'builtin4', 4
), (
  5, 'SLACK_POST_MESSAGE',
  'Posts messages to Slack channels using specified workspace tokens and customizable message content.',
  'builtin5', 5
), (
  6, 'PYTHON_COMMAND',
  'Executes Python scripts or commands with specified arguments and environment.',
  'builtin6', 6
), (
  65533, 'LLM_CHAT',
  'Generates chat interactions using large language models with specified messages and configuration parameters.',
  'builtin65533', 65533
), (
  65534, 'LLM_COMPLETION',
  'Generates text completions using large language models with specified prompts and configuration parameters.',
  'builtin65534', 65534
), (
  65535, 'INLINE_WORKFLOW',
  'Executes a workflow defined directly within the job request. Steps are run sequentially and the definition is not stored for future reuse. Uses the same schema as REUSABLE_WORKFLOW for workflow definition.',
  'builtin65535', 65535
), (
  65532, 'REUSABLE_WORKFLOW',
  'Defines and saves workflow definitions that can be executed multiple times using their ID reference. These workflows can be reused across different job requests.',
  'builtin65532', 65532
), (
  -1, 'CREATE_WORKFLOW',
  'Creates a new REUSABLE_WORKFLOW worker from a workflow definition. The workflow is validated and stored as a worker that can be executed multiple times.',
  'builtin-1', -1
);


-- function set definition
DROP TABLE IF EXISTS function_set;
CREATE TABLE `function_set` (
  `id` BIGINT(10) PRIMARY KEY,
  `name` VARCHAR(128) NOT NULL, -- name for identification
  `description` TEXT NOT NULL, -- function set description
  `category` INT(10) NOT NULL DEFAULT 0, -- function set category (optional)
  UNIQUE KEY `name` (`name`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- function set target definition
DROP TABLE IF EXISTS function_set_target;
CREATE TABLE `function_set_target` (
  `id` BIGINT(10) PRIMARY KEY AUTO_INCREMENT,
  `set_id` BIGINT(10) NOT NULL, -- function set id
  `target_id` BIGINT(10) NOT NULL, -- function set target id(worker or runner)
  `target_type` INT(10) NOT NULL DEFAULT 0, -- function set target type (runner: 0 or worker: 1)
  UNIQUE KEY `set_target` (`set_id`, `target_id`, `target_type`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Indexes for admin UI filtering and sorting
-- Note: MySQL does not support IF NOT EXISTS for CREATE INDEX (SQLite only)
-- This script runs only once during docker-entrypoint-initdb.d initialization
CREATE INDEX idx_runner_type ON `runner`(`type`);
CREATE INDEX idx_runner_created_at ON `runner`(`created_at`);

CREATE INDEX idx_worker_runner_id ON `worker`(`runner_id`);
CREATE INDEX idx_worker_channel ON `worker`(`channel`);
CREATE INDEX idx_worker_periodic_interval ON `worker`(`periodic_interval`);
CREATE INDEX idx_worker_created_at ON `worker`(`created_at`);

