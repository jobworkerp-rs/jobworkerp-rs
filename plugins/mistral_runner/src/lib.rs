//! MistralRS Local LLM Runner Plugin
//!
//! This plugin provides local LLM inference using MistralRS, completely independent
//! of the main jobworkerp build. It supports:
//! - Chat completion with tool calling
//! - Text completion
//! - Streaming responses
//! - gRPC-based tool execution via FunctionService

use std::alloc::System;
use std::panic;

use jobworkerp_runner::runner::plugins::MultiMethodPluginRunner;

pub mod cancel;
pub mod chat;
pub mod completion;
pub mod conversion;
pub mod core;
pub mod grpc;
pub mod plugin;
pub mod tracing_helper;

// Plugin-specific proto definitions
pub mod mistral_proto {
    tonic::include_proto!("mistral_runner");
}

// Use System allocator for FFI safety
#[global_allocator]
static ALLOCATOR: System = System;

/// Multi-method plugin loader FFI symbol
///
/// # Safety
/// This function is called from C code via libloading.
/// The returned Box must be freed by calling `free_multi_method_plugin`.
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn load_multi_method_plugin() -> Box<dyn MultiMethodPluginRunner + Send + Sync> {
    // Set panic hook for FFI boundary
    panic::set_hook(Box::new(|panic_info| {
        tracing::error!("MistralPlugin panic: {:?}", panic_info);
    }));

    Box::new(plugin::MistralPlugin::new())
}

/// Multi-method plugin unloader FFI symbol
///
/// # Safety
/// The `ptr` must be a valid pointer returned by `load_multi_method_plugin`.
#[unsafe(no_mangle)]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn free_multi_method_plugin(ptr: Box<dyn MultiMethodPluginRunner + Send + Sync>) {
    // Use catch_unwind for safe drop at FFI boundary
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        drop(ptr);
    }));
}
