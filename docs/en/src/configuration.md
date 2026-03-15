# Configuration

## Worker/Job Definition

- **job.run_after_time**: Specifies the execution time of the job (epoch time)
- **job.timeout**: Timeout duration
- **worker.periodic_interval**: Interval for periodic job execution (must be greater than 1)
- **worker.retry_policy**: Retry policy for job execution failures (RetryType: CONSTANT, LINEAR, EXPONENTIAL), maximum retries (max_retry), maximum interval (max_interval), etc.
- **worker.use_static**: Allows static allocation of runner processes for parallelism (pooling runners without initializing them each time)
- **worker.broadcast_results**: Enables real-time notification of job execution results (true/false)
  - Multiple clients can simultaneously retrieve results (uses Redis pubsub)

## RDB Definition

Database schema:
- [MySQL schema](https://github.com/jobworkerp-rs/jobworkerp-rs/blob/main/infra/sql/mysql/002_worker.sql)
- [SQLite schema](https://github.com/jobworkerp-rs/jobworkerp-rs/blob/main/infra/sql/sqlite/002_schema.sql)

(The runner table contains fixed records as built-in features)

## Environment Variables

(Refer to the [dot.env](https://github.com/jobworkerp-rs/jobworkerp-rs/blob/main/dot.env) file for specific examples)

| Category | Environment Variable Name | Description | Default Value |
|---------|----------------------------|-------------|---------------|
| **Runner Execution Settings** | PLUGINS_RUNNER_DIR | Directory for storing plugins | ./ |
| | DOCKER_GID | Docker group ID (for DockerRunner) | - |
| **Job Queue Settings** | WORKER_DEFAULT_CONCURRENCY | Default channel concurrency | Number of CPU cores (`num_cpus::get()`). dot.env uses 8 as an example. |
| | WORKER_CHANNELS | Names of additional job queue channels (comma-separated) | - |
| | WORKER_CHANNEL_CONCURRENCIES | Concurrency of additional job queue channels (comma-separated) | - |
| **Log Settings** | LOG_LEVEL | Log level (trace, debug, info, warn, error) | info |
| | LOG_FILE_DIR | Directory for log output | - |
| | LOG_USE_JSON | Whether to output logs in JSON format (boolean) | false |
| | LOG_USE_STDOUT | Whether to output logs to stdout (boolean) | true |
| | OTLP_ADDR | OpenTelemetry gRPC endpoint for distributed tracing and metrics | - (disabled when unset) |
| **Storage Settings** | STORAGE_TYPE | Standalone: Single instance, Scalable: Multiple instances | Standalone |
| | JOB_QUEUE_EXPIRE_JOB_RESULT_SECONDS | Maximum wait time for worker.broadcast_results=true | 86400 (dot.env recommends 3600) |
| | JOB_QUEUE_FETCH_INTERVAL | Interval for periodic fetch of jobs stored in RDB | 1000 |
| | STORAGE_RESTORE_AT_STARTUP | Flag for restoring jobs after crashes | false |
| **gRPC Settings** | GRPC_ADDR | gRPC server address:port | 0.0.0.0:9000 |
| | USE_GRPC_WEB | Whether to use gRPC web on the gRPC server (boolean) | false |
| **Feature Flags** | MCP_ENABLED | Enable MCP server mode (all-in-one mode only) | false |
| | MCP_ADDR | MCP server bind address | 127.0.0.1:8000 |
| | AG_UI_ENABLED | Enable AG-UI server (all-in-one mode only) | false |
| | AG_UI_ADDR | AG-UI server bind address | 127.0.0.1:8001 |
| **Job Status** | JOB_STATUS_RDB_INDEXING | Enable RDB indexing for job status (allows querying) | false |
| | JOB_STATUS_CLEANUP_INTERVAL_HOURS | Cleanup task interval (hours) | 1 |
| | JOB_STATUS_RETENTION_HOURS | Retention period for deleted records (hours) | 24 |
| **MCP Settings** | MCP_CONFIG | Path to MCP server configuration file | mcp-settings.toml |
| **Worker Instance Settings** | WORKER_INSTANCE_ENABLED | Enable/disable worker instance registration | true |
| | WORKER_INSTANCE_HEARTBEAT_INTERVAL_SEC | Heartbeat interval | 30 |
| | WORKER_INSTANCE_TIMEOUT_SEC | Inactive timeout (Scalable only) | 90 |
| | WORKER_INSTANCE_CLEANUP_INTERVAL_SEC | Cleanup interval (seconds) | 300 |
| **Workflow Settings** | WORKFLOW_TASK_DEFAULT_TIMEOUT_SEC | Task default timeout (seconds) | None (no limit). dot.env uses 3600 as example |
| | WORKFLOW_HTTP_TIMEOUT_SEC | HTTP resource fetch timeout (seconds) | 120 (dot.env uses 20 as example) |
| | WORKFLOW_CHECKPOINT_EXPIRE_SEC | Checkpoint expiration (seconds) | None (no limit). dot.env uses 86400 as example |
| | WORKFLOW_CHECKPOINT_MAX_COUNT | Maximum checkpoint count | None (no limit). dot.env uses 100000 as example |
| | WORKFLOW_SKIP_SCHEMA_VALIDATION | Skip schema validation | false |
| **Runner/Plugin Settings** | PLUGIN_LOAD_TIMEOUT_SECS | Plugin load timeout (seconds) | 10 |
| | MCP_CONNECTION_TIMEOUT_SECS | MCP connection timeout (seconds) | 15 |

For additional environment variables (database connections, Redis, cache, LLM API keys, etc.), see [Advanced Configuration](./configuration-advanced.md).
