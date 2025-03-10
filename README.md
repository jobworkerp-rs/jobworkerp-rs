# jobworkerp-rs

[Japanese ver.](README_ja.md)

## Overview

jobworkerp-rs is a scalable job worker system implemented in Rust.
The job worker system is used to process CPU-intensive and I/O-intensive tasks asynchronously.
Using gRPC, you can define [Workers](proto/protobuf/jobworkerp/service/worker.proto), register [Jobs](proto/protobuf/jobworkerp/service/job.proto) for task execution, and retrieve execution results.
Processing capabilities can be extended through plugins.

### Main Features

- Storage options for job queues: Choose between Redis and RDB (MySQL or SQLite) based on requirements
- Three methods for retrieving job execution results: Direct retrieval (DIRECT), Later retrieval (LISTEN_AFTER), No result retrieval (NONE)
- Job execution channel configuration with parallel execution settings per channel
  - For example, you can set GPU channel to execute with parallelism of 1, while normal channel executes with parallelism of 4
- Scheduled execution at specific times and periodic execution at fixed intervals
- Retry functionality for failed jobs: Configure retry count and intervals (Exponential backoff and others)
- Extensible job execution content (Runner) through plugins

<!-- START doctoc generated TOC please keep comment here to allow auto update -->
<!-- DON'T EDIT THIS SECTION, INSTEAD RE-RUN doctoc TO UPDATE -->

