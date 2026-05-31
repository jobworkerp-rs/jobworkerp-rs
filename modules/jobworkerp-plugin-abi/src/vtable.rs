//! `#[repr(C)]` vtable backing the V2 plugin trait object.
//!
//! Replaces `Box<dyn MultiMethodPluginRunnerV2>` so the host and plugin
//! can use independent rustc versions. The vtable exposes one
//! `extern "C" fn` per trait method; the proc macro `register_plugin_v2!`
//! generates the thunks that bridge the plugin's high-level `PluginV2`
//! implementation onto these slots.
//!
//! # ABI version policy
//!
//! * `abi_version: u32` = `(major as u32) << 16 | (minor as u32)`.
//! * `vtable_size: u32` = `mem::size_of::<PluginVtable>()` at plugin build
//!   time. The host accepts a plugin when `plugin_major == host_major`
//!   AND `plugin_minor <= host_minor` AND `plugin_size <= host_size`.
//! * Adding a new method at the **tail** of the struct increments
//!   `minor`. Existing plugins keep working: the host detects the size
//!   shortfall and falls back to a default implementation.
//! * Changing an existing method's signature increments `major`. Plugins
//!   must be rebuilt against the new host crate.
//!
//! See `manual/{en,ja}/src/plugin-development-v2.md` for the policy
//! documented to plugin authors.

use crate::cancel::FfiCancellationToken;
use crate::sink::{OutputSink, OutputSinkWithFinal};
use crate::types::{FfiBytes, FfiKvPairList, FfiOption, FfiResult};
use async_ffi::FfiFuture;

/// ABI major version. Bump on breaking layout / signature changes.
pub const PLUGIN_V2_ABI_MAJOR: u16 = 1;
/// ABI minor version. Bump on tail-appended methods (forward-compatible).
///
/// Minor history:
/// - 0: initial release (`setup_client_stream_channel` + EOF via sink drop).
/// - 1: adds `setup_client_stream_channel_v2` so plugins can receive the
///   feed `is_final` flag in-band via `OutputSinkWithFinal::send_raw_with_final`.
pub const PLUGIN_V2_ABI_MINOR: u16 = 1;
/// Packed major/minor for transport over the vtable header.
pub const PLUGIN_V2_ABI_VERSION: u32 =
    ((PLUGIN_V2_ABI_MAJOR as u32) << 16) | (PLUGIN_V2_ABI_MINOR as u32);

/// Minimum acceptable `vtable_size`. Anything smaller signals corruption
/// or an old `Box<dyn Trait>` payload being misinterpreted.
pub const VTABLE_SIZE_MIN: u32 = 128;
/// Maximum acceptable `vtable_size`. Far above any plausible future
/// expansion; rejects garbage values from misloaded plugins.
pub const VTABLE_SIZE_MAX: u32 = 4096;

/// Pointers below this value are treated as obviously invalid (null,
/// misaligned, or stale Box<dyn Trait> fat-pointer halves). Page-aligned
/// addresses on all supported OSes start well above 4 KiB.
pub const MIN_VALID_VTABLE_PTR: usize = 4096;

/// Outcome of a `run` call. The result and metadata are split into named
/// fields so future additions stay tail-appendable without breaking ABI.
#[repr(C)]
pub struct V2RunOutcome {
    pub result: FfiResult<FfiBytes, FfiBytes>,
    pub metadata: FfiKvPairList,
}

/// Plugin vtable. All method pointers receive the opaque `state` as
/// `*mut ()` (immutable methods narrow to `*const` internally; we keep
/// the signature uniform for proc-macro thunk generation).
#[repr(C)]
pub struct PluginVtable {
    // === Header (must be the first two fields, layout-stable) ===
    pub abi_version: u32,
    pub vtable_size: u32,

