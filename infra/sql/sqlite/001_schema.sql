PRAGMA encoding = 'UTF-8';

-- use detail types (BIGINT) only for annotation (available for framework such as prisma)

CREATE TABLE IF NOT EXISTS `worker` (
    `id` INTEGER PRIMARY KEY AUTOINCREMENT,
    `name` TEXT NOT NULL UNIQUE,
    `schema_id` BIGINT NOT NULL,
    `operation` BLOB NOT NULL,
    `retry_type` INT NOT NULL,
    `interval` INT NOT NULL,
    `max_interval` INT NOT NULL,
    `max_retry` INT NOT NULL,
    `basis` FLOAT NOT NULL,
    `periodic_interval` INT NOT NULL,
    `channel` TEXT,
    `queue_type` INT NOT NULL,
    `response_type` INT NOT NULL,
    `store_success` BOOLEAN NOT NULL,
    `store_failure` BOOLEAN NOT NULL,
    `next_workers` TEXT NOT NULL,
    `use_static` BOOLEAN NOT NULL
);

CREATE TABLE IF NOT EXISTS `job` (
    `id` INTEGER PRIMARY KEY,
    `worker_id` BIGINT NOT NULL,
    `arg` BLOB NOT NULL,
    `uniq_key` TEXT UNIQUE,
    `enqueue_time` BIGINT NOT NULL,
    `grabbed_until_time` BIGINT,
    `run_after_time` BIGINT NOT NULL,
    `retried` INT NOT NULL,
    `priority` INT NOT NULL,
    `timeout` BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS `job_result` (
    `id` INTEGER PRIMARY KEY,
    `job_id` BIGINT NOT NULL,
    `worker_id` BIGINT NOT NULL,
    `arg` BLOB NOT NULL,
    `uniq_key` TEXT,
    `status` INT NOT NULL,
    `output` BLOB NOT NULL,
    `retried` INT NOT NULL,
    `priority` INT(10) NOT NULL,
    `enqueue_time` BIGINT NOT NULL,
    `run_after_time` BIGINT NOT NULL,
    `start_time` BIGINT NOT NULL,
    `end_time` BIGINT NOT NULL,
    `timeout` BIGINT NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS `worker_schema` (
    `id` INTEGER PRIMARY KEY,
    `name` TEXT NOT NULL UNIQUE,
    `file_name` VARCHAR(512) NOT NULL UNIQUE -- file name of the runner dynamic library
);

-- builtin runner definitions (operation_type != 1 cannot edit or delete)
-- (file_name is not real file name(built-in runner), but just a name for identification)
INSERT INTO worker_schema (id, name, file_name) VALUES (
  1, 'SlackJobResult', 'builtin0'
), (
  2, 'Command', 'builtin1'
), (
  3, 'HttpRequest', 'builtin2'
), (
  4, 'GrpcUnary', 'builtin3'
), (
  5, 'Docker', 'builtin4'
);