- [Command Examples](#command-examples)
  - [Build and Launch](#build-and-launch)
    - [Launch Example Using Docker Image](#launch-example-using-docker-image)
  - [Execution Examples Using jobworkerp-client](#execution-examples-using-jobworkerp-client)
- [Detailed Features of jobworkerp-worker](#detailed-features-of-jobworkerp-worker)
  - [Built-in Functions of worker.runner_id](#built-in-functions-of-workerrunner_id)
  - [Job Queue Types](#job-queue-types)
  - [Result Storage (worker.store_success, worker.store_failure)](#result-storage-workerstore_success-workerstore_failure)
  - [Result Retrieval Methods (worker.response_type)](#result-retrieval-methods-workerresponse_type)
- [Other Details](#other-details)
  - [Worker Definition](#worker-definition)
  - [RDB Definition](#rdb-definition)
  - [Other Environment Variables](#other-environment-variables)
- [About Plugins](#about-plugins)
  - [About Error Codes](#about-error-codes)
- [Other](#other)

<!-- END doctoc generated TOC please keep comment here to allow auto update -->

## Command Examples

### Build and Launch

```shell
# prepare .env file
$ cp dot.env .env

# build release binaries (use mysql)
$ cargo build --release --features mysql

# build release binaries
$ cargo build --release

# Run the all-in-one server by release binary
$ ./target/release/all-in-one

# Run gRPC front server and worker by release binary
$ ./target/release/worker &
$ ./target/release/grpc-front &
```

#### Launch Example Using Docker Image

- Please refer to docker-compose.yml and docker-compose-scalable.yml

### Execution Examples Using jobworkerp-client

Using [jobworkerp-client](https://github.com/jobworkerp-rs/jobworkerp-client-rs), you can create/retrieve workers, enqueue jobs, and get processing results as follows:

(If you don't need to encode/decode worker.runner_settings, job.job_arg, and job_result.output, you can also execute using grpcurl. Reference: [proto files](proto/protobuf/jobworkerp/service/))

setup:

```shell
# clone
$ git clone https://github.com/jobworkerp-rs/jobworkerp-client-rs
$ cd jobworkerp-client-rs

# build
$ cargo build --release

# run (show help)
$ ./target/release/jobworkerp-client

# list runner (need launching jobworkerp-rs in localhost:9000(default))
$ ./target/release/jobworkerp-client runner list
```

one shot job (with result: response-type DIRECT)

```shell
# create worker (specify runner id from runner list)
1. $ ./target/release/jobworkerp-client worker create --name "ExampleRequest" --runner-id 2 --settings '{"base_url":"https://www.example.com/search"}' --response-type DIRECT

# enqueue job (ls . ..)
# specify worker_id value or worker name created by `worker create` (command 1. response)
2-1. $ ./target/release/jobworkerp-client job enqueue --worker 1 --args '{"headers":[],"method":"GET","path":"/search","queries":[{"key":"q","value":"test"}]}'
2-2. $ ./target/release/jobworkerp-client job enqueue --worker "ExampleRequest" --args '{"headers":[],"method":"GET","path":"/search","queries":[{"key":"q","value":"test"}]}'
```

one shot job (listen result after request: response-type LISTEN_AFTER)

```shell
# create shell command `sleep` worker (must specify store_success and store_failure to be true)
1. $ ./target/release/jobworkerp-client worker create --name "SleepWorker" --runner-id 1 --settings '{"name":"sleep"}' --response-type LISTEN_AFTER --store-success --store-failure

# enqueue job
# sleep 60 seconds
2. $ ./target/debug/jobworkerp-client job enqueue --worker 'SleepWorker' --args '{"args":["60"]}'

# listen job (long polling with grpc)
# specify job_id created by `job enqueue` (command 2. response)
3. $ ./target/release/jobworkerp-client job-result listen --job-id <got job id above> --timeout 70000 --worker 'SleepWorker'
# (The response is returned as soon as the result is available, to all clients to listen. You can request repeatedly)
```

periodic job

```shell
# create periodic worker (repeat per 3 seconds)
1. $ ./target/release/jobworkerp-client worker create --name "PeriodicEchoWorker" --runner-id 1 --settings '{"name":"echo"}' --periodic 3000 --response-type NO_RESULT --store-success --store-failure

# enqueue job (echo Hello World !)
# start job at [epoch second] % 3 == 1, per 3 seconds by run_after_time (epoch milliseconds)
# (If run_after_time is not specified, the command is executed repeatedly based on enqueue_time)
2. $ ./target/debug/jobworkerp-client job enqueue --worker 'PeriodicEchoWorker' --args '{"args":["Hello", "World", "!"]}' --run-after-time 1000

# stop periodic job 
# specify job_id created by `job enqueue` (command 2. response)
3. $ ./target/debug/jobworkerp-client job delete --id <got job id above>
```

## Detailed Features of jobworkerp-worker

### Built-in Functions of worker.runner_id

The following features are built into the runner definition.
Each feature requires setting necessary values in protobuf format for worker.runner_settings and job.args. The protobuf definitions can be obtained from runner.runner_settings_proto and runner.job_arg_proto.

- COMMAND: Command execution ([CommandRunner](infra/src/infra/runner/command.rs)): Specify the target command in worker.runner_settings and arguments in job.args
- HTTP_REQUEST: HTTP request using reqwest ([RequestRunner](infra/src/infra/runner/request.rs)): Specify base url in worker.runner_settings, and headers, queries, method, body, path in job.args. Receives response body as result
- GRPC_UNARY: gRPC unary request ([GrpcUnaryRunner](infra/src/infra/runner/grpc_unary.rs)): Specify url and path in JSON format in worker.runner_settings (example: `{"url":"http://localhost:9000","path":"jobworkerp.service.WorkerService/FindList"}`). job.args should be protobuf-encoded (bytes) RPC arguments. Response is received as protobuf binary.
- DOCKER: Docker run execution ([DockerRunner](infra/src/infra/runner/docker.rs)): Specify FromImage (image to pull), Repo (repository), Tag, Platform(`os[/arch[/variant]]`) in worker.runner_settings, and Image (execution image name) and Cmd (command line array) in job.args
  - Environment variable `DOCKER_GID`: Specify GID with permission to connect to /var/run/docker.sock. The jobworkerp execution process needs to have permission to use this GID.
  - Running on k8s pod is currently untested. (Due to the above restriction, it's expected to require Docker Outside of Docker or Docker in Docker configuration in the docker image)

### Job Queue Types

Environment variable `STORAGE_TYPE`

- Standalone: Immediate jobs use memory (mpsc, mpmc channel), while scheduled jobs are stored in RDB (sqlite, mysql). Only supports single instance execution
- Scalable: Immediate jobs use Redis, while scheduled jobs are stored in RDB (sqlite, mysql). This allows multiple grpc-front and worker instances to be configured
  - Must be built with `--features mysql` when building with cargo

worker.queue_type

- NORMAL: Immediate execution jobs (regular jobs without time specification) are stored in channel (redis), while periodic and scheduled jobs are stored in db
- WITH_BACKUP: Immediate execution jobs are stored in both channel and RDB (can restore jobs from RDB during failures)
- FORCED_RDB: Immediate execution jobs are also stored only in RDB (may result in slower execution)

### Result Storage (worker.store_success, worker.store_failure)

- Execution results are saved to RDB (job_result table) on success/failure based on worker.store_success and worker.store_failure settings
- Results can be referenced after execution using [JobResultService](proto/protobuf/jobworkerp/service/job_result.proto)

### Result Retrieval Methods (worker.response_type)

- No result retrieval (NO_RESULT): (Default value) Returns Job ID in response. If results are stored, they can be retrieved after job completion using [JobResultService/FindListByJobId](proto/protobuf/jobworkerp/service/job_result.proto)
- Later retrieval (LISTEN_AFTER): After enqueue, results can be retrieved immediately after execution completion using the Listen feature of [job_result](proto/protobuf/jobworkerp/service/job_result.proto) service (Long polling)
  - Multiple clients can Listen and receive the same results (delivered via Redis pubsub)
- Direct retrieval (DIRECT): Waits for execution completion in the enqueue request and returns results directly in the response (If results are not stored, only the requesting client can obtain results)

## Other Details

- Time units are in milliseconds unless otherwise specified

### Worker Definition

- run_after_time: Job execution time (epoch time)
- timeout: Timeout duration
- worker.periodic_interval: Repeated job execution (specify 1 or greater)
- worker.retry_policy: Specify retry method for job execution failures (RetryType: CONSTANT, LINEAR, EXPONENTIAL), maximum attempts (max_retry), maximum time interval (max_interval), etc.
- worker.next_workers: Execute different workers using the result as arguments after job completion (specify worker.ids with comma separation)
  - Must specify workers that can process the result value directly as job_arg
- worker.use_static (in testing): Ability to statically allocate runner processes according to parallelism degree (avoid initialization each time by pooling execution runners)

### RDB Definition

- [MySQL schema](infra/sql/mysql/002_worker.sql)
- [SQLite schema](infra/sql/sqlite/001_schema.sql)

(runner contains fixed records as built-in functions)

### Other Environment Variables

- Execution runner settings
  - `PLUGINS_RUNNER_DIR`: Plugin storage directory
  - `DOCKER_GID`: Docker group ID (for DockerRunner)
- Job queue channel and parallelism
  - `WORKER_DEFAULT_CONCURRENCY`: Default channel parallelism
  - `WORKER_CHANNELS`: Additional job queue channel names (comma-separated)
  - `WORKER_CHANNEL_CONCURRENCIES`: Additional job queue channel parallelism (comma-separated, corresponding to WORKER_CHANNELS)
- Log settings
  - `LOG_LEVEL`: Log level (trace, debug, info, warn, error)
  - `LOG_FILE_DIR`: Log output directory
  - `LOG_USE_JSON`: Whether to output logs in JSON format (boolean)
  - `LOG_USE_STDOUT`: Whether to output logs to standard output (boolean)
  - `OTLP_ADDR` (in testing): Request metrics collection via otlp (ZIPKIN_ADDR)
- Job queue settings
  - `STRAGE_TYPE`
    - `Standalone`: Uses RDB and memory (mpmc channel). Operates assuming single instance execution. (Build without mysql specification and use SQLite)
    - `Scalable`: Uses RDB and Redis. Operates assuming multiple instance execution. (Build with `--features mysql` and use mysql as RDB)
  - `JOB_QUEUE_EXPIRE_JOB_RESULT_SECONDS`: Maximum wait time for results when response_type is LISTEN_AFTER
  - `JOB_QUEUE_FETCH_INTERVAL`: Time interval for periodic fetching of jobs stored in RDB
  - `STORAGE_REFLESH_FROM_RDB`: When jobs remain in RDB with queue_type=WITH_BACKUP due to crashes etc., setting this to true allows re-registration to Redis for processing resumption
- GRPC settings
  - `GRPC_ADDR`: gRPC server address:port
  - `USE_GRPC_WEB`: Whether to use gRPC web in gRPC server (boolean)

## About Plugins

- Implement [Runner trait](infra/src/infra/runner/plugins.rs) as dylib
  - Registered as runner when placed in directory specified by environment variable `PLUGINS_RUNNER_DIR`
  - Implementation example: [HelloPlugin](plugins/hello_runner/src/lib.rs)

### About Error Codes

TBD

## Other

- Specifying `--feature mysql` during cargo build uses mysql as RDB. Without specification, SQLite3 is used as RDB.
- For periodic execution jobs, periodic interval (milliseconds) cannot be shorter than JOB_QUEUE_FETCH_INTERVAL (periodic job fetch query interval from RDB) in .env
  - For scheduled jobs, execution at exact times is possible even with fetch and execution time differences due to prefetching from RDB
- Workers wait for completion of executing jobs before terminating upon receiving SIGINT (Ctrl + c) signal
- Job IDs use snowflake. Since 10 bits of host part of each host's IPv4 address is used as machine ID, avoid operating in subnets with host parts exceeding 10 bits or operating instances with same host parts in different subnets. (May issue duplicate job IDs)
- Running worker.type = DOCKER on k8s environment requires Docker Outside Of Docker or Docker in Docker configuration (untested)
- If a runner causes panic, the worker process itself will likely crash. Therefore, it's recommended to operate workers with fault-tolerant systems like supervisord or kubernetes deployment. (C-unwind implementation is under consideration for future improvements)

*Table of Contents: generated with [DocToc](https://github.com/thlorenz/doctoc)*
