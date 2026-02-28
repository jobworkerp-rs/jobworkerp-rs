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

## Worker Pooling and use_static

When using `use_static=true` on workers (e.g., for local LLMs), the runner instance is pooled and reused across job executions. This is critical for resources that have expensive initialization.

`StreamingType::Internal` preserves this pooling behavior by:
1. Using the existing `worker_id` when enqueueing (not creating a temp worker)
2. Returning immediately from enqueue (not blocking on Direct response)
3. Allowing the caller to manage stream subscription independently

This ensures that heavy resources like local LLM models are not re-initialized for each job.

## FeedToStream: Sending Data to Running Streaming Jobs

The `FeedToStream` RPC allows clients to send additional data to a running streaming job. This enables interactive streaming scenarios such as real-time audio processing, where the client feeds audio chunks while the runner processes and returns results.

### Prerequisites

For a job to accept feed data, all of the following must be true:

| Condition | Reason |
|-----------|--------|
| Job is in `Running` status | Feed is only meaningful during execution |
| `streaming_type != None` | Runner must be using `run_stream()` |
| Worker has `use_static=true` | Runner instance must be pooled and persistent |
| Channel concurrency = 1 | Feed target runner must be unambiguous |
| Runner method has `need_feed=true` | Runner must explicitly support feed |

### Protocol

```protobuf
// In JobService
rpc FeedToStream(FeedToStreamRequest) returns (FeedToStreamResponse);

message FeedToStreamRequest {
  jobworkerp.data.JobId job_id = 1;  // Target job (from EnqueueForStream response header)
  bytes data = 2;                     // Feed data payload
  bool is_final = 3;                  // Signal end of feed
}

message FeedToStreamResponse {
  bool accepted = 1;
}
```

### Usage Flow

```
1. EnqueueForStream(worker_id, args) → job_id (from response header x-job-id-bin)
   ↓ (output stream starts)
2. FeedToStream(job_id, data_chunk_1, is_final=false)
3. FeedToStream(job_id, data_chunk_2, is_final=false)
4. FeedToStream(job_id, last_chunk, is_final=true)
   ↓ (runner processes final data, output stream ends)
5. Client receives remaining output and End trailer
```

### Data Transport

- **Scalable mode (Redis)**: Feed data is published via Redis Pub/Sub (`job_feed:{job_id}`), and a bridge task on the worker subscribes and forwards to the runner's `mpsc` channel.
- **Standalone mode (Channel)**: Feed data is sent directly via an in-process `mpsc` channel stored in `ChanFeedSenderStore`.

### Error Cases

| Case | gRPC Status |
|------|-------------|
| Job not found | `NOT_FOUND` |
| Job not running | `FAILED_PRECONDITION` |
| Job not streaming | `FAILED_PRECONDITION` |
| Runner method lacks `need_feed=true` | `FAILED_PRECONDITION` |
| Feed channel unavailable (job completed) | `UNAVAILABLE` |

For detailed specification, see `docs/feed-stream-spec.md`.

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
