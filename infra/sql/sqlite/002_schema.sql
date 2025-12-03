PRAGMA encoding = 'UTF-8';

-- use detail types (BIGINT) only for annotation (available for framework such as prisma)

CREATE TABLE IF NOT EXISTS `worker` (
    `id` INTEGER PRIMARY KEY AUTOINCREMENT,
    `name` TEXT NOT NULL UNIQUE,
    `description` TEXT NOT NULL,
    `runner_id` BIGINT NOT NULL,
    `runner_settings` BLOB NOT NULL,
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
    `use_static` BOOLEAN NOT NULL,
    `broadcast_results` BOOLEAN NOT NULL,
    `created_at` BIGINT NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS `job` (
    `id` INTEGER PRIMARY KEY,
    `worker_id` BIGINT NOT NULL,
    `args` BLOB NOT NULL,
    `uniq_key` TEXT UNIQUE,
    `enqueue_time` BIGINT NOT NULL,
    `grabbed_until_time` BIGINT,
    `run_after_time` BIGINT NOT NULL,
    `retried` INT NOT NULL,
    `priority` INT NOT NULL,
    `timeout` BIGINT NOT NULL,
    `request_streaming` BOOLEAN NOT NULL,
    `using` TEXT
);

CREATE TABLE IF NOT EXISTS `job_result` (
    `id` INTEGER PRIMARY KEY,
    `job_id` BIGINT NOT NULL,
    `worker_id` BIGINT NOT NULL,
    `args` BLOB NOT NULL,
    `uniq_key` TEXT,
    `status` INT NOT NULL,
    `output` BLOB NOT NULL,
    `retried` INT NOT NULL,
    `priority` INT(10) NOT NULL,
    `enqueue_time` BIGINT NOT NULL,
    `run_after_time` BIGINT NOT NULL,
    `start_time` BIGINT NOT NULL,
    `end_time` BIGINT NOT NULL,
    `timeout` BIGINT NOT NULL DEFAULT 0,
    `request_streaming` BOOLEAN NOT NULL,
    `using` TEXT
);

CREATE TABLE IF NOT EXISTS `runner` (
    `id` INTEGER PRIMARY KEY,
    `name` TEXT NOT NULL UNIQUE,
    `description` TEXT NOT NULL,
    `definition` TEXT NOT NULL, -- runner definition (mcp definition or plugin file name)
    `type` INT(10) NOT NULL, -- runner type. enum: command, request, grpc_unary, plugin
    `created_at` BIGINT NOT NULL DEFAULT 0
);

-- builtin runner definitions (runner.type != 0 cannot edit or delete)
INSERT OR IGNORE INTO runner (`id`, `name`, `description`,`definition`, `type`) VALUES (
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
CREATE TABLE IF NOT EXISTS `function_set` (
  `id` BIGINT PRIMARY KEY,
  `name` TEXT NOT NULL, -- name for identification
  `description` TEXT NOT NULL, -- function set description
  `category` INT NOT NULL DEFAULT 0 -- function set category (optional)
);
CREATE UNIQUE INDEX IF NOT EXISTS `name` ON function_set(`name`);

-- function set target definition
CREATE TABLE IF NOT EXISTS `function_set_target` (
  `id` INTEGER PRIMARY KEY AUTOINCREMENT,
  `set_id` BIGINT NOT NULL, -- function set id
  `target_id` BIGINT NOT NULL, -- function set target id(worker or runner)
  `target_type` INTEGER NOT NULL DEFAULT 0, -- function set target type (runner: 0 or worker: 1)
  `using` TEXT -- optional using parameter for runner sub-methods
);

CREATE UNIQUE INDEX IF NOT EXISTS `set_target` ON function_set_target(`set_id`, `target_id`, `target_type`, `using`);

-- Indexes for admin UI filtering and sorting
CREATE INDEX IF NOT EXISTS idx_runner_type ON runner(type);
CREATE INDEX IF NOT EXISTS idx_runner_created_at ON runner(created_at);

CREATE INDEX IF NOT EXISTS idx_worker_runner_id ON worker(runner_id);
CREATE INDEX IF NOT EXISTS idx_worker_channel ON worker(channel);
CREATE INDEX IF NOT EXISTS idx_worker_periodic_interval ON worker(periodic_interval);
CREATE INDEX IF NOT EXISTS idx_worker_created_at ON worker(created_at);

-- JobProcessingStatus RDB Index Table (Sprint 3)
-- Purpose: Enable advanced job status search in large-scale environments (100万+ jobs)
-- Default: Disabled (JOB_STATUS_RDB_INDEXING=false for hobby use)
CREATE TABLE IF NOT EXISTS `job_processing_status` (
    -- Primary key
    `job_id` BIGINT PRIMARY KEY,

    -- Job status (PENDING=1, RUNNING=2, WAIT_RESULT=3, CANCELLING=4)
    `status` INT NOT NULL,

    -- Job attributes (for filtering)
    `worker_id` BIGINT NOT NULL,
    `channel` TEXT NOT NULL,
    `priority` INT NOT NULL,
    `enqueue_time` BIGINT NOT NULL,

    -- Timestamp information
    `pending_time` BIGINT,  -- Time when job entered PENDING state (milliseconds)
    `start_time` BIGINT,    -- Time when job entered RUNNING state (milliseconds)

    -- Real-time output availability
    `is_streamable` BOOLEAN NOT NULL DEFAULT 0,      -- Enqueued via EnqueueForStream
    `broadcast_results` BOOLEAN NOT NULL DEFAULT 0,  -- Worker's broadcast_results setting

    -- Metadata
    `version` BIGINT NOT NULL,  -- Optimistic locking version number
    `deleted_at` BIGINT,        -- Logical deletion timestamp (NULL: active, NOT NULL: deleted)
    `updated_at` BIGINT NOT NULL  -- Last update timestamp (for sync delay detection)
);

-- Indexes with partial WHERE clause for active records only
CREATE INDEX IF NOT EXISTS idx_jps_status_active
    ON job_processing_status(status)
    WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_jps_worker_id_active
    ON job_processing_status(worker_id)
    WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_jps_channel_active
    ON job_processing_status(channel)
    WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_jps_start_time_active
    ON job_processing_status(start_time)
    WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_jps_status_start
    ON job_processing_status(status, start_time DESC)
    WHERE deleted_at IS NULL;