    // === Sync metadata ===
    pub name: unsafe extern "C" fn(*mut ()) -> FfiBytes,
    pub description: unsafe extern "C" fn(*mut ()) -> FfiBytes,
    pub runner_settings_proto: unsafe extern "C" fn(*mut ()) -> FfiBytes,
    pub settings_schema: unsafe extern "C" fn(*mut ()) -> FfiBytes,
    pub method_proto_map: unsafe extern "C" fn(*mut ()) -> FfiKvPairList,
    pub method_json_schema_map: unsafe extern "C" fn(*mut ()) -> FfiKvPairList,
    pub supports_client_stream: unsafe extern "C" fn(*mut (), FfiOption<FfiBytes>) -> bool,
    pub client_stream_data_proto: unsafe extern "C" fn(*mut (), FfiOption<FfiBytes>) -> FfiBytes,
    pub setup_client_stream_channel:
        unsafe extern "C" fn(*mut (), FfiOption<FfiBytes>) -> FfiOption<OutputSink>,

    // === Cancellation ===
    pub set_cancellation_token: unsafe extern "C" fn(*mut (), FfiCancellationToken),

    // === Async surface ===
    pub load: unsafe extern "C" fn(*mut (), FfiBytes) -> FfiFuture<FfiResult<(), FfiBytes>>,
    pub run: unsafe extern "C" fn(
        *mut (),
        FfiBytes,
        FfiKvPairList,
        FfiOption<FfiBytes>,
    ) -> FfiFuture<V2RunOutcome>,
    pub run_stream: unsafe extern "C" fn(
        *mut (),
        FfiBytes,
        FfiKvPairList,
        FfiOption<FfiBytes>,
        OutputSink,
    ) -> FfiFuture<FfiResult<FfiKvPairList, FfiBytes>>,

    // === Lifecycle ===
    pub drop_state: unsafe extern "C" fn(*mut ()),

    // === Reserved tail slot from minor 0 (DO NOT replace) ===
    /// Reserved slot present in every minor-0 vtable. Its signature must
    /// stay exactly as published, because the host's per-slot offset
    /// check identifies *new* slots by the bytes that follow this one.
    /// Replacing it with a different-signature method would alias an
    /// existing offset and corrupt all minor-0 plugins still in the
    /// field. New methods MUST be appended below.
    pub reserved_method_0: unsafe extern "C" fn(*mut ()) -> FfiResult<FfiBytes, FfiBytes>,

    // === Minor 1 additions (added 2026-06): is_final-aware client stream ===
    /// Variant of `setup_client_stream_channel` that returns an
    /// `OutputSinkWithFinal` so the host can forward feed chunks together
    /// with the originating `is_final` flag. Plugins that opt in receive
    /// the flag in-band on their internal `Receiver<(Vec<u8>, bool)>`,
    /// removing the need to infer EOF solely from `Receiver::recv == None`.
    /// Plugins built against minor 0 do not populate this slot — the
    /// host detects the size shortfall via `vtable_size` AND verifies
    /// `abi_minor >= 1` before dispatching, so a minor-0 vtable (which
    /// has `reserved_method_0` ending exactly at this slot's start
    /// offset) can never be misdispatched here.
    pub setup_client_stream_channel_v2:
        unsafe extern "C" fn(*mut (), FfiOption<FfiBytes>) -> FfiOption<OutputSinkWithFinal>,

    // === Reserved tail slot (ABI tail-append runway) ===
    /// Reserved slot kept for the next minor bump. Same rule as
    /// `reserved_method_0`: never replace, only append after it.
    pub reserved_method_1: unsafe extern "C" fn(*mut ()) -> FfiResult<FfiBytes, FfiBytes>,
}

impl PluginVtable {
    /// Major component of the packed `abi_version` field.
    pub fn abi_major(&self) -> u16 {
        (self.abi_version >> 16) as u16
    }

    /// Minor component of the packed `abi_version` field.
    pub fn abi_minor(&self) -> u16 {
        (self.abi_version & 0xFFFF) as u16
    }
}

/// FFI-safe instance produced by `load_multi_method_plugin_v2`. The host
/// wraps it in [`PluginInstance`] before storing so the underlying dylib
/// stays loaded as long as the instance lives.
#[repr(C)]
pub struct PluginInstanceRaw {
    pub state: *mut (),
    pub vtable: &'static PluginVtable,
}

