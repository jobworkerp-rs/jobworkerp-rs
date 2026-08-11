-- Completed MySQL schema.
--
-- This file is a reference for a database after all migrations have been
-- applied. Existing installations should apply numbered migrations instead.

CREATE TABLE `worker` (
  `id` BIGINT(10) PRIMARY KEY AUTO_INCREMENT,
  `name` VARCHAR(128) NOT NULL,
  `description` TEXT NOT NULL,
  `runner_id` BIGINT(10) NOT NULL,
  `runner_settings` MEDIUMBLOB NOT NULL,
  `retry_type` INT(10) NOT NULL,
  `interval` INT(10) NOT NULL DEFAULT 0,
  `max_interval` INT(10) NOT NULL DEFAULT 0,
  `max_retry` INT(10) NOT NULL DEFAULT 0,
  `basis` FLOAT(10) NOT NULL DEFAULT 2.0,
  `periodic_interval` INT(10) NOT NULL DEFAULT 0,
  `channel` VARCHAR(32) DEFAULT NULL,
  `queue_type` INT(10) NOT NULL DEFAULT 0,
  `response_type` INT(10) NOT NULL DEFAULT 0,
  `store_success` TINYINT(1) NOT NULL DEFAULT 0,
  `store_failure` TINYINT(1) NOT NULL DEFAULT 0,
  `use_static` TINYINT(1) NOT NULL DEFAULT 0,
  `broadcast_results` TINYINT(1) NOT NULL DEFAULT 0,
  `created_at` BIGINT(20) NOT NULL DEFAULT 0,
  UNIQUE KEY `name` (`name`),
  KEY `idx_worker_runner_id` (`runner_id`),
  KEY `idx_worker_channel` (`channel`),
  KEY `idx_worker_periodic_interval` (`periodic_interval`),
  KEY `idx_worker_created_at` (`created_at`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE `job` (
  `id` BIGINT(20) PRIMARY KEY,
  `worker_id` BIGINT(20) NOT NULL,
  `args` MEDIUMBLOB,
  `uniq_key` VARCHAR(128) DEFAULT NULL,
  `enqueue_time` BIGINT(20) NOT NULL,
  `grabbed_until_time` BIGINT(20) NOT NULL DEFAULT '0',
  `run_after_time` BIGINT(20) NOT NULL DEFAULT '0',
  `retried` INT(10) NOT NULL DEFAULT '0',
  `priority` INT(10) NOT NULL DEFAULT '0',
  `timeout` BIGINT(20) NOT NULL DEFAULT 0,
  `request_streaming` TINYINT(1) NOT NULL DEFAULT 0,
  `using` VARCHAR(255) DEFAULT NULL,
  KEY `worker_id_key` (`worker_id`),
  KEY `find_job_key` (`run_after_time`, `grabbed_until_time`, `worker_id`, `priority`),
  KEY `find_job_key2` (`run_after_time`, `grabbed_until_time`, `priority`),
  UNIQUE KEY `uniq_key_idx` (`uniq_key`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE `job_execution_overrides` (
  `job_id` BIGINT(20) PRIMARY KEY,
  `response_type` INT(10) DEFAULT NULL,
  `store_success` TINYINT(1) DEFAULT NULL,
  `store_failure` TINYINT(1) DEFAULT NULL,
  `broadcast_results` TINYINT(1) DEFAULT NULL,
  `retry_type` INT(10) DEFAULT NULL,
  `retry_interval` INT(10) UNSIGNED DEFAULT NULL,
  `retry_max_interval` INT(10) UNSIGNED DEFAULT NULL,
  `retry_max_retry` INT(10) UNSIGNED DEFAULT NULL,
  `retry_basis` FLOAT DEFAULT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE `job_result` (
  `id` BIGINT(20) PRIMARY KEY,
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
  `timeout` BIGINT(20) NOT NULL DEFAULT 0,
  `request_streaming` TINYINT(1) NOT NULL DEFAULT 0,
  `using` VARCHAR(255) DEFAULT NULL,
  KEY `job_id_key` (`job_id`, `end_time`),
  KEY `worker_id_key` (`worker_id`, `job_id`),
  KEY `uniq_key_idx` (`uniq_key`),
  KEY `idx_job_result_status` (`status`),
  KEY `idx_job_result_start_time` (`start_time`),
  KEY `idx_job_result_end_time` (`end_time`),
  KEY `idx_job_result_end_status` (`end_time`, `status`),
  KEY `idx_job_result_worker_end` (`worker_id`, `end_time` DESC)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE `runner` (
  `id` BIGINT(10) PRIMARY KEY,
  `name` VARCHAR(128) NOT NULL,
  `description` TEXT NOT NULL,
  `definition` TEXT NOT NULL,
  `type` INT(10) NOT NULL,
  `created_at` BIGINT(20) NOT NULL DEFAULT 0,
  UNIQUE KEY `name` (`name`),
  KEY `idx_runner_type` (`type`),
  KEY `idx_runner_created_at` (`created_at`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

INSERT IGNORE INTO runner (id, name, description, definition, type) VALUES
  (1, 'COMMAND',
   'Executes shell commands with specified arguments in the operating system environment.',
   'builtin1', 1),
  (2, 'HTTP_REQUEST',
   'Sends HTTP requests to specified URLs with configured methods, headers, and body content.',
   'builtin2', 2),
  (4, 'DOCKER',
   'Runs Docker containers with specified images, environment variables, and command arguments.',
   'builtin4', 4),
  (5, 'SLACK_POST_MESSAGE',
   'Posts messages to Slack channels using specified workspace tokens and customizable message content.',
   'builtin5', 5),
  (6, 'PYTHON_COMMAND',
   'Executes Python scripts or commands with specified arguments and environment.',
   'builtin6', 6),
  (32768, 'LLM',
   'Unified LLM runner with multiple methods: completion (text completion) and chat (conversation with history). Requires using parameter to specify method.',
   'builtin32768', 32768),
  (32769, 'WORKFLOW',
   'Unified workflow runner with multiple methods: run (execute workflow, default) and create (create workflow worker). Using defaults to run if not specified.',
   'builtin32769', 32769),
  (8, 'FUNCTION_SET_SELECTOR',
   'Lists available FunctionSets with tool summaries for LLM tool selection. Used as a meta-tool to help LLM discover and select appropriate FunctionSets.',
   'builtin8', 8),
  (32770, 'GRPC',
   'Unified gRPC runner with multiple methods: unary (gRPC unary call, default) and streaming (gRPC server streaming call). Using defaults to unary if not specified.',
   'builtin32770', 32770);

CREATE TABLE `function_set` (
  `id` BIGINT(10) PRIMARY KEY,
  `name` VARCHAR(128) NOT NULL,
  `description` TEXT NOT NULL,
  `category` INT(10) NOT NULL DEFAULT 0,
  UNIQUE KEY `name` (`name`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE `function_set_target` (
  `id` BIGINT(10) PRIMARY KEY AUTO_INCREMENT,
  `set_id` BIGINT(10) NOT NULL,
  `target_id` BIGINT(10) NOT NULL,
  `target_type` INT(10) NOT NULL DEFAULT 0,
  `using` VARCHAR(255),
  UNIQUE KEY `set_target` (`set_id`, `target_id`, `target_type`, `using`(191))
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE `job_processing_status` (
    `job_id` BIGINT PRIMARY KEY,
    `status` INT NOT NULL COMMENT 'PENDING=1, RUNNING=2, WAIT_RESULT=3, CANCELLING=4',
    `worker_id` BIGINT NOT NULL,
    `channel` VARCHAR(255) NOT NULL,
    `priority` INT NOT NULL,
    `enqueue_time` BIGINT NOT NULL,
    `pending_time` BIGINT COMMENT 'Timestamp when job entered PENDING state (milliseconds)',
    `start_time` BIGINT COMMENT 'Timestamp when job entered RUNNING state (milliseconds)',
    `is_streamable` BOOLEAN NOT NULL DEFAULT 0 COMMENT 'Whether job was enqueued via EnqueueForStream',
    `broadcast_results` BOOLEAN NOT NULL DEFAULT 0 COMMENT 'Worker broadcast_results setting',
    `version` BIGINT NOT NULL COMMENT 'Optimistic lock version number',
    `deleted_at` BIGINT COMMENT 'Logical deletion timestamp (NULL: active, NOT NULL: deleted)',
    `updated_at` BIGINT NOT NULL COMMENT 'Last update timestamp (for detecting sync delays)',
    `worker_instance_id` BIGINT COMMENT 'Logical worker instance that entered RUNNING',
    KEY `idx_jps_status_active` (`status`, `deleted_at`),
    KEY `idx_jps_worker_id_active` (`worker_id`, `deleted_at`),
    KEY `idx_jps_channel_active` (`channel`, `deleted_at`),
    KEY `idx_jps_start_time_active` (`start_time` DESC, `deleted_at`),
    KEY `idx_jps_status_start` (`status`, `start_time` DESC, `deleted_at`),
    KEY `idx_jps_recovery_instance_running` (`worker_instance_id`, `status`, `deleted_at`, `job_id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

ANALYZE TABLE job_processing_status;
