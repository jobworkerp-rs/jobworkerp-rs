use crate::runner::FeedData;
use crate::runner::RunnerSpec;
use crate::runner::RunnerTrait;
use std::collections::HashMap;
use std::sync::Arc;

use super::{CancellationToken, PluginRunnerVariant};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use futures::executor::block_on;
use futures::stream::BoxStream;
use futures::stream::StreamExt;
use proto::jobworkerp::data::ResultOutputItem;
use proto::jobworkerp::data::Trailer;
use proto::jobworkerp::data::result_output_item;
use tokio::sync::RwLock;
use tokio::sync::mpsc;

use super::super::cancellation::CancelMonitoring;
use super::super::cancellation_helper::{CancelMonitoringHelper, UseCancelMonitoringHelper};
use proto::jobworkerp::data::{JobData, JobId, JobResult};

/// Variant type for lock-free collect_stream dispatch
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginVariantType {
    Legacy,
    MultiMethod,
    MultiMethodV2,
}

// Re-exports for readability at the call sites inside this file.
use super::ffi::{
    kv_to_string_map as meta_from_ffi, option_str_to_ffi as using_to_ffi,
    string_map_to_kv as meta_to_ffi,
};

/// Stable label for a `PluginRunnerVariant` enum arm. Used by safe
/// error-fallback paths when the cached `variant_type` and the actual
/// variant disagree — we surface the discrepancy in logs/errors instead
/// of panicking the worker process.
fn variant_kind(v: &PluginRunnerVariant) -> &'static str {
    match v {
        PluginRunnerVariant::Legacy(_) => "Legacy",
        PluginRunnerVariant::MultiMethod(_) => "MultiMethod",
        PluginRunnerVariant::MultiMethodV2(_) => "MultiMethodV2",
    }
}

/// Outcome of awaiting a V2 plugin's `run_stream` `JoinHandle`. Outer
/// `Result` is the join result (panic / cancellation); inner is the
/// plugin's own success/failure (`Ok(final_metadata)` / `Err(message)`).
///
/// FFI types (`FfiKvPairList` / `FfiBytes`) are decoded into
/// `HashMap<String, String>` / `String` immediately after the future
/// resolves, so downstream code keeps using ordinary Rust types.
type V2RunStreamOutcome = std::result::Result<
    std::result::Result<HashMap<String, String>, String>,
    tokio::task::JoinError,
>;

/// Convert the outcome of a V2 plugin's `run_stream` future, observed
/// **before any chunk has been yielded downstream**, into either:
/// - `Ok(empty_stream)` — the plugin completed successfully without
///   producing any data; emit only the End trailer with its metadata.
/// - `Err(...)` — the plugin returned Err (e.g. the default "not
///   implemented" impl), was cancelled, or its task panicked. Surface as
///   an outer `run_stream` Err so the job is marked failed.
fn finalize_pre_stream(res: V2RunStreamOutcome) -> Result<BoxStream<'static, ResultOutputItem>> {
    match res {
        Ok(Ok(metadata)) => {
            let end = ResultOutputItem {
                item: Some(result_output_item::Item::End(Trailer { metadata })),
            };
            Ok(futures::stream::iter(std::iter::once(end)).boxed())
        }
        Ok(Err(e)) => Err(anyhow!("v2 plugin run_stream error: {}", e)),
        Err(join_err) if join_err.is_cancelled() => {
            Err(anyhow!("v2 plugin run_stream was cancelled"))
        }
        Err(join_err) => Err(anyhow!("v2 plugin task join error: {:?}", join_err)),
    }
}

/**
 * PluginRunner wrapper
 * (for self mutability (run(), cancel()))
 */
#[derive(Clone)]
pub struct PluginRunnerWrapperImpl {
    variant: Arc<RwLock<PluginRunnerVariant>>,
    /// Cached variant type for lock-free collect_stream dispatch
    variant_type: PluginVariantType,
    cancel_helper: Option<CancelMonitoringHelper>,
}

impl PluginRunnerWrapperImpl {
    pub fn new(variant: Arc<RwLock<PluginRunnerVariant>>) -> Self {
        // Determine variant type at construction time
        let variant_type = {
            let guard = block_on(variant.read());
            match &*guard {
                PluginRunnerVariant::Legacy(_) => PluginVariantType::Legacy,
                PluginRunnerVariant::MultiMethod(_) => PluginVariantType::MultiMethod,
                PluginRunnerVariant::MultiMethodV2(_) => PluginVariantType::MultiMethodV2,
            }
        };
        Self {
            variant,
            variant_type,
            cancel_helper: None,
        }
    }

