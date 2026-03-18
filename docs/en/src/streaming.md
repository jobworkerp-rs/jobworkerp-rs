# Streaming in jobworkerp-rs

This document describes the streaming functionality in jobworkerp-rs, including the different streaming types and their behavior.

## Overview

jobworkerp-rs supports streaming execution for runners that implement the `run_stream()` method. Streaming allows runners to emit results incrementally rather than returning a single result at the end of execution. This is particularly useful for:

- LLM responses that generate tokens incrementally
- Long-running commands that produce output over time
- Real-time progress updates

## StreamingType

The `StreamingType` enum controls how streaming is handled for job execution. It is specified when enqueueing a job.

### STREAMING_TYPE_NONE (0)

Default mode. No streaming is used.

- Runner's `run()` method is called (not `run_stream()`)
- Result is returned as a single `JobResult` after job completion
- For `ResponseType::Direct` workers, the enqueue call blocks until completion

### STREAMING_TYPE_RESPONSE (1)

Full streaming mode for client consumption.

- Runner's `run_stream()` method is called
- Results are streamed back to the client via pub/sub as `ResultOutputItem` messages
- Client receives `Data` chunks as they are produced, followed by `End` trailer
- For `ResponseType::Direct` workers, the enqueue call blocks and returns the stream
- Compatible with `request_streaming=true` (legacy boolean field)

### STREAMING_TYPE_INTERNAL (2)

Internal streaming mode for workflow orchestration.

- Runner's `run_stream()` method is called internally
- Results are collected using `RunnerSpec::collect_stream()`
- Final aggregated result is sent as `ResultOutputItem::FinalCollected`
- **Key behavior**: Even for `ResponseType::Direct` workers, enqueue returns immediately
- Caller is responsible for subscribing to the stream and collecting results

This mode is designed for workflow steps that:
1. Need to leverage streaming-capable runners (e.g., LLM with incremental token generation)
2. Want the final aggregated result as a single chunk for the next workflow step
3. Need to preserve worker pooling (`use_static=true`) for heavy resources like local LLMs

## Behavior Matrix

| StreamingType | ResponseType | Enqueue Behavior | Result Delivery |
|---------------|--------------|------------------|-----------------|
| None | Direct | Blocks until completion | Single JobResult |
| None | NoResult | Returns immediately | Via Listen/store |
| Response | Direct | Blocks, returns stream | Stream via pub/sub |
| Response | NoResult | Returns immediately | Stream via pub/sub |
| Internal | Direct | **Returns immediately** | Stream + FinalCollected |
| Internal | NoResult | Returns immediately | Stream + FinalCollected |

Note: `Internal` mode always returns immediately regardless of `ResponseType`, allowing the caller to subscribe to the stream before data is published.

## ResultOutputItem Message Types

When using streaming modes, results are delivered as `ResultOutputItem` messages:

```protobuf
message ResultOutputItem {
  oneof item {
    bytes data = 1;           // Incremental data chunk
    Trailer end = 2;          // End of stream marker
    bytes final_collected = 3; // Aggregated result (Internal mode)
  }
}
```

- **Data**: Individual chunks of streaming output
- **End**: Marks the end of the stream with optional metadata
- **FinalCollected**: Contains the result of `collect_stream()` aggregation (only in Internal mode)

## Usage Examples

### Client-facing Streaming (Response mode)

```rust
// Enqueue with Response streaming
let (job_id, _result, stream) = job_app.enqueue_job(
    metadata,
    Some(&worker_id),
    None,
    args,
    None,
    0,
    Priority::Medium as i32,
    timeout,
    None,
    StreamingType::Response,
    None,
).await?;

// Consume stream
while let Some(item) = stream.next().await {
    match item.item {
        Some(Item::Data(data)) => { /* process chunk */ }
        Some(Item::End(_)) => break,
        _ => {}
    }
}
```

### Workflow Internal Streaming (Internal mode)

```rust
// Enqueue with Internal streaming - returns immediately
let (job_id, _result, _stream) = job_app.enqueue_job(
    metadata,
    Some(&worker_id),
    None,
    args,
    None,
    0,
    Priority::Medium as i32,
    timeout,
    None,
    StreamingType::Internal,
    None,
).await?;

// Subscribe to stream separately
let stream = pubsub_repo.subscribe_result_stream(&job_id, timeout_ms).await?;

// Collect results
let mut final_result = None;
while let Some(item) = stream.next().await {
    match item.item {
        Some(Item::Data(data)) => { /* forward to UI or collect */ }
        Some(Item::FinalCollected(data)) => {
            final_result = Some(data);
        }
        Some(Item::End(_)) => break,
        _ => {}
    }
}

// Use final_result for next workflow step
```

