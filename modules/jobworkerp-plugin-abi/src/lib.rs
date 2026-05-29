//! Stable ABI definitions for V2 jobworkerp plugins.
//!
//! This crate provides everything a plugin author needs to implement
//! a V2 plugin against jobworkerp-rs: the `#[repr(C)]` FFI primitive
//! types ([`FfiBytes`], [`FfiVec`], [`FfiOption`], [`FfiResult`],
//! [`FfiCancellationToken`], [`OutputSink`]), the vtable layout
//! ([`PluginVtable`], [`PluginInstance`], [`PluginInstanceRaw`]) and
//! the high-level [`PluginV2`] trait + helpers
//! ([`CancelToken`], [`HighLevelSink`]).
//!
//! It is shared by the jobworkerp-rs host runner and the
//! `jobworkerp-client-rs` plugin SDK so both sides resolve identical
//! types and ABI version constants automatically.
//!
//! See `manual/{en,ja}/src/plugin-development-v2.md` in the
//! jobworkerp-rs repository for the plugin author guide.

pub mod cancel;
pub mod sink;
pub mod types;
pub mod v2;
pub mod vtable;

// Re-exports for backwards compatibility with the previous
// `jobworkerp_runner::runner::plugins::ffi::*` module layout. New code
// should import from the per-file modules above.
pub mod ffi {
    pub use crate::cancel::*;
    pub use crate::sink::*;
    pub use crate::types::*;
    pub use crate::vtable::*;
}

// Crates re-exported for the proc-macro `register_plugin_v2!` so plugin
// authors only need a single dependency on `jobworkerp-plugin-abi` (the
// macro lives in `jobworkerp-plugin-abi-macros`).
pub use async_ffi;
pub use async_trait;
pub use futures;
pub use prost;

// Top-level convenience re-exports.
pub use cancel::{FfiCancellationToken, OwnedCancelHandle, from_tokio_util};
pub use sink::OutputSink;
pub use types::{
    FfiBytes, FfiKvPair, FfiKvPairList, FfiOption, FfiResult, FfiVec, kv_to_string_map,
    option_str_to_ffi, string_map_to_kv,
};
pub use v2::{CancelToken, HighLevelSink, PluginV2};
pub use vtable::{
    MIN_VALID_VTABLE_PTR, PLUGIN_V2_ABI_MAJOR, PLUGIN_V2_ABI_MINOR, PLUGIN_V2_ABI_VERSION,
    PluginInstance, PluginInstanceRaw, PluginVtable, V2RunOutcome, VTABLE_SIZE_MAX,
    VTABLE_SIZE_MIN,
};
