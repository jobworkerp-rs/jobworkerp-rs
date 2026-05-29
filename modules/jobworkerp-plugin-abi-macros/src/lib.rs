//! Procedural macros for the V2 plugin trait.
//!
//! See `jobworkerp-plugin-abi` for the trait/type definitions reached via
//! `::jobworkerp_plugin_abi::*` paths, and `plugin-development-v2.md` in
//! the jobworkerp-rs manual for the plugin author guide.

use proc_macro::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Expr, Result, Token, Type, parse_macro_input};

struct RegisterArgs {
    plugin_ty: Type,
    init_expr: Expr,
}

impl Parse for RegisterArgs {
    fn parse(input: ParseStream) -> Result<Self> {
        let plugin_ty: Type = input.parse()?;
        input.parse::<Token![,]>()?;
        let init_expr: Expr = input.parse()?;
        // Allow a trailing comma for cosmetic reasons.
        let _ = input.parse::<Token![,]>();
        Ok(Self {
            plugin_ty,
            init_expr,
        })
    }
}

/// Register a `PluginV2` implementation as a dynamically-loadable V2
/// plugin. Expands to:
///
/// * A `static PluginVtable` populated with per-method `extern "C" fn`
///   thunks that translate FFI-safe argument types to the high-level
///   `PluginV2` trait.
/// * Each thunk wraps the synchronous setup in `std::panic::catch_unwind`
///   and the asynchronous body in `futures::FutureExt::catch_unwind` so
///   panics never cross the FFI boundary.
/// * `load_multi_method_plugin_v2`: builds an instance using the provided
///   initialization expression (`MyPlugin::new()` or any expression
///   returning a `MyPlugin`), boxes it, and returns a `PluginInstanceRaw`
///   referencing the static vtable.
///
/// # Example
///
/// ```ignore
/// register_plugin_v2!(MyPlugin, MyPlugin::new());
/// // or with a fallible constructor:
/// register_plugin_v2!(MyPlugin, MyPlugin::from_env().expect("init"));
/// ```
#[proc_macro]
pub fn register_plugin_v2(input: TokenStream) -> TokenStream {
    let RegisterArgs {
        plugin_ty,
        init_expr,
    } = parse_macro_input!(input as RegisterArgs);

    let expanded = quote! {
        const _: () = {
            // Pull every type referenced by the thunks into scope from
            // the shared ABI crate; the plugin only needs a dependency on
            // `jobworkerp-plugin-abi` (and this proc-macro crate) — every
            // helper, including the `async-ffi` / `futures` / `prost`
            // re-exports, lives behind a single dependency.
            use ::jobworkerp_plugin_abi::ffi::{
                FfiBytes,
                FfiKvPair,
                FfiKvPairList,
                FfiOption,
                FfiResult,
                FfiVec,
                FfiCancellationToken,
                OutputSink,
                PluginInstanceRaw,
                PluginVtable,
                V2RunOutcome,
                PLUGIN_V2_ABI_VERSION,
            };
            use ::jobworkerp_plugin_abi::v2::{
                CancelToken,
                HighLevelSink,
                PluginV2,
            };
            use ::jobworkerp_plugin_abi::futures::{self, FutureExt as _RunnerFutureExt};
            use ::jobworkerp_plugin_abi::async_ffi::{
                FfiFuture,
                FutureExt as _AsyncFfiFutureExt,
            };
            use ::std::collections::HashMap;
            use ::std::panic::{catch_unwind, AssertUnwindSafe};

            // ------------------------------------------------------------------
            // Helpers
            // ------------------------------------------------------------------

            // Re-use the shared ABI conversion helpers so allocation
            // ownership rules (each FfiBytes drops on its embedded
            // `drop_fn`) are enforced in one place. These helpers live
            // in `::jobworkerp_plugin_abi::ffi`.
            use ::jobworkerp_plugin_abi::ffi::{
                kv_to_string_map as kv_to_hashmap,
                string_map_to_kv as hashmap_to_kv,
            };

            fn ffi_string_to_string(b: FfiBytes) -> String {
                // Goes through `copy_to_vec` internally, so the FfiBytes
                // (possibly host-allocated) drops via its own drop_fn.
                b.into_string_lossy()
            }

            /// Pack a `HashMap<String, Vec<u8>>` (the plugin already
            /// supplies protobuf-encoded values) into an FfiKvPairList.
            fn schema_map_to_kv(map: HashMap<String, Vec<u8>>) -> FfiKvPairList {
                let pairs: Vec<FfiKvPair> = map
                    .into_iter()
                    .map(|(k, bytes)| FfiKvPair {
                        key: FfiBytes::from_vec(k.into_bytes()),
                        value: FfiBytes::from_vec(bytes),
                    })
                    .collect();
                FfiVec::from_vec(pairs)
            }

            // ------------------------------------------------------------------
            // Thunks
            // ------------------------------------------------------------------

            type PluginTy = #plugin_ty;

            unsafe fn plugin_mut<'a>(state: *mut ()) -> &'a mut PluginTy {
                unsafe { &mut *(state as *mut PluginTy) }
            }
            unsafe fn plugin_ref<'a>(state: *mut ()) -> &'a PluginTy {
                unsafe { &*(state as *const PluginTy) }
            }

            unsafe extern "C" fn __thunk_name(state: *mut ()) -> FfiBytes {
                let result = catch_unwind(AssertUnwindSafe(|| unsafe {
                    plugin_ref(state).name()
                }));
                FfiBytes::from_vec(result.unwrap_or_else(|_| String::new()).into_bytes())
            }
            unsafe extern "C" fn __thunk_description(state: *mut ()) -> FfiBytes {
                let result = catch_unwind(AssertUnwindSafe(|| unsafe {
                    plugin_ref(state).description()
                }));
                FfiBytes::from_vec(result.unwrap_or_else(|_| String::new()).into_bytes())
            }
            unsafe extern "C" fn __thunk_runner_settings_proto(state: *mut ()) -> FfiBytes {
                let result = catch_unwind(AssertUnwindSafe(|| unsafe {
                    plugin_ref(state).runner_settings_proto()
                }));
                FfiBytes::from_vec(result.unwrap_or_else(|_| String::new()).into_bytes())
            }
            unsafe extern "C" fn __thunk_settings_schema(state: *mut ()) -> FfiBytes {
                let result = catch_unwind(AssertUnwindSafe(|| unsafe {
                    plugin_ref(state).settings_schema()
                }));
                FfiBytes::from_vec(result.unwrap_or_else(|_| String::new()).into_bytes())
            }
            unsafe extern "C" fn __thunk_method_proto_map(state: *mut ()) -> FfiKvPairList {
                let result = catch_unwind(AssertUnwindSafe(|| unsafe {
                    plugin_ref(state).method_proto_map()
                }));
                match result {
                    Ok(map) => schema_map_to_kv(map),
                    Err(_) => FfiVec::empty(),
                }
            }
            unsafe extern "C" fn __thunk_method_json_schema_map(state: *mut ()) -> FfiKvPairList {
                let result = catch_unwind(AssertUnwindSafe(|| unsafe {
                    plugin_ref(state).method_json_schema_map()
                }));
                match result {
                    Ok(Some(map)) => schema_map_to_kv(map),
                    Ok(None) | Err(_) => FfiVec::empty(),
                }
            }
            unsafe extern "C" fn __thunk_supports_client_stream(
                state: *mut (),
                using: FfiOption<FfiBytes>,
            ) -> bool {
                let using_str: Option<String> =
                    using.into_option().map(ffi_string_to_string);
                catch_unwind(AssertUnwindSafe(|| unsafe {
                    plugin_ref(state).supports_client_stream(using_str.as_deref())
                }))
                .unwrap_or(false)
            }
            unsafe extern "C" fn __thunk_client_stream_data_proto(
                state: *mut (),
                using: FfiOption<FfiBytes>,
            ) -> FfiBytes {
                let using_str: Option<String> =
                    using.into_option().map(ffi_string_to_string);
                let result = catch_unwind(AssertUnwindSafe(|| unsafe {
                    plugin_ref(state).client_stream_data_proto(using_str.as_deref())
                }));
                match result {
                    Ok(Some(s)) => FfiBytes::from_vec(s.into_bytes()),
                    _ => FfiBytes::empty(),
                }
            }
            unsafe extern "C" fn __thunk_setup_client_stream_channel(
                state: *mut (),
                using: FfiOption<FfiBytes>,
            ) -> FfiOption<OutputSink> {
                let using_str: Option<String> =
                    using.into_option().map(ffi_string_to_string);
                let plugin = unsafe { plugin_mut(state) };
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    ::jobworkerp_plugin_abi::futures::executor::block_on(
                        plugin.setup_client_stream_channel(using_str.as_deref()),
                    )
                }));
                match outcome {
                    Ok(Some(sink)) => FfiOption::Some(sink),
                    _ => FfiOption::None,
                }
            }
            unsafe extern "C" fn __thunk_set_cancellation_token(
                state: *mut (),
                token: FfiCancellationToken,
            ) {
                let plugin = unsafe { plugin_mut(state) };
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    plugin.set_cancellation_token(CancelToken::from_ffi(token));
                }));
            }

            // Pointer wrapper used to carry the plugin state pointer into
            // async blocks. `impl` blocks must live at module scope, so we
            // declare the wrapper once and reuse it across thunks.
            struct StatePtr(*mut ());
            // SAFETY: the plugin author guarantees its state is `Send`; the
            // V2 docs require this. The pointer is treated as opaque inside
            // the thunks (only dereferenced through the plugin trait).
            unsafe impl Send for StatePtr {}

            unsafe extern "C" fn __thunk_load(
                state: *mut (),
                settings: FfiBytes,
            ) -> FfiFuture<FfiResult<(), FfiBytes>> {
                let captured = StatePtr(state);
                async move {
                    // Resolve the plugin reference once and drop the
                    // `StatePtr` immediately so the raw pointer is no
                    // longer captured across the await boundary.
                    let plugin: &mut PluginTy = unsafe { &mut *(captured.0 as *mut PluginTy) };
                    // Drop the StatePtr immediately so the raw pointer is no
                    // longer in scope across the await boundary; otherwise the
                    // resulting future would not be Send (the `*mut ()` field
                    // is captured by reference into the async block).
                    #[allow(clippy::drop_non_drop)]
                    drop(captured);
                    // Use `copy_to_vec` so the host-allocated `settings`
                    // buffer drops through its own embedded drop_fn rather
                    // than via the plugin's allocator.
                    let fut = plugin.load(settings.copy_to_vec());
                    let result = AssertUnwindSafe(fut).catch_unwind().await;
                    match result {
                        Ok(Ok(())) => FfiResult::Ok(()),
                        Ok(Err(e)) => FfiResult::Err(FfiBytes::from_vec(e.into_bytes())),
                        Err(_) => FfiResult::Err(FfiBytes::from_vec(
                            b"panic during PluginV2::load".to_vec(),
                        )),
                    }
                }
                .into_ffi()
            }

            unsafe extern "C" fn __thunk_run(
                state: *mut (),
                args: FfiBytes,
                metadata: FfiKvPairList,
                using: FfiOption<FfiBytes>,
            ) -> FfiFuture<V2RunOutcome> {
                let captured = StatePtr(state);
                let using_owned: Option<String> = using.into_option().map(ffi_string_to_string);
                async move {
                    let plugin: &mut PluginTy = unsafe { &mut *(captured.0 as *mut PluginTy) };
                    // Drop the StatePtr immediately so the raw pointer is no
                    // longer in scope across the await boundary; otherwise the
                    // resulting future would not be Send (the `*mut ()` field
                    // is captured by reference into the async block).
                    #[allow(clippy::drop_non_drop)]
                    drop(captured);
                    let meta = kv_to_hashmap(metadata);
                    let fut = plugin.run(args.copy_to_vec(), meta, using_owned);
                    let outcome = AssertUnwindSafe(fut).catch_unwind().await;
                    match outcome {
                        Ok((Ok(payload), meta)) => V2RunOutcome {
                            result: FfiResult::Ok(FfiBytes::from_vec(payload)),
                            metadata: hashmap_to_kv(meta),
                        },
                        Ok((Err(e), meta)) => V2RunOutcome {
                            result: FfiResult::Err(FfiBytes::from_vec(e.into_bytes())),
                            metadata: hashmap_to_kv(meta),
                        },
                        Err(_) => V2RunOutcome {
                            result: FfiResult::Err(FfiBytes::from_vec(
                                b"panic during PluginV2::run".to_vec(),
                            )),
                            metadata: FfiVec::empty(),
                        },
                    }
                }
                .into_ffi()
            }

            unsafe extern "C" fn __thunk_run_stream(
                state: *mut (),
                args: FfiBytes,
                metadata: FfiKvPairList,
                using: FfiOption<FfiBytes>,
                output: OutputSink,
            ) -> FfiFuture<FfiResult<FfiKvPairList, FfiBytes>> {
                let captured = StatePtr(state);
                let using_owned: Option<String> = using.into_option().map(ffi_string_to_string);
                async move {
                    let plugin: &mut PluginTy = unsafe { &mut *(captured.0 as *mut PluginTy) };
                    // Drop the StatePtr immediately so the raw pointer is no
                    // longer in scope across the await boundary; otherwise the
                    // resulting future would not be Send (the `*mut ()` field
                    // is captured by reference into the async block).
                    #[allow(clippy::drop_non_drop)]
                    drop(captured);
                    let meta = kv_to_hashmap(metadata);
                    let sink = HighLevelSink::from_ffi(output);
                    let fut = plugin.run_stream(args.copy_to_vec(), meta, using_owned, sink);
                    let outcome = AssertUnwindSafe(fut).catch_unwind().await;
                    match outcome {
                        Ok(Ok(meta)) => FfiResult::Ok(hashmap_to_kv(meta)),
                        Ok(Err(e)) => FfiResult::Err(FfiBytes::from_vec(e.into_bytes())),
                        Err(_) => FfiResult::Err(FfiBytes::from_vec(
                            b"panic during PluginV2::run_stream".to_vec(),
                        )),
                    }
                }
                .into_ffi()
            }

            unsafe extern "C" fn __thunk_drop_state(state: *mut ()) {
                // Drop the boxed plugin. Wrapped in catch_unwind so a
                // panicking destructor cannot tear down the host.
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    drop(unsafe { Box::from_raw(state as *mut PluginTy) });
                }));
            }

            // Plugins that predate the tail-appended slot return a smaller
            // vtable_size. The current cancel_test plugin opts to keep the
            // reserved slot populated by reusing this default thunk so the
            // host can exercise the slot end-to-end.
            unsafe extern "C" fn __thunk_reserved_method_0(
                _state: *mut (),
            ) -> FfiResult<FfiBytes, FfiBytes> {
                FfiResult::Err(FfiBytes::from_vec(
                    b"reserved_method_0 not implemented".to_vec(),
                ))
            }

            // -- Sentinel thunks for the "constructor panicked" instance.
            // Each one returns the cheapest valid value for its slot. The
            // host detects the panic via `state.is_null()` and never
            // dispatches through this vtable, but defining them safely
            // keeps the layout valid in case validation order ever shifts.
            unsafe extern "C" fn __invalid_bytes(_state: *mut ()) -> FfiBytes {
                FfiBytes::empty()
            }
            unsafe extern "C" fn __invalid_kv(_state: *mut ()) -> FfiKvPairList {
                FfiVec::empty()
            }
            unsafe extern "C" fn __invalid_supports_stream(
                _state: *mut (),
                _using: FfiOption<FfiBytes>,
            ) -> bool {
                false
            }
            unsafe extern "C" fn __invalid_client_stream_data(
                _state: *mut (),
                _using: FfiOption<FfiBytes>,
            ) -> FfiBytes {
                FfiBytes::empty()
            }
            unsafe extern "C" fn __invalid_setup_client_stream_channel(
                _state: *mut (),
                _using: FfiOption<FfiBytes>,
            ) -> FfiOption<OutputSink> {
                FfiOption::None
            }
            unsafe extern "C" fn __invalid_set_cancellation_token(
                _state: *mut (),
                _token: FfiCancellationToken,
            ) {
            }
            unsafe extern "C" fn __invalid_load(
                _state: *mut (),
                _settings: FfiBytes,
            ) -> FfiFuture<FfiResult<(), FfiBytes>> {
                async move {
                    FfiResult::Err(FfiBytes::from_vec(
                        b"plugin constructor panicked during load_multi_method_plugin_v2"
                            .to_vec(),
                    ))
                }
                .into_ffi()
            }
            unsafe extern "C" fn __invalid_run(
                _state: *mut (),
                _args: FfiBytes,
                _metadata: FfiKvPairList,
                _using: FfiOption<FfiBytes>,
            ) -> FfiFuture<V2RunOutcome> {
                async move {
                    V2RunOutcome {
                        result: FfiResult::Err(FfiBytes::from_vec(
                            b"plugin constructor panicked during load_multi_method_plugin_v2"
                                .to_vec(),
                        )),
                        metadata: FfiVec::empty(),
                    }
                }
                .into_ffi()
            }
            unsafe extern "C" fn __invalid_run_stream(
                _state: *mut (),
                _args: FfiBytes,
                _metadata: FfiKvPairList,
                _using: FfiOption<FfiBytes>,
                _output: OutputSink,
            ) -> FfiFuture<FfiResult<FfiKvPairList, FfiBytes>> {
                async move {
                    FfiResult::Err(FfiBytes::from_vec(
                        b"plugin constructor panicked during load_multi_method_plugin_v2"
                            .to_vec(),
                    ))
                }
                .into_ffi()
            }
            unsafe extern "C" fn __invalid_drop_state(_state: *mut ()) {
                // Nothing to drop: the host detects state.is_null() and skips
                // drop_state. This is here only so the slot is non-null.
            }

            // ------------------------------------------------------------------
            // Static vtable
            // ------------------------------------------------------------------

            static __PLUGIN_V2_VTABLE: PluginVtable = PluginVtable {
                abi_version: PLUGIN_V2_ABI_VERSION,
                // sizeof(PluginVtable) at the time the plugin was built.
                vtable_size: ::std::mem::size_of::<PluginVtable>() as u32,
                name: __thunk_name,
                description: __thunk_description,
                runner_settings_proto: __thunk_runner_settings_proto,
                settings_schema: __thunk_settings_schema,
                method_proto_map: __thunk_method_proto_map,
                method_json_schema_map: __thunk_method_json_schema_map,
                supports_client_stream: __thunk_supports_client_stream,
                client_stream_data_proto: __thunk_client_stream_data_proto,
                setup_client_stream_channel: __thunk_setup_client_stream_channel,
                set_cancellation_token: __thunk_set_cancellation_token,
                load: __thunk_load,
                run: __thunk_run,
                run_stream: __thunk_run_stream,
                drop_state: __thunk_drop_state,
                reserved_method_0: __thunk_reserved_method_0,
            };

            // Returned alongside a null state when the plugin constructor
            // panics. Identifiable via `vtable_size == 0`, which the host
            // rejects by range check anyway; the host's first-pass
            // `state.is_null()` check produces a clearer error message.
            static __PLUGIN_V2_INVALID_VTABLE: PluginVtable = PluginVtable {
                abi_version: PLUGIN_V2_ABI_VERSION,
                vtable_size: 0,
                name: __invalid_bytes,
                description: __invalid_bytes,
                runner_settings_proto: __invalid_bytes,
                settings_schema: __invalid_bytes,
                method_proto_map: __invalid_kv,
                method_json_schema_map: __invalid_kv,
                supports_client_stream: __invalid_supports_stream,
                client_stream_data_proto: __invalid_client_stream_data,
                setup_client_stream_channel: __invalid_setup_client_stream_channel,
                set_cancellation_token: __invalid_set_cancellation_token,
                load: __invalid_load,
                run: __invalid_run,
                run_stream: __invalid_run_stream,
                drop_state: __invalid_drop_state,
                reserved_method_0: __thunk_reserved_method_0,
            };

            // ------------------------------------------------------------------
            // FFI entry point
            // ------------------------------------------------------------------

            #[unsafe(no_mangle)]
            #[allow(improper_ctypes_definitions)]
            pub unsafe extern "C" fn load_multi_method_plugin_v2() -> PluginInstanceRaw {
                // Construct the plugin under catch_unwind so a panicking
                // constructor (e.g. `Runtime::build().expect(...)` failing
                // due to an environment issue) returns an invalid sentinel
                // instance instead of unwinding across the extern "C"
                // boundary, which would abort the host process.
                let built = catch_unwind(AssertUnwindSafe(|| -> PluginTy { #init_expr }));
                match built {
                    Ok(plugin) => {
                        let boxed = Box::new(plugin);
                        PluginInstanceRaw {
                            state: Box::into_raw(boxed) as *mut (),
                            vtable: &__PLUGIN_V2_VTABLE,
                        }
                    }
                    Err(_) => PluginInstanceRaw {
                        state: ::std::ptr::null_mut(),
                        vtable: &__PLUGIN_V2_INVALID_VTABLE,
                    },
                }
            }

            #[unsafe(no_mangle)]
            #[allow(improper_ctypes_definitions)]
            pub unsafe extern "C" fn free_multi_method_plugin_v2(inst: PluginInstanceRaw) {
                // Skip the sentinel instance produced when the constructor
                // panicked (state = null + __PLUGIN_V2_INVALID_VTABLE):
                // calling its drop_state would `Box::from_raw(null)` and
                // segfault. Dispatch through inst.vtable.drop_state (not
                // __PLUGIN_V2_VTABLE.drop_state) so the free path matches
                // whichever vtable load_* returned — the sentinel installs
                // its own no-op drop_state in case future callers stop
                // checking for null themselves.
                if inst.state.is_null() {
                    return;
                }
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    unsafe { (inst.vtable.drop_state)(inst.state) };
                }));
            }
        };
    };

    expanded.into()
}