### Retrieving Streaming Results via gRPC (ListenStream)

When `broadcast_results` is enabled on a worker and the job uses streaming (`streaming_type` = Response or Internal), clients can retrieve the streaming output via `JobResultService.ListenStream`. This method uses the same `ListenRequest` as `Listen`, but returns a `stream ResultOutputItem` instead of a single `JobResult`.

- The `JobResult` metadata (status, timestamps, output, etc.) is returned in the gRPC response header `x-job-result-bin` (protobuf-encoded binary).
- The stream body delivers `Data` chunks, `End` trailer, and `FinalCollected` (Internal mode only).

```protobuf
// In JobResultService
rpc ListenStream(ListenRequest) returns (stream ResultOutputItem);
```

Usage flow:

```text
1. Enqueue a job (e.g., via JobService.Enqueue)
2. Call JobResultService.ListenStream(job_id, worker_id/worker_name, timeout)
3. Read x-job-result-bin from response header to get JobResult metadata
4. Consume stream: Data chunks → End trailer (or FinalCollected for Internal mode)
```

### RPC Correspondence for Streaming vs Non-Streaming

The following table shows which RPCs to use depending on whether streaming is enabled:

| Scenario | Enqueue RPC | Result Retrieval RPC | Related Settings |
|----------|------------|---------------------|-----------------|
| Non-streaming | `JobService.Enqueue` (uses `StreamingType::None`) | `JobResultService.Listen` or `FindListByJobId` | - |
| Streaming (single client) | `JobService.EnqueueForStream` (uses `StreamingType::Response`) | Stream returned directly from `EnqueueForStream` | (Job) `streaming_type` = Response |
| Streaming (multiple clients) | `JobService.Enqueue` or `EnqueueForStream` | `JobResultService.ListenStream` | (Worker) `broadcast_results=true`, (Job) `streaming_type` = Response or Internal |
| **Client streaming + Direct** | **`JobService.EnqueueWithClientStream`** | **Response stream directly** | Runner `require_client_stream=true` |
| **Client streaming + NoResult** | **`JobService.EnqueueWithClientStream`** | **`JobResultService.ListenStream` (separate client)** | Runner `require_client_stream=true`, (Worker) `broadcast_results=true` |

- `Listen` called on a streaming job returns an error because streaming results cannot be collapsed into a single `JobResult` response. Use `ListenStream` instead.
- `EnqueueForStream` returns the stream directly in its response, so a separate `ListenStream` call is not needed for the requesting client. However, when `broadcast_results=true`, additional clients can subscribe to the same streaming results via `ListenStream`.
- `EnqueueWithClientStream` combines job enqueue and feed data delivery into a single bidirectional stream, eliminating the timing constraints of `FeedToStream`.

## Worker Pooling and use_static

When using `use_static=true` on workers (e.g., for local LLMs), the runner instance is pooled and reused across job executions. This is critical for resources that have expensive initialization.

`StreamingType::Internal` preserves this pooling behavior by:
1. Using the existing `worker_id` when enqueueing (not creating a temp worker)
2. Returning immediately from enqueue (not blocking on Direct response)
3. Allowing the caller to manage stream subscription independently

This ensures that heavy resources like local LLM models are not re-initialized for each job.

## EnqueueWithClientStream: Client Streaming

The `EnqueueWithClientStream` RPC combines job enqueue and client streaming data delivery into a single bidirectional gRPC stream. It resolves the constraints of the deprecated `FeedToStream` RPC.

### Advantages over FeedToStream

| FeedToStream constraint | EnqueueWithClientStream |
|------------------------|------------------------|
| Requires `use_static=true` | Not required |
| Requires channel concurrency = 1 | Not required |
| Requires job to be in `Running` status before feed | Automatic: enqueue and feed in single stream |
| Separate Enqueue + Feed RPCs (race condition risk) | Single unified stream |
| Scalable mode: Redis Pub/Sub (message loss risk) | Scalable mode: Redis List + BLPOP (no message loss) |

### Protocol Definition

```protobuf
message ClientStreamRequest {
  oneof request {
    JobRequest job_request = 1;                   // First message: job enqueue request
    jobworkerp.data.FeedDataTransport feed_data = 2;  // Subsequent: feed data chunks
  }
}

// In JobService
rpc EnqueueWithClientStream(stream ClientStreamRequest)
    returns (stream jobworkerp.data.ResultOutputItem);
```

### Usage Flow

#### Direct mode (`response_type=Direct`)

The sending client directly receives the result stream.