/// Host-side wrapper around `PluginInstanceRaw` that anchors the dylib
/// to the instance lifetime. Dropping `PluginInstance` calls
/// `vtable.drop_state` on the plugin state; the dylib itself is kept
/// alive by the global plugin library cache (`Box::leak()` in
/// `RunnerPluginLoader`), so the vtable function pointers remain valid
/// for the duration of the process.
pub struct PluginInstance {
    pub state: *mut (),
    pub vtable: &'static PluginVtable,
    /// Anchors the leaked `&'static Library` for traceability. The
    /// reference is never dereferenced after construction; storing it
    /// keeps the lifetime relationship explicit and makes future code
    /// that wants to switch to ref-counted libraries easier to retrofit.
    /// Prefixed with `_` so it isn't mistakenly accessed.
    pub _library: &'static libloading::Library,
}

// SAFETY: the underlying state is the plugin's responsibility (it must
// honour `Send`/`Sync` for its own object). The vtable pointers and the
// `&'static Library` are themselves `Send + Sync`.
unsafe impl Send for PluginInstance {}
unsafe impl Sync for PluginInstance {}

impl Drop for PluginInstance {
    fn drop(&mut self) {
        // The plugin library is leaked into a `'static` slot at load
        // time, so the vtable function pointer is guaranteed to outlive
        // this Drop. Calling drop_state here frees the plugin's boxed
        // state; the library itself stays mapped for the rest of the
        // process lifetime (intentional: see RunnerPluginLoader docs).
        unsafe { (self.vtable.drop_state)(self.state) };
    }
}

impl PluginInstance {
    /// Returns true when the plugin's vtable advertises the
    /// `setup_client_stream_channel_v2` slot (ABI minor 1+). Plugins built
    /// against minor 0 report a smaller `vtable_size`; the host must fall
    /// back to `setup_client_stream_channel` and signal EOF via sink-drop
    /// when this returns false.
    ///
    /// Defense in depth: we check **both** `abi_minor >= 1` *and* the
    /// `vtable_size` offset. The minor check is the authoritative source
    /// of truth — without it, a minor-0 plugin whose vtable happens to
    /// reach the same offset (because its `reserved_method_0` tail slot
    /// is the same width as a function pointer) would mis-dispatch
    /// through the v2 slot's incompatible signature. The size check
    /// stays for symmetry and to catch malformed vtables that advertise
    /// minor 1 without actually populating the slot.
    pub fn supports_setup_client_stream_channel_v2(&self) -> bool {
        if self.vtable.abi_minor() < 1 {
            return false;
        }
        let base = self.vtable as *const PluginVtable as usize;
        let slot = std::ptr::addr_of!(self.vtable.setup_client_stream_channel_v2) as usize;
        let needed = (slot - base)
            + std::mem::size_of::<
                unsafe extern "C" fn(
                    *mut (),
                    FfiOption<FfiBytes>,
                ) -> FfiOption<OutputSinkWithFinal>,
            >();
        (self.vtable.vtable_size as usize) >= needed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_version_packing() {
        let major = (PLUGIN_V2_ABI_VERSION >> 16) as u16;
        let minor = (PLUGIN_V2_ABI_VERSION & 0xFFFF) as u16;
        assert_eq!(major, PLUGIN_V2_ABI_MAJOR);
        assert_eq!(minor, PLUGIN_V2_ABI_MINOR);
    }

    #[test]
    fn vtable_size_constants_sane() {
        let size = std::mem::size_of::<PluginVtable>() as u32;
        assert!(size >= VTABLE_SIZE_MIN, "host vtable smaller than minimum");
        assert!(size <= VTABLE_SIZE_MAX, "host vtable larger than maximum");
    }

    #[test]
    fn pointer_field_widths_are_word_sized() {
        // Every fn pointer in the vtable should be exactly one usize wide
        // so the layout is predictable across compilers.
        let fn_size = std::mem::size_of::<unsafe extern "C" fn(*mut ()) -> FfiBytes>();
        assert_eq!(fn_size, std::mem::size_of::<usize>());
    }
}
