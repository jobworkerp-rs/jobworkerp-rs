//! FFI-safe primitive value types shared between host and plugins.
//!
//! Each owned buffer carries its own `drop_fn` so host and plugin can use
//! independent global allocators (one of them may set `#[global_allocator]`
//! to e.g. mimalloc/jemalloc and the buffer still gets freed via the
//! allocator that created it).

use core::marker::PhantomData;
use core::ptr::NonNull;

/// Default `drop_fn` for buffers allocated by the Rust global allocator.
///
/// SAFETY: `ptr` / `len` / `cap` must originate from `Vec::into_raw_parts`
/// (i.e. a `Vec<u8>::with_capacity(cap)` populated to `len` bytes) and must
/// not be aliased. `cap == 0` is treated as a noop because `Vec::new()` /
/// `dangling()` paths return `cap == 0`.
unsafe extern "C" fn ffi_bytes_drop_rust_global(ptr: *mut u8, len: usize, cap: usize) {
    if cap != 0 {
        drop(unsafe { Vec::from_raw_parts(ptr, len, cap) });
    }
}

/// No-op `drop_fn` used by `FfiBytes::empty()`.
unsafe extern "C" fn ffi_bytes_drop_noop(_ptr: *mut u8, _len: usize, _cap: usize) {}

/// FFI-safe owned byte buffer.
///
/// The buffer carries its own allocator hook (`drop_fn`) so host and plugin
/// can rely on independent allocators. The buffer's contents are owned;
/// the caller must not retain `ptr` after handing the `FfiBytes` over.
///
/// # Empty representation
///
/// `FfiBytes::empty()` returns a buffer with `len == 0`, `cap == 0`, a
/// dangling-but-aligned pointer and `drop_fn = ffi_bytes_drop_noop`.
/// `is_empty()` checks `len == 0` only; the pointer value is unspecified.
#[repr(C)]
pub struct FfiBytes {
    ptr: *mut u8,
    len: usize,
    cap: usize,
    drop_fn: unsafe extern "C" fn(ptr: *mut u8, len: usize, cap: usize),
}

// SAFETY: the buffer is owned; no aliasing pointers exist on the caller side.
unsafe impl Send for FfiBytes {}
unsafe impl Sync for FfiBytes {}

impl FfiBytes {
    /// Construct an `FfiBytes` from a `Vec<u8>`. Ownership of the allocation
    /// is transferred to the returned value; freeing happens via the same
    /// global allocator that created the `Vec`.
    pub fn from_vec(v: Vec<u8>) -> Self {
        let mut v = std::mem::ManuallyDrop::new(v);
        let (ptr, len, cap) = (v.as_mut_ptr(), v.len(), v.capacity());
        Self {
            ptr,
            len,
            cap,
            drop_fn: ffi_bytes_drop_rust_global,
        }
    }

    /// An empty buffer with a no-op drop. Useful as a placeholder return
    /// value where no payload exists.
    pub fn empty() -> Self {
        Self {
            ptr: NonNull::<u8>::dangling().as_ptr(),
            len: 0,
            cap: 0,
            drop_fn: ffi_bytes_drop_noop,
        }
    }

    /// Length in bytes. Zero means no payload (matches `is_empty`).
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Borrow the bytes as a slice. Returns an empty slice when `len == 0`
    /// regardless of the pointer value.
    pub fn as_slice(&self) -> &[u8] {
        if self.len == 0 {
            &[]
        } else {
            // SAFETY: invariants of `FfiBytes::from_vec` guarantee `ptr`
            // points to `len` initialised bytes.
            unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
        }
    }

    /// Decode the buffer as UTF-8, falling back to lossy replacement on
    /// invalid bytes. Safe to call on FFI-received buffers because the
    /// underlying allocation is consumed via `copy_to_vec` (which goes
    /// through `as_slice` + `to_vec`) and then released by `FfiBytes::Drop`.
    pub fn into_string_lossy(self) -> String {
        // Borrow first, then let `self` drop normally so the embedded
        // `drop_fn` runs on the original (possibly plugin-side) allocator.
        let copy = self.as_slice().to_vec();
        drop(self);
        String::from_utf8(copy)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
    }

