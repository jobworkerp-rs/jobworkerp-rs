//! V2 plugin sample (async-ffi).
//!
//! Reference implementation cited from the plugin development guide; the
//! authoritative documentation lives in the manual and rustdoc:
//! - `manual/en/src/plugin-development-v2.md` (English) /
//!   `manual/ja/src/plugin-development-v2.md` (Japanese): step-by-step
//!   author guide, including the six dylib/runtime constraints summarized
//!   below.
//! - `MultiMethodPluginRunnerV2` rustdoc: trait-level contract.
//!
//! Constraints in one line:
//! 1. Bring your own multi-thread tokio runtime (>=1 worker); spawn there.
//! 2. `FfiFuture<T>` is `Send + 'static`; move state in via clone/`Arc`.
//! 3. `CancellationToken` is the only cancel channel; `select!` on it.
//! 4. Host drops the future on timeout; plugin task continues until it
//!    observes the token — so always race against `token.cancelled()`.
//! 5. No tokio I/O in `Drop`.
//! 6. Pin `async-ffi`, `tokio`, `tokio-util` to the host's workspace
//!    versions; their layouts cross the FFI boundary.

use anyhow::Result;
use async_ffi::{FfiFuture, FutureExt};
use jobworkerp_runner::runner::plugins::{CancellationToken, MultiMethodPluginRunnerV2};
use std::collections::HashMap;
use std::time::Duration;

#[unsafe(no_mangle)]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn load_multi_method_plugin_v2() -> Box<dyn MultiMethodPluginRunnerV2 + Send + Sync>
{
    Box::new(CancelTestPlugin::new())
}

#[unsafe(no_mangle)]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn free_multi_method_plugin_v2(
    ptr: Box<dyn MultiMethodPluginRunnerV2 + Send + Sync>,
) {
    drop(ptr);
}

pub struct CancelTestPlugin {
    /// Per-plugin tokio runtime. See module docs for why this is necessary
    /// (dylib + tokio `thread_local!` context isolation).
    rt: tokio::runtime::Runtime,
    token: Option<CancellationToken>,
}

impl Default for CancelTestPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelTestPlugin {
    pub fn new() -> Self {
        // A multi-threaded runtime with a dedicated worker is required here:
        // tasks spawned via `handle.spawn()` are driven by runtime worker
        // threads on their own. A `new_current_thread()` runtime drives tasks
        // only inside `block_on(...)`, so `handle.spawn(...)` would queue the
        // task and never advance — making cooperative cancellation impossible
        // because the spawned future would never reach its `tokio::select!`.
        //
        // One worker is enough for this single-task-at-a-time sample. Plugins
        // that need internal parallelism may use more workers.
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("failed to build plugin tokio runtime");
        Self { rt, token: None }
    }
}

/// Parse a sentinel `sleep:<ms>` from raw args; default to 2000 ms.
fn parse_sleep_ms(args: &[u8]) -> u64 {
    std::str::from_utf8(args)
        .ok()
        .and_then(|s| s.strip_prefix("sleep:"))
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(2000)
}

/// Bridge a plugin-runtime `JoinHandle` into an `FfiFuture` the host can
/// await. Wraps `JoinError` into the plugin's `T` via `on_join_err`. This
/// pattern (spawn on plugin runtime → await JoinHandle in FfiFuture) is the
/// canonical way to keep tokio I/O on the plugin runtime while still
/// surfacing the result through the FFI boundary.
fn bridge_join<T: Send + 'static>(
    join: tokio::task::JoinHandle<T>,
    on_join_err: impl FnOnce(tokio::task::JoinError) -> T + Send + 'static,
) -> FfiFuture<T> {
    async move {
        match join.await {
            Ok(out) => out,
            Err(e) => on_join_err(e),
        }
    }
    .into_ffi()
}

impl MultiMethodPluginRunnerV2 for CancelTestPlugin {
    fn name(&self) -> String {
        "CancelTest".to_string()
    }

    fn description(&self) -> String {
        "V2 plugin sample: async-ffi + cooperative cancellation".to_string()
    }

