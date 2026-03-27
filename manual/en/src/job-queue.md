# Job Queue and Results

## Job Queue Types

Environment variable `STORAGE_TYPE`

- Standalone: Immediate jobs use memory (mpsc, mpmc channel), while scheduled jobs are stored in RDB (sqlite, mysql). Only supports single instance execution
- Scalable: Immediate jobs use Redis, while scheduled jobs are stored in RDB (mysql). This allows multiple grpc-front and worker instances to be configured
  - Must be built with `--features mysql` when building with cargo

worker.queue_type

- NORMAL: Immediate execution jobs (regular jobs without time specification) are stored in channel (redis), while periodic and scheduled jobs are stored in db
- WITH_BACKUP: Immediate execution jobs are stored in both channel and RDB (can restore jobs from RDB during failures)
- DB_ONLY: Immediate execution jobs are also stored only in RDB (may result in slower execution)

## Result Storage (worker.store_success, worker.store_failure)

- Execution results are saved to RDB (job_result table) on success/failure based on worker.store_success and worker.store_failure settings
- Results can be referenced after execution using [JobResultService](https://github.com/jobworkerp-rs/jobworkerp-rs/blob/main/proto/protobuf/jobworkerp/service/job_result.proto)

## Result Retrieval Methods (worker.response_type)

There are two methods for retrieving results via worker.response_type:

- No result retrieval (NO_RESULT): (Default value) Returns Job ID in response. If results are stored, they can be retrieved after job completion using [JobResultService/FindListByJobId](https://github.com/jobworkerp-rs/jobworkerp-rs/blob/main/proto/protobuf/jobworkerp/service/job_result.proto)
- Direct retrieval (DIRECT): Waits for execution completion in the enqueue request and returns results directly in the response (If results are not stored, only the requesting client can obtain results)

Additionally, when worker.broadcast_results is enabled:

- Immediate result notification: After enqueue, results can be retrieved immediately after execution completion using the Listen feature of [job_result](https://github.com/jobworkerp-rs/jobworkerp-rs/blob/main/proto/protobuf/jobworkerp/service/job_result.proto) service (Long polling method)
  - Multiple clients can Listen and receive the same results (delivered via Redis pubsub)
  - See the "listen result after request" example in the [Client Usage](client-usage.md) section
- **Streaming result retrieval** (JobResultService.ListenStream): For jobs with [streaming](streaming.md) enabled (`streaming_type` = Response or Internal), streaming output can be retrieved as a `ResultOutputItem` stream via gRPC server streaming. The `JobResult` metadata (status, timestamps, etc.) is returned in the gRPC response header (`x-job-result-bin`), while the stream body delivers `Data` chunks, `End` trailer, and `FinalCollected` (Internal mode only). Uses the same `ListenRequest` as Listen.
- You can continuously receive execution results from a specific worker as a stream (JobResultService.ListenByWorker)