    /// Build a wrapper around an in-process V2 plugin. Intended for tests
    /// that exercise the wrapper without loading a real `.so`; the
    /// production loader (`find_plugin_runner_by_name`) uses `new` directly.
    ///
    /// Gated on `test-utils` so the public API does not expose
    /// `PluginRunnerVariant`'s internal representation to general callers.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn from_v2_for_test(instance: super::ffi::PluginInstance) -> Self {
        Self::new(Arc::new(RwLock::new(PluginRunnerVariant::MultiMethodV2(
            instance,
        ))))
    }

    /// Attach a cancellation monitoring helper. Used by the app-wrapper layer to
    /// inject the pubsub-backed cancellation manager after the plugin is loaded;
    /// V2 plugins additionally receive the token at setup time.
    pub fn set_cancel_helper(&mut self, helper: CancelMonitoringHelper) {
        self.cancel_helper = Some(helper);
    }
    /// V2 stream path: spawn the plugin's `run_stream` future on the host
    /// runtime, then yield chunks directly from the raw mpsc receiver inside
    /// the output stream. The plugin's final metadata becomes the End trailer.
    ///
    /// Errors surfaced before any chunk has been sent (e.g. the default
    /// `Err("not implemented")` impl, parameter validation, internal
    /// `variant_type` drift) are returned as `Err(...)` from this function
    /// so callers like `run_and_stream` mark the job as failed instead of
    /// emitting an empty success stream.
    ///
    /// Mid-stream errors (the plugin sent some chunks, then returned Err)
    /// still terminate the stream with an End trailer — there is no
    /// in-band error item on `ResultOutputItem`, so consumers must consult
    /// metadata / logs to detect partial failures. This matches V1's
    /// `receive_stream` behaviour.
    async fn run_stream_v2(
        &self,
        arg: &[u8],
        metadata: HashMap<String, String>,
        using: Option<&str>,
        cancel_token: Option<CancellationToken>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        let (raw_tx, mut raw_rx) = mpsc::channel::<Vec<u8>>(16);
        let variant_clone = self.variant.clone();
        let arg_owned = arg.to_vec();
        let using_owned = using.map(|s| s.to_string());
        let metadata_for_fallback = metadata.clone();

        let mut plugin_fut = tokio::spawn(async move {
            let fut = {
                let mut guard = variant_clone.write().await;
                match &mut *guard {
                    super::PluginRunnerVariant::MultiMethodV2(plugin) => {
                        let sink = super::ffi::OutputSink::from_sender(raw_tx);
                        let args_ffi = super::ffi::FfiBytes::from_vec(arg_owned);
                        let meta_ffi = meta_to_ffi(metadata);
                        let using_ffi = using_to_ffi(using_owned.as_deref());
                        // SAFETY: state and vtable were validated at load
                        // time; the FFI types own their buffers and are
                        // consumed by the plugin.
                        unsafe {
                            (plugin.vtable.run_stream)(
                                plugin.state,
                                args_ffi,
                                meta_ffi,
                                using_ffi,
                                sink,
                            )
                        }
                    }
                    other => {
                        // Cached variant_type and the actual variant disagree.
                        // Bail safely instead of panicking the worker process.
                        return Err(format!(
                            "internal: variant_type cached as MultiMethodV2 but variant is {}",
                            variant_kind(other)
                        ));
                    }
                }
            };
            // Decode FFI types to plain HashMap/String for downstream code.
            match fut.await {
                super::ffi::FfiResult::Ok(meta_ffi) => Ok(meta_from_ffi(meta_ffi)),
                super::ffi::FfiResult::Err(err_bytes) => {
                    Err(String::from_utf8_lossy(err_bytes.as_slice()).into_owned())
                }
            }
        });

        // Race the first chunk against `plugin_fut`:
        //
        // - `plugin_fut` completes first: the plugin may have buffered
        //   chunks into the channel before exiting (e.g. a short synchronous
        //   `send(chunk).await; Ok(meta)`). Drain those chunks; only if
        //   none arrived treat this as a pre-stream outcome
        //   (empty-success Ok or hard Err via `finalize_pre_stream`).
        //   Without the drain biased select! would silently lose data.
        // - `raw_rx.recv()` yields the first chunk: enter the streaming
        //   loop with that chunk; `plugin_fut` is resolved later for the
        //   End trailer.
        // - `raw_rx.recv()` yields `None`: the sender was dropped before
        //   any chunk; await `plugin_fut` for the outcome.
        let mut plugin_outcome: Option<V2RunStreamOutcome> = None;
        // Pending chunks buffered into the channel before `plugin_fut`
        // returned, in the order they were sent. Yielded ahead of the
        // streaming loop so consumers see every chunk the plugin emitted.
        let mut pending_chunks: Vec<Vec<u8>> = Vec::new();
        let first_chunk: Vec<u8> = tokio::select! {
            biased;
            res = &mut plugin_fut => {
                raw_rx.close();
                while let Ok(c) = raw_rx.try_recv() {
                    pending_chunks.push(c);
                }
                if pending_chunks.is_empty() {
                    return finalize_pre_stream(res);
                }
                plugin_outcome = Some(res);
                pending_chunks.remove(0)
            }
            v = raw_rx.recv() => match v {
                Some(chunk) => chunk,
                None => return finalize_pre_stream(plugin_fut.await),
            },
        };

        let st = async_stream::stream! {
            yield ResultOutputItem {
                item: Some(result_output_item::Item::Data(first_chunk)),
            };
            for chunk in pending_chunks {
                yield ResultOutputItem {
                    item: Some(result_output_item::Item::Data(chunk)),
                };
            }

            // Skip the streaming loop entirely when the plugin already
            // finished before we entered the stream — `raw_rx` is closed
            // and `pending_chunks` is already exhausted.
            if plugin_outcome.is_none() {
                loop {
                    let chunk_opt = match cancel_token.as_ref() {
                        Some(token) => tokio::select! {
                            v = raw_rx.recv() => v,
                            _ = token.cancelled() => {
                                tracing::info!("Plugin stream cancelled by token");
                                plugin_fut.abort();
                                None
                            }
                        },
                        None => raw_rx.recv().await,
                    };
                    match chunk_opt {
                        Some(chunk) => yield ResultOutputItem {
                            item: Some(result_output_item::Item::Data(chunk)),
                        },
                        None => break,
                    }
                }
            }

            let outcome = match plugin_outcome {
                Some(o) => o,
                None => plugin_fut.await,
            };
            let final_meta = match outcome {
                Ok(Ok(meta)) => meta,
                Ok(Err(e)) => {
                    // Mid-stream error: the consumer already received some
                    // data, so we cannot promote this to a hard Err on
                    // `run_stream` — log loudly and terminate with End.
                    tracing::error!("v2 plugin run_stream error after partial output: {}", e);
                    metadata_for_fallback
                }
                Err(join_err) if join_err.is_cancelled() => metadata_for_fallback,
                Err(join_err) => {
                    tracing::error!("v2 plugin task join error: {:?}", join_err);
                    metadata_for_fallback
                }
            };
            yield ResultOutputItem {
                item: Some(result_output_item::Item::End(Trailer {
                    metadata: final_meta,
                })),
            };
        }
        .boxed();
        Ok(st)
    }

    async fn create(&self, settings: Vec<u8>) -> Result<()> {
        // V2 plugins return an FfiFuture from load(); awaiting it directly on
        // the host runtime is correct (spawn_blocking would prevent the
        // future from being driven cooperatively).
        if self.variant_type == PluginVariantType::MultiMethodV2 {
            let fut = {
                let mut guard = self.variant.write().await;
                match &mut *guard {
                    super::PluginRunnerVariant::MultiMethodV2(plugin) => {
                        let settings_ffi = super::ffi::FfiBytes::from_vec(settings);
                        // SAFETY: state and vtable were validated at load time.
                        unsafe { (plugin.vtable.load)(plugin.state, settings_ffi) }
                    }
                    other => {
                        return Err(anyhow!(
                            "internal: variant_type cached as MultiMethodV2 but variant is {}",
                            variant_kind(other)
                        ));
                    }
                }
            };
            match fut.await {
                super::ffi::FfiResult::Ok(()) => return Ok(()),
                super::ffi::FfiResult::Err(err_bytes) => {
                    let msg = String::from_utf8_lossy(err_bytes.as_slice()).into_owned();
                    return Err(anyhow!("plugin load error: {}", msg));
                }
            }
        }

        let variant = Arc::clone(&self.variant);
        #[allow(unstable_name_collisions)]
        tokio::task::spawn_blocking(|| async move {
            let mut guard = variant.write().await;
            if let Some(p) = guard.as_multi_method_v1_mut() {
                p.load(settings)
            } else if let super::PluginRunnerVariant::Legacy(plugin) = &mut *guard {
                plugin.load(settings)
            } else {
                Err(anyhow!(
                    "internal: variant_type cached as V1/Legacy but variant is {}",
                    variant_kind(&guard)
                ))
            }
        })
        .await
        .map_err(|e| anyhow!("plugin runner lock error: {:?}", e))?
        .await?;
        Ok(())
    }
}

