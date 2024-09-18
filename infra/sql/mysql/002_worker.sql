DROP TABLE IF EXISTS worker;
CREATE TABLE `worker` (
  `id` BIGINT(10) PRIMARY KEY AUTO_INCREMENT,
  `name` VARCHAR(128) NOT NULL,
  `schema_id` BIGINT(10) NOT NULL,
  `operation` MEDIUMBLOB NOT NULL,
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

-- runner definition
DROP TABLE IF EXISTS runner_schema;
CREATE TABLE `runner_schema` (
  `id` BIGINT(10) PRIMARY KEY,
  `name` VARCHAR(128) NOT NULL, -- name for display
  `operation_type` INT(10) NOT NULL, -- enum: command:1, httpRequest:2, grpcUnary:3, docker:4, plugin:1)
  `operation_proto` MEDIUMTEXT NOT NULL, -- protobuf definition for the runner operation(main message must be named end with "Operation")
  `job_arg_proto` MEDIUMTEXT NOT NULL, -- protobuf definition for job argument of the runner operation (main message must be named end with "Arg")
  UNIQUE KEY `name` (`name`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- builtin runner definitions (operation_type != 1 cannot edit or delete)
INSERT INTO runner_schema (id, name, operation_type, operation_proto, job_arg_proto) VALUES (
  -1, 'SlackJobResult', -1,
'// slack builtin operation.
message SlackJobResultOperation {}',
'// slack builtin arg.
message SlackJobResultArg {
    message ResultMessageData {
        int64 result_id                            = 1; // job result id (jobworkerp.data.JobResultId::value)
        int64 job_id                               = 2; // job id (jobworkerp.data.JobId::value)
        string worker_name                         = 3;
        jobworkerp.data.ResultStatus status        = 4; // job result status
        ResultOutput output                        = 5; // job response data
        uint32 retried                             = 6;
        int64 enqueue_time                         = 7; // job enqueue time
        int64 run_after_time                       = 8; // job run after this time (specified by client)
        int64 start_time                           = 9; // job start time
        int64 end_time                             = 10; // job end time
    }
    ResultMessageData message = 1;
    optional string channel = 2;
}'
 ), (
  1, 'Command', 0,
'// command job operation
message CommandOperation {}',
'// CommandArg is a list of arguments for a CommandRunner.
message CommandArg {
  // command name and arguments(e.g. "echo hello world")
  repeated string args = 1;
}'
 ), (
  2, 'HttpRequest', 2,
'// http request job operation
message HttpRequestOperation {
  string base_url = 1;
}',
'message KeyValue {
  string key = 1;
  string value = 2;
}
// HttpRequestArg is a list of arguments for a HttpRequestRunner.
message HttpRequestArg {
  // http headers
  repeated KeyValue headers = 1;
  // http method (GET, POST, PUT, DELETE, PATCH)
  string  method = 2;
  // http path (e.g. "/api/v1/users")
  string  path = 3;
  // http body
  optional string  body = 4;
  // http query parameters
  repeated KeyValue queries = 5;
}'
), (
  3, 'GrpcUnary', 3,
'// grpc unary job operation
message GrpcUnaryOperation {
  string host = 1;
  string port = 2;
}',
'message GrpcUnaryArg {
  // grpc method name (fqdn)
  string method = 1;
  // grpc request body
  bytes request = 2;
}'
), (
  4, 'Docker', 4,
'// docker job operation
message DockerOperation {
  /// Name of the image to pull. The name may include a tag or digest. This parameter may only be
  /// used when pulling an image. The pull is cancelled if the HTTP connection is closed.
  optional string from_image = 1;
  /// Source to import. The value may be a URL from which the image can be retrieved or `-` to
  /// read the image from the request body. This parameter may only be used when importing an
  /// image.
  optional string from_src = 2;
  /// Repository name given to an image when it is imported. The repo may include a tag. This
  /// parameter may only be used when importing an image.
  optional string repo = 3;
  /// Tag or digest. If empty when pulling an image, this causes all tags for the given image to
  /// be pulled.
  optional string tag = 4;
  /// Platform in the format `os[/arch[/variant]]`
  optional string platform = 5;

  /// An object mapping ports to an empty object in the form:  `{\"<port>/<tcp|udp|sctp>\": {}}`
  // repeated string exposed_ports = 6;

  /// A list of environment variables to set inside the container in the form `[\"VAR=value\", ...]`. A variable without `=` is removed from the environment, rather than to have an empty value.
  repeated string env = 7;

  /// An object mapping mount point paths inside the container to empty objects.
  repeated string volumes = 8;

  /// The working directory for commands to run in.
  optional string working_dir = 9;

  /// The entry point for the container as a string or an array of strings.  If the array consists of exactly one empty string (`[\"\"]`) then the entry point is reset to system default (i.e., the entry point used by docker when there is no `ENTRYPOINT` instruction in the `Dockerfile`).
  repeated string entrypoint = 10;

  /// Disable networking for the container.
  // optional bool network_disabled = 11;

  /// MAC address of the container.  Deprecated: this field is deprecated in API v1.44 and up. Use EndpointSettings.MacAddress instead.
  // optional string mac_address = 12;
}',
'message DockerArg {
  /// The name of the image to use when creating the container (not used when starting a container from an image).
  optional string image = 1;

  /// User-defined key/value metadata.
  // pub labels: Option<HashMap<String, String>>,

 
  /// The user that commands are run as inside the container.
  optional string user = 2;

  /// An object mapping ports to an empty object in the form:  `{\"<port>/<tcp|udp|sctp>\": {}}`
  repeated string exposed_ports = 3;

  /// A list of environment variables to set inside the container in the form `[\"VAR=value\", ...]`. A variable without `=` is removed from the environment, rather than to have an empty value.
  repeated string env = 4;

  /// Command to run specified as a string or an array of strings.
  repeated string cmd = 5;

  /// Command is already escaped (Windows only)
  optional bool args_escaped = 6;

  /// An object mapping mount point paths inside the container to empty objects.
  repeated string volumes = 7;

  /// The working directory for commands to run in.
  optional string working_dir = 8;

  /// The entry point for the container as a string or an array of strings.  If the array consists of exactly one empty string (`[\"\"]`) then the entry point is reset to system default (i.e., the entry point used by docker when there is no `ENTRYPOINT` instruction in the `Dockerfile`).
  repeated string entrypoint = 9;

  /// Disable networking for the container.
  optional bool network_disabled = 10;

  /// MAC address of the container.  Deprecated: this field is deprecated in API v1.44 and up. Use EndpointSettings.MacAddress instead.
  optional string mac_address = 11;

  /// Shell for when `RUN`, `CMD`, and `ENTRYPOINT` uses a shell.
  repeated string shell = 12;
}'
);

