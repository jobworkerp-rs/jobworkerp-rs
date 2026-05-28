# JobWorkerP Plugin Development Guide (V2 / async-ffi)

This guide explains how to build a JobWorkerP plugin against the **V2 plugin
trait (`MultiMethodPluginRunnerV2`)**. V2 is the newer trait that improves
cancellation handling and lets plugin authors write async work
directly; it is loaded through an independent FFI symbol
(`load_multi_method_plugin_v2`). V2 coexists with V1
([Plugin Development (V1)](./plugin-development.md)) on the same host
and does not break the binary compatibility of existing V1 plugins.

> **Caution**: The
> [Server Stability Warning in the V1 guide](./plugin-development.md#overview)
> applies unchanged: a panic in your plugin crashes the host. V2 adds one
> rule on top: build `tokio::runtime::Runtime` inside `load()` (which can
> return `Err`) rather than in `new()` (which cannot).

## Why V2

V1 (`MultiMethodPluginRunner`) is a synchronous trait. When a long-running
`run()` is interrupted by a timeout, the host cannot reclaim the variant
write lock and has to discard the wrapper instance. V2 fixes this:

| Aspect | V1 | V2 |
|--------|----|----|
| async surface | `fn` (synchronous) | `async_ffi::FfiFuture<T>` |
| cancellation | `cancel()` / `is_canceled()` | observe `CancellationToken` via `select!` |
| streaming | `begin_stream` + pull-based `receive_stream` | push-based `run_stream(args, ..., output: Sender<Vec<u8>>)` |
| lock release on timeout | held by the blocking call → wrapper discarded | future drop releases immediately, wrapper reusable |
| FFI symbol | `load_multi_method_plugin` | `load_multi_method_plugin_v2` |

New plugins should target V2.

## V2-specific Constraints (Read First)

### 1. Bring your own tokio runtime

Plugins are loaded as `cdylib`, so the `tokio` crate linked into the plugin
keeps its own `thread_local!` runtime context that is **invisible to the
host**. Calling `tokio::time::sleep`, `Handle::current()`, etc. inside an
`async move {}` block awaited directly on the host runtime will panic with
`there is no reactor running`.

Plugins MUST:

1. Own a `tokio::runtime::Runtime` built with `Builder::new_multi_thread()`
   and **at least one worker thread**.
   - `new_current_thread()` does not advance spawned tasks unless someone
     calls `block_on(...)`, so `handle.spawn(...)` queues but never runs,
     defeating cooperative cancellation.
2. `handle.spawn(...)` the actual async work onto that runtime.
3. Return an `FfiFuture` that awaits the resulting `JoinHandle` to bridge
   the result back to the host.

### 2. `FfiFuture<T>` is `Send + 'static`

The returned future cannot borrow `&mut self`. Move any state into the
`async move {}` block — `clone()` cheap handles like `CancellationToken`,
or hold mutable state behind `Arc<Mutex<_>>`.

### 3. Cancellation goes through `CancellationToken` only

V2 removed `cancel()` / `is_canceled()`. The host calls
`set_cancellation_token(token)` with a `tokio_util::sync::CancellationToken`;
plugins observe `token.cancelled().await` inside their spawned task via
`tokio::select!`.

`CancellationToken` is `Arc<AtomicBool> + Notify` internally — it is
**runtime-independent**, so cancelling on the host runtime and observing
on the plugin runtime works correctly.

### 4. Behaviour on timeout

The wrapper reports `should_detach_on_timeout() == false`, so a timeout
drops the host-side `FfiFuture` while keeping the wrapper instance
reusable. However the **task spawned on the plugin runtime keeps
running** until it either completes naturally or observes
`token.cancelled()`. The host therefore signals the token on timeout to
abort plugin-side work; without that signal the plugin will finish its
work as scheduled.

### 5. No tokio I/O in `Drop`

Do not call tokio I/O inside `Drop` impls of plugin structs. Either runtime
may already be shutting down by the time `Drop` runs, and tokio calls
outside a runtime context panic.

### 6. Pin workspace dependencies to match the host

The following crates cross the FFI boundary; pin them to the **same
workspace version as the host**:

- `async-ffi` — `FfiFuture<T>` layout
- `tokio` — `JoinHandle`, plus `Sender<Vec<u8>>` for `run_stream`
- `tokio-util` — `CancellationToken` layout

## Step-by-Step Guide

### 1. Cargo.toml

Start from the V1
[Cargo.toml block](./plugin-development.md#1-configure-cargotoml) and add
these crates (versions must exactly match the host's workspace pins):

```toml
[dependencies]
async-ffi = "0.5"
tokio-util = { version = "0.7", features = ["full"] }
```

### 2. Protobuf definitions and build.rs

Identical to V1 (see
[V1 guide](./plugin-development.md#2-define-protobufs)).
The sync metadata methods (`runner_settings_proto` /
`method_proto_map` / ...) have the same signatures and the same proto
definition constraints as V1.

### 3. Plugin implementation

```rust
use async_ffi::{FfiFuture, FutureExt};
use jobworkerp_runner::runner::plugins::{
    CancellationToken, MultiMethodPluginRunnerV2,
};
use std::collections::HashMap;
use std::time::Duration;

// 1. FFI loader (V2 symbol: load_multi_method_plugin_v2)
#[unsafe(no_mangle)]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn load_multi_method_plugin_v2()
    -> Box<dyn MultiMethodPluginRunnerV2 + Send + Sync>
{
    Box::new(MyV2Plugin::new())
}

#[unsafe(no_mangle)]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn free_multi_method_plugin_v2(
    ptr: Box<dyn MultiMethodPluginRunnerV2 + Send + Sync>,
) {
    drop(ptr);
}

// 2. Plugin struct
pub struct MyV2Plugin {
    /// Plugin-owned tokio runtime — the plugin's `tokio` has its own
    /// `thread_local!` context that the host cannot share. All async work
    /// must run on this runtime.
    rt: tokio::runtime::Runtime,
    token: Option<CancellationToken>,
}

impl MyV2Plugin {
    pub fn new() -> Self {
        // multi_thread with >= 1 worker is required. current_thread won't
        // advance spawned tasks unless someone calls block_on(), which
        // would defeat cooperative cancellation. One worker is enough if
        // the plugin does not need internal parallelism.
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("failed to build plugin tokio runtime");
        Self { rt, token: None }
    }
}

// 3. trait impl
impl MultiMethodPluginRunnerV2 for MyV2Plugin {
    fn name(&self) -> String { "MyV2Plugin".to_string() }
    fn description(&self) -> String { "V2 sample".to_string() }
    fn runner_settings_proto(&self) -> String {
        include_str!("../protobuf/my_runner.proto").to_string()
    }

    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        HashMap::from([(
            "run".to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: include_str!("../protobuf/my_job_args.proto").to_string(),
                result_proto: include_str!("../protobuf/my_result.proto").to_string(),
                description: Some("Main execution method".to_string()),
                output_type: proto::jobworkerp::data::StreamingOutputType::Both as i32,
                ..Default::default()
            },
        )])
    }

    fn set_cancellation_token(&mut self, token: CancellationToken) {
        // The host hands a fresh token per job. Older tokens belong to
        // completed/cancelled jobs; just overwrite.
        self.token = Some(token);
    }

    fn load(&mut self, _settings: Vec<u8>) -> FfiFuture<Result<(), String>> {
        async move { Ok(()) }.into_ffi()
    }

    fn run(
        &mut self,
        _args: Vec<u8>,
        metadata: Vec<(String, String)>,
        _using: Option<String>,
    ) -> FfiFuture<(Result<Vec<u8>, String>, Vec<(String, String)>)> {
        // Move state into the async block — the returned future is 'static
        // and cannot borrow &mut self.
        let token = self.token.clone();
        let handle = self.rt.handle().clone();

        // Spawn on the plugin runtime, then await the JoinHandle from the
        // FfiFuture so the result reaches the host.
        let join = handle.spawn(async move {
            match token {
                Some(t) => tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {
                        (Ok(b"done".to_vec()), metadata)
                    }
                    _ = t.cancelled() => {
                        (Err("cancelled".to_string()), metadata)
                    }
                },
                None => {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    (Ok(b"done".to_vec()), metadata)
                }
            }
        });

        async move {
            match join.await {
                Ok(out) => out,
                Err(e) => (Err(format!("join error: {e}")), Vec::new()),
            }
        }
        .into_ffi()
    }
}
```

## Streaming: `run_stream(output_sender)`

V2 streaming is push-based: the host creates an mpsc channel and hands
the sender to the plugin, instead of V1's pull-based
`begin_stream` + `receive_stream` loop.

```rust
fn run_stream(
    &mut self,
    args: Vec<u8>,
    metadata: Vec<(String, String)>,
    using: Option<String>,
    // Host-created output channel. The plugin sends chunks via `send()`.
    // Dropping the sender (typically by returning from the future)
    // signals end-of-stream and the host emits the End trailer.
    output: tokio::sync::mpsc::Sender<Vec<u8>>,
) -> FfiFuture<Result<Vec<(String, String)>, String>>;
```

- **Emit chunks**: call `output.send(chunk).await?` in order.
- **Termination**: when the plugin's future resolves with
  `Ok(final_metadata)`, the host attaches `final_metadata` to the End
  trailer and closes the stream.
- **Error before any chunk**: returning `Err(message)` without ever
  sending a chunk surfaces as the outer `run_stream` `Err`, marking the
  job as failed.
- **Error after some chunks**: the host emits the End trailer with the
  input metadata and logs the error; consumers have already received
  data, so the failure cannot be promoted to an outer `Err`
  (`ResultOutputItem` has no in-band error variant).
- **Cancellation**: dropping the host-side future drops the receiver;
  `output.send` then returns `Err`, or the plugin can `select!` on
  `token.cancelled()` to abort early.

### Example

```rust
fn run_stream(
    &mut self,
    _args: Vec<u8>,
    metadata: Vec<(String, String)>,
    _using: Option<String>,
    output: tokio::sync::mpsc::Sender<Vec<u8>>,
) -> FfiFuture<Result<Vec<(String, String)>, String>> {
    let token = self.token.clone();
    let handle = self.rt.handle().clone();

    let join = handle.spawn(async move {
        for i in 0..5 {
            let tick = tokio::time::sleep(Duration::from_millis(100));
            let cancelled = match &token {
                Some(t) => tokio::select! {
                    _ = tick => false,
                    _ = t.cancelled() => true,
                },
                None => { tick.await; false }
            };
            if cancelled { return Err("cancelled".to_string()); }
            if output.send(format!("chunk-{i}").into_bytes()).await.is_err() {
                // The host stopped consuming.
                return Err("output channel closed".to_string());
            }
        }
        Ok(metadata)
    });

    async move {
        match join.await {
            Ok(r) => r,
            Err(e) => Err(format!("join error: {e}")),
        }
    }
    .into_ffi()
}
```

### Compatibility note

`tokio::sync::mpsc::Sender<T>` is runtime-independent for the same reason
as `CancellationToken` (see [Constraint #3](#3-cancellation-goes-through-cancellationtoken-only)),
so the host-created sender works on the plugin runtime. The `tokio` crate
version must still match between host and plugin.

## Build and Deploy

Same as V1. `cargo build --release` produces the `.so`; drop it into the
directory pointed to by the `PLUGINS_RUNNER_DIR` env var. The host probes
for the V2 symbol (`load_multi_method_plugin_v2`) first, then falls back
to V1 (`load_multi_method_plugin`) and legacy (`load_plugin`).

## Migrating from V1 to V2

V1 ([Plugin Development (V1)](./plugin-development.md)) is still
supported; migration is optional. See the comparison table above for the
motivation. The mechanical changes are:

| V1 | V2 |
|----|----|
| `fn run(&mut self, ...) -> (Result<Vec<u8>>, HashMap<String, String>)` | `fn run(&mut self, ...) -> FfiFuture<(Result<Vec<u8>, String>, Vec<(String, String)>)>` |
| `metadata: HashMap<String, String>` | `metadata: Vec<(String, String)>` (ABI-stable) |
| `Result<Vec<u8>>` (anyhow) | `Result<Vec<u8>, String>` (ABI-stable) |
| `fn begin_stream` + `fn receive_stream` | `fn run_stream(.., output: Sender<Vec<u8>>) -> FfiFuture<Result<Vec<(String, String)>, String>>` |
| `fn cancel(&mut self) -> bool` | `fn set_cancellation_token(&mut self, token)` + observe via `tokio::select!` |
| `self.rt.block_on(async {...})` | `self.rt.handle().spawn(async {...})` + `JoinHandle::await` |

Reference implementation:
[`plugins/cancel_test/src/lib.rs`](https://github.com/jobworkerp-rs/jobworkerp-rs/blob/main/plugins/cancel_test/src/lib.rs)