impl RunnerSpec for PluginRunnerWrapperImpl {
    fn name(&self) -> String {
        block_on(self.variant.read()).name()
    }
    fn runner_settings_proto(&self) -> String {
        block_on(self.variant.read()).runner_settings_proto()
    }
    fn method_proto_map(
        &self,
    ) -> std::collections::HashMap<String, proto::jobworkerp::data::MethodSchema> {
        block_on(self.variant.read()).method_proto_map()
    }

    fn method_json_schema_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodJsonSchema> {
        block_on(self.variant.read()).method_json_schema_map()
    }

    fn settings_schema(&self) -> String {
        block_on(self.variant.read()).settings_schema()
    }

    // V1/legacy plugins block on a synchronous run() while holding the variant
    // write lock, so on timeout the lock cannot be reclaimed — the wrapper
    // instance must be discarded.
    //
    // V2 plugins return an FfiFuture awaited by the wrapper; on timeout the
    // caller drops the future and the guard drops with it, freeing the lock
    // immediately. The instance can be safely reused.
    fn should_detach_on_timeout(&self) -> bool {
        block_on(self.variant.read()).should_detach_on_timeout()
    }

    /// Collect streaming plugin output into a single result.
    ///
    /// For MultiMethod plugins, delegates to the plugin's collect_stream.
    /// For Legacy plugins, keeps only the last data chunk (protobuf binary concatenation is invalid).
    ///
    /// Note: Uses cached variant_type to dispatch without locking.
    /// For MultiMethod, we get the plugin's collect_stream future while briefly
    /// holding the lock, then release the lock before awaiting. This allows the stream
    /// (which needs write lock internally) to be processed without deadlock.
    fn collect_stream(
        &self,
        stream: BoxStream<'static, ResultOutputItem>,
        using: Option<&str>,
    ) -> crate::runner::CollectStreamFuture {
        let variant = self.variant.clone();
        let variant_type = self.variant_type;
        // Forwarded to multi-method plugins for method-aware aggregation;
        // owned because `CollectStreamFuture` outlives the caller borrow.
        let using_owned: Option<String> = using.map(|s| s.to_string());

        Box::pin(async move {
            match variant_type {
                PluginVariantType::Legacy | PluginVariantType::MultiMethodV2 => {
                    // Legacy + V2: keep only the last chunk (or FinalCollected
                    // payload if the plugin provides one). V2 no longer exposes
                    // a per-plugin collect_stream — host applies the same
                    // last-data semantics that Legacy uses, which is what every
                    // existing V2 plugin (only cancel_test today) expects.
                    let mut last_data: Option<Vec<u8>> = None;
                    let mut metadata = HashMap::new();
                    let mut stream = stream;
                    let mut final_collected: Option<Vec<u8>> = None;

                    while let Some(item) = stream.next().await {
                        match item.item {
                            Some(result_output_item::Item::Data(data)) => {
                                last_data = Some(data);
                            }
                            Some(result_output_item::Item::End(trailer)) => {
                                metadata = trailer.metadata;
                                break;
                            }
                            Some(result_output_item::Item::FinalCollected(data)) => {
                                final_collected = Some(data);
                            }
                            None => {}
                        }
                    }

                    if let Some(data) = final_collected {
                        return Ok((data, metadata));
                    }

                    Ok((last_data.unwrap_or_default(), metadata))
                }
                PluginVariantType::MultiMethod => {
                    // Get the collect_stream future from plugin (brief lock).
                    let future = {
                        let guard = variant.read().await;
                        let using_ref = using_owned.as_deref();
                        match &*guard {
                            super::PluginRunnerVariant::MultiMethod(p) => {
                                Ok(p.collect_stream(stream, using_ref))
                            }
                            super::PluginRunnerVariant::Legacy(_)
                            | super::PluginRunnerVariant::MultiMethodV2(_) => Err(anyhow!(
                                "internal: variant_type cached as MultiMethod but variant differs"
                            )),
                        }
                    };
                    // Lock released, now await the future which processes the stream
                    future?.await
                }
            }
        })
    }
}
#[async_trait]
impl RunnerTrait for PluginRunnerWrapperImpl {
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        self.create(settings).await?;
        Ok(())
    }
    // arg: assumed as utf-8 string, specify multiple arguments with \n separated
    #[allow(unstable_name_collisions)]
    async fn run(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
        using: Option<&str>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        // 1. Get method map for legacy plugin detection
        let method_map = self.method_proto_map();

        // 2. Legacy plugin detection: single method named DEFAULT_METHOD_NAME
        let is_legacy =
            method_map.len() == 1 && method_map.contains_key(proto::DEFAULT_METHOD_NAME);

        // 3. Handle 'using' parameter
        if let Some(u) = using {
            if is_legacy {
                tracing::warn!(
                    "Plugin '{}' is legacy (single method '{}'), ignoring 'using' parameter: {}",
                    self.name(),
                    proto::DEFAULT_METHOD_NAME,
                    u
                );
            } else if !method_map.contains_key(u) {
                return (
                    Err(anyhow!(
                        "Unknown method '{}' for plugin '{}'. Available methods: {:?}",
                        u,
                        self.name(),
                        method_map.keys().collect::<Vec<_>>()
                    )),
                    metadata,
                );
            }
        }

        // 4. Execute plugin.
        //
        // V1/legacy: synchronous run() is offloaded to spawn_blocking with the
        // variant write lock held for the whole call. The lock cannot be
        // reclaimed on timeout (the blocking task keeps running), so the
        // wrapper instance must be detached afterwards.
        //
        // V2: run() returns an FfiFuture<...> awaited on the host runtime.
        // The write lock is held only for the duration of the await; on
        // timeout the caller drops the future, the guard drops with it, and
        // the lock is reclaimed immediately so the instance can be reused.
        if self.variant_type == PluginVariantType::MultiMethodV2 {
            // Drop the guard after obtaining the FfiFuture so concurrent
            // callers can grab the lock for unrelated bookkeeping while this
            // future runs. On timeout the caller drops the future and any
            // resources it captured are released; the wrapper instance is
            // reusable (see `should_detach_on_timeout`).
            let fut = {
                let mut guard = self.variant.write().await;
                match &mut *guard {
                    super::PluginRunnerVariant::MultiMethodV2(plugin) => {
                        let args_ffi = super::ffi::FfiBytes::from_vec(arg.to_vec());
                        let meta_ffi = meta_to_ffi(metadata.clone());
                        let using_ffi = using_to_ffi(using);
                        // SAFETY: state and vtable were validated at load time.
                        unsafe { (plugin.vtable.run)(plugin.state, args_ffi, meta_ffi, using_ffi) }
                    }
                    other => {
                        let kind = variant_kind(other);
                        return (
                            Err(anyhow!(
                                "internal: variant_type cached as MultiMethodV2 but variant is {kind}"
                            )),
                            metadata,
                        );
                    }
                }
            };
            let outcome = fut.await;
            let meta_back = meta_from_ffi(outcome.metadata);
            let result = match outcome.result {
                super::ffi::FfiResult::Ok(bytes) => Ok(bytes.into_vec()),
                super::ffi::FfiResult::Err(err_bytes) => {
                    let msg = String::from_utf8_lossy(err_bytes.as_slice()).into_owned();
                    tracing::warn!("in running pluginRunner (v2): {}", msg);
                    Err(anyhow!("plugin error: {}", msg))
                }
            };
            return (result, meta_back);
        }

        // V1/legacy path: synchronous FFI call on blocking pool.
        // The variant write lock is held for the whole run(); request_cancellation
        // uses try_read/try_write so it never blocks the timeout path on this lock.
        let variant = self.variant.clone();
        let arg1 = arg.to_vec();
        let using_owned: Option<String> = using.map(|s| s.to_string());
        // Kept for the JoinError (panic) path, where the blocking task returns nothing.
        let metadata_for_error = metadata.clone();

        let blocking_handle = tokio::task::spawn_blocking(move || {
            let mut guard = block_on(variant.write());
            if let Some(p) = guard.as_multi_method_v1_mut() {
                p.run(arg1, metadata, using_owned.as_deref())
            } else if let super::PluginRunnerVariant::Legacy(plugin) = &mut *guard {
                plugin.run(arg1, metadata)
            } else {
                let kind = variant_kind(&guard);
                (
                    Err(anyhow!(
                        "internal: variant_type cached as V1/Legacy but variant is {kind}"
                    )),
                    metadata,
                )
            }
        });

        match blocking_handle.await {
            Ok((r, meta)) => (
                r.map_err(|e| {
                    tracing::warn!("in running pluginRunner: {:?}", e);
                    e
                }),
                meta,
            ),
            Err(join_err) => {
                // Plugin panicked across the FFI boundary on the blocking thread.
                // The caller's catch_unwind cannot observe a panic on another
                // thread, so surface it here as a normal Err instead.
                tracing::error!("plugin run blocking task failed: {:?}", join_err);
                (
                    Err(anyhow!("plugin runner panicked: {:?}", join_err)),
                    metadata_for_error,
                )
            }
        }
    }

    async fn run_stream(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
        using: Option<&str>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        // 1. Get method map for legacy plugin detection
        let method_map = self.method_proto_map();

        // 2. Legacy plugin detection: single method named DEFAULT_METHOD_NAME
        let is_legacy =
            method_map.len() == 1 && method_map.contains_key(proto::DEFAULT_METHOD_NAME);

        // 3. Handle 'using' parameter
        if let Some(u) = using {
            if is_legacy {
                tracing::warn!(
                    "Plugin '{}' is legacy (single method '{}'), ignoring 'using' parameter: {}",
                    self.name(),
                    proto::DEFAULT_METHOD_NAME,
                    u
                );
            } else if !method_map.contains_key(u) {
                return Err(anyhow!(
                    "Unknown method '{}' for plugin '{}'. Available methods: {:?}",
                    u,
                    self.name(),
                    method_map.keys().collect::<Vec<_>>()
                ));
            }
        }

        // 4. Begin stream
        //
        // V2 plugins use a push-based `run_stream(output_sender)`: the host
        // creates an mpsc channel, hands the sender to the plugin, drains
        // the receiver directly inside the output stream, and emits the End
        // trailer with the metadata returned by the plugin's future.
        //
        // V1/legacy uses pull-based `begin_stream` + `receive_stream` with a
        // blocking pool task feeding an mpsc channel that the output stream
        // forwards.
        let cancel_token = if let Some(helper) = &self.cancel_helper {
            Some(helper.get_cancellation_token().await)
        } else {
            None
        };

        if self.variant_type == PluginVariantType::MultiMethodV2 {
            return self.run_stream_v2(arg, metadata, using, cancel_token).await;
        }

        // ----- V1/legacy below -----
        let variant = self.variant.clone();
        let arg1 = arg.to_vec();
        let (result_tx, mut result_rx) = mpsc::channel::<ResultOutputItem>(16);
        let metadata_for_blocking = metadata.clone();

        {
            let mut guard = variant.write().await;
            let result = if let Some(p) = guard.as_multi_method_v1_mut() {
                p.begin_stream(arg1, metadata.clone(), using)
            } else if let super::PluginRunnerVariant::Legacy(plugin) = &mut *guard {
                plugin.begin_stream(arg1, metadata.clone())
            } else {
                Err(anyhow!(
                    "internal: variant_type cached as V1/Legacy but variant is {}",
                    variant_kind(&guard)
                ))
            };
            result.map_err(|e| {
                tracing::warn!("in running pluginRunner: {:?}", e);
                anyhow!("in running pluginRunner: {:?}", e)
            })?;
        }
        let variant_clone = self.variant.clone();

        // Per-iteration lock acquisition so request_cancellation can call
        // plugin.cancel() between iterations. Lock is released before send()
        // so the consumer never blocks the sender.
        let blocking_handle = tokio::task::spawn_blocking(move || {
            loop {
                let (maybe_v, is_cancelled) = {
                    let mut guard = block_on(variant_clone.write());
                    let v = if let Some(p) = guard.as_multi_method_v1_mut() {
                        p.receive_stream()
                    } else if let super::PluginRunnerVariant::Legacy(plugin) = &mut *guard {
                        plugin.receive_stream()
                    } else {
                        // variant_type cached as V1/Legacy but the variant is
                        // something else: end the stream safely instead of
                        // panicking the worker process.
                        let kind = variant_kind(&guard);
                        Err(anyhow!(
                            "internal: variant_type cached as V1/Legacy but variant is {kind}"
                        ))
                    };
                    let c = if let Some(p) = guard.as_multi_method_v1() {
                        p.is_canceled()
                    } else if let super::PluginRunnerVariant::Legacy(plugin) = &*guard {
                        plugin.is_canceled()
                    } else {
                        // Cached/actual mismatch: treat as cancelled to terminate
                        // the loop quickly. The receive_stream Err above will
                        // also send the End marker.
                        true
                    };
                    (v, c)
                };

                match maybe_v {
                    Ok(Some(v)) => {
                        let item = ResultOutputItem {
                            item: Some(result_output_item::Item::Data(v)),
                        };
                        if block_on(result_tx.send(item)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => {
                        let _ = block_on(result_tx.send(ResultOutputItem {
                            item: Some(result_output_item::Item::End(Trailer {
                                metadata: metadata_for_blocking,
                            })),
                        }));
                        break;
                    }
                    Err(e) => {
                        tracing::warn!("Error occurred: {:}", e);
                        let _ = block_on(result_tx.send(ResultOutputItem {
                            item: Some(result_output_item::Item::End(Trailer {
                                metadata: metadata_for_blocking,
                            })),
                        }));
                        break;
                    }
                }
                if is_cancelled {
                    let _ = block_on(result_tx.send(ResultOutputItem {
                        item: Some(result_output_item::Item::End(Trailer {
                            metadata: metadata_for_blocking.clone(),
                        })),
                    }));
                    break;
                }
            }
        });

        let st = async_stream::stream! {
            loop {
                let item = if let Some(ref token) = cancel_token {
                    tokio::select! {
                        result = result_rx.recv() => result,
                        _ = token.cancelled() => {
                            tracing::info!("Plugin stream cancelled by token");
                            break;
                        }
                    }
                } else {
                    result_rx.recv().await
                };

                match item {
                    Some(output_item) => {
                        let is_end = matches!(
                            output_item.item,
                            Some(result_output_item::Item::End(_))
                        );
                        yield output_item;
                        if is_end {
                            break;
                        }
                    }
                    None => {
                        // Channel closed without End marker (abnormal termination)
                        yield ResultOutputItem {
                            item: Some(result_output_item::Item::End(Trailer { metadata })),
                        };
                        break;
                    }
                }
            }
            // Close receiver so any pending send() in spawn_blocking returns Err,
            // preventing deadlock when blocking_handle.await waits for task completion.
            result_rx.close();
            while result_rx.recv().await.is_some() {}
            match blocking_handle.await {
                Ok(()) => {}
                Err(e) => {
                    tracing::error!("Plugin blocking task failed: {:?}", e);
                }
            }
        }
        .boxed();
        Ok(st)
    }

    fn supports_client_stream(&self, using: Option<&str>) -> bool {
        block_on(self.variant.read()).supports_client_stream(using)
    }

    fn setup_client_stream_channel(
        &mut self,
        using: Option<&str>,
    ) -> Option<mpsc::Sender<FeedData>> {
        let mut guard = block_on(self.variant.write());
        match &mut *guard {
            super::PluginRunnerVariant::MultiMethod(p) => {
                let raw_tx = p.setup_client_stream_channel(using)?;
                let (feed_tx, mut feed_rx) = mpsc::channel::<FeedData>(32);
                tokio::spawn(async move {
                    while let Some(feed) = feed_rx.recv().await {
                        if raw_tx.send(feed.data).await.is_err() {
                            break;
                        }
                        if feed.is_final {
                            break;
                        }
                    }
                });
                Some(feed_tx)
            }
            super::PluginRunnerVariant::MultiMethodV2(plugin) => {
                let using_ffi = using_to_ffi(using);
                // SAFETY: state and vtable validated at load time. The vtable
                // returns FfiOption<OutputSink>; None means the plugin doesn't
                // support client streaming for the given method.
                let sink_opt =
                    unsafe { (plugin.vtable.setup_client_stream_channel)(plugin.state, using_ffi) };
                let sink = sink_opt.into_option()?;
                let (feed_tx, mut feed_rx) = mpsc::channel::<FeedData>(32);
                tokio::spawn(async move {
                    while let Some(feed) = feed_rx.recv().await {
                        let send_result = sink.send_raw(feed.data).await;
                        if matches!(send_result, super::ffi::FfiResult::Err(_)) {
                            break;
                        }
                        if feed.is_final {
                            break;
                        }
                    }
                    // `sink` drop closes the plugin's mpsc Receiver.
                });
                Some(feed_tx)
            }
            super::PluginRunnerVariant::Legacy(_) => None,
        }
    }
}

#[async_trait]
impl CancelMonitoring for PluginRunnerWrapperImpl {
    /// Initialize cancellation monitoring for specific job.
    ///
    /// For V2 plugins, additionally hand the cancellation token to the plugin
    /// here — BEFORE run() starts. run() later takes the variant write lock on
    /// a blocking thread and holds it for the entire FFI call; injecting the
    /// token after that point would either deadlock or require touching the
    /// plugin past the lock. Doing it during setup is the only window where the
    /// wrapper can safely push state into the plugin.
    async fn setup_cancellation_monitoring(
        &mut self,
        job_id: JobId,
        job_data: &JobData,
    ) -> Result<Option<JobResult>> {
        if let Some(helper) = &mut self.cancel_helper {
            let result = helper.setup_monitoring_impl(job_id, job_data).await?;
            if self.variant_type == PluginVariantType::MultiMethodV2 {
                let token = helper.get_cancellation_token().await;
                // run() has not started yet, so this write lock does not race
                // with the blocking task's lock.
                self.variant.write().await.set_cancellation_token(token);
            }
            Ok(result)
        } else {
            tracing::debug!("No cancel monitoring configured for job {}", job_id.value);
            Ok(None)
        }
    }

    /// Cleanup cancellation monitoring
    async fn cleanup_cancellation_monitoring(&mut self) -> Result<()> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.cleanup_monitoring_impl().await
        } else {
            Ok(())
        }
    }

    /// Signals cancellation token for PluginRunnerWrapperImpl
    async fn request_cancellation(&mut self) -> Result<()> {
        // 1. Signal cancellation token
        if let Some(helper) = &self.cancel_helper {
            let token = helper.get_cancellation_token().await;
            if !token.is_cancelled() {
                token.cancel();
                tracing::info!("PluginRunnerWrapperImpl: cancellation token signaled");
            }
        } else {
            tracing::warn!("PluginRunnerWrapperImpl: no cancellation helper available");
        }

        // 2. Call plugin's cancel() for plugin-specific cleanup, best-effort.
        // Legacy plugins use &self, MultiMethod plugins use &mut self.
        //
        // run() (and run_stream) offload the synchronous plugin call to a blocking
        // thread while holding the variant write lock for its whole duration, so the
        // lock needed here may be held. We must NOT await it: request_cancellation is
        // called from the timeout branch of the job runner, and blocking on the lock
        // would defer the timeout result until the plugin call finishes, defeating
        // the timeout. Use try_read/try_write so a busy plugin is left untouched and
        // we return immediately. The cancellation token signaled in step 1 already
        // covers token-observing paths (run_stream); a synchronous run() is
        // uninterruptible anyway, so skipping cancel() here loses nothing.
        // V2 has no plugin.cancel() — the token signaled above is the only
        // cancellation channel, so there is nothing else to do here.
        if self.variant_type == PluginVariantType::MultiMethodV2 {
            return Ok(());
        }

        const BUSY_MSG: &str = "PluginRunnerWrapperImpl: variant lock held (plugin call in progress), skipping cancel()";
        let cancelled = match self.variant_type {
            PluginVariantType::Legacy => match self.variant.try_read() {
                Ok(guard) => {
                    if let super::PluginRunnerVariant::Legacy(plugin) = &*guard {
                        plugin.cancel()
                    } else {
                        false
                    }
                }
                Err(_) => {
                    tracing::debug!("{}", BUSY_MSG);
                    false
                }
            },
            PluginVariantType::MultiMethod => match self.variant.try_write() {
                Ok(mut guard) => {
                    if let Some(p) = guard.as_multi_method_v1_mut() {
                        p.cancel()
                    } else {
                        false
                    }
                }
                Err(_) => {
                    tracing::debug!("{}", BUSY_MSG);
                    false
                }
            },
            // V2 returns early at the top of this function before this match.
            // Should never reach here in practice; return false as a safe
            // no-op if it ever does (e.g., after future refactors).
            PluginVariantType::MultiMethodV2 => false,
        };
        if cancelled {
            tracing::info!("PluginRunnerWrapperImpl: plugin cancelled successfully");
        }

        Ok(())
    }

    /// Reset for pooling
    async fn reset_for_pooling(&mut self) -> Result<()> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.reset_for_pooling_impl().await
        } else {
            Ok(())
        }
    }
}

