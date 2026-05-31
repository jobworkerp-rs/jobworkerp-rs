//! V2 plugin sample (high-level `PluginV2` trait + `register_plugin_v2!`).
//!
//! Demonstrates:
//! - Owning a dedicated tokio runtime inside the plugin (dylib + tokio
//!   `thread_local!` context isolation).
//! - Cooperative cancellation via the high-level `CancelToken` wrapper.
//! - Push-based streaming through `HighLevelSink::send`.
//!
//! See `manual/{en,ja}/src/plugin-development-v2.md` for the author guide.

use jobworkerp_plugin_abi::register_plugin_v2;
use jobworkerp_plugin_abi::v2::{CancelToken, HighLevelSink, PluginV2};
use prost::Message;
use proto::DEFAULT_METHOD_NAME;
use proto::jobworkerp::data::MethodSchema;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::sync::mpsc;

/// Shared slot the plugin uses to hand the v2 feed receiver from
/// `setup_client_stream_channel_v2` over to `run_stream`. Aliased to
/// keep the struct field below readable (and to keep clippy's
/// `type_complexity` lint quiet).
type FeedV2RxSlot = std::sync::Arc<Mutex<Option<mpsc::Receiver<(Vec<u8>, bool)>>>>;

pub struct CancelTestPlugin {
    /// Per-plugin tokio runtime — required because the dylib's tokio
    /// thread-local context is independent of the host runtime.
    rt: tokio::runtime::Runtime,
    token: Option<CancelToken>,
    /// Receiver paired with the `OutputSinkWithFinal` returned from
    /// `setup_client_stream_channel_v2`. `run_stream` consumes it when
    /// `using == Some("feed_v2")` so the test can observe in-band
    /// `is_final` delivery via the new ABI slot.
    feed_v2_rx: FeedV2RxSlot,
}

impl Default for CancelTestPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelTestPlugin {
    pub fn new() -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("failed to build plugin tokio runtime");
        Self {
            rt,
            token: None,
            feed_v2_rx: std::sync::Arc::new(Mutex::new(None)),
        }
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

#[async_trait::async_trait]
impl PluginV2 for CancelTestPlugin {
    fn name(&self) -> String {
        "CancelTest".to_string()
    }
    fn description(&self) -> String {
        "V2 plugin sample: async-ffi + cooperative cancellation".to_string()
    }
    fn settings_schema(&self) -> String {
        // No configurable settings.
        String::new()
    }
    fn method_proto_map(&self) -> HashMap<String, Vec<u8>> {
        // Single DEFAULT_METHOD_NAME entry so the wrapper treats this as a
        // single-method plugin and bypasses `using` validation. The
        // `PluginV2` trait exchanges protobuf-encoded bytes so the ABI
        // crate doesn't have to depend on the proto definitions.
        HashMap::from([(
            DEFAULT_METHOD_NAME.to_string(),
            MethodSchema::default().encode_to_vec(),
        )])
    }
    // `method_json_schema_map` defaults to `None`, which lets the host
    // synthesise JSON schemas from `method_proto_map` automatically.

    fn set_cancellation_token(&mut self, token: CancelToken) {
        self.token = Some(token);
    }

    fn supports_client_stream(&self, using: Option<&str>) -> bool {
        // Only the "feed_v2" alias exercises the minor-1 ABI slot. Tests
        // pin to this value so existing run_stream callers (which pass
        // None) stay on the original non-feed path.
        using == Some("feed_v2")
    }

    /// Provide a `(Vec<u8>, bool)` receiver paired with the
    /// `OutputSinkWithFinal` the host writes feed chunks into. Stashing
    /// the receiver on `self` lets `run_stream` drain it when invoked
    /// with `using = Some("feed_v2")`.
    async fn setup_client_stream_channel_v2(
        &mut self,
        using: Option<&str>,
    ) -> Option<jobworkerp_plugin_abi::OutputSinkWithFinal> {
        if using != Some("feed_v2") {
            return None;
        }
        let (tx, rx) = mpsc::channel::<(Vec<u8>, bool)>(16);
        // Replace any leftover receiver from a previous session so
        // back-to-back invocations start from a clean state.
        *self.feed_v2_rx.lock().await = Some(rx);
        Some(jobworkerp_plugin_abi::OutputSinkWithFinal::from_sender(tx))
    }

    async fn load(&mut self, _settings: Vec<u8>) -> Result<(), String> {
        Ok(())
    }

