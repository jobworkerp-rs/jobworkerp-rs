//! V2 plugin loader: constructor-panic safety.
//!
//! Uses the `plugin_runner_panic_ctor_test` dylib whose constructor calls
//! `panic!`. Without the macro's `catch_unwind` wrap, the unwind would
//! cross the `extern "C"` boundary and abort the host. This test asserts
//! that `load_plugin_file` returns a normal error and the host stays up.

use jobworkerp_runner::runner::plugins::Plugins;
use jobworkerp_runner::runner::plugins::ffi::PluginInstanceRaw;
use libloading::{Library, Symbol};
use std::path::PathBuf;

fn locate_panic_ctor_plugin() -> Option<PathBuf> {
    // The dylib name matches the package name with underscores. cargo test
    // typically builds workspace members before running tests, so the .so
    // file is available under target/{debug,release}/.
    let ext = if cfg!(target_os = "macos") {
        ".dylib"
    } else if cfg!(target_os = "windows") {
        ".dll"
    } else {
        ".so"
    };
    let prefix = if cfg!(target_os = "windows") {
        ""
    } else {
        "lib"
    };
    let file = format!("{prefix}plugin_runner_panic_ctor_test{ext}");
    let candidates = [
        format!("./target/debug/{file}"),
        format!("../target/debug/{file}"),
        format!("./target/release/{file}"),
        format!("../target/release/{file}"),
    ];
    for c in candidates {
        let p = PathBuf::from(&c);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Exercise the plugin's `free_multi_method_plugin_v2` directly with the
/// sentinel that `load_multi_method_plugin_v2` returns after a constructor
/// panic. Without the null-guard in `free_*`, this would deref a null
/// `state` via `Box::from_raw` and segfault — surviving the call (and the
/// subsequent normal-flow assertion) is the regression check.
#[test]
fn v2_plugin_free_handles_sentinel_instance_after_constructor_panic() {
    let Some(path) = locate_panic_ctor_plugin() else {
        eprintln!(
            "skipping: plugin_runner_panic_ctor_test dylib not found under ./target or ../target"
        );
        return;
    };

    unsafe {
        let lib = Library::new(&path).expect("dylib loads");
        let load: Symbol<extern "C" fn() -> PluginInstanceRaw> = lib
            .get(b"load_multi_method_plugin_v2\0")
            .expect("V2 load symbol present");
        let free: Symbol<unsafe extern "C" fn(PluginInstanceRaw)> = lib
            .get(b"free_multi_method_plugin_v2\0")
            .expect("V2 free symbol present");

        let inst = load();
        // Sanity: the constructor panicked, so the loader returned the
        // sentinel (state = null, vtable_size = 0).
        assert!(
            inst.state.is_null(),
            "expected sentinel state, got non-null"
        );
        assert_eq!(
            inst.vtable.vtable_size, 0,
            "expected sentinel vtable_size = 0"
        );

        // The free function must accept the sentinel without dereferencing
        // the null state. If it forwarded to the normal vtable's drop_state
        // (the pre-fix behaviour), Box::from_raw(null) would segfault here.
        free(inst);
    }
}

#[tokio::test]
async fn v2_plugin_constructor_panic_is_returned_as_error_not_abort() {
    let Some(path) = locate_panic_ctor_plugin() else {
        // The dylib isn't built in this configuration; skip rather than fail.
        // (cargo test on the runner crate alone may not build sibling
        // workspace members.)
        eprintln!(
            "skipping: plugin_runner_panic_ctor_test dylib not found under ./target or ../target"
        );
        return;
    };

    let plugins = Plugins::new();
    let result = plugins
        .load_plugin_file(None, path.to_str().expect("valid utf8"), false)
        .await;

    // The host must observe the failure as a normal Result::Err instead of
    // aborting. The constructor panic surfaces as the sentinel
    // (state = null, vtable_size = 0); validate_v2_vtable converts that
    // into the "constructor panicked" message.
    let err = match result {
        Err(e) => e,
        Ok(_) => panic!("loading a panicking-constructor plugin must fail"),
    };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("constructor panicked")
            || msg.contains("vtable")
            || msg.contains("FFI symbols"),
        "expected a constructor-panic-related error, got: {msg}"
    );
}