impl UseCancelMonitoringHelper for PluginRunnerWrapperImpl {
    fn cancel_monitoring_helper(&self) -> Option<&CancelMonitoringHelper> {
        self.cancel_helper.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::super::MultiMethodPluginRunner;
    use super::*;

    /// Minimal MultiMethodPluginRunner mock whose run() is configurable, used to
    /// exercise the spawn_blocking path of PluginRunnerWrapperImpl::run() without
    /// loading a real .so (avoids FFI unwinding concerns in the panic test).
    struct MockPlugin {
        // Behavior selector for run(): "panic", "sleep", or "ok".
        behavior: &'static str,
    }

    impl MultiMethodPluginRunner for MockPlugin {
        fn name(&self) -> String {
            "MockPlugin".to_string()
        }
        fn description(&self) -> String {
            String::new()
        }
        fn load(&mut self, _settings: Vec<u8>) -> Result<()> {
            Ok(())
        }
        fn run(
            &mut self,
            _args: Vec<u8>,
            metadata: HashMap<String, String>,
            _using: Option<&str>,
        ) -> (Result<Vec<u8>>, HashMap<String, String>) {
            match self.behavior {
                "panic" => panic!("intentional test panic"),
                "sleep" => {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    (Ok(b"slept".to_vec()), metadata)
                }
                _ => (Ok(b"ok".to_vec()), metadata),
            }
        }
        fn cancel(&mut self) -> bool {
            false
        }
        fn is_canceled(&self) -> bool {
            false
        }
        fn runner_settings_proto(&self) -> String {
            String::new()
        }
        fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
            // A single DEFAULT_METHOD_NAME entry makes the wrapper treat this as a
            // legacy-style single-method plugin, so the 'using' validation in run()
            // is bypassed and execution reaches the MultiMethod branch directly.
            HashMap::from([(
                proto::DEFAULT_METHOD_NAME.to_string(),
                proto::jobworkerp::data::MethodSchema::default(),
            )])
        }
    }

