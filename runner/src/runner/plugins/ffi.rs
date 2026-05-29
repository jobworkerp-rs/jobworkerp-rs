//! FFI-safe primitive types and helpers for the V2 plugin trait.
//!
//! All definitions live in the shared `jobworkerp-plugin-abi` crate so
//! the host runner and the `jobworkerp-client-rs` plugin SDK resolve
//! identical types automatically. This module is kept as a thin
//! re-export shim for compatibility with code that still imports
//! through `jobworkerp_runner::runner::plugins::ffi::*`.

pub use jobworkerp_plugin_abi::ffi::*;
