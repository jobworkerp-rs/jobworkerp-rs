use crate::runner::FeedData;
use crate::runner::RunnerSpec;
use crate::runner::RunnerTrait;
use std::collections::HashMap;
use std::sync::Arc;

use super::PluginRunnerVariant;
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
            }
        };
        Self {
            variant,
            variant_type,
            cancel_helper: None,
        }
    }
    async fn create(&self, settings: Vec<u8>) -> Result<()> {
        let variant = Arc::clone(&self.variant);
        #[allow(unstable_name_collisions)]
        tokio::task::spawn_blocking(|| async move {
            let mut guard = variant.write().await;
            match &mut *guard {
                super::PluginRunnerVariant::Legacy(plugin) => plugin.load(settings),
                super::PluginRunnerVariant::MultiMethod(plugin) => plugin.load(settings),
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
        let guard = block_on(self.variant.read());
        match &*guard {
            super::PluginRunnerVariant::Legacy(plugin) => plugin.name(),
            super::PluginRunnerVariant::MultiMethod(plugin) => plugin.name(),
        }
    }
    fn runner_settings_proto(&self) -> String {
        let guard = block_on(self.variant.read());
        match &*guard {
            super::PluginRunnerVariant::Legacy(plugin) => plugin.runner_settings_proto(),
            super::PluginRunnerVariant::MultiMethod(plugin) => plugin.runner_settings_proto(),
        }
    }
    fn method_proto_map(
        &self,
    ) -> std::collections::HashMap<String, proto::jobworkerp::data::MethodSchema> {
        let guard = block_on(self.variant.read());
        match &*guard {
            super::PluginRunnerVariant::Legacy(plugin) => {
                // Auto-convert from legacy methods
                let mut map = HashMap::new();
                map.insert(
                    proto::DEFAULT_METHOD_NAME.to_string(),
                    proto::jobworkerp::data::MethodSchema {
                        args_proto: plugin.job_args_proto(),
                        result_proto: plugin.result_output_proto().unwrap_or_default(),
                        description: Some(plugin.description()),
                        output_type: plugin.output_type() as i32,
                        ..Default::default()
                    },
                );
                tracing::debug!(
                    "Auto-converted legacy plugin '{}' to method_proto_map with '{}' method",
                    plugin.name(),
                    proto::DEFAULT_METHOD_NAME
                );
                map
            }
            super::PluginRunnerVariant::MultiMethod(plugin) => plugin.method_proto_map(),
        }
    }

    fn method_json_schema_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodJsonSchema> {
        let guard = block_on(self.variant.read());
        match &*guard {
            super::PluginRunnerVariant::Legacy(plugin) => {
                // This is a legacy plugin - use arguments_schema() and output_json_schema()
                let mut map = HashMap::new();
                map.insert(
                    proto::DEFAULT_METHOD_NAME.to_string(),
                    proto::jobworkerp::data::MethodJsonSchema {
                        args_schema: plugin.arguments_schema(),
                        result_schema: plugin.output_json_schema(),
                        client_stream_data_schema: None,
                    },
                );
                tracing::debug!(
                    "Auto-converted legacy plugin '{}' JSON schemas with '{}' method",
                    plugin.name(),
                    proto::DEFAULT_METHOD_NAME
                );
                map
            }
            super::PluginRunnerVariant::MultiMethod(plugin) => {
                if let Some(custom_schemas) = plugin.method_json_schema_map() {
                    custom_schemas
                } else {
                    // Fall back to automatic Protobuf→JSON Schema conversion
                    proto::jobworkerp::data::MethodJsonSchema::from_proto_map(
                        self.method_proto_map(),
                    )
                }
            }
        }
    }

    fn settings_schema(&self) -> String {
        let guard = block_on(self.variant.read());
        match &*guard {
            super::PluginRunnerVariant::Legacy(plugin) => plugin.settings_schema(),
            super::PluginRunnerVariant::MultiMethod(plugin) => plugin.settings_schema(),
        }
    }

    // run() executes the synchronous plugin call on a blocking thread holding the
    // variant write lock; a timeout cannot stop it, so the lock stays held. Reusing
    // this instance would block the next job — discard it instead.
    fn should_detach_on_timeout(&self) -> bool {
        true
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
        _using: Option<&str>,
    ) -> crate::runner::CollectStreamFuture {
        let variant = self.variant.clone();
        let variant_type = self.variant_type;

        Box::pin(async move {
            match variant_type {
                PluginVariantType::Legacy => {
                    // Legacy plugins: keep only the last chunk (no custom collect_stream)
                    // Process stream without holding any lock
                    let mut last_data: Option<Vec<u8>> = None;
                    let mut metadata = HashMap::new();
                    let mut stream = stream;
                    // Store FinalCollected data if received, to return after End(trailer)
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
                                // Store FinalCollected data and continue loop to capture End(trailer) metadata
                                final_collected = Some(data);
                            }
                            None => {}
                        }
                    }

                    // If FinalCollected was received, return it with collected metadata
                    if let Some(data) = final_collected {
                        return Ok((data, metadata));
                    }

                    Ok((last_data.unwrap_or_default(), metadata))
                }
                PluginVariantType::MultiMethod => {
                    // Get the collect_stream future from plugin (brief lock)
                    let future = {
                        let guard = variant.read().await;
                        if let super::PluginRunnerVariant::MultiMethod(plugin) = &*guard {
                            // Pass None for using since we don't have access to it here
                            // The plugin's collect_stream should handle this appropriately
                            plugin.collect_stream(stream, None)
                        } else {
                            unreachable!("variant_type mismatch")
                        }
                    };
                    // Lock released, now await the future which processes the stream
                    future.await
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

        // 4. Execute plugin on a blocking thread.
        // The plugin run() is a synchronous FFI call that may block for a long
        // time (CPU-bound work or blocking I/O). Running it on a tokio worker
        // would stall every other task sharing that worker, so move it onto the
        // blocking pool. This mirrors run_stream(), which already offloads the
        // synchronous receive loop. Uses futures::executor::block_on for the
        // lock acquisition (no tokio runtime re-entry risk).
        //
        // Cancellation note: the synchronous run() cannot be interrupted once
        // started. On timeout the caller drops this future, which drops the
        // JoinHandle; per tokio semantics that does NOT cancel the blocking task,
        // so the plugin runs to completion on the blocking thread and its result
        // is discarded. This is intentional and no worse than the previous
        // inline call (which also ran to completion, but blocked a tokio worker).
        // The variant write lock is held for the whole run(); request_cancellation
        // uses try_read/try_write so it never blocks the timeout path on this lock.
        let variant = self.variant.clone();
        let arg1 = arg.to_vec();
        let using_owned: Option<String> = using.map(|s| s.to_string());
        // Kept for the JoinError (panic) path, where the blocking task returns nothing.
        let metadata_for_error = metadata.clone();

        let blocking_handle = tokio::task::spawn_blocking(move || {
            let mut guard = block_on(variant.write());
            match &mut *guard {
                super::PluginRunnerVariant::Legacy(plugin) => plugin.run(arg1, metadata),
                super::PluginRunnerVariant::MultiMethod(plugin) => {
                    plugin.run(arg1, metadata, using_owned.as_deref())
                }
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
        let variant = self.variant.clone();
        let arg1 = arg.to_vec();
        {
            let mut guard = variant.write().await;
            // begin stream (set argument and setup stream)
            let result = match &mut *guard {
                super::PluginRunnerVariant::Legacy(plugin) => {
                    plugin.begin_stream(arg1, metadata.clone())
                }
                super::PluginRunnerVariant::MultiMethod(plugin) => {
                    plugin.begin_stream(arg1, metadata.clone(), using)
                }
            };
            result.map_err(|e| {
                tracing::warn!("in running pluginRunner: {:?}", e);
                anyhow!("in running pluginRunner: {:?}", e)
            })?;
        }
        let variant_clone = self.variant.clone();

        let cancel_token = if let Some(helper) = &self.cancel_helper {
            Some(helper.get_cancellation_token().await)
        } else {
            None
        };

        // Run the entire receive loop in a single spawn_blocking task to minimize
        // latency between receive_stream() calls. Each per-iteration spawn_blocking
        // added ~30ms overhead (thread scheduling + RwLock acquisition), causing feed
        // data to accumulate in the plugin's internal buffer and multiple chunks to be
        // processed in a single receive_stream() call. This broke streaming plugins
        // (e.g. WhisperPlugin) that expect 1 chunk per call for correct incremental output.
        //
        // Cancellation constraint: if receive_stream() blocks for a long time, cancel
        // latency equals that blocking duration. Current plugins (Hello, Whisper) use
        // rt.block_on(rx.recv()) which returns promptly when the feed channel closes.
        // Plugin implementations MUST ensure receive_stream() does not block indefinitely
        // (see PluginRunner::receive_stream() doc for the contract).
        //
        // Uses futures::executor::block_on (lightweight, no tokio runtime re-entry risk)
        // rather than tokio's Handle::block_on, so there is no thread starvation concern
        // from runtime re-entry. spawn_blocking thread pool saturation is bounded by
        // the number of active plugin instances (typically 1-3).
        let (result_tx, mut result_rx) = mpsc::channel::<ResultOutputItem>(16);
        // Clone metadata for the blocking task (End marker trailer).
        // The original `metadata` is kept for the async stream's abnormal-termination fallback.
        let metadata_for_blocking = metadata.clone();

        let blocking_handle = tokio::task::spawn_blocking(move || {
            loop {
                // Acquire lock per iteration so request_cancellation can call plugin.cancel()
                // between iterations. Lock is released before send() to avoid blocking the sender.
                let (maybe_v, is_cancelled) = {
                    let mut guard = block_on(variant_clone.write());
                    let v = match &mut *guard {
                        super::PluginRunnerVariant::Legacy(plugin) => plugin.receive_stream(),
                        super::PluginRunnerVariant::MultiMethod(plugin) => plugin.receive_stream(),
                    };
                    let c = match &*guard {
                        super::PluginRunnerVariant::Legacy(plugin) => plugin.is_canceled(),
                        super::PluginRunnerVariant::MultiMethod(plugin) => plugin.is_canceled(),
                    };
                    (v, c)
                    // guard dropped here
                };

                match maybe_v {
                    Ok(Some(v)) => {
                        let item = ResultOutputItem {
                            item: Some(result_output_item::Item::Data(v)),
                        };
                        if block_on(result_tx.send(item)).is_err() {
                            // Receiver dropped (consumer cancelled)
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
                    // Send End marker before breaking to avoid relying on async stream fallback
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
            // Await the blocking task to detect panics (e.g. from FFI boundary)
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
        let guard = block_on(self.variant.read());
        match &*guard {
            PluginRunnerVariant::Legacy(_) => false,
            PluginRunnerVariant::MultiMethod(plugin) => plugin.supports_client_stream(using),
        }
    }

    fn setup_client_stream_channel(
        &mut self,
        using: Option<&str>,
    ) -> Option<mpsc::Sender<FeedData>> {
        let mut guard = block_on(self.variant.write());
        match &mut *guard {
            PluginRunnerVariant::Legacy(_) => None,
            PluginRunnerVariant::MultiMethod(plugin) => {
                // Plugin provides Sender<Vec<u8>>, bridge to Sender<FeedData>
                plugin.setup_client_stream_channel(using).map(|raw_tx| {
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
                    feed_tx
                })
            }
        }
    }
}

#[async_trait]
impl CancelMonitoring for PluginRunnerWrapperImpl {
    /// Initialize cancellation monitoring for specific job
    async fn setup_cancellation_monitoring(
        &mut self,
        job_id: JobId,
        job_data: &JobData,
    ) -> Result<Option<JobResult>> {
        // Clear branching based on helper availability
        if let Some(helper) = &mut self.cancel_helper {
            helper.setup_monitoring_impl(job_id, job_data).await
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
                    if let super::PluginRunnerVariant::MultiMethod(plugin) = &mut *guard {
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