    fn wrapper_with(behavior: &'static str) -> PluginRunnerWrapperImpl {
        let variant = PluginRunnerVariant::MultiMethod(Box::new(MockPlugin { behavior }));
        PluginRunnerWrapperImpl::new(Arc::new(RwLock::new(variant)))
    }

    #[tokio::test]
    async fn run_panic_returns_err_and_preserves_metadata() {
        let mut w = wrapper_with("panic");
        let input_meta = HashMap::from([("k".to_string(), "v".to_string())]);
        let (r, meta) = w.run(b"x", input_meta.clone(), None).await;
        assert!(r.is_err(), "panic in plugin run() must surface as Err");
        // metadata_for_error clone must be returned on the JoinError path.
        assert_eq!(meta, input_meta);
    }

    #[tokio::test]
    async fn run_multimethod_returns_result() {
        let mut w = wrapper_with("ok");
        let input_meta = HashMap::from([("k".to_string(), "v".to_string())]);
        let (r, meta) = w.run(b"x", input_meta.clone(), None).await;
        assert_eq!(r.unwrap(), b"ok".to_vec());
        assert_eq!(meta, input_meta);
    }

    // Plugin runners hold the variant lock on a blocking thread past a timeout, so
    // they must be discarded from the pool rather than reused.
    #[tokio::test]
    async fn plugin_should_detach_on_timeout() {
        let w = wrapper_with("ok");
        assert!(w.should_detach_on_timeout());
    }

