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
- `EnqueueWithClientStream` combines job enqueue and feed data delivery into a single bidirectional stream.

## Worker Pooling and use_static

When using `use_static=true` on workers (e.g., for local LLMs), the runner instance is pooled and reused across job executions. This is critical for resources that have expensive initialization.

`StreamingType::Internal` preserves this pooling behavior by:
1. Using the existing `worker_id` when enqueueing (not creating a temp worker)
2. Returning immediately from enqueue (not blocking on Direct response)
3. Allowing the caller to manage stream subscription independently

This ensures that heavy resources like local LLM models are not re-initialized for each job.

## EnqueueWithClientStream: Client Streaming

The `EnqueueWithClientStream` RPC combines job enqueue and client streaming data delivery into a single bidirectional gRPC stream.

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

- **Standalone mode**: Feed data is delivered via in-process `mpsc` channel (`ChanFeedSenderStore`). The gRPC handler does **not** wait for the Dispatcher to register the feed channel — it starts reading the client stream immediately and calls `publish_feed`, which buffers chunks in the per-job `JobFeedState.buffer` when the runner has not yet registered. On registration the host drains the buffer in arrival order via `drain_loop`, so feeds sent before the runner pool slot was free are never lost.
- **Scalable mode**: Feed data is pushed to a Redis List (`job_feed_buf:{job_id}`) via RPUSH. The worker's feed bridge reads via BLPOP. No message loss since the List buffers data.

In both modes the gRPC frontend returns its response stream as soon as the V2 host has handed back its `BoxStream` (which now happens before the plugin emits its first chunk); the feed forwarder runs concurrently. This means a client that only starts sending feed data after observing the response cannot deadlock against a busy `use_static=true, concurrency=1` worker pool.

### In-band `is_final` delivery to plugins (ABI minor 1+)

V2 plugin ABI minor 1 adds `OutputSinkWithFinal` so the host can carry the originating `is_final` flag all the way to the plugin's feed receiver. Plugins that opt in implement `PluginV2::setup_client_stream_channel_v2` and receive `(Vec<u8>, bool)` chunks; they can finish `run_stream` deterministically the moment `is_final == true` arrives.

- **Plugin built against minor 0** (or returning `None` from the v2 setup): host falls back to the original `setup_client_stream_channel`. EOF reaches the plugin only when its mpsc `Receiver` yields `None`, which happens once the host drops the sink (after observing `is_final=true` on the wire).
- **Plugin built against minor 1+ that returns `Some` from the v2 setup**: host forwards each feed chunk as `OutputSinkWithFinal::send_raw_with_final(bytes, is_final)`; the plugin sees the flag on its `mpsc::Receiver<(Vec<u8>, bool)>` and can return from `run_stream` without waiting for the receiver to close.

The host also drops the sink after the final chunk in the minor-1 path, so plugins that ignore the flag still observe the legacy EOF cue — opting in is purely additive. The minor bump is forward-compatible: existing `.so` files keep working unchanged. See [Plugin development (V2)](./plugin-development-v2.md#in-band-is_final-on-feed-chunks-minor-1-abi) for the plugin author's perspective.

### Error Cases

| Case | gRPC Status | Timing |
|------|-------------|--------|
| First message is not `job_request` | `INVALID_ARGUMENT` | Stream start |
| Worker not found | `NOT_FOUND` | Stream start |
| Runner does not support client streaming | `INVALID_ARGUMENT` | Stream start |
| Runner does not support streaming output | `INVALID_ARGUMENT` | Stream start |
| `job_request` sent after first message | `INVALID_ARGUMENT` | During feed |
| Pending feed buffer exceeds `PENDING_BUFFER_MAX` (1024) before runner registers | Oldest non-final chunk dropped, warning logged | During feed |
| No chunk emitted for `JobData.timeout` ms while the runner stream is being drained | Runner cancellation token fired, stream terminated with End trailer | During execution |
| Runner execution error | Status code per `ResultStatus` | During execution |
| Client disconnect (no half-close) | Cancel notification to runner | Any time |

### Configuration

| Environment Variable | Default | Description |
|---------------------|---------|-------------|
| `JOB_QUEUE_FEED_DISPATCH_TIMEOUT` | `5000` | In **Scalable** mode this seeds the Redis List TTL (`ttl_secs = feed_dispatch_timeout * 2`). In **Standalone** mode it is no longer used for feed delivery — the pre-registration buffer (`PENDING_BUFFER_MAX = 1024`) bounds memory instead, and stream-drain stalls are handled by the per-job `JobData.timeout` idle window. |

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
