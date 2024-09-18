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

CREATE TABLE IF NOT EXISTS `runner_schema` (
    `id` BIGINT NOT NULL PRIMARY KEY,
    `name` TEXT NOT NULL UNIQUE,
    `operation_type` INT NOT NULL,
    `operation_proto` TEXT NOT NULL,
    `job_arg_proto` TEXT NOT NULL
);

-- builtin runner definitions (operation_type != 1 cannot edit or delete)
INSERT or IGNORE INTO runner_schema (id, name, operation_type, operation_proto, job_arg_proto) VALUES (
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
 1, 'command', 0,
'// command job operation
message CommandOperation {}',
'// CommandArg is a list of arguments for a CommandRunner.
message CommandArg {
  // command name and arguments(e.g. "echo hello world")
  repeated string args = 1;
}'
 ), (
 2, 'httpRequest', 2,
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
 3, 'grpcUnary', 3,
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
 4, 'docker', 4,
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