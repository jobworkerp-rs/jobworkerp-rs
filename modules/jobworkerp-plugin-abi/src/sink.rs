//! FFI-safe output sink used by `run_stream`.
//!
//! Replaces `tokio::sync::mpsc::Sender<Vec<u8>>` at the V2 plugin boundary.
//! The sink is push-based: the plugin calls `send(bytes)`; awaiting the
//! returned future delivers the chunk into the host-owned receiver.
//!
//! # Drop contract (**plugin authors must read**)
//!
//! A plugin MUST complete every in-flight `send` future before dropping the
//! `OutputSink` (or its high-level wrapper `HighLevelSink`). Dropping the
//! sink while a `send` future is still pending makes the future reference
//! a freed sink state, which is UB.
//!
//! The high-level `HighLevelSink::send` returns a future borrowed from
//! `&self`, which lets Rust's borrow checker reject most violations
//! statically. The low-level `OutputSink` is `unsafe` to use directly and
//! requires the caller to uphold the contract.
//!
//! # Performance
//!
//! Each `send` call returns an `FfiFuture<...>` which currently incurs one
//! `Box<dyn Future>` allocation per chunk. A benchmark
//! (`runner/benches/sink_throughput.rs`) compares against
//! `tokio::sync::mpsc::Sender::send` to track regressions; if the relative
//! overhead exceeds the 1.5x baseline budget we switch to a try_send +
//! wait_writable scheme in a follow-up.

use crate::types::{FfiBytes, FfiResult};
use async_ffi::{FfiFuture, FutureExt};
use std::sync::Mutex;
use tokio::sync::mpsc;

/// Host-side state backing an `OutputSink::from_sender` instance.
///
/// The `Mutex` is needed because `tokio::mpsc::Sender::send` takes `&self`
/// but the plugin holds the sink through a `*mut ()` and may be on a
/// different thread. We don't expect concurrent sends from the plugin —
/// the sink is documented as non-`Sync` — but the boxed state must still
/// be safe to access from arbitrary threads through the thunk.
struct SenderState {
    tx: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
}

unsafe extern "C" fn host_send(
    state: *mut (),
    bytes: FfiBytes,
) -> FfiFuture<FfiResult<(), FfiBytes>> {
    // SAFETY: `state` was produced by `Box::into_raw(Box::new(SenderState))`
    // and remains valid until `host_drop_sender` is called. The `Box` is
    // accessed by reference here so the underlying allocation stays put.
    let state_ptr = state as *mut SenderState;
    let state_ref: &SenderState = unsafe { &*state_ptr };

    // Extract the sender clone before constructing the async block so the
    // future does not capture a raw pointer.
    let maybe_tx = state_ref
        .tx
        .lock()
        .expect("OutputSink sender mutex poisoned")
        .clone();

    async move {
        let Some(tx) = maybe_tx else {
            return FfiResult::Err(FfiBytes::from_vec(b"sink already closed".to_vec()));
        };
        // The plugin allocator may not match the host's; consume the
        // incoming FfiBytes via copy_to_vec so the original buffer is
        // released by its own `drop_fn` rather than Vec::from_raw_parts
        // routing through the wrong allocator.
        let payload = bytes.copy_to_vec();
        match tx.send(payload).await {
            Ok(()) => FfiResult::Ok(()),
            // `unsent` was allocated on the host (we just produced it via
            // copy_to_vec), so wrapping it back into `FfiBytes::from_vec`
            // is safe — the plugin must use `copy_to_vec` (or
            // `into_string_lossy`) to inspect the bytes safely. This is
            // the documented sink contract.
            Err(mpsc::error::SendError(unsent)) => FfiResult::Err(FfiBytes::from_vec(unsent)),
        }
    }
    .into_ffi()
}

unsafe extern "C" fn host_drop_sender(state: *mut ()) {
    // SAFETY: `state` was produced by `Box::into_raw(Box::new(SenderState))`.
    // Reclaim and drop the box, which closes the underlying mpsc sender.
    drop(unsafe { Box::from_raw(state as *mut SenderState) });
}

/// FFI-safe output sink.
#[repr(C)]
pub struct OutputSink {
    state: *mut (),
    send: unsafe extern "C" fn(*mut (), FfiBytes) -> FfiFuture<FfiResult<(), FfiBytes>>,
    drop_state: unsafe extern "C" fn(*mut ()),
}

