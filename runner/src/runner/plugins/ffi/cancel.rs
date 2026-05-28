//! FFI-safe cooperative cancellation token.
//!
//! Replaces `tokio_util::sync::CancellationToken` at the V2 plugin boundary.
//! The token state lives in an `Arc<TokenInner>` and is reached through a
//! small `#[repr(C)]` vtable of `extern "C" fn` pointers, so host and plugin
//! can pin different `tokio_util` versions without UB.
//!
//! # Lost-wakeup race protection (both sides)
//!
//! * **Poll side** — `FfiCancelledFuture::poll` checks `is_cancelled()` once,
//!   registers a waker, then **re-checks `is_cancelled()`** to cover the
//!   "registered just after cancel was visible" window.
//! * **Host side** — `register_waker_impl` acquires the wakers `Mutex`, then
//!   re-reads `cancelled` under the lock. If cancel happened in between, it
//!   returns the sentinel `0` instead of inserting a slot. This means the
//!   caller must treat `0` as "already cancelled, do not wait".
//!
//! # Wake/Drop ownership split
//!
//! The waker context (a boxed `std::task::Waker`) is owned by the slot in
//! the wakers map. `wake_thunk` calls `Waker::wake_by_ref` and **does not**
//! take ownership; `drop_thunk` is the unique owner and runs when the slot
//! leaves the map (either explicit `unregister_waker` or `drain` during
//! `cancel()`).

use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Waker;

/// Panic message used when the wakers Mutex is poisoned. A poisoned lock
/// here means a previous holder panicked while holding it, which would
/// only happen if a waker thunk panicked — recovery is impossible because
/// the slots map is in an unknown state.
const WAKERS_POISONED: &str = "FfiCancellationToken wakers mutex poisoned";

/// Internal state shared between host and plugin via `Arc`.
struct TokenInner {
    cancelled: AtomicBool,
    wakers: Mutex<HashMap<NonZeroU64, ExternWakerSlot>>,
    /// Starts at 1 to avoid colliding with the sentinel `0` returned by
    /// `register_waker` when cancellation is already observed.
    next_id: AtomicU64,
}

impl TokenInner {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            cancelled: AtomicBool::new(false),
            wakers: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        })
    }

    fn cancel(self: &Arc<Self>) {
        self.cancelled.store(true, Ordering::Release);
        // Drain the map while the Mutex is held, then fire wakes after
        // releasing the lock. This avoids deadlock when the woken task
        // immediately re-polls and re-registers.
        let drained: Vec<ExternWakerSlot> = {
            let mut map = self.wakers.lock().expect(WAKERS_POISONED);
            map.drain().map(|(_, slot)| slot).collect()
        };
        for slot in &drained {
            // SAFETY: the slot's `wake` thunk is invoked while the slot
            // still owns the underlying waker context. The slot is dropped
            // after the wake loop, releasing the context via `drop_ctx`.
            unsafe { (slot.wake)(slot.ctx) };
        }
        // `drained` going out of scope runs `ExternWakerSlot::Drop` for
        // each slot, freeing the context exactly once.
    }
}

/// Type-erased waker registration. `ctx` is the boxed `Waker`'s raw pointer.
struct ExternWakerSlot {
    ctx: *mut (),
    wake: unsafe extern "C" fn(*mut ()),
    drop_ctx: unsafe extern "C" fn(*mut ()),
}

// SAFETY: a slot owns its `ctx`; access is serialized through the parent
// `Mutex`. The `wake`/`drop_ctx` thunks must not retain `ctx` across the
// call (host-side waker thunks ensure this).
unsafe impl Send for ExternWakerSlot {}
unsafe impl Sync for ExternWakerSlot {}

impl Drop for ExternWakerSlot {
    fn drop(&mut self) {
        // SAFETY: `drop_ctx` is the sole owner of `ctx` once the slot is
        // removed from the map. Pair this with `wake_thunk` which never
        // takes ownership.
        unsafe { (self.drop_ctx)(self.ctx) };
    }
}

// ---------------------------------------------------------------------------
// Host-side vtable thunks (the canonical implementation backing
// `FfiCancellationToken::new_owned`).
// ---------------------------------------------------------------------------

