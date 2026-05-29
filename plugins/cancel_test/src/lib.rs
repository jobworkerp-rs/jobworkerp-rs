//! V2 plugin sample (high-level `PluginV2` trait + `register_plugin_v2!`).
//!
//! Demonstrates:
//! - Owning a dedicated tokio runtime inside the plugin (dylib + tokio
//!   `thread_local!` context isolation).
//! - Cooperative cancellation via the high-level `CancelToken` wrapper.
//! - Push-based streaming through `HighLevelSink::send`.
//!
//! See `manual/{en,ja}/src/plugin-development-v2.md` for the author guide.

use jobworkerp_plugin_abi::v2::{CancelToken, HighLevelSink, PluginV2};
use jobworkerp_plugin_abi_macros::register_plugin_v2;
use prost::Message;
use proto::DEFAULT_METHOD_NAME;
use proto::jobworkerp::data::MethodSchema;
use std::collections::HashMap;
use std::time::Duration;

pub struct CancelTestPlugin {
    /// Per-plugin tokio runtime — required because the dylib's tokio
    /// thread-local context is independent of the host runtime.
    rt: tokio::runtime::Runtime,
    token: Option<CancelToken>,
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
        _using: Option<String>,
        output: HighLevelSink,
    ) -> Result<HashMap<String, String>, String> {
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