// SAFETY: the sink owns its state and the internal `Mutex<Option<Sender>>`
// serialises concurrent access. Plugin authors are still expected to drive
// `send` sequentially (the drop contract requires awaiting in-flight
// futures before drop), but the underlying state is thread-safe.
unsafe impl Send for OutputSink {}
unsafe impl Sync for OutputSink {}

impl OutputSink {
    /// Wrap a `tokio::sync::mpsc::Sender<Vec<u8>>` for use across the FFI
    /// boundary. The caller retains the matching `Receiver`.
    pub fn from_sender(tx: mpsc::Sender<Vec<u8>>) -> Self {
        let state = Box::new(SenderState {
            tx: Mutex::new(Some(tx)),
        });
        Self {
            state: Box::into_raw(state) as *mut (),
            send: host_send,
            drop_state: host_drop_sender,
        }
    }

    /// Send a chunk and await delivery. The caller must not drop the sink
    /// while any `send` future is still pending.
    ///
    /// # Safety contract
    ///
    /// Pending futures borrow `&self`; the high-level wrapper
    /// `HighLevelSink::send` enforces this at the type level.
    pub fn send_raw(&self, bytes: Vec<u8>) -> FfiFuture<FfiResult<(), FfiBytes>> {
        let ffi = FfiBytes::from_vec(bytes);
        // SAFETY: vtable thunk; state is valid for the lifetime of `&self`.
        unsafe { (self.send)(self.state, ffi) }
    }
}

impl Drop for OutputSink {
    fn drop(&mut self) {
        // SAFETY: `drop_state` reclaims the boxed state. Plugin authors are
        // responsible for awaiting in-flight `send` futures before drop.
        unsafe { (self.drop_state)(self.state) };
    }
}

/// FFI-safe output sink that carries an explicit `is_final` flag with
/// every chunk.
///
/// Companion of [`OutputSink`]; differs only in that the host can signal
/// "this is the last feed" to the plugin in-band instead of relying on
/// receiver-drop (`None` from `mpsc::recv`) as the sole EOF cue. Plugins
/// that consume feed data and need to flush state before returning (e.g.
/// streaming ASR runners with a final-utterance hook) implement
/// `PluginV2::setup_client_stream_channel_v2` to receive an
/// `OutputSinkWithFinal`. Existing plugins keep using the original
/// `OutputSink`; the host falls back when the new vtable slot is not
/// populated.
///
/// The drop contract is identical to `OutputSink`: every in-flight
/// `send_with_final` future MUST complete before the sink is dropped.
#[repr(C)]
pub struct OutputSinkWithFinal {
    state: *mut (),
    send_with_final:
        unsafe extern "C" fn(*mut (), FfiBytes, bool) -> FfiFuture<FfiResult<(), FfiBytes>>,
    drop_state: unsafe extern "C" fn(*mut ()),
}

// SAFETY: same reasoning as `OutputSink` — the Mutex inside the state
// box serialises concurrent access, plugin authors are expected to drive
// `send_with_final` sequentially.
unsafe impl Send for OutputSinkWithFinal {}
unsafe impl Sync for OutputSinkWithFinal {}

/// `(chunk, is_final)` pair shipped over the `OutputSinkWithFinal`
/// channel. Aliased so the surrounding code stays readable and clippy's
/// `type_complexity` lint stays happy.
pub type FinalFeedChunk = (Vec<u8>, bool);

/// Host-side state for `OutputSinkWithFinal::from_sender`.
struct SenderWithFinalState {
    tx: Mutex<Option<mpsc::Sender<FinalFeedChunk>>>,
}

unsafe extern "C" fn host_send_with_final(
    state: *mut (),
    bytes: FfiBytes,
    is_final: bool,
) -> FfiFuture<FfiResult<(), FfiBytes>> {
    // SAFETY: mirrors `host_send` — `state` was produced by
    // `Box::into_raw(Box::new(SenderWithFinalState))` and remains valid
    // until `host_drop_sender_with_final` runs.
    let state_ptr = state as *mut SenderWithFinalState;
    let state_ref: &SenderWithFinalState = unsafe { &*state_ptr };

    let maybe_tx = state_ref
        .tx
        .lock()
        .expect("OutputSinkWithFinal sender mutex poisoned")
        .clone();

    async move {
        let Some(tx) = maybe_tx else {
            return FfiResult::Err(FfiBytes::from_vec(b"sink already closed".to_vec()));
        };
        let payload = bytes.copy_to_vec();
        match tx.send((payload, is_final)).await {
            Ok(()) => FfiResult::Ok(()),
            Err(mpsc::error::SendError((unsent, _))) => FfiResult::Err(FfiBytes::from_vec(unsent)),
        }
    }
    .into_ffi()
}