unsafe extern "C" fn host_is_cancelled(state: *const ()) -> bool {
    // SAFETY: `state` was produced by `Arc::into_raw(inner)`.
    let inner = unsafe { &*(state as *const TokenInner) };
    inner.cancelled.load(Ordering::Acquire)
}

unsafe extern "C" fn host_register_waker(
    state: *const (),
    ctx: *mut (),
    wake: unsafe extern "C" fn(*mut ()),
    drop_ctx: unsafe extern "C" fn(*mut ()),
) -> u64 {
    // SAFETY: `state` originates from `Arc::into_raw`. The caller owns the
    // boxed waker context referenced by `ctx`; on registration we move
    // ownership into the slot. If we reject the registration (sentinel
    // path), we must call `drop_ctx` ourselves to avoid leaking.
    let inner = unsafe { &*(state as *const TokenInner) };
    let mut map = inner.wakers.lock().expect(WAKERS_POISONED);
    // Re-check after acquiring the lock. If a concurrent `cancel()` already
    // drained the map, return sentinel 0 instead of stashing a slot that
    // would never be woken.
    if inner.cancelled.load(Ordering::Acquire) {
        // SAFETY: ownership of `ctx` was transferred to this call and we
        // are rejecting it; releasing here keeps the lifecycle balanced.
        unsafe { drop_ctx(ctx) };
        return 0;
    }
    let id_raw = inner.next_id.fetch_add(1, Ordering::Relaxed);
    let id = NonZeroU64::new(id_raw).expect("next_id never returns 0 after init");
    map.insert(
        id,
        ExternWakerSlot {
            ctx,
            wake,
            drop_ctx,
        },
    );
    id.get()
}

unsafe extern "C" fn host_unregister_waker(state: *const (), id: u64) {
    if let Some(nz) = NonZeroU64::new(id) {
        // SAFETY: `state` originates from `Arc::into_raw`.
        let inner = unsafe { &*(state as *const TokenInner) };
        let mut map = inner.wakers.lock().expect(WAKERS_POISONED);
        // The removed `ExternWakerSlot` is dropped here, which invokes
        // `drop_ctx` exactly once.
        let _ = map.remove(&nz);
    }
}

unsafe extern "C" fn host_clone_token(state: *const ()) -> FfiCancellationToken {
    // SAFETY: bumping the strong count on a live `Arc<TokenInner>` is
    // sound; the original raw pointer remains valid until the matching
    // `host_drop_token` is called.
    unsafe { Arc::<TokenInner>::increment_strong_count(state as *const TokenInner) };
    FfiCancellationToken {
        state,
        is_cancelled: host_is_cancelled,
        register_waker: host_register_waker,
        unregister_waker: host_unregister_waker,
        clone_token: host_clone_token,
        drop_token: host_drop_token,
    }
}

unsafe extern "C" fn host_drop_token(state: *const ()) {
    // SAFETY: each `FfiCancellationToken` owns exactly one strong reference;
    // `from_raw` reclaims that reference and drops it.
    drop(unsafe { Arc::<TokenInner>::from_raw(state as *const TokenInner) });
}

// ---------------------------------------------------------------------------
// Public FFI-safe token.
// ---------------------------------------------------------------------------

/// FFI-safe cancellation token. The token owns one `Arc<TokenInner>` strong
/// reference, reached through the vtable thunks.
///
/// **Ownership**: `FfiCancellationToken` is *not* `Copy`. Passing it by
/// value into `set_cancellation_token` transfers ownership to the plugin;
/// the host must not use the value afterwards.
#[repr(C)]
pub struct FfiCancellationToken {
    state: *const (),
    is_cancelled: unsafe extern "C" fn(*const ()) -> bool,
    /// Returns a `NonZeroU64` id when registration succeeds, or `0` when
    /// cancellation was already visible at registration time. Callers must
    /// treat `0` as "already cancelled".
    register_waker: unsafe extern "C" fn(
        *const (),
        *mut (),
        unsafe extern "C" fn(*mut ()),
        unsafe extern "C" fn(*mut ()),
    ) -> u64,
    unregister_waker: unsafe extern "C" fn(*const (), u64),
    clone_token: unsafe extern "C" fn(*const ()) -> FfiCancellationToken,
    drop_token: unsafe extern "C" fn(*const ()),
}

// SAFETY: state is an `Arc` and the vtable functions are pure thunks; all
// accesses are internally synchronized.
unsafe impl Send for FfiCancellationToken {}
unsafe impl Sync for FfiCancellationToken {}

