//! High-level V2 plugin trait and convenience wrappers.
//!
//! Plugin authors implement [`PluginV2`] with ordinary Rust types
//! (`Vec<u8>`, `HashMap<String, String>`, `Result<_, String>`); the
//! `register_plugin_v2!` proc-macro generates the `extern "C"` thunks that
//! marshal to the FFI-safe vtable in [`super::ffi::vtable::PluginVtable`].
//!
//! This file deliberately does not depend on the proc-macro crate so it
//! can compile during early-stage builds; the macro lives in
//! `modules/jobworkerp-runner-macros` and is added in a later commit.

use super::ffi::cancel::FfiCancellationToken;
use super::ffi::sink::OutputSink;
use super::ffi::types::FfiResult;
use proto::jobworkerp::data::{MethodJsonSchema, MethodSchema};
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

/// Plugin author trait. Implementations use ordinary Rust types; the
/// proc-macro [`register_plugin_v2!`](crate::register_plugin_v2) generates
/// the FFI thunks bridging to [`super::ffi::vtable::PluginVtable`].
///
/// All async methods return `BoxFuture<'_, ...>` rather than `async fn`
/// so the trait remains dyn-safe; this keeps the proc-macro generator
/// simple (it constructs one thunk per method, captures `&mut self`
/// through an `Arc<Mutex<MyPlugin>>` if needed).
#[async_trait::async_trait]
pub trait PluginV2: Send + Sync + 'static {
    fn name(&self) -> String;
    fn description(&self) -> String;
    fn runner_settings_proto(&self) -> String {
        String::new()
    }
    fn settings_schema(&self) -> String;
    fn method_proto_map(&self) -> HashMap<String, MethodSchema>;
    fn method_json_schema_map(&self) -> Option<HashMap<String, MethodJsonSchema>> {
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
    use crate::runner::plugins::ffi::cancel::FfiCancellationToken;
    use crate::runner::plugins::ffi::sink::OutputSink;
    use std::time::Duration;
    use tokio::sync::mpsc;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_token_clone_uses_vtable_clone() {
        // Two clones of the high-level CancelToken share the same Arc;
        // dropping one must not disturb the other.
        let (ffi, handle) = FfiCancellationToken::new_owned();
        let token = CancelToken::from_ffi(ffi);
        let clone = token.clone();
        assert!(!token.is_cancelled());
        assert!(!clone.is_cancelled());
        handle.cancel();
        // Allow propagation
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