    // Verifies request_cancellation() does not block on the variant lock while a
    // synchronous run() holds it on the blocking pool. Without try_read/try_write,
    // the MultiMethod write-lock acquisition here would wait until run() finishes,
    // deferring the timeout path. We assert request_cancellation returns well before
    // the 500ms run() completes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn request_cancellation_does_not_block_on_running_plugin() {
        let mut w = wrapper_with("sleep");

        // Spawn run() on a separate task so it holds the variant write lock while
        // sleeping ~500ms on the blocking pool.
        let mut w_run = w.clone();
        let run_handle = tokio::spawn(async move { w_run.run(b"x", HashMap::new(), None).await });

        // Give run() time to acquire the write lock inside spawn_blocking.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let start = std::time::Instant::now();
        w.request_cancellation().await.unwrap();
        let elapsed = start.elapsed();

        // If try_write blocked on the held lock, this would take ~450ms (until run
        // finishes). It must return promptly instead.
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "request_cancellation blocked on the plugin lock: {elapsed:?}"
        );

        let (r, _meta) = run_handle.await.unwrap();
        assert!(r.is_ok());
    }

    // Verifies the synchronous plugin run() runs on the blocking pool rather than
    // a tokio worker: with worker_threads=1, a concurrent lightweight task must
    // still make progress while run() sleeps. The old inline implementation would
    // starve the concurrent task on a single-worker runtime.
    // Ignored by default because it is timing-dependent.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    #[ignore]
    async fn run_does_not_block_worker() {
        use std::sync::Arc as StdArc;
        use std::sync::atomic::{AtomicU64, Ordering};

        let counter = StdArc::new(AtomicU64::new(0));
        let counter_clone = counter.clone();
        let ticker = tokio::spawn(async move {
            for _ in 0..40 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                counter_clone.fetch_add(1, Ordering::SeqCst);
            }
        });

        let mut w = wrapper_with("sleep");
        let (r, _meta) = w.run(b"x", HashMap::new(), None).await;
        assert!(r.is_ok());

        // While run() slept ~500ms on the blocking pool, the ticker on the single
        // tokio worker should have advanced. If run() had blocked the worker, the
        // counter would still be 0.
        assert!(
            counter.load(Ordering::SeqCst) > 0,
            "concurrent task was starved; plugin run() likely blocked the tokio worker"
        );
        ticker.abort();
    }
}