```text
Client                                    Server
  │                                          │
  │─── ClientStreamRequest(job_request) ────>│  Job accepted
  │<── [headers: x-job-id-bin, x-job-result-bin]
  │<── ResultOutputItem(Data) ──────────────│  Initial output
  │─── ClientStreamRequest(feed_data) ──────>│  Feed data
  │<── ResultOutputItem(Data) ──────────────│  Intermediate output
  │─── ClientStreamRequest(feed_data,        │
  │         is_final=true) ─────────────────>│  Final feed data
  │<── ResultOutputItem(End(trailer)) ──────│  Stream ends
```

#### NoResult mode (`response_type=NoResult`)

The sending client only feeds data. A separate client receives results via `ListenStream`.

```text
Feed Client                               Server
  │                                          │
  │─── ClientStreamRequest(job_request) ────>│  Job accepted
  │<── [headers: x-job-id-bin]               │
  │─── ClientStreamRequest(feed_data) ──────>│  Feed data
  │─── ClientStreamRequest(feed_data,        │
  │         is_final=true) ─────────────────>│  Final feed data
  │<── ResultOutputItem(End(trailer)) ──────│  Feed complete, stream ends

Listener Client                            Server
  │─── ListenStream(job_id, worker_id) ────>│  Subscribe to results
  │<── ResultOutputItem(Data) ──────────────│
  │<── ResultOutputItem(End(trailer)) ──────│
```

> **Note**: In NoResult mode, the gRPC response stream for the feed client does not complete until all feed data has been delivered (i.e., the client sends `is_final=true`). The feed client must complete sending all data before the stream ends, even though it does not receive result data.

### `require_client_stream` Flag

Runners that need client streaming input must set `require_client_stream=true` in their `MethodSchema`. This flag is validated by `check_worker_streaming`:

- **EnqueueWithClientStream**: Requires `require_client_stream=true` on the runner method. Rejects if false.
- **Enqueue / EnqueueForStream**: Rejects runners with `require_client_stream=true` (reverse guard). These runners must be invoked via `EnqueueWithClientStream`.

### Data Transport

- **Standalone mode**: Feed data is delivered via in-process `mpsc` channel (`ChanFeedSenderStore`). The gRPC handler waits for the Dispatcher to register the feed channel using `tokio::sync::Notify`.
- **Scalable mode**: Feed data is pushed to a Redis List (`job_feed_buf:{job_id}`) via RPUSH. The worker's feed bridge reads via BLPOP. No message loss since the List buffers data.

### Error Cases

| Case | gRPC Status | Timing |
|------|-------------|--------|
| First message is not `job_request` | `INVALID_ARGUMENT` | Stream start |
| Worker not found | `NOT_FOUND` | Stream start |
| Runner does not support client streaming | `INVALID_ARGUMENT` | Stream start |
| Runner does not support streaming output | `INVALID_ARGUMENT` | Stream start |
| Feed channel dispatch timeout | `DEADLINE_EXCEEDED` | Before feed starts |
| `job_request` sent after first message | `INVALID_ARGUMENT` | During feed |
| Runner execution error | Status code per `ResultStatus` | During execution |
| Client disconnect (no half-close) | Cancel notification to runner | Any time |

### Configuration

| Environment Variable | Default | Description |
|---------------------|---------|-------------|
| `JOB_QUEUE_FEED_DISPATCH_TIMEOUT` | `5000` | Maximum wait time for feed channel readiness (Standalone mode). In Scalable mode, used as Redis List TTL base. |

## [Deprecated] FeedToStream: Sending Data to Running Streaming Jobs

> **⚠ Deprecated**: `FeedToStream` is deprecated. Use `EnqueueWithClientStream` instead. `FeedToStream` will be removed in a future version.
>
> **Important**: In Scalable mode, `FeedToStream` is no longer functional. The underlying Redis Pub/Sub mechanism has been replaced by Redis List + BLPOP for `EnqueueWithClientStream`. In Standalone mode, `FeedToStream` continues to work.

> **⚠ Deprecation Notice**: The current `FeedToStream` implementation uses unary RPCs to send individual data chunks, which is suboptimal for continuous data feeding scenarios. This will be replaced with **gRPC client streaming** in a future release to provide a more natural and efficient interface. The API and behavior described below are subject to change.

The `FeedToStream` RPC allows clients to send additional data to a running streaming job. This enables interactive streaming scenarios such as real-time audio processing, where the client feeds audio chunks while the runner processes and returns results.

### Prerequisites

For a job to accept feed data, all of the following must be true:

