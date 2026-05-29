//! V2 plugin loader: constructor-panic safety.
//!
//! Uses the `plugin_runner_panic_ctor_test` dylib whose constructor calls
//! `panic!`. Without the macro's `catch_unwind` wrap, the unwind would
//! cross the `extern "C"` boundary and abort the host. This test asserts
//! that `load_plugin_file` returns a normal error and the host stays up.

use jobworkerp_runner::runner::plugins::Plugins;
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