    fn runner_settings_proto(&self) -> String {
        String::new()
    }

    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        // Single DEFAULT_METHOD_NAME entry so the wrapper treats this as a
        // single-method plugin and bypasses the `using` validation in run().
        HashMap::from([(
            proto::DEFAULT_METHOD_NAME.to_string(),
            proto::jobworkerp::data::MethodSchema::default(),
        )])
    }

    fn set_cancellation_token(&mut self, token: CancellationToken) {
        self.token = Some(token);
    }

    fn load(&mut self, _settings: Vec<u8>) -> FfiFuture<Result<(), String>> {
        async move { Ok(()) }.into_ffi()
    }

    /// Sleep for the requested duration unless the host signals cancellation
    /// via the stored token.
    ///
    /// The work is spawned on the plugin's own runtime (so `tokio::time::sleep`
    /// has a reactor); the FfiFuture awaits the resulting `JoinHandle` to
    /// bridge the result back to the host. If the host drops the FfiFuture
    /// (e.g., on timeout) the JoinHandle is dropped too, but the spawned task
    /// keeps running until it observes `token.cancelled()` — so the host MUST
    /// signal the token to actually free plugin-side resources.
    fn run(
        &mut self,
        args: Vec<u8>,
        metadata: Vec<(String, String)>,
        _using: Option<String>,
    ) -> FfiFuture<(Result<Vec<u8>, String>, Vec<(String, String)>)> {
        let token = self.token.clone();
        let sleep_ms = parse_sleep_ms(&args);
        let handle = self.rt.handle().clone();

        // Spawn the actual work on the plugin runtime, then await the
        // JoinHandle from the host runtime side via the returned FfiFuture.
        let join = handle.spawn(async move {
            match token {
                Some(t) => tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(sleep_ms)) => {
                        (Ok(b"completed".to_vec()), metadata)
                    }
                    _ = t.cancelled() => {
                        (Err("cancelled".to_string()), metadata)
                    }
                },
                None => {
                    tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
                    (Ok(b"completed".to_vec()), metadata)
                }
            }
        });

        bridge_join(join, |e| {
            (Err(format!("plugin task join error: {e}")), Vec::new())
        })
    }

    /// Streaming variant of `run`: emit `sleep_ms / 100` chunks at 100 ms
    /// intervals, each chunk being the index as ASCII bytes. Cancellation via
    /// the token aborts the stream early. Returns the input metadata
    /// unchanged as the End trailer.
    ///
    /// Demonstrates the push-based V2 stream contract: the host hands an
    /// `mpsc::Sender<Vec<u8>>` and the plugin emits chunks until done or
    /// cancelled. The returned `FfiFuture` resolves with the final metadata.
    fn run_stream(
        &mut self,
        args: Vec<u8>,
        metadata: Vec<(String, String)>,
        _using: Option<String>,
        output: tokio::sync::mpsc::Sender<Vec<u8>>,
    ) -> FfiFuture<Result<Vec<(String, String)>, String>> {
        let token = self.token.clone();
        let total_ms = parse_sleep_ms(&args);
        let handle = self.rt.handle().clone();

        let join = handle.spawn(async move {
            let chunk_count = (total_ms / 100).max(1);
            for i in 0..chunk_count {
                // Race a 100ms tick against cancellation.
                let tick = tokio::time::sleep(Duration::from_millis(100));
                let cancelled = match &token {
                    Some(t) => tokio::select! {
                        _ = tick => false,
                        _ = t.cancelled() => true,
                    },
                    None => {
                        tick.await;
                        false
                    }
                };
                if cancelled {
                    return Err("cancelled".to_string());
                }
                let chunk = format!("{i}").into_bytes();
                if output.send(chunk).await.is_err() {
                    // Host dropped the receiver; nothing more to do.
                    return Err("output channel closed".to_string());
                }
            }
            Ok(metadata)
        });

        bridge_join(join, |e| Err(format!("plugin task join error: {e}")))
    }
}
