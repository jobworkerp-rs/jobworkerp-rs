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

/// Try to extract a `JoinHandle`'s output without awaiting. Returns `Some`
/// only if the handle is already finished, so `run_stream_v2` can peek at
/// a synchronously-returned plugin outcome (e.g. an immediate `Err`) and
/// turn it into an outer `Err(...)` before handing the stream back.
fn pending_now<T>(
    handle: &mut tokio::task::JoinHandle<T>,
) -> Option<Result<T, tokio::task::JoinError>> {
    use std::future::Future as _;
    use std::pin::Pin;
    use std::task::Context;
    let waker = futures::task::noop_waker_ref();
    let mut cx = Context::from_waker(waker);
    match Pin::new(handle).poll(&mut cx) {
        std::task::Poll::Ready(r) => Some(r),
        std::task::Poll::Pending => None,
    }
}

/// Metadata key used to surface a V2 plugin `run_stream` error to callers
/// when the host has already returned its `BoxStream` and can no longer
/// fail the outer call. Downstream consumers (e.g. `run_and_stream` /
/// `collect_stream`) inspect the End trailer for this key to decide whether
/// the job succeeded or failed.
///
/// Exposed crate-publicly so out-of-crate callers (notably
/// `worker_app::worker::runner` building synthetic End trailers for
/// idle-timeout terminations) can plant the same key — `collect_stream`
/// here treats any matching value as a failure regardless of source.
pub const V2_STREAM_ERROR_META_KEY: &str = "jobworkerp.plugin.run_stream_error";

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
    /// runtime and return a `BoxStream` **immediately**, before the plugin
    /// produces any chunk. The first-chunk wait happens inside the returned
    /// stream so client-streaming callers (`EnqueueWithClientStream`) can
    /// receive their gRPC response and start forwarding feed data even when
    /// the plugin only emits output after consuming feed.
    ///
    /// Errors that prevent the stream from starting (e.g. an internal
    /// `variant_type` drift) are still returned as `Err(...)` from this
    /// function — they happen before `plugin_fut` is spawned. Errors that
    /// surface from the plugin itself (the default `Err("not implemented")`
    /// impl, mid-stream Err) are recorded in the End trailer metadata under
    /// `V2_STREAM_ERROR_META_KEY` so the job can be marked failed by
    /// `run_and_stream` / `collect_stream` after the stream is drained.
    /// This is a semantic change vs. earlier revisions that returned
    /// pre-stream Err as an outer `Err(...)`: returning Err once the stream
    /// has been handed back to gRPC is no longer possible without
    /// dropping the consumer's prefix.
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
        // One clone of the caller's metadata: the FFI thunk consumes the
        // original via `meta_to_ffi(metadata)` below, while the eager-Err
        // / fallback paths need to surface the metadata back to the
        // consumer. The two paths are mutually exclusive (eager-Err
        // returns before the stream is built), so a single owned copy
        // covers both.
        let metadata_fallback = metadata.clone();
        let mut plugin_fut = tokio::spawn(async move {
            // Hold the write guard for the lifetime of the FfiFuture: the
            // proc-macro-generated thunk captures `&mut PluginTy` across
            // the await, so dropping the guard before the future resolves
            // would let a concurrent `run`/`run_stream` create a second
            // `&mut` to the same plugin state — aliasing UB.
            let mut guard = variant_clone.write().await;
            let fut = match &mut *guard {
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
            };
            // Decode FFI types to plain HashMap/String for downstream code.
            let outcome = match fut.await {
                super::ffi::FfiResult::Ok(meta_ffi) => Ok(meta_from_ffi(meta_ffi)),
                super::ffi::FfiResult::Err(err_bytes) => Err(err_bytes.into_string_lossy()),
            };
            drop(guard);
            outcome
        });

        // Eager pre-stream Err detection: yield to the runtime several
        // times so the spawned `plugin_fut` has a chance to advance through
        // the FFI thunk (which itself awaits an `FfiFuture`). If the plugin
        // returned `Err(...)` synchronously (e.g. the default
        // `Err("not implemented")` impl, or a parameter-validation Err that
        // touches no real I/O) it will be Ready by then, and we can surface
        // the outer `Err(...)` exactly like the pre-rewrite path did —
        // letting `worker-app::run_and_stream` turn it into a failed
        // JobResult instead of a Success with the error hidden in End
        // trailer metadata. Plugins that genuinely wait for I/O before
        // returning will not be Ready yet; they fall through to the stream
        // body where their error lands in the End trailer (and the
        // collect_stream path picks it up via `V2_STREAM_ERROR_META_KEY`).
        //
        // The eager peek is best-effort. A few yields cover the FFI thunk
        // plus the `variant.write().await` lock acquisition on an
        // uncontended wrapper; deeper waits are by design not detected
        // here so we never block the caller on plugin work.
        const EAGER_ERR_YIELD_ROUNDS: usize = 8;
        let mut eager_outcome: Option<V2RunStreamOutcome> = None;
        for _ in 0..EAGER_ERR_YIELD_ROUNDS {
            tokio::task::yield_now().await;
            if let Some(res) = pending_now(&mut plugin_fut) {
                eager_outcome = Some(res);
                break;
            }
        }
        if let Some(res) = eager_outcome {
            // The plugin already finished. Drain any chunks it managed to
            // queue synchronously so we do not silently drop output.
            raw_rx.close();
            let mut pending_chunks: Vec<Vec<u8>> = Vec::new();
            while let Ok(c) = raw_rx.try_recv() {
                pending_chunks.push(c);
            }
            match res {
                Ok(Ok(meta)) if pending_chunks.is_empty() => {
                    // Pre-stream Ok with no chunks: empty success stream.
                    let end = ResultOutputItem {
                        item: Some(result_output_item::Item::End(Trailer { metadata: meta })),
                    };
                    return Ok(futures::stream::iter(std::iter::once(end)).boxed());
                }
                Ok(Err(e)) if pending_chunks.is_empty() => {
                    // Pre-stream Err with no chunks: surface as outer Err
                    // so `run_and_stream`'s status handling marks the job
                    // failed instead of returning Success with the error
                    // hidden in metadata.
                    return Err(anyhow!("v2 plugin run_stream error: {e}"));
                }
                Err(join_err) if join_err.is_cancelled() && pending_chunks.is_empty() => {
                    return Err(anyhow!("v2 plugin run_stream was cancelled"));
                }
                Err(join_err) if pending_chunks.is_empty() => {
                    return Err(anyhow!("v2 plugin task join error: {join_err:?}"));
                }
                other => {
                    // Plugin emitted at least one chunk before finishing.
                    // Hand back a stream that yields the buffered chunks
                    // followed by the End trailer (error, if any, lands in
                    // metadata) — the consumer has already started reading.
                    let final_meta = match other {
                        Ok(Ok(meta)) => meta,
                        Ok(Err(e)) => {
                            tracing::error!("v2 plugin run_stream error after partial output: {e}");
                            let mut m = metadata_fallback;
                            m.insert(V2_STREAM_ERROR_META_KEY.to_string(), e);
                            m
                        }
                        Err(join_err) if join_err.is_cancelled() => metadata_fallback,
                        Err(join_err) => {
                            tracing::error!("v2 plugin task join error: {join_err:?}");
                            let mut m = metadata_fallback;
                            m.insert(
                                V2_STREAM_ERROR_META_KEY.to_string(),
                                format!("plugin task join error: {join_err:?}"),
                            );
                            m
                        }
                    };
                    let items = pending_chunks
                        .into_iter()
                        .map(|c| ResultOutputItem {
                            item: Some(result_output_item::Item::Data(c)),
                        })
                        .chain(std::iter::once(ResultOutputItem {
                            item: Some(result_output_item::Item::End(Trailer {
                                metadata: final_meta,
                            })),
                        }));
                    return Ok(futures::stream::iter(items).boxed());
                }
            }
        }

        let st = async_stream::stream! {
            // Race the first chunk against `plugin_fut`:
            //
            // - `plugin_fut` completes first: the plugin may have buffered
            //   chunks into the channel before exiting (e.g. a short
            //   synchronous `send(chunk).await; Ok(meta)`). Close the
            //   receiver to stop accepting new sends, drain everything
            //   already queued, then yield those chunks before the End
            //   trailer. Without the drain a biased select! would silently
            //   lose data.
            // - `raw_rx.recv()` yields a chunk: yield it and fall through
            //   to the streaming loop. `plugin_fut` resolves later for the
            //   End trailer.
            // - `raw_rx.recv()` yields `None`: the sender was dropped
            //   before any chunk; await `plugin_fut` for the outcome.
            let mut plugin_outcome: Option<V2RunStreamOutcome> = None;
            let mut entered_streaming_loop = false;
            tokio::select! {
                biased;
                res = &mut plugin_fut => {
                    raw_rx.close();
                    while let Ok(c) = raw_rx.try_recv() {
                        yield ResultOutputItem {
                            item: Some(result_output_item::Item::Data(c)),
                        };
                    }
                    plugin_outcome = Some(res);
                }
                v = raw_rx.recv() => match v {
                    Some(chunk) => {
                        yield ResultOutputItem {
                            item: Some(result_output_item::Item::Data(chunk)),
                        };
                        entered_streaming_loop = true;
                    }
                    None => {
                        // Awaiting `&mut plugin_fut` keeps ownership in the
                        // outer binding, leaving the streaming loop's
                        // `plugin_fut.abort()` reachable in other branches.
                        plugin_outcome = Some((&mut plugin_fut).await);
                    }
                }
            }

            // Streaming loop: only entered if a real chunk arrived first.
            // When `plugin_fut` won the race we already drained `raw_rx`
            // and closed it, so re-entering here would block forever.
            if entered_streaming_loop {
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
                    // Plugin returned Err. We can no longer fail the outer
                    // `run_stream` call (the BoxStream was already handed
                    // back), so record the error in the End trailer
                    // metadata and let callers decide. Covers both
                    // pre-stream and mid-stream errors — they look the
                    // same from the consumer's point of view.
                    tracing::error!("v2 plugin run_stream error: {}", e);
                    let mut meta = metadata_fallback;
                    meta.insert(V2_STREAM_ERROR_META_KEY.to_string(), e);
                    meta
                }
                Err(join_err) if join_err.is_cancelled() => metadata_fallback,
                Err(join_err) => {
                    tracing::error!("v2 plugin task join error: {:?}", join_err);
                    let mut meta = metadata_fallback;
                    meta.insert(
                        V2_STREAM_ERROR_META_KEY.to_string(),
                        format!("plugin task join error: {join_err:?}"),
                    );
                    meta
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
            // Hold the write guard across the FfiFuture await: the
            // proc-macro-generated thunk captures `&mut PluginTy` across
            // the await, so dropping the guard mid-load would let a
            // concurrent `run`/`run_stream`/`load` on a wrapper clone
            // construct a second `&mut` to the same plugin state — the
            // same aliasing UB the run paths guard against.
            let mut guard = self.variant.write().await;
            let fut = match &mut *guard {
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
            };
            let outcome = fut.await;
            drop(guard);
            match outcome {
                super::ffi::FfiResult::Ok(()) => return Ok(()),
                super::ffi::FfiResult::Err(err_bytes) => {
                    return Err(anyhow!(
                        "plugin load error: {}",
                        err_bytes.into_string_lossy()
                    ));
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

                    // V2 plugins can no longer surface a pre-stream error
                    // through `run_stream`'s outer `Err` (the BoxStream is
                    // handed back before the plugin runs). Instead, the
                    // host injects the error message into the End trailer
                    // metadata; failing the collect here keeps the job's
                    // failure semantics consistent with the pre-rewrite
                    // behaviour.
                    if let Some(err) = metadata.remove(V2_STREAM_ERROR_META_KEY) {
                        return Err(anyhow!("v2 plugin run_stream error: {err}"));
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
            // Hold the write guard across the FfiFuture await: the
            // proc-macro-generated thunk captures `&mut PluginTy` across
            // the await, so dropping the guard mid-flight would let a
            // concurrent `run`/`run_stream` on the same wrapper (clones
            // share the Arc<RwLock>) create a second `&mut` to the same
            // plugin state — aliasing UB. Dropping the future on timeout
            // still releases the guard immediately, so wrappers stay
            // reusable (`should_detach_on_timeout = false`).
            let mut guard = self.variant.write().await;
            let fut = match &mut *guard {
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
            };
            let outcome = fut.await;
            drop(guard);
            let meta_back = meta_from_ffi(outcome.metadata);
            let result = match outcome.result {
                // V2 plugin returns plugin-allocator bytes; copy through
                // the host allocator before releasing the FfiBytes via its
                // embedded drop_fn (see `FfiBytes::copy_to_vec` docs).
                super::ffi::FfiResult::Ok(bytes) => Ok(bytes.copy_to_vec()),
                super::ffi::FfiResult::Err(err_bytes) => {
                    let msg = err_bytes.into_string_lossy();
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
                // Prefer the minor-1 `setup_client_stream_channel_v2` slot
                // when the plugin advertises it via `vtable_size`. That
                // sink carries `is_final` in-band so the plugin can return
                // from `run_stream` deterministically once it sees the
                // final feed, instead of having to infer EOF from sink
                // drop alone. Older plugins (or plugins that opt out by
                // returning None from the v2 setup) fall through to the
                // original sink + sink-drop EOF behaviour.
                let using_ffi = using_to_ffi(using);
                if plugin.supports_setup_client_stream_channel_v2() {
                    // SAFETY: vtable validated at load time; the v2 slot
                    // is populated when supports_setup_client_stream_channel_v2()
                    // is true. The fn pointer signature is enforced by
                    // PluginVtable so the call site matches the thunk.
                    let sink_opt = unsafe {
                        (plugin.vtable.setup_client_stream_channel_v2)(plugin.state, using_ffi)
                    };
                    if let Some(sink) = sink_opt.into_option() {
                        let (feed_tx, mut feed_rx) = mpsc::channel::<FeedData>(32);
                        tokio::spawn(async move {
                            while let Some(feed) = feed_rx.recv().await {
                                let is_final = feed.is_final;
                                let send_result =
                                    sink.send_raw_with_final(feed.data, is_final).await;
                                if matches!(send_result, super::ffi::FfiResult::Err(_)) {
                                    break;
                                }
                                if is_final {
                                    break;
                                }
                            }
                            // `sink` drop still closes the plugin's
                            // mpsc Receiver; sending `is_final=true`
                            // beforehand lets the plugin choose its own
                            // termination order without depending on
                            // the drop signal.
                        });
                        return Some(feed_tx);
                    }
                    // v2 slot returned None — the plugin opted out of the
                    // is_final-aware variant for this `using` value.
                    // Re-encode `using` because `using_ffi` was consumed.
                }

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