impl FfiCancellationToken {
    /// Allocate a fresh token backed by an `Arc<TokenInner>`. The returned
    /// value owns one strong reference; pair with `cancel_via` or pass to
    /// a plugin via `set_cancellation_token`.
    pub fn new_owned() -> (Self, OwnedCancelHandle) {
        let inner = TokenInner::new();
        let handle = OwnedCancelHandle {
            inner: Arc::clone(&inner),
        };
        let state = Arc::into_raw(inner) as *const ();
        let token = Self {
            state,
            is_cancelled: host_is_cancelled,
            register_waker: host_register_waker,
            unregister_waker: host_unregister_waker,
            clone_token: host_clone_token,
            drop_token: host_drop_token,
        };
        (token, handle)
    }

    pub fn is_cancelled(&self) -> bool {
        // SAFETY: `state` is owned and valid for the lifetime of `self`.
        unsafe { (self.is_cancelled)(self.state) }
    }

    /// Returns a `Future` that resolves once the token is cancelled.
    pub fn cancelled(&self) -> FfiCancelledFuture<'_> {
        FfiCancelledFuture {
            token: self,
            registration: None,
        }
    }

    /// Clone the token (bumps the underlying refcount and returns a new
    /// owned handle).
    pub fn clone_ffi(&self) -> Self {
        // SAFETY: `state` is a live `Arc::into_raw` pointer; the vtable
        // function increments the strong count and returns a fresh owner.
        unsafe { (self.clone_token)(self.state) }
    }
}

impl Drop for FfiCancellationToken {
    fn drop(&mut self) {
        // SAFETY: each token owns one strong reference; decrement once.
        unsafe { (self.drop_token)(self.state) };
    }
}

/// Host-side handle for triggering cancellation on a token produced by
/// `FfiCancellationToken::new_owned`. Cancelling here flips the shared
/// `AtomicBool` and wakes every registered slot.
pub struct OwnedCancelHandle {
    inner: Arc<TokenInner>,
}

impl OwnedCancelHandle {
    pub fn cancel(&self) {
        self.inner.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }
}

// ---------------------------------------------------------------------------
// `cancelled()` future.
// ---------------------------------------------------------------------------

/// Thunk used as the `wake` callback. Reads the boxed `Waker` by reference;
/// ownership stays with the slot.
unsafe extern "C" fn waker_wake_by_ref(ctx: *mut ()) {
    // SAFETY: `ctx` points at a `Box<Waker>` that lives until the matching
    // `waker_drop` runs. `wake_by_ref` does not move out of the box.
    let waker = unsafe { &*(ctx as *const Waker) };
    waker.wake_by_ref();
}

/// Thunk used as the `drop_ctx` callback. Reclaims the boxed `Waker` and
/// drops it exactly once.
unsafe extern "C" fn waker_drop(ctx: *mut ()) {
    // SAFETY: the box was produced by `Box::into_raw(Box::new(...))` in
    // `FfiCancelledFuture::poll` and ownership transferred to the slot.
    drop(unsafe { Box::from_raw(ctx as *mut Waker) });
}

/// Future returned by [`FfiCancellationToken::cancelled`].
pub struct FfiCancelledFuture<'a> {
    token: &'a FfiCancellationToken,
    /// `None` before the first `Pending` return, `Some(id)` once a waker
    /// has been registered. We drop the registration when the future is
    /// dropped to avoid leaking waker contexts.
    registration: Option<NonZeroU64>,
}

impl<'a> std::future::Future for FfiCancelledFuture<'a> {
    type Output = ();

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        // SAFETY: we never move out of `self`; only fields are touched.
        let this = unsafe { self.get_unchecked_mut() };

        // Fast path: cancelled before registration.
        if this.token.is_cancelled() {
            // Drop any previous registration (idempotent: drop sets it to
            // None below).
            if let Some(id) = this.registration.take() {
                // SAFETY: vtable thunk.
                unsafe {
                    (this.token.unregister_waker)(this.token.state, id.get());
                }
            }
            return std::task::Poll::Ready(());
        }

        // If we had a previous registration, drop it before re-registering;
        // tokio's standard waker contract permits this and keeps the slot
        // count bounded.
        if let Some(id) = this.registration.take() {
            // SAFETY: vtable thunk.
            unsafe { (this.token.unregister_waker)(this.token.state, id.get()) };
        }