unsafe extern "C" fn host_drop_sender_with_final(state: *mut ()) {
    // SAFETY: see `host_drop_sender`.
    drop(unsafe { Box::from_raw(state as *mut SenderWithFinalState) });
}

impl OutputSinkWithFinal {
    /// Wrap a `tokio::sync::mpsc::Sender<FinalFeedChunk>` for use across
    /// the FFI boundary. The caller retains the matching `Receiver`.
    pub fn from_sender(tx: mpsc::Sender<FinalFeedChunk>) -> Self {
        let state = Box::new(SenderWithFinalState {
            tx: Mutex::new(Some(tx)),
        });
        Self {
            state: Box::into_raw(state) as *mut (),
            send_with_final: host_send_with_final,
            drop_state: host_drop_sender_with_final,
        }
    }

    /// Send a chunk together with the `is_final` flag and await delivery.
    /// Same drop contract as `OutputSink::send_raw`.
    pub fn send_raw_with_final(
        &self,
        bytes: Vec<u8>,
        is_final: bool,
    ) -> FfiFuture<FfiResult<(), FfiBytes>> {
        let ffi = FfiBytes::from_vec(bytes);
        // SAFETY: vtable thunk; state is valid for `&self`.
        unsafe { (self.send_with_final)(self.state, ffi, is_final) }
    }
}

impl Drop for OutputSinkWithFinal {
    fn drop(&mut self) {
        // SAFETY: same as `OutputSink::drop`.
        unsafe { (self.drop_state)(self.state) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sink_send_success() {
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(16);
        let sink = OutputSink::from_sender(tx);
        let fut = sink.send_raw(b"hello".to_vec());
        let result = fut.await;
        assert!(matches!(result, FfiResult::Ok(())));
        let received = rx.recv().await.expect("chunk delivered");
        assert_eq!(received, b"hello");
        drop(sink);
        // After sink drop the receiver observes channel close.
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sink_receiver_dropped_returns_err() {
        let (tx, rx) = mpsc::channel::<Vec<u8>>(1);
        let sink = OutputSink::from_sender(tx);
        drop(rx);
        let result = sink.send_raw(b"x".to_vec()).await;
        match result {
            FfiResult::Err(bytes) => {
                assert_eq!(bytes.as_slice(), b"x");
            }
            FfiResult::Ok(()) => panic!("expected Err with unsent payload"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sink_drop_releases_sender() {
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(4);
        let sink = OutputSink::from_sender(tx);
        // Without sending anything, dropping the sink must close the channel.
        drop(sink);
        let first = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("recv resolves promptly");
        assert!(first.is_none(), "no chunks were sent");
        // Subsequent recv yields None (closed).
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sink_sequential_sends_preserve_order() {
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(16);
        let sink = OutputSink::from_sender(tx);
        for i in 0..5u8 {
            let res = sink.send_raw(vec![i]).await;
            assert!(matches!(res, FfiResult::Ok(())));
        }
        drop(sink);
        let mut got = Vec::new();
        while let Some(chunk) = rx.recv().await {
            got.push(chunk[0]);
        }
        assert_eq!(got, vec![0u8, 1, 2, 3, 4]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sink_with_final_carries_flag_through_receiver() {
        let (tx, mut rx) = mpsc::channel::<(Vec<u8>, bool)>(8);
        let sink = OutputSinkWithFinal::from_sender(tx);
        let res = sink.send_raw_with_final(b"hello".to_vec(), false).await;
        assert!(matches!(res, FfiResult::Ok(())));
        let res = sink.send_raw_with_final(b"world".to_vec(), true).await;
        assert!(matches!(res, FfiResult::Ok(())));
        let first = rx.recv().await.expect("first chunk");
        assert_eq!(first, (b"hello".to_vec(), false));
        let second = rx.recv().await.expect("second chunk");
        assert_eq!(second, (b"world".to_vec(), true));
        // Sink still alive, but receiver observes channel close on drop.
        drop(sink);
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sink_with_final_receiver_dropped_returns_err_with_payload() {
        let (tx, rx) = mpsc::channel::<(Vec<u8>, bool)>(1);
        let sink = OutputSinkWithFinal::from_sender(tx);
        drop(rx);
        let result = sink.send_raw_with_final(b"x".to_vec(), true).await;
        match result {
            FfiResult::Err(bytes) => assert_eq!(bytes.as_slice(), b"x"),
            FfiResult::Ok(()) => panic!("expected Err with unsent payload"),
        }
    }
}