    /// Sleep for the requested duration unless the host signals cancellation
    /// via the stored token. Work runs on the plugin's own runtime so
    /// `tokio::time::sleep` has a reactor; the FfiFuture awaits the
    /// resulting `JoinHandle` to bridge the result back to the host.
    async fn run(
        &mut self,
        args: Vec<u8>,
        metadata: HashMap<String, String>,
        _using: Option<String>,
    ) -> (Result<Vec<u8>, String>, HashMap<String, String>) {
        let sleep_ms = parse_sleep_ms(&args);
        let token = self.token.clone();
        let handle = self.rt.handle().clone();
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
        join.await
            .unwrap_or_else(|e| (Err(format!("plugin task join error: {e}")), HashMap::new()))
    }

    /// Streaming variant: emit `total_ms / 100` chunks at 100 ms intervals,
    /// each chunk being the index as ASCII bytes. Cancellation via the
    /// token aborts the stream early.
    ///
    /// The `HighLevelSink` is moved into the spawned task and dropped only
    /// after the task returns, so no in-flight `send` future ever outlives
    /// the sink (V2 drop contract).
    async fn run_stream(
        &mut self,
        args: Vec<u8>,
        metadata: HashMap<String, String>,
        using: Option<String>,
        output: HighLevelSink,
    ) -> Result<HashMap<String, String>, String> {
        // `err:<msg>` sentinel: return the message immediately, before any
        // chunk is sent. Exercises the host's pre-stream Err path so it can
        // be verified without resorting to a synthetic plugin.
        if let Some(msg) = std::str::from_utf8(&args)
            .ok()
            .and_then(|s| s.strip_prefix("err:"))
        {
            drop(output);
            return Err(msg.to_string());
        }

        // `using == "feed_v2"`: drain the `(Vec<u8>, bool)` receiver paired
        // with the `OutputSinkWithFinal` registered earlier. Each chunk is
        // echoed back through the `HighLevelSink` so the test can assert
        // the host forwarder routed feed data here, and the loop exits as
        // soon as `is_final == true` arrives — exercising the in-band EOF
        // signalling the minor-1 ABI was added for.
        if using.as_deref() == Some("feed_v2") {
            let feed_slot = self.feed_v2_rx.clone();
            let handle = self.rt.handle().clone();
            let join = handle.spawn(async move {
                let mut maybe_rx = feed_slot.lock().await;
                let Some(mut rx) = maybe_rx.take() else {
                    return Err("feed_v2 receiver not initialised".to_string());
                };
                drop(maybe_rx);
                while let Some((data, is_final)) = rx.recv().await {
                    // Echo back so the test can see the chunk; the
                    // `is_final` boolean is encoded as a trailing byte
                    // (`1` / `0`) so the consumer's assertions can stay
                    // simple.
                    let mut framed = data;
                    framed.push(if is_final { 1 } else { 0 });
                    if let Err(e) = output.send(framed).await {
                        return Err(format!("output closed: {e}"));
                    }
                    if is_final {
                        return Ok(metadata);
                    }
                }
                // Receiver yielded None before is_final was observed —
                // surface as Err so the test can distinguish this from
                // the clean is_final path.
                Err("feed_v2 receiver closed before is_final".to_string())
            });
            return join
                .await
                .unwrap_or_else(|e| Err(format!("plugin task join error: {e}")));
        }

        let total_ms = parse_sleep_ms(&args);
        let token = self.token.clone();
        let handle = self.rt.handle().clone();
        let join = handle.spawn(async move {
            let chunks = (total_ms / 100).max(1);
            for i in 0..chunks {
                let cancelled = match &token {
                    Some(t) => tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(100)) => false,
                        _ = t.cancelled() => true,
                    },
                    None => {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        false
                    }
                };
                if cancelled {
                    return Err("cancelled".to_string());
                }
                // Each send awaits delivery before moving on; no parallel
                // send futures, so the drop contract is trivially met.
                if let Err(e) = output.send(format!("{i}").into_bytes()).await {
                    return Err(format!("output closed: {e}"));
                }
            }
            Ok(metadata)
            // `output` drops here, after the loop — sink Drop is safe
            // because no send futures outlive this scope.
        });
        join.await
            .unwrap_or_else(|e| Err(format!("plugin task join error: {e}")))
    }
}

register_plugin_v2!(CancelTestPlugin, CancelTestPlugin::new());