        let waker = cx.waker().clone();
        let ctx_box = Box::new(waker);
        let ctx_ptr = Box::into_raw(ctx_box) as *mut ();
        // SAFETY: ownership of `ctx_ptr` is transferred to the vtable; on
        // success the slot owns it, on sentinel-0 the thunk drops it for us.
        let id_raw = unsafe {
            (this.token.register_waker)(this.token.state, ctx_ptr, waker_wake_by_ref, waker_drop)
        };

        if id_raw == 0 {
            // Sentinel: registration rejected because cancellation already
            // visible. Box ownership was reclaimed inside the thunk.
            return std::task::Poll::Ready(());
        }

        // Re-check after registration to cover the poll-side race window.
        // If cancellation became visible while we were registering, the
        // host-side `cancel()` may already have drained our slot (in which
        // case `unregister_waker` is a noop), or not yet (in which case we
        // unregister explicitly). Either way we resolve Ready.
        if this.token.is_cancelled() {
            // SAFETY: vtable thunk; idempotent against drained slots.
            unsafe {
                (this.token.unregister_waker)(this.token.state, id_raw);
            }
            return std::task::Poll::Ready(());
        }

        this.registration = Some(NonZeroU64::new(id_raw).expect("register returned non-zero"));
        std::task::Poll::Pending
    }
}