    /// Borrowed counterpart of `into_string_lossy`.
    pub fn to_string_lossy(&self) -> String {
        String::from_utf8_lossy(self.as_slice()).into_owned()
    }

    /// Copy the bytes into a fresh `Vec<u8>` using the **current** crate's
    /// allocator, then release the original buffer via its embedded
    /// `drop_fn`. This is the cross-allocator-safe way to consume an
    /// `FfiBytes` that may have been produced by code linking a different
    /// `#[global_allocator]` (e.g. a plugin built with mimalloc against a
    /// host using the system allocator).
    pub fn copy_to_vec(self) -> Vec<u8> {
        let copy = self.as_slice().to_vec();
        drop(self);
        copy
    }

    /// Reclaim the buffer as a `Vec<u8>` *without copying*.
    ///
    /// # Safety
    ///
    /// The caller must guarantee the buffer was produced by `from_vec`
    /// **on the same allocator** as the current crate. Calling this on
    /// an `FfiBytes` received across an FFI boundary where host and
    /// plugin link different `#[global_allocator]`s is undefined behaviour:
    /// `Vec::from_raw_parts` will route deallocation through the wrong
    /// allocator. Use [`copy_to_vec`](Self::copy_to_vec) for buffers that
    /// crossed an FFI boundary.
    pub unsafe fn into_vec_unchecked(self) -> Vec<u8> {
        let me = std::mem::ManuallyDrop::new(self);
        if me.cap == 0 {
            Vec::new()
        } else {
            // SAFETY: caller guarantees same-allocator round-trip with `from_vec`.
            unsafe { Vec::from_raw_parts(me.ptr, me.len, me.cap) }
        }
    }
}

impl Drop for FfiBytes {
    fn drop(&mut self) {
        // SAFETY: drop_fn was set at construction to the matching allocator
        // hook. `cap == 0` is handled by the hook.
        unsafe { (self.drop_fn)(self.ptr, self.len, self.cap) };
    }
}

/// FFI-safe replacement for `Option<T>` with a stable discriminant layout.
#[repr(C, u8)]
pub enum FfiOption<T> {
    Some(T),
    None,
}

impl<T> FfiOption<T> {
    pub fn from_option(o: Option<T>) -> Self {
        match o {
            Some(v) => Self::Some(v),
            None => Self::None,
        }
    }

    pub fn into_option(self) -> Option<T> {
        match self {
            Self::Some(v) => Some(v),
            Self::None => None,
        }
    }

    pub fn is_some(&self) -> bool {
        matches!(self, Self::Some(_))
    }

    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }
}

/// FFI-safe replacement for `Result<T, E>` with a stable discriminant layout.
#[repr(C, u8)]
pub enum FfiResult<T, E> {
    Ok(T),
    Err(E),
}

impl<T, E> FfiResult<T, E> {
    pub fn from_result(r: Result<T, E>) -> Self {
        match r {
            Ok(v) => Self::Ok(v),
            Err(e) => Self::Err(e),
        }
    }

    pub fn into_result(self) -> Result<T, E> {
        match self {
            Self::Ok(v) => Ok(v),
            Self::Err(e) => Err(e),
        }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok(_))
    }

    pub fn is_err(&self) -> bool {
        matches!(self, Self::Err(_))
    }
}

/// Default `drop_fn` for `FfiVec<T>` buffers allocated by the Rust global
/// allocator.
///
/// SAFETY: `ptr` / `len` / `cap` must originate from a `Vec<T>` produced in
/// this process; aliasing is forbidden.
unsafe extern "C" fn ffi_vec_drop_rust_global<T>(ptr: *mut u8, len: usize, cap: usize) {
    if cap != 0 {
        drop(unsafe { Vec::from_raw_parts(ptr.cast::<T>(), len, cap) });
    }
}

unsafe extern "C" fn ffi_vec_drop_noop(_ptr: *mut u8, _len: usize, _cap: usize) {}

/// FFI-safe owned `Vec<T>`-like buffer. Element type `T` must itself be
/// FFI-safe (`#[repr(C)]`) for the buffer to round-trip across the boundary
/// without UB. The drop hook is type-erased via `*mut u8` to keep the type
/// `#[repr(C)]` regardless of `T`.
#[repr(C)]
pub struct FfiVec<T> {
    ptr: *mut T,
    len: usize,
    cap: usize,
    drop_fn: unsafe extern "C" fn(ptr: *mut u8, len: usize, cap: usize),
    _marker: PhantomData<T>,
}

