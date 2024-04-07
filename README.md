# jobworkerp-rs

[Japanese ver.](README_ja.md)

## Overview

jobworkerp-rs is a job worker system implemented in Rust.
The function to be executed is defined as [worker](proto/protobuf/jobworkerp/data/worker.proto), and [job](proto/protobuf/jobworkerp/data/job.proto), which is an execution instruction to the worker, and then register (enqueue) the job to execute the function.

A worker execution function (runner) can be added in the form of a plugin.

### Features

- 3 types of job queues: Redis, RDB(mysql or sqlite), Hybrid (Redis + mysql)
  - RDB allows jobs to be backed up as needed
- Three ways to retrieve results: directly (DIRECT), listen and get later (LISTEN_AFTER), or not get results (NONE)
- Customizable Job execution channel and number of parallel executions per channel
  - For example, the `'gpu'` channel can be set to run at a parallelism 1, while the normal channel can be set to run at a parallelism 4, etc.
- Execution at specified time, periodic execution at regular intervals
- Retry: Set retry count and interval (e.g., Exponential backoff)
- Extension of execution runner by plug-ins

<!-- START doctoc generated TOC please keep comment here to allow auto update -->
<!-- DON'T EDIT THIS SECTION, INSTEAD RE-RUN doctoc TO UPDATE -->

