//! FFI-safe primitive types and helpers for the V2 plugin trait.
//!
//! The submodules in this directory replace `tokio` / `tokio_util` / `HashMap`
//! types that previously crossed the FFI boundary, removing the Rust-ABI
//! pinning requirement on `tokio`. Only `async-ffi`'s `FfiFuture<T>` retains
//! an exact-pin contract; everything else uses `#[repr(C)]` data structures
//! with self-describing allocator vtables.
//!
//! See `manual/{en,ja}/src/plugin-development-v2.md` for the plugin author
//! contract.

pub mod cancel;
pub mod sink;
pub mod types;
pub mod vtable;

pub use cancel::{FfiCancellationToken, OwnedCancelHandle, from_tokio_util};
pub use sink::OutputSink;
pub use types::{FfiBytes, FfiKvPair, FfiKvPairList, FfiOption, FfiResult, FfiVec};
pub use vtable::{
    PLUGIN_V2_ABI_MAJOR, PLUGIN_V2_ABI_MINOR, PLUGIN_V2_ABI_VERSION, PluginInstance,
    PluginInstanceRaw, PluginVtable, V2RunOutcome, VTABLE_SIZE_MAX, VTABLE_SIZE_MIN,
};
