# JobWorkerP plugin development (V2 / async-ffi)

This guide describes how to build a V2 JobWorkerP plugin against the
`PluginV2` trait and the `register_plugin_v2!` proc macro. V2 plugins are
loaded through the FFI symbol `load_multi_method_plugin_v2`. V1 (see
[Plugin development (V1)](./plugin-development.md)) and V2 plugins can
coexist on the same host process.

> **Warning**: the server stability constraints from the V1 guide
> ([overview](./plugin-development.md#overview)) still apply — a panic
> inside a plugin will tear down the whole host process. V2 additionally
> requires that `tokio::runtime::Runtime` construction live inside
> `load()` (which is fallible) rather than in `new()`.

## Why V2

V1 (`MultiMethodPluginRunner`) was a synchronous trait: a long-running
`run()` blocked on the host thread, so on timeout the host could not
reclaim the wrapper instance — it had to be discarded. V2 fixes that:

| Aspect | V1 | V2 |
|--------|----|----|
| Async surface | `fn` (synchronous) | `async fn` via `PluginV2` (the macro lowers to `FfiFuture<T>`) |
| Cancellation | `cancel()` / `is_canceled()` | `CancelToken::cancelled().await` inside `select!` |
| Streaming | `begin_stream` + pull `receive_stream` | `run_stream(args, ..., output: HighLevelSink)` (push-based) |
| Lock release on timeout | wrapper discarded | future drop releases the lock immediately |
| FFI symbol | `load_multi_method_plugin` | `load_multi_method_plugin_v2` |

Prefer V2 for new plugins.

## Architecture: two-layer API

The plugin author writes a **high-level** `PluginV2` impl using plain
Rust types (`Vec<u8>`, `HashMap<String, String>`, `Result<_, String>`).
The `register_plugin_v2!` proc macro lowers it to the **low-level**
`#[repr(C)]` FFI surface:

```text
impl PluginV2 for MyPlugin { async fn run(...) -> ... }
        |
        | register_plugin_v2!(MyPlugin, MyPlugin::new());
        v
static PLUGIN_VTABLE: PluginVtable = PluginVtable {
    name:  __thunk_name,   // extern "C" fn(*mut ()) -> FfiBytes
    run:   __thunk_run,    // extern "C" fn(...) -> FfiFuture<V2RunOutcome>
    ...
};
load_multi_method_plugin_v2() -> PluginInstanceRaw { state, vtable }
```

`#[repr(C)]` everywhere means:

- The host and plugin can use **independent rustc versions** (the trait
  object vtable no longer crosses the boundary).
- The host and plugin can use **independent `tokio` / `tokio-util`
  versions** (their types stay inside the plugin or host runtime; only
  `FfiCancellationToken` / `OutputSink` cross).
- The only required exact-pin is `async-ffi` (the `FfiFuture<T>` layout
  must match).

## V2-specific constraints (must read)

### 1. Bring your own tokio runtime

The plugin is loaded as a `cdylib`, so its linked `tokio` crate has
its own `thread_local!` runtime context that the **host runtime cannot
see**. Awaiting `tokio::time::sleep` / `Handle::current()` directly
inside the future returned to the host panics with
`there is no reactor running`.

Therefore a V2 plugin must:

1. Build its own `tokio::runtime::Runtime` with
   `Builder::new_multi_thread()` and **at least one worker thread**.
   (`new_current_thread()` only advances tasks while `block_on(...)`
   is on the stack, so `handle.spawn(...)` would queue work that never
   runs, defeating cooperative cancellation.)
2. Spawn actual async work via `handle.spawn(...)`.
3. Use `.await` to bridge the spawned `JoinHandle` back to the host
   through the high-level trait return value (see `cancel_test` for the
   canonical pattern).

### 2. The future returned by `async fn run/run_stream` is `Send + 'static`

Internally the proc macro converts your `async fn` body into an
`FfiFuture<T>` (which is `Send + 'static`). State you reference must be
cloned into the `async move {}` block or shared via `Arc<Mutex<...>>` —
the future cannot hold a `&mut self` borrow.

### 3. Cancellation goes through `CancelToken` only

V2 has no `cancel()` / `is_canceled()`. The host calls
`set_cancellation_token(token)` once per job (before `run`/`run_stream`).
Plugins observe cancellation by selecting on `token.cancelled().await`
inside their spawned task.

`CancelToken` wraps an `FfiCancellationToken` whose internals are an
`Arc<AtomicBool> + waker map`. **No tokio runtime is involved** — the
host can `cancel()` from any thread and the plugin's
`cancelled().await` resolves promptly.

### 4. Timeout behaviour

The host's `should_detach_on_timeout()` returns `false` for V2 plugins,
so on timeout the host drops the `FfiFuture` but keeps the wrapper. The
plugin-side spawned task, however, **keeps running on the plugin runtime
until it observes the token or completes naturally**.

The host must `token.cancel()` together with the timeout to free
plugin-side resources promptly.

### 5. No tokio I/O inside `Drop`

Don't call `tokio::time::sleep` (or other Handle-dependent APIs) inside
a `Drop` impl. The host runtime may already be in shutdown when the drop
runs, and the call would panic outside a runtime context.

### 6. Pin only `async-ffi`

The single exact-pinned dependency is `async-ffi = "=0.5.0"`. Tokio,
tokio-util and rustc can move freely between host and plugin. The
historic Constraint 6 has been narrowed to this single line.

## FFI safety contract

The proc macro buries the unsafe surface, but the contract underneath
remains:

1. **Panics are caught** at the FFI boundary. Each thunk wraps the
   synchronous portion in `std::panic::catch_unwind` and the async
   portion in `futures::FutureExt::catch_unwind`; panics from your code
   become `FfiResult::Err`. Catch-unwind cannot recover from
   foreign-exception panics — those abort per Rust 1.81+ semantics, so
   keep panicking paths short and explicit.
2. **Never `mem::forget`** an `FfiBytes` / `FfiVec` / `OutputSink` /
   `FfiCancellationToken` / `PluginInstance` — Drop is how the matching
   allocator hook is invoked.
3. **Sink concurrency** — `OutputSink::send` (via `HighLevelSink::send`)
   should be called sequentially per plugin task. Drop the sink (or let
   the spawned future drop it) only after all `send` futures have
   completed.
4. **Allocator hint** — `FfiBytes` carries its own free function so a
   plugin built against `#[global_allocator] = mimalloc` works against a
   host using the system allocator. Sharing the global allocator is
   still recommended for performance.

## Step-by-step guide

### 1. Cargo.toml

```toml
[package]
name = "my_plugin"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
# V2 plugin ABI — shared between jobworkerp-rs host and jobworkerp-client.
# Resolve via the jobworkerp-rs git repository so layout stays in sync.
# This one crate exposes everything plugin authors need: FFI types, the
# `PluginV2` trait, and the `register_plugin_v2!` proc macro (re-exported
# from `jobworkerp-plugin-abi-macros`).
jobworkerp-plugin-abi = { git = "https://gitea.sutr.app/jobworkerp-rs/jobworkerp-rs.git", branch = "main", package = "jobworkerp-plugin-abi" }

# A proto crate of your choice (host runtime proto, jobworkerp-client
# proto, or your own) to produce protobuf-encoded `MethodSchema` bytes.
proto       = { path = "../../proto" }
prost       = "0.14"

anyhow      = "1.0"
async-trait = "0.1"
tokio       = { version = "1", features = ["full"] }
```

`jobworkerp-plugin-abi` re-exports `async-ffi`, `async-trait`, `futures`,
`prost` and the `register_plugin_v2!` proc macro itself, so the macro
expansion resolves them through `::jobworkerp_plugin_abi::*` without
the plugin author needing extra dependencies.

### 2. Protobuf definition and build.rs

Identical to V1 — `runner_settings_proto` and `method_proto_map`
produce the same payloads. See the
[V1 guide section "Protobuf definitions"](./plugin-development.md#2-protobuf-definitions).

### 3. Plugin implementation

```rust
use jobworkerp_plugin_abi::register_plugin_v2;
use jobworkerp_plugin_abi::v2::{CancelToken, HighLevelSink, PluginV2};
use prost::Message;
use proto::DEFAULT_METHOD_NAME;
use proto::jobworkerp::data::MethodSchema;
use std::collections::HashMap;
use std::time::Duration;

pub struct MyPlugin {
    rt: tokio::runtime::Runtime,
    token: Option<CancelToken>,
}

impl Default for MyPlugin {
    fn default() -> Self { Self::new() }
}

impl MyPlugin {
    pub fn new() -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("plugin runtime");
        Self { rt, token: None }
    }
}

#[async_trait::async_trait]
impl PluginV2 for MyPlugin {
    fn name(&self) -> String { "MyPlugin".to_string() }
    fn description(&self) -> String { "Sample V2 plugin".to_string() }
    fn settings_schema(&self) -> String { String::new() }
    fn method_proto_map(&self) -> HashMap<String, Vec<u8>> {
        // `PluginV2` exchanges schemas as protobuf-encoded bytes so the
        // ABI crate stays proto-free. Encode with `prost::Message`.
        HashMap::from([(
            DEFAULT_METHOD_NAME.to_string(),
            MethodSchema::default().encode_to_vec(),
        )])
    }
    // `method_json_schema_map` defaults to `None` — the host derives
    // JSON schemas from `method_proto_map` automatically.

    fn set_cancellation_token(&mut self, token: CancelToken) {
        self.token = Some(token);
    }

    async fn load(&mut self, _settings: Vec<u8>) -> Result<(), String> { Ok(()) }

    async fn run(
        &mut self,
        _args: Vec<u8>,
        metadata: HashMap<String, String>,
        _using: Option<String>,
    ) -> (Result<Vec<u8>, String>, HashMap<String, String>) {
        let token = self.token.clone();
        let handle = self.rt.handle().clone();
        let join = handle.spawn(async move {
            match token {
                Some(t) => tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(500)) => {
                        (Ok(b"done".to_vec()), metadata)
                    }
                    _ = t.cancelled() => {
                        (Err("cancelled".to_string()), metadata)
                    }
                },
                None => {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    (Ok(b"done".to_vec()), metadata)
                }
            }
        });
        join.await.unwrap_or_else(|e| (Err(format!("join: {e}")), HashMap::new()))
    }

    async fn run_stream(
        &mut self,
        _args: Vec<u8>,
        metadata: HashMap<String, String>,
        _using: Option<String>,
        output: HighLevelSink,
    ) -> Result<HashMap<String, String>, String> {
        let token = self.token.clone();
        let handle = self.rt.handle().clone();
        let join = handle.spawn(async move {
            for i in 0..5u32 {
                if let Some(t) = &token {
                    if t.is_cancelled() { return Err("cancelled".to_string()); }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                output.send(format!("chunk {i}").into_bytes()).await
                    .map_err(|e| format!("sink closed: {e}"))?;
            }
            Ok(metadata)
        });
        join.await.unwrap_or_else(|e| Err(format!("join: {e}")))
    }
}

register_plugin_v2!(MyPlugin, MyPlugin::new());
```

The macro form is `register_plugin_v2!(PluginType, init_expr)`. The
init expression runs once per `load_multi_method_plugin_v2` call (one
per logical plugin instance), so plugin authors can perform environment
reads or other fallible setup before returning the constructed plugin.

## Migration from earlier V2 builds

Plugins built against the previous V2 trait-object surface
(`Box<dyn MultiMethodPluginRunnerV2>`) must be recompiled against the
new API. The FFI symbol name has not changed, but the return type has:
the host now expects `PluginInstanceRaw { state, vtable }` and rejects
old `Box<dyn Trait>` payloads via vtable-header sanity checks. Plugin
repositories using V2 should:

1. Update the `jobworkerp-runner` (and `jobworkerp-client`) dependency
   to a commit that includes this work.
2. Replace `impl MultiMethodPluginRunnerV2 for ...` with the
   `impl PluginV2 for ...` shape shown above.
3. Replace `extern "C" fn load_multi_method_plugin_v2(...)` with
   `register_plugin_v2!(MyPlugin, MyPlugin::new());`.

`llama-cpp-plugin` is the only known V2 user in the wild; it must be
released in lockstep with this change.

## Performance

The streaming hot path was benchmarked with
`cargo run --release --bin sink_throughput -p jobworkerp-runner`
(`runner/benches/sink_throughput.rs`):

| Path | 10000 × 10 bytes |
|------|------------------|
| Baseline `tokio::mpsc::Sender::send` | ~2.7 ms |
| `OutputSink::send_raw` (V2) | ~2.5 ms |

`OutputSink::send_raw` runs within the 1.5x budget we set for the FFI
boundary; in this measurement it was actually slightly faster than the
tokio baseline. If a future change pushes the ratio outside the budget,
swap the per-call `FfiFuture` allocation for a `try_send` + readiness
notification scheme.

## Reference implementation

See `plugins/cancel_test/src/lib.rs` for a complete working example
that exercises every path: sync metadata, cooperative cancellation,
streaming with the Drop contract honoured, and `register_plugin_v2!`
usage.