unsafe impl<T: Send> Send for FfiVec<T> {}
unsafe impl<T: Sync> Sync for FfiVec<T> {}

impl<T> FfiVec<T> {
    pub fn from_vec(v: Vec<T>) -> Self {
        let mut v = std::mem::ManuallyDrop::new(v);
        let (ptr, len, cap) = (v.as_mut_ptr(), v.len(), v.capacity());
        Self {
            ptr,
            len,
            cap,
            drop_fn: ffi_vec_drop_rust_global::<T>,
            _marker: PhantomData,
        }
    }

    pub fn empty() -> Self {
        Self {
            ptr: NonNull::<T>::dangling().as_ptr(),
            len: 0,
            cap: 0,
            drop_fn: ffi_vec_drop_noop,
            _marker: PhantomData,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_slice(&self) -> &[T] {
        if self.len == 0 {
            &[]
        } else {
            // SAFETY: invariants of `from_vec`.
            unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
        }
    }

    /// Reclaim the outer buffer as a `Vec<T>` *without copying*.
    ///
    /// # Safety
    ///
    /// The caller must guarantee the buffer was produced by
    /// [`from_vec`](Self::from_vec) **on the same allocator** as the
    /// current crate. Calling this on an `FfiVec<T>` received across an
    /// FFI boundary where host and plugin link different
    /// `#[global_allocator]`s is undefined behaviour:
    /// `Vec::from_raw_parts` will route deallocation through the wrong
    /// allocator. Use [`drain_into`](Self::drain_into) for buffers that
    /// crossed an FFI boundary.
    pub unsafe fn into_vec_unchecked(self) -> Vec<T> {
        let me = std::mem::ManuallyDrop::new(self);
        if me.cap == 0 {
            Vec::new()
        } else {
            // SAFETY: caller guarantees same-allocator round-trip with `from_vec`.
            unsafe { Vec::from_raw_parts(me.ptr, me.len, me.cap) }
        }
    }

    /// Drain elements one-by-one, applying `f` to each, then release the
    /// outer buffer via its own `drop_fn`. Safe to call across an FFI
    /// boundary because:
    ///
    /// * Each element is moved out with `std::ptr::read` before `f` runs,
    ///   so element destructors fire inside `f` against whatever inner
    ///   allocator the element captured (e.g. inner `FfiBytes::drop_fn`).
    /// * The outer buffer is then released by invoking `drop_fn` with a
    ///   doctored `len = 0`. `ffi_vec_drop_rust_global<T>` reconstructs
    ///   `Vec::from_raw_parts(ptr, 0, cap)` so it deallocates the backing
    ///   storage without re-running element destructors (which would
    ///   double-drop the values `f` already consumed).
    pub fn drain_into<R>(self, mut f: impl FnMut(T) -> R) -> Vec<R> {
        let mut out = Vec::with_capacity(self.len);
        let len = self.len;
        let ptr = self.ptr;
        for i in 0..len {
            // SAFETY: `i < len`, `ptr` is valid for `len` initialised T's.
            let element = unsafe { std::ptr::read(ptr.add(i)) };
            out.push(f(element));
        }
        // Suppress the default Drop (which would re-run element destructors
        // through Vec::drop), then release the outer buffer manually with
        // `len = 0` so only the allocation itself is freed.
        let me = std::mem::ManuallyDrop::new(self);
        if me.cap != 0 {
            // SAFETY: drop_fn matches the allocator that produced this
            // buffer; `len = 0` tells the hook to skip element destructors
            // and only release the storage.
            unsafe { (me.drop_fn)(me.ptr.cast::<u8>(), 0, me.cap) };
        }
        out
    }
}

impl<T> Drop for FfiVec<T> {
    fn drop(&mut self) {
        // SAFETY: drop_fn was set at construction. Element destructors run
        // via `Vec::from_raw_parts` -> `Vec::drop`.
        unsafe { (self.drop_fn)(self.ptr.cast::<u8>(), self.len, self.cap) };
    }
}

/// Key-value pair used in `FfiKvPairList` (replaces `HashMap<String, Vec<u8>>`
/// across the FFI boundary). The key is UTF-8 bytes; the value is typically
/// a protobuf-encoded message.
#[repr(C)]
pub struct FfiKvPair {
    pub key: FfiBytes,
    pub value: FfiBytes,
}

/// FFI-safe owned list of key/value pairs (protobuf-encoded values).
pub type FfiKvPairList = FfiVec<FfiKvPair>;

/// Build an `FfiOption<FfiBytes>` from an optional UTF-8 string slice.
/// Used by host wrappers when forwarding the `using` argument across the
/// FFI boundary.
pub fn option_str_to_ffi(s: Option<&str>) -> FfiOption<FfiBytes> {
    FfiOption::from_option(s.map(|s| FfiBytes::from_vec(s.as_bytes().to_vec())))
}

impl FfiKvPair {
    /// Convenience constructor for UTF-8 string metadata pairs.
    pub fn from_string_pair(key: String, value: String) -> Self {
        Self {
            key: FfiBytes::from_vec(key.into_bytes()),
            value: FfiBytes::from_vec(value.into_bytes()),
        }
    }
}

/// HashMap<String, String> ↔ FfiKvPairList helpers used on both the host
/// and plugin sides to marshal job metadata. UTF-8 conversion is lossy on
/// invalid bytes.
pub fn string_map_to_kv(m: std::collections::HashMap<String, String>) -> FfiKvPairList {
    let pairs: Vec<FfiKvPair> = m
        .into_iter()
        .map(|(k, v)| FfiKvPair::from_string_pair(k, v))
        .collect();
    FfiVec::from_vec(pairs)
}

/// Decode a plugin-produced metadata list into a host-owned `HashMap`.
/// Uses `FfiVec::drain_into` so the outer buffer and inner `FfiBytes`
/// allocations are released through their own `drop_fn`s — never through
/// the host allocator.
pub fn kv_to_string_map(list: FfiKvPairList) -> std::collections::HashMap<String, String> {
    list.drain_into(|p| (p.key.into_string_lossy(), p.value.into_string_lossy()))
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn ffi_bytes_round_trip() {
        let original: Vec<u8> = b"hello world".to_vec();
        let cap = original.capacity();
        let len = original.len();

        let ffi = FfiBytes::from_vec(original.clone());
        assert_eq!(ffi.len(), len);
        assert_eq!(ffi.as_slice(), original.as_slice());

        // SAFETY: `ffi` was produced in this test from `from_vec` on the
        // current crate's global allocator, so the unchecked reclamation
        // round-trips through the same allocator.
        let restored = unsafe { ffi.into_vec_unchecked() };
        assert_eq!(restored, original);
        // Capacity should round-trip when no reallocation happens in between.
        assert_eq!(restored.capacity(), cap);
    }

    #[test]
    fn ffi_bytes_empty() {
        let ffi = FfiBytes::empty();
        assert_eq!(ffi.len(), 0);
        assert!(ffi.is_empty());
        assert_eq!(ffi.as_slice(), &[] as &[u8]);
        // Dropping must not segfault despite the dangling pointer.
        drop(ffi);
    }

    #[test]
    fn ffi_bytes_drop_no_leak_via_into_vec() {
        // Round-trip without explicit drop: `into_vec_unchecked` re-acquires
        // ownership and lets the standard allocator handle release.
        let ffi = FfiBytes::from_vec(vec![1, 2, 3, 4, 5]);
        // SAFETY: same-allocator round-trip within this test.
        let v = unsafe { ffi.into_vec_unchecked() };
        assert_eq!(v, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn ffi_bytes_drop_invokes_alloc_hook() {
        // Smoke test that Drop releases the allocation. We can't directly
        // observe the underlying allocator without instrumentation, so we
        // run a large batch and rely on miri / address sanitizer in CI to
        // catch leaks. The visible assertion is that creation + drop does
        // not panic for many iterations.
        for i in 0..1024 {
            let v: Vec<u8> = (0..i).map(|x| (x & 0xff) as u8).collect();
            let ffi = FfiBytes::from_vec(v);
            assert_eq!(ffi.len(), i);
            drop(ffi);
        }
    }

    #[test]
    fn ffi_option_round_trip() {
        let some: Option<i32> = Some(42);
        let none: Option<i32> = None;
        assert_eq!(FfiOption::from_option(some).into_option(), Some(42));
        assert_eq!(FfiOption::from_option(none).into_option(), None);

        let s = FfiOption::Some(7);
        let n: FfiOption<i32> = FfiOption::None;
        assert!(s.is_some());
        assert!(n.is_none());
    }

    #[test]
    fn ffi_result_round_trip() {
        let ok: Result<u32, String> = Ok(1);
        let err: Result<u32, String> = Err("boom".to_string());
        assert_eq!(FfiResult::from_result(ok).into_result(), Ok(1));
        assert_eq!(
            FfiResult::from_result(err).into_result(),
            Err("boom".to_string())
        );
    }

    #[test]
    fn ffi_vec_kv_pair_round_trip() {
        let mut original: HashMap<String, Vec<u8>> = HashMap::new();
        original.insert("alpha".to_string(), b"one".to_vec());
        original.insert("beta".to_string(), b"two".to_vec());
        original.insert("gamma".to_string(), b"three".to_vec());

        let pairs: Vec<FfiKvPair> = original
            .iter()
            .map(|(k, v)| FfiKvPair {
                key: FfiBytes::from_vec(k.clone().into_bytes()),
                value: FfiBytes::from_vec(v.clone()),
            })
            .collect();
        let list = FfiKvPairList::from_vec(pairs);
        assert_eq!(list.len(), original.len());

        let restored: HashMap<String, Vec<u8>> = list
            .drain_into(|p| {
                // SAFETY: same-allocator round-trip within this test (the
                // inner FfiBytes were produced via from_vec on the current
                // crate's allocator).
                let k =
                    String::from_utf8(unsafe { p.key.into_vec_unchecked() }).expect("utf-8 key");
                let v = unsafe { p.value.into_vec_unchecked() };
                (k, v)
            })
            .into_iter()
            .collect::<HashMap<_, _>>();
        assert_eq!(restored, original);
    }

    #[test]
    fn ffi_vec_empty() {
        let v: FfiVec<u32> = FfiVec::empty();
        assert_eq!(v.len(), 0);
        assert!(v.is_empty());
        drop(v);
    }

    #[test]
    fn ffi_kv_pair_list_empty() {
        let list = FfiKvPairList::empty();
        assert_eq!(list.len(), 0);
        assert!(list.is_empty());
        drop(list);
    }

    #[test]
    fn ffi_vec_drain_into_preserves_elements_and_releases_outer() {
        // Build a kv list, drain each pair into a (String, String), and
        // verify both the content and that we don't double-drop on the
        // outer buffer (would crash if `drain_into` ran element destructors
        // twice).
        let pairs = vec![
            FfiKvPair::from_string_pair("a".into(), "alpha".into()),
            FfiKvPair::from_string_pair("b".into(), "beta".into()),
        ];
        let list = FfiKvPairList::from_vec(pairs);
        let drained: Vec<(String, String)> =
            list.drain_into(|p| (p.key.into_string_lossy(), p.value.into_string_lossy()));
        assert_eq!(drained.len(), 2);
        assert!(drained.contains(&("a".to_string(), "alpha".to_string())));
        assert!(drained.contains(&("b".to_string(), "beta".to_string())));
    }

    #[test]
    fn ffi_bytes_copy_to_vec_copies_and_releases_original() {
        // copy_to_vec must produce a host-owned Vec equal to the original,
        // while releasing the FfiBytes through its own drop_fn. We can't
        // observe the drop_fn directly without instrumentation, but the
        // copy invariant is enough to lock the behaviour.
        let ffi = FfiBytes::from_vec(b"hello".to_vec());
        let copy = ffi.copy_to_vec();
        assert_eq!(copy, b"hello");
    }

    // Note: a protobuf round-trip test exists in the host `jobworkerp-runner`
    // crate (`runner/src/runner/plugins.rs` test module) where the real
    // `MethodSchema` proto type is available. This crate stays
    // proto-independent so plugin authors can encode/decode against any
    // matching proto definition.
}