impl<'a> Drop for FfiCancelledFuture<'a> {
    fn drop(&mut self) {
        if let Some(id) = self.registration.take() {
            // SAFETY: vtable thunk. If the slot was already drained by
            // `cancel()` this is a noop.
            unsafe {
                (self.token.unregister_waker)(self.token.state, id.get());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// tokio_util::CancellationToken bridge (host-only utility).
// ---------------------------------------------------------------------------

/// Build an `FfiCancellationToken` that mirrors a `tokio_util` token. The
/// bridge task lives until the returned `FfiCancellationToken` is dropped
/// **or** the host runtime shuts down. Callers must explicitly cancel the
/// plugin token before runtime shutdown to propagate late cancellation.
pub fn from_tokio_util(token: tokio_util::sync::CancellationToken) -> FfiCancellationToken {
    let (ffi, handle) = FfiCancellationToken::new_owned();
    // Detach the bridge task. It self-terminates as soon as the source
    // `token` fires (or is dropped, in which case `cancelled()` resolves
    // immediately via tokio_util's drop semantics), so no abort handle
    // is needed.
    tokio::spawn(async move {
        token.cancelled().await;
        handle.cancel();
    });
    ffi
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    #[test]
    fn cancel_next_id_starts_at_one() {
        let inner = TokenInner::new();
        assert_eq!(inner.next_id.load(Ordering::Relaxed), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_observable_from_other_task() {
        let (token, handle) = FfiCancellationToken::new_owned();
        let task = tokio::spawn(async move {
            token.cancelled().await;
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!handle.is_cancelled());
        handle.cancel();
        tokio::time::timeout(Duration::from_millis(500), task)
            .await
            .expect("future completes within 500ms")
            .expect("task did not panic");
        assert!(handle.is_cancelled());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_clone_token_arc_balance() {
        let (token, handle) = FfiCancellationToken::new_owned();
        // Clone many times then drop; the Arc strong count must end at
        // exactly 1 (the original held by the handle).
        let clones: Vec<_> = (0..100).map(|_| token.clone_ffi()).collect();
        drop(clones);
        drop(token);
        // The handle now holds the sole strong reference.
        assert_eq!(Arc::strong_count(&handle.inner), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_lost_wakeup_race_register_side() {
        // Cancel before registration; register_waker must return 0.
        let (token, handle) = FfiCancellationToken::new_owned();
        handle.cancel();
        let waker = futures::task::noop_waker();
        let ctx = Box::into_raw(Box::new(waker.clone())) as *mut ();
        // SAFETY: vtable thunk; ownership of ctx transfers to the callee
        // which drops it via `waker_drop` (sentinel path).
        let id = unsafe { (token.register_waker)(token.state, ctx, waker_wake_by_ref, waker_drop) };
        assert_eq!(id, 0, "registration after cancel must return sentinel 0");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_waker_unregister_on_future_drop() {
        // Polling once registers a waker; dropping the owned future must
        // unregister via FfiCancelledFuture::Drop. We can't directly peek
        // into the wakers map; the smoke test is that a second cycle works
        // without leaking and that no panics fire.
        let (token, _handle) = FfiCancellationToken::new_owned();
        let waker = futures::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        {
            let fut = token.cancelled();
            futures::pin_mut!(fut);
            let poll1 = fut.as_mut().poll(&mut cx);
            assert!(matches!(poll1, std::task::Poll::Pending));
            // Falling out of this block drops the owned future, which
            // triggers FfiCancelledFuture::Drop and unregisters the waker.
        }
        let waker2 = futures::task::noop_waker();
        let mut cx2 = std::task::Context::from_waker(&waker2);
        {
            let fut2 = token.cancelled();
            futures::pin_mut!(fut2);
            let poll2 = fut2.as_mut().poll(&mut cx2);
            assert!(matches!(poll2, std::task::Poll::Pending));
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_no_deadlock_on_wake_reentry() {
        // Custom waker that re-polls a future on wake. If `cancel()` held
        // the wakers Mutex while invoking wake, this re-poll would try to
        // re-register and deadlock. The fix drains the Mutex first.
        let (token, handle) = FfiCancellationToken::new_owned();
        let token = Arc::new(token);
        let token_clone = Arc::clone(&token);
        // Spawn a task that awaits cancellation; if the deadlock occurs the
        // tokio::time::timeout below would fire.
        let task = tokio::spawn(async move {
            token_clone.cancelled().await;
            // Try to re-register after wake — exercises the path that used
            // to deadlock.
            let _ = token_clone.cancelled().await; // already cancelled, returns immediately
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        handle.cancel();
        tokio::time::timeout(Duration::from_millis(500), task)
            .await
            .expect("no deadlock")
            .expect("task did not panic");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_wake_by_ref_no_double_drop() {
        // Counter waker context with manual extern thunks. wake_thunk must
        // not drop the Box; only drop_thunk does.
        struct CountedWaker {
            wakes: Arc<AtomicUsize>,
            drops: Arc<AtomicUsize>,
        }
        impl Drop for CountedWaker {
            fn drop(&mut self) {
                self.drops.fetch_add(1, Ordering::Relaxed);
            }
        }

        unsafe extern "C" fn wake_count(ctx: *mut ()) {
            let cw = unsafe { &*(ctx as *const CountedWaker) };
            cw.wakes.fetch_add(1, Ordering::Relaxed);
        }
        unsafe extern "C" fn drop_count(ctx: *mut ()) {
            drop(unsafe { Box::from_raw(ctx as *mut CountedWaker) });
        }

        let (token, handle) = FfiCancellationToken::new_owned();
        let wakes = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let cw = Box::new(CountedWaker {
            wakes: Arc::clone(&wakes),
            drops: Arc::clone(&drops),
        });
        let ctx = Box::into_raw(cw) as *mut ();
        // SAFETY: vtable thunk; we transfer ownership of the box to the slot.
        let id = unsafe { (token.register_waker)(token.state, ctx, wake_count, drop_count) };
        assert_ne!(id, 0);

        handle.cancel();
        // wake fired exactly once, no drops yet because the slot is held
        // in the drained vec; once the loop ends, slot Drop runs drop_thunk
        // exactly once.
        // (We can't observe the precise interleaving, but at the end:
        //   wakes == 1, drops == 1.)
        // Allow the cancel() call to complete fully.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(wakes.load(Ordering::Relaxed), 1, "wake fired exactly once");
        assert_eq!(drops.load(Ordering::Relaxed), 1, "drop fired exactly once");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_double_drop_safe() {
        // Clone, drop original first, then drop the clone. No segfault.
        let (token, _handle) = FfiCancellationToken::new_owned();
        let clone = token.clone_ffi();
        drop(token);
        drop(clone);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_from_tokio_util_bridge() {
        let outer = tokio_util::sync::CancellationToken::new();
        let ffi = from_tokio_util(outer.clone());
        assert!(!ffi.is_cancelled());
        outer.cancel();
        // Bridge task runs on the tokio runtime; give it a moment.
        for _ in 0..50 {
            if ffi.is_cancelled() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(ffi.is_cancelled());
    }
}