| Condition | Reason |
|-----------|--------|
| Job is in `Running` status | Feed is only meaningful during execution |
| `streaming_type != None` | Runner must be using `run_stream()` |
| Worker has `use_static=true` | Runner instance must be pooled and persistent |
| Channel concurrency = 1 | Feed target runner must be unambiguous (single host required) |
| Runner method has `require_client_stream=true` | Runner must explicitly support client streaming |

### Protocol

```protobuf
// In JobService (deprecated)
rpc FeedToStream(FeedToStreamRequest) returns (FeedToStreamResponse) {
  option deprecated = true;
}

message FeedToStreamRequest {
  jobworkerp.data.JobId job_id = 1;
  bytes data = 2;
  bool is_final = 3;
}

message FeedToStreamResponse {
  bool accepted = 1;
}
```

### Usage Flow

```text
1. EnqueueForStream(worker_id, args) → job_id (from response header x-job-id-bin)
   ↓ (output stream starts)
2. FeedToStream(job_id, data_chunk_1, is_final=false)
3. FeedToStream(job_id, data_chunk_2, is_final=false)
4. FeedToStream(job_id, last_chunk, is_final=true)
   ↓ (runner processes final data, output stream ends)
5. Client receives remaining output and End trailer
```

### Data Transport

- **Standalone mode (Channel)**: Feed data is sent directly via an in-process `mpsc` channel stored in `ChanFeedSenderStore`.
- **Scalable mode**: Not supported. Use `EnqueueWithClientStream` instead.

### Error Cases

| Case | gRPC Status |
|------|-------------|
| Job not found | `NOT_FOUND` |
| Job not running | `FAILED_PRECONDITION` |
| Job not streaming | `FAILED_PRECONDITION` |
| Runner method lacks `require_client_stream=true` | `FAILED_PRECONDITION` |
| Feed channel unavailable (job completed) | `INTERNAL` |

## Implementation Notes

### Race Condition Prevention

For `Internal` mode, the enqueue returns immediately to allow the caller to subscribe to the pub/sub stream before the worker publishes data. This prevents a race condition where:
1. Job is enqueued and worker starts processing
2. Worker completes and publishes stream data
3. Caller tries to subscribe but data is already gone (pub/sub doesn't buffer)

### collect_stream()

Each runner implementing streaming should provide a `collect_stream()` method in its `RunnerSpec` trait implementation. This method:
1. Receives the stream of `ResultOutputItem`
2. Aggregates/merges the data chunks appropriately for the runner type
3. Returns the final collected bytes

For example, an LLM runner might concatenate all token chunks, while a command runner might merge stdout/stderr appropriately.

## Migration from FeedToStream to EnqueueWithClientStream

### Why Migrate

`FeedToStream` has several constraints that `EnqueueWithClientStream` eliminates:

- **`use_static=true` no longer required**: Feed channel is bound per `job_id`, not per runner instance
- **Channel concurrency=1 no longer required**: Job-level feed isolation removes the need for single-host constraint
- **No race condition risk**: Enqueue and feed happen in a single stream, no timing coordination needed
- **Scalable mode fully supported**: Redis List + BLPOP replaces Pub/Sub, eliminating message loss risk

### Important Changes

1. **Scalable mode**: `FeedToStream` no longer works in Scalable mode. Redis Pub/Sub has been replaced by Redis List + BLPOP.
2. **Field rename**: `need_feed` → `require_client_stream` (proto field number unchanged, wire-compatible)
3. **Trait rename**: `supports_feed()` → `supports_client_stream()`, `setup_feed_channel()` → `setup_client_stream_channel()`

### Migration Steps

1. Update runner's `need_feed=true` → `require_client_stream=true` (same proto field number, wire-compatible)
2. Replace the two-step Enqueue + FeedToStream flow with single `EnqueueWithClientStream` stream
3. Optionally relax `use_static=true` and concurrency=1 constraints if no longer needed

### Code Example

**Before (FeedToStream)**:
```text
1. EnqueueForStream(worker_id, args)
   → job_id from x-job-id-bin header
   → start receiving output stream
2. FeedToStream(job_id, data_chunk_1, is_final=false)
3. FeedToStream(job_id, data_chunk_2, is_final=false)
4. FeedToStream(job_id, last_chunk, is_final=true)
5. Receive remaining output + End trailer
```

**After (EnqueueWithClientStream)**:
```text
1. Open EnqueueWithClientStream stream
2. Send ClientStreamRequest(job_request)
   → receive x-job-id-bin header + start receiving output
3. Send ClientStreamRequest(feed_data: chunk_1, is_final=false)
4. Send ClientStreamRequest(feed_data: chunk_2, is_final=false)
5. Send ClientStreamRequest(feed_data: last_chunk, is_final=true)
6. Receive remaining output + End trailer
```
