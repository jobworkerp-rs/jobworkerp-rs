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

use super::cancel::FfiCancellationToken;
use super::sink::OutputSink;
use super::types::{FfiBytes, FfiKvPairList, FfiOption, FfiResult};
use async_ffi::FfiFuture;
use std::sync::Arc;

/// ABI major version. Bump on breaking layout / signature changes.
pub const PLUGIN_V2_ABI_MAJOR: u16 = 1;
/// ABI minor version. Bump on tail-appended methods (forward-compatible).
pub const PLUGIN_V2_ABI_MINOR: u16 = 0;
/// Packed major/minor for transport over the vtable header.
pub const PLUGIN_V2_ABI_VERSION: u32 =
    ((PLUGIN_V2_ABI_MAJOR as u32) << 16) | (PLUGIN_V2_ABI_MINOR as u32);

/// Minimum acceptable `vtable_size`. Anything smaller signals corruption
/// or an old `Box<dyn Trait>` payload being misinterpreted.
pub const VTABLE_SIZE_MIN: u32 = 128;
/// Maximum acceptable `vtable_size`. Far above any plausible future
/// expansion; rejects garbage values from misloaded plugins.
pub const VTABLE_SIZE_MAX: u32 = 4096;

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

    // === Reserved tail slot (ABI tail-append exercise) ===
    /// Reserved slot kept for forward compatibility. Plugins built before
    /// the slot existed report a smaller `vtable_size`; the host falls back
    /// to a no-op default in that case. Replace with a real method when a
    /// genuine need arises, and add a new `reserved_method_1` in the same
    /// commit so the policy keeps a runway.
    pub reserved_method_0: unsafe extern "C" fn(*mut ()) -> FfiResult<FfiBytes, FfiBytes>,
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
/// `Arc<Library>` to the instance lifetime. Dropping `PluginInstance`
/// first calls `vtable.drop_state` on the plugin state, then releases
/// the `Library` Arc — guaranteeing that the vtable pointers stay valid
/// for `drop_state`. This ordering is structural; do not bypass it.
pub struct PluginInstance {
    pub state: *mut (),
    pub vtable: &'static PluginVtable,
    /// Keeps the dynamic library mapped while `state` is alive.
    pub library: Arc<libloading::Library>,
}

// SAFETY: the underlying state is the plugin's responsibility (it must
// honour `Send`/`Sync` for its own object). The vtable pointers and the
// `Arc<Library>` are themselves `Send + Sync`.
unsafe impl Send for PluginInstance {}
unsafe impl Sync for PluginInstance {}

impl Drop for PluginInstance {
    fn drop(&mut self) {
        // Order matters: free the plugin state via the vtable's
        // `drop_state` thunk first, while the dylib (and therefore the
        // function pointer) is still mapped.
        unsafe { (self.vtable.drop_state)(self.state) };
        // `library` Drop then runs, releasing the `Arc<Library>` strong
        // reference; if it was the last one, `dlclose` happens here.
    }
}

impl PluginInstance {
    /// Returns true when the plugin's vtable advertises the reserved
    /// tail slot. False indicates the plugin was built before the slot
    /// existed; the host must fall back to a default for that method.
    pub fn supports_reserved_method_0(&self) -> bool {
        // Offset of the reserved slot relative to the vtable start.
        // The plugin's `vtable_size` must reach past this offset for the
        // slot to be considered initialised.
        let needed = std::mem::size_of::<PluginVtable>() as u32;
        self.vtable.vtable_size >= needed
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
