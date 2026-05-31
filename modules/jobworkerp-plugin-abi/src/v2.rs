//! High-level V2 plugin trait and convenience wrappers.
//!
//! Plugin authors implement [`PluginV2`] with ordinary Rust types
//! (`Vec<u8>`, `HashMap<String, String>`, `Result<_, String>`); the
//! `register_plugin_v2!` proc-macro (in the
//! `jobworkerp-plugin-abi-macros` crate) generates the `extern "C"`
//! thunks that marshal to the FFI-safe vtable in
//! [`crate::vtable::PluginVtable`].

use crate::cancel::FfiCancellationToken;
use crate::sink::{OutputSink, OutputSinkWithFinal};
use crate::types::FfiResult;
use std::collections::HashMap;
use std::future::Future;

/// High-level cooperative cancellation token surfaced to plugin authors.
///
/// Internally wraps an [`FfiCancellationToken`] and shares its underlying
/// `Arc<TokenInner>`. `clone` calls the host-provided `clone_token`
/// vtable function so the reference count is managed by the same code
/// that produced it.
pub struct CancelToken {
    inner: FfiCancellationToken,
}

impl CancelToken {
    /// Wrap an existing FFI token. The token's strong reference is moved
    /// into `self`.
    pub fn from_ffi(inner: FfiCancellationToken) -> Self {
        Self { inner }
    }

    /// Borrow the underlying FFI token. Useful for tests and bridging.
    pub fn as_ffi(&self) -> &FfiCancellationToken {
        &self.inner
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    pub fn cancelled(&self) -> impl Future<Output = ()> + '_ {
        self.inner.cancelled()
    }
}

impl Clone for CancelToken {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone_ffi(),
        }
    }
}

/// High-level streaming output sink surfaced to plugin authors.
///
/// `send` is bound to `&self`, so the borrow checker rejects most
/// scenarios where an in-flight `send` future would outlive the sink.
pub struct HighLevelSink {
    inner: OutputSink,
}

impl HighLevelSink {
    pub fn from_ffi(inner: OutputSink) -> Self {
        Self { inner }
    }

    /// Send a chunk to the host and await delivery.
    ///
    /// On `Err` the payload is the bytes that failed to deliver (typically
    /// because the host-side receiver was dropped). The future borrows
    /// `&self`, so the sink cannot be dropped while sends are pending.
    pub async fn send(&self, bytes: Vec<u8>) -> Result<(), String> {
        let fut = self.inner.send_raw(bytes);
        match fut.await {
            FfiResult::Ok(()) => Ok(()),
            FfiResult::Err(unsent) => {
                // `unsent` was allocated on the host side (sink thunk),
                // so the plugin must consume it through `copy_to_vec`
                // rather than reclaiming the buffer with its own
                // allocator.
                let payload = unsent.copy_to_vec();
                Err(format!(
                    "output sink closed; {} bytes undelivered",
                    payload.len()
                ))
            }
        }
    }
}

/// High-level streaming output sink that carries the originating
/// `is_final` flag.
///
/// Companion of [`HighLevelSink`]; the only difference is
/// `send(bytes, is_final)` — plugins that opt into the minor-1 ABI by
/// implementing `PluginV2::setup_client_stream_channel_v2` receive one
/// of these and can finish their `run_stream` deterministically when
/// they observe `is_final=true` (instead of having to wait for the
/// receiver's `None` from sink drop).
pub struct HighLevelSinkWithFinal {
    inner: OutputSinkWithFinal,
}

impl HighLevelSinkWithFinal {
    pub fn from_ffi(inner: OutputSinkWithFinal) -> Self {
        Self { inner }
    }

    /// Send a chunk together with the originating `is_final` flag and
    /// await delivery. Same error semantics as `HighLevelSink::send`.
    pub async fn send(&self, bytes: Vec<u8>, is_final: bool) -> Result<(), String> {
        let fut = self.inner.send_raw_with_final(bytes, is_final);
        match fut.await {
            FfiResult::Ok(()) => Ok(()),
            FfiResult::Err(unsent) => {
                let payload = unsent.copy_to_vec();
                Err(format!(
                    "output sink closed; {} bytes undelivered",
                    payload.len()
                ))
            }
        }
    }
}