- [Command Examples](#command-examples)
  - [Example of build, startup](#example-of-build-startup)
    - [RDB schemas for your setup](#rdb-schemas-for-your-setup)
  - [Example of execution (grpcurl)](#example-of-execution-grpcurl)
    - [Note](#note)
- [Functions of jobworkerp-worker (worker and runner)](#functions-of-jobworkerp-worker-worker-and-runner)
  - [Type of worker.runner_type](#type-of-workerrunner_type)
  - [Job queue type (config:storage_type, worker.queue_type)](#job-queue-type-configstorage_type-workerqueue_type)
    - [Store results (worker.store_success, worker.store_failure)](#store-results-workerstore_success-workerstore_failure)
    - [How to retrieve results (worker.response_type)](#how-to-retrieve-results-workerresponse_type)
- [Other](#other)
  - [worker function](#worker-function)
  - [Other functions](#other-functions)
- [About the plugin](#about-the-plugin)
- [Specification details, limitations](#specification-details-limitations)
  - [About the combination of env.STORAGE_TYPE and worker.queue_type and the queue to use](#about-the-combination-of-envstorage_type-and-workerqueue_type-and-the-queue-to-use)
  - [The combination of env.STORAGE_TYPE and worker.response_type to be used and the behavior of JobResultService::Listen](#the-combination-of-envstorage_type-and-workerresponse_type-to-be-used-and-the-behavior-of-jobresultservicelisten)
    - [Details of the notation and contents of the table](#details-of-the-notation-and-contents-of-the-table)
  - [Error codes](#error-codes)
- [Other status](#other-status)
- [future works](#future-works)

<!-- END doctoc generated TOC please keep comment here to allow auto update -->

## Command Examples

--------

### Example of build, startup

```shell
# Run all-in-one binary from cargo (RDB storage: sqlite3)
$ cargo run --bin all-in-one

# build release binaries
$ cargo build --release

# prepare .env file for customizing settings
$ cp dot.env .env
# (modify to be appliable to your environment)

# Run the all-in-one server by release binary
$ ./target/release/all-in-one

# Run gRPC front server and worker by release binary
$ ./target/release/grpc-front &
$ ./target/release/worker &

```

- run with docker (minimum configuration: not recommended)
  ```
   $ mkdir log
   $ chmod 777 log
   $ docker run -p 9000:9000 -v ./plugins:/home/jobworkerp/plugins -v ./dot.env:/home/jobworkerp/.env -v ./log:/home/jobworkerp/log ghcr.io/jobworkerp-rs/jobworkerp:latest
  ```

- run with docker compose example (hybrid storage configuration)
  ```
   $ mkdir log
   $ chmod 777 log
   $ mkdir plugins
   (copy plugin files to ./plugins and edit compose.env for plugin settings)
   $ docker-compose up
  ```

### Example of execution (grpcurl)

[protobuf definitions](proto/protobuf/jobworkerp/)

one shot job (no result)

```shell

# create worker

1. $ grpcurl -d '{"name":"EchoWorker","type":"COMMAND","operation":"echo","next_workers":[],"retry_policy":{"type":"EXPONENTIAL","interval":"1000","max_interval":"60000","max_retry":"3","basis":"2"},"store_failure":true}' \
    -plaintext \
    localhost:9000 jobworkerp.service.WorkerService/Create

# enqueue job (echo 'HELLO!', specify by base64 encoding)
# specify worker_id created by WorkerService/Create (command 1. response)
2. $ grpcurl -d '{"arg":"SEVMTE8hCg==","worker_id":{"value":"1"},"timeout":"360000","run_after_time":"3000"}' \
    -plaintext \
    localhost:9000 jobworkerp.service.JobService/Enqueue

```

one shot job (use Listen)

```shell

# create sleep worker (need store_success and store_failure to be true in rdb storage)
1. $ grpcurl -d '{"name":"ListenSleepResultWorker","type":"COMMAND","operation":"sleep","next_workers":[],"retry_policy":{"type":"EXPONENTIAL","interval":"1000","max_interval":"60000","max_retry":"3","basis":"2"},"response_type":"LISTEN_AFTER","store_success":true,"store_failure":true}' \
    -plaintext \
    localhost:9000 jobworkerp.service.WorkerService/Create

# enqueue job
# specify worker_id created by WorkerService/Create (command 1. response)
# (timeout value(milliseconds) must be greater than sleep time)
2. $ grpcurl -d '{"arg":"MjAK","worker_id":{"value":"2"},"timeout":"22000"}' \
    -plaintext \
    localhost:9000 jobworkerp.service.JobService/Enqueue

# listen job
# specify job_id created by JobService/Enqueue (command 2. response)
$ grpcurl -d '{"job_id":{"value":"<got job id above>"},"worker_id":{"value":"2"},"timeout":"22000"}' \
    -plaintext \
    localhost:9000 jobworkerp.service.JobResultService/Listen

# (The response is returned as soon as the result is available)
    
```

periodic job

```shell

# create periodic worker (repeat per 3 seconds)
# in rdb storate(queue), store_success and store_failure must be specified
$ grpcurl -d '{"name":"EchoPeriodicWorker","type":"COMMAND","operation":"echo","retry_policy":{"type":"EXPONENTIAL","interval":"1000","max_interval":"60000","max_retry":"3","basis":"2"},"periodic_interval":3000,"store_failure":true}' \
    -plaintext \
    localhost:9000 jobworkerp.service.WorkerService/Create

# enqueue job (echo 'HELLO!')
# specify worker_id created by WorkerService/Create (↑)
# start job at [epoch second] % 3 == 1, per 3 seconds by run_after_time (epoch milliseconds) (see info log of jobworkerp-worker)
# (If run_after_time is not specified, the command is executed repeatedly based on enqueue_time)
$ grpcurl -d '{"arg":"SEVMTE8hCg==","worker_id":{"value":"3"},"timeout":"60000","run_after_time":"1000"}' \
    -plaintext \
    localhost:9000 jobworkerp.service.JobService/Enqueue
```

#### Note

- When using SQLite as RDB, it does not have high parallel execution performance, so I recommend using it in MySQL for heavy parallel use.
- The periodic_interval cannot be shorter than JOB_QUEUE_FETCH_INTERVAL in .env.

## Functions of jobworkerp-worker (worker and runner)

A worker defines a job to be executed, and a runner executes the job according to the worker's definition and obtains the results. There are currently five types of functions to execute in worker.runner_type.

### Type of worker.runner_type

- COMMAND: command execution ([CommandRunner](worker-app/src/worker/runner/impls/command.rs)): specify target command in worker.operation and arguments in job.arg
- REQUEST: http request by reqwest ([RequestRunner](worker-app/src/worker/runner/impls/request.rs)): base url in worker.operation, json in job.arg Specify headers, queries, method, body, and path in the form (e.g., `{"headers":{"Content-Type":["plain/text"]}, "queries":[["q", "rust"],["ie", "UTF-8"]]," path": "search", "method": "GET"}`). Receive response body as result
- GRPC_UNARY: gRPC unary request ([GrpcUnaryRunner](worker-app/src/worker/runner/impls/grpc_unary.rs)): Specify url and path in json format in worker.operation Specify the rpc argument in protobuf encoding (bytes) in job.arg. The response is a protobuf byte. The response will be a protobuf binary.
- DOCKER: docker run execution ([DockerRunner](worker-app/src/worker/runner/impls/docker.rs)): FromImage (image to pull) in json format in worker.operation, Repo (repository), Tag, and Platform (`os[/arch[/variant]]`) in json format in worker.operation (all optional, specified in advance of execution). Example:`{"FromImage": "busybox:latest"}`, specifying the Image (name of the image to execute) and Cmd (an array of execution command lines) in Json format in job.args (example: `{"Image": "busybox:latest","Cmd": [" ls","-alh","/"]}`)
  - Using bollard library for docker API call.
  - DOCKER_GID must be specified in .env.
    - Specify a GID with permission to connect to /var/run/docker.sock. You must have permission to use this GID as an execution process.
  - Running on k8s pod is currently untested. (Due to the above limitations, you will probably need to configure docker image to allow DockerOutsideOfDocker or DockerInDocker).
- PLUGIN: Run plugin ([PluginRunner](worker-app/src/plugins/runner.rs)): Specify the runner.name of the implemented plugin in worker.operation and the arg to be passed to plugin arg to be passed to the plugin runner.
  - [Sample imprementation](plugins/hello_runner/Cargo.toml)
- You can imprement Custom Runner: implement Runner trait, add RunnerType, create Runner in factory.

### Job queue type (config:storage_type, worker.queue_type)

- RDB (MySQL or SQLite: recommend MySQL for heavy use)
- Redis (redis only queue is not recommended: naive implementation exists in current version for periodic, run_after_time jobs, worker definition and job result management)
- Redis + RDB (Hybrid mode: execute delayed or periodic execution, backup of Redis job by RDB, immediate execution by Redis)

#### Store results (worker.store_success, worker.store_failure)

- If worker.store_success or worker.store_failure is specified, the results are stored in storage (Redis only in case of Redis, other results are stored in RDB table job_result) when execution succeeds or fails.
- The results can be obtained from [JobResultService](proto/protobuf/jobworkerp/service/job_result.proto).

#### How to retrieve results (worker.response_type)

- No result retrieval (NO_RESULT): (default value) Job ID is returned in response. If the result is stored, it can be retrieved using [JobResultService/FindListByJobId](proto/protobuf/jobworkerp/service/job_result.proto) after the job is finished.
- Get it later (LISTEN_AFTER): When enqueueing, after the same response as NO_RESULT, the result can be obtained using Listen of the [job_result](proto/protobuf/jobworkerp/service/job_result.proto) service. The result can be obtained by long polling.
  - Multiple clients can listen and get the result by using Redis pubsub to transmit the result.
- DIRECT: Wait until the result is enqueued, and the result is returned directly as a response (if the result is not stored, it is a request). (If the result is not stored, only the requesting client can get the result.)

## Other

### worker function

- the job execution time by specifying run_after_time (epoch time in milliseconds).
- Timeout time can be specified in milliseconds.
- Periodic execution can be specified by specifying worker.periodic_interval.
- Specify the execution channel name and the number of parallel executions to have specific workers using the channel with the same name execute at a specified level of parallelism.
- The retry method (CONSTANT, LINEAR, EXPONENTIAL), maximum number of retries, maximum time interval, etc. can be specified when a job fails by specifying worker.retry_policy.
- It is possible to specify that the results are stored in RDB in case of successful or unsuccessful execution.
- By defining worker.next_workers, the result of job execution can be passed as an argument to another worker as a processing argument to chain jobs (specify worker.id as a number, separated by commas)
  - Example: Slack notification of results with SlackResultNotificationRunner(worker_id=-1) as result: worker.next_workers="-1"
- The following one built-in worker (a worker that executes a specific function can be used directly by specifying a specific workerId) is available
  - Result notification by slack (SlackResultNotificationRunner: worker_id=-1 (reserved)): The specified job.arg is notified as the body of the job, along with various time information.
    - The environment variable must start with SLACK_ ([example](dot.env))
- By specifying worker.use_static=true, you can pool runners and use them without initializing them each time.
- When using SQLite, it does not have high parallel execution performance. (Recommend using MySQL or combination with Redis)

### RDB schemas for setup

- [MySQL schema](infra/sql/mysql/002_worker.sql)
- [SQLite schema](infra/sql/sqlite/001_schema.sql)

### Other functions

- The worker waits for the end of the execution of the running job by SIGINT (Ctrl + c) signal and exits.
- Obtaining request metrics using jaeger and zipkin (otlp is currently being tested)
- It has a memory cache for worker information, and the cache is volatilized according to changes made by rpc (if Redis is available, the memory cache for each instance is volatilized by using pubsub).
- Log output can be configured in the .env file. When outputting logs to a file, the file name is suffixed with the IPv4 address of each host. (When outputting to shared storage, the file is not written to the same file to prevent file corruption.)
- jobworkerp-front can use gRPC-Web by configuration.

## About the plugin

Place the dylib implementing the Runner trait in the directory specified in env.PLUGINS_RUNNER_DIR (in .env).
It will be loaded when worker is started.

Implementation example: [HelloPluginRunner](plugins/hello_runner/src/lib.rs)

## Specification details, limitations

### About the combination of env.STORAGE_TYPE and worker.queue_type and the queue to use

If worer.periodic_interval or job.run_after_time is specified (*), RDB is used as queue if available.

(*) For more details, if worer.periodic_interval or (job.run_after_time - milliseconds of the current time) is greater than JOB_QUEUE_FETCH_INTERVAL in .

|     | STORAGE_TYPE.rdb | STORAGE_TYPE.redis | STORAGE_TYPE.hybrid |
| ----|----|----|----|
| QueueType::RDB | Use RDB | Error | Use RDB |
| QueueType::REDIS | Error | Use Redis | Use Redis |
| QueueType::HYBRID | Use RDB | Use Redis | Use Redis + Backup to RDB|

- Using Redis: Queue operation of jobs using RPUSH and BLPOP of Redis (no recovery in case of job timeout)
- Using RDB: Job queue operation is performed by fetching RDB at regular intervals (env.JOB_QUEUE_FETCH_INTERVAL) and allocating jobs by updating grabbed_until_time. (with recovery in case of job timeout)
- Use Redis + Backup to RDB: Operate job queue by using RPUSH and BLPOP of Redis. If a job times out or is forced to restart, the job is automatically recovered from RDB and re-executed if the timeout time has passed while the job is popped from Redis. env.JOB_QUEUE_WITHOUT_RECOVERY_HYBRID= true to turn off the automatic recovery function.

Restore will restore all stored timeout jobs from RDB to Redis at runtime, and env.STORAGE_REFLESH_FROM_RDB=true will restore all stored timeout jobs from RDB to Redis only once at worker startup (heavy processing if a large number of jobs are queued). (This can be a heavy process if you have a large number of jobs in queue).

### The combination of env.STORAGE_TYPE and worker.response_type to be used and the behavior of JobResultService::Listen

Basically, stored items can be retrieved by JobResultService::Find\* method (depending on worker.store_success and worker.store_failure settings), but depending on worker.response_type setting, it may not be possible to retrieve stored items by JobResultService::Find* method. JobResultService::Listen to get the result as follows.
(Basically, the behavior differs depending on whether Redis is available or not; if only RDB is used, an error will occur because results cannot be returned if the setting does not store them in RDB.)

|    | STORAGE_TYPE.rdb | STORAGE_TYPE.redis | STORAGE_TYPE.hybrid |
|----|----|----|----|
| response_type::NO_RESULT | Error when specifying a worker for which store_\*=true | error | error |
| response_type::LISTEN_AFTER | Error when specifying a worker for which store_\*=true | Listen for a given amount of time | Listen for a given amount of time |
|response_type::DIRECT | Error when specifying a worker for which store_\*=true | Error | Error |

#### Details of the notation and contents of the table

- Error : Listen is not possible combination because it does not detect the end of the Job (InvalidParameter error).
- Error when specifying a worker for which store_\*=true: Error occurs when a worker is not specified for which both worker.store_success and worker.store_failure are true JobResultService::: Listen Listen will return a response when a result is obtained (in the case of RDB, it will take up to JOB_QUEUE_FETCH_INTERVAL after the result is obtained due to periodic fetch).
- Listen is available for a certain period of time: after the result is returned, JobResultService::Listen is available for JOB_QUEUE_EXPIRE_JOB_RESULT_SECONDS (if the request was made before the result is returned, the response will be returned when the result is returned). (If the request was made before the results are returned, the response will be returned when the results are returned). After that, it works the same as "find* and so on". (The jobResult is stored in Redis with "expire" for Listen)
- reponse_type::LISTEN_AFTER is realized by the client listening using pubsub to subscribe to the results if Redis is available. In RDB, it simply loops through the stored results, fetches them, and waits for them, so as a result, any response_type can listen. As a result, you can listen to any response_type. (We don't even dare to make an error.)

### Error codes

TBD

## Other status

- env.STORAGE_TYPE=redis has some bad implementations such as information acquisition. This may be a problem in special cases such as job queues that are jammed with a large number of jobs.
- Snowflake is used for paying out job id. Since the host part of the IPv4 address of each 10-bit host is used as the machine id, if you use a subnet with a host part exceeding 10 bits or use instances with the same host part on different subnets, there is a possibility that duplicate job ids will be issued. (Or retry JobService.Enqueue until the AlreadyExists error due to duplicate job id disappears.)
- If you want to run worker.type = DOCKER on a worker in a k8s environment, you need to configure Docker Outside Of Docker or Docker in Docker (not tested yet).

## future works

(If there are users other than myself...)

- Add additional rpc's that may be needed (e.g. JobService/FindListByWorkerId)
- Redis cluster support: redis-rs will support [when pubsub is supported](https://github.com/redis-rs/redis-rs/issues/492)
  - Where redis pubsub is used: When a worker definition is changed, redis pubsub notifies each server of the update and the cache is volatilized. Also, when response_type=LISTEN_AFTER, the results are notified to each front by pubsub.
- Support for OpenTelemetry Collector: Currently only implemented and not tested.
- When a panic occurs in the runner, the worker process itself probably crashes. Therefore, it is recommended to use fault-tolerant operations such as supervisord or kubernetes deployment for the worker. (The application of C-unwind will be discussed in the future.)
- Enhanced documentation

*Table of Contents: generated with [DocToc](https://github.com/thlorenz/doctoc)*

*Generated by machine translation from Japanese*