/// Plugin author trait. Implementations use ordinary Rust types; the
/// `register_plugin_v2!` proc-macro (in `jobworkerp-plugin-abi-macros`)
/// generates the FFI thunks bridging to [`crate::vtable::PluginVtable`].
///
/// # Why `Vec<u8>` for the schema maps
///
/// Schema values cross the boundary as protobuf-encoded bytes
/// (`MethodSchema::encode_to_vec()` etc.) so this trait does **not**
/// depend on any protobuf-generated types. The host decodes the bytes
/// into its own proto types after receiving them. Plugin authors are
/// expected to depend on a proto crate (host or client) of their choice
/// and encode the messages themselves.
#[async_trait::async_trait]
pub trait PluginV2: Send + Sync + 'static {
    fn name(&self) -> String;
    fn description(&self) -> String;
    fn runner_settings_proto(&self) -> String {
        String::new()
    }
    fn settings_schema(&self) -> String;

    /// Map of method name → protobuf-encoded `MethodSchema` payload.
    /// Use `schema.encode_to_vec()` on the plugin side.
    fn method_proto_map(&self) -> HashMap<String, Vec<u8>>;

    /// Map of method name → protobuf-encoded `MethodJsonSchema` payload.
    /// Returning `None` triggers automatic conversion from
    /// `method_proto_map()` on the host side.
    fn method_json_schema_map(&self) -> Option<HashMap<String, Vec<u8>>> {
        None
    }
    fn supports_client_stream(&self, _using: Option<&str>) -> bool {
        false
    }
    fn client_stream_data_proto(&self, _using: Option<&str>) -> Option<String> {
        None
    }
    /// Returns an `OutputSink` whose Receiver lives inside the plugin.
    /// The host writes chunks into the returned sink; the plugin's
    /// internal receiver consumes them.
    async fn setup_client_stream_channel(&mut self, _using: Option<&str>) -> Option<OutputSink> {
        None
    }

    /// Minor-1 (ABI version `1.1`) opt-in variant of
    /// `setup_client_stream_channel`. Plugins that override this method
    /// receive an [`OutputSinkWithFinal`] whose paired receiver yields
    /// `(Vec<u8>, bool)` so the originating `is_final` flag arrives
    /// in-band — allowing `run_stream` to return deterministically when
    /// the final feed has been observed, instead of having to wait for
    /// `Receiver::recv == None` (which only fires when the host drops
    /// the sink).
    ///
    /// Default returns `None`, in which case the host falls back to
    /// `setup_client_stream_channel` and uses sink-drop as the sole EOF
    /// signal — exactly the pre-minor-1 behaviour.
    ///
    /// Plugins SHOULD implement only one of the two streaming setup
    /// methods. If both return `Some`, the host prefers the v2 sink and
    /// the v1 sink's receiver will never see data; the v1 sink is then
    /// dropped immediately, which still produces `None` on its paired
    /// receiver.
    async fn setup_client_stream_channel_v2(
        &mut self,
        _using: Option<&str>,
    ) -> Option<OutputSinkWithFinal> {
        None
    }

    fn set_cancellation_token(&mut self, token: CancelToken);

    async fn load(&mut self, settings: Vec<u8>) -> Result<(), String>;

    async fn run(
        &mut self,
        args: Vec<u8>,
        metadata: HashMap<String, String>,
        using: Option<String>,
    ) -> (Result<Vec<u8>, String>, HashMap<String, String>);

    async fn run_stream(
        &mut self,
        args: Vec<u8>,
        metadata: HashMap<String, String>,
        using: Option<String>,
        output: HighLevelSink,
    ) -> Result<HashMap<String, String>, String>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cancel::FfiCancellationToken;
    use crate::sink::OutputSink;
    use std::time::Duration;
    use tokio::sync::mpsc;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_token_clone_uses_vtable_clone() {
        let (ffi, handle) = FfiCancellationToken::new_owned();
        let token = CancelToken::from_ffi(ffi);
        let clone = token.clone();
        assert!(!token.is_cancelled());
        assert!(!clone.is_cancelled());
        handle.cancel();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(token.is_cancelled());
        assert!(clone.is_cancelled());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn high_level_sink_send_round_trip() {
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(4);
        let sink = HighLevelSink::from_ffi(OutputSink::from_sender(tx));
        sink.send(b"a".to_vec()).await.expect("first send ok");
        sink.send(b"b".to_vec()).await.expect("second send ok");
        drop(sink);
        let mut got = Vec::new();
        while let Some(chunk) = rx.recv().await {
            got.push(chunk);
        }
        assert_eq!(got, vec![b"a".to_vec(), b"b".to_vec()]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn high_level_sink_err_on_receiver_drop() {
        let (tx, rx) = mpsc::channel::<Vec<u8>>(1);
        let sink = HighLevelSink::from_ffi(OutputSink::from_sender(tx));
        drop(rx);
        let err = sink.send(b"x".to_vec()).await.expect_err("send fails");
        assert!(err.contains("output sink closed"), "msg = {}", err);
    }
}
