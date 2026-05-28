pub mod ffi;
pub mod impls;
pub mod loader;

use self::loader::RunnerPluginLoader;
use crate::schema_to_json_string;
use anyhow::Result;
use async_ffi::FfiFuture;
use itertools::Itertools;
use jobworkerp_base::error::JobWorkerError;
use std::{
    collections::HashMap,
    fs::{self, ReadDir},
    path::Path,
    sync::Arc,
};
use tokio::sync::RwLock as TokioRwLock;
// `async-ffi` re-export so plugin authors do not have to pin the version
// themselves (host pin is authoritative; mismatches break FfiFuture layout).
pub use async_ffi::{self, FutureExt as FfiFutureExt};
// `CancellationToken` crosses the FFI boundary as a `tokio_util` type;
// host and plugin MUST share the same workspace pin for its layout to agree.
pub use tokio_util::sync::CancellationToken;

/// FFI-safe `metadata` payload used by V2 plugins. `HashMap` is not ABI-stable
/// across separately-compiled crates, so V2 trades it for `Vec<(String, String)>`
/// at the boundary; the host converts to/from `HashMap` outside the FFI call.
pub type V2Metadata = Vec<(String, String)>;

/// Return type of `MultiMethodPluginRunnerV2::run()`.
pub type V2RunResult = (Result<Vec<u8>, String>, V2Metadata);

pub struct PluginMetadata {
    pub name: String,
    pub description: String,
    pub filename: String,
}

/// Plugin loader trait for managing dynamic library loading
///
/// # Important Notes on Memory Management
///
/// Due to the unsafe nature of dynamic library unloading (see libloading discussion:
/// https://users.rust-lang.org/t/how-to-avoid-library-and-symbol-drops-in-crate-libloading/85701),
/// implementations of this trait typically keep loaded libraries in memory indefinitely.
///
/// This means:
/// - `unload()`: Only removes logical registration, physical library remains in memory
/// - `overwrite=true` in `load_path()`: Creates duplicate physical libraries in memory
/// - Memory accumulation is intentional to prevent crashes from dangling references
#[async_trait::async_trait]
pub trait PluginLoader: Send + Sync {
    /// Load a plugin from the specified path
    ///
    /// # Parameters
    /// - `name`: Optional custom name for the plugin (uses plugin's internal name if None)
    /// - `path`: Path to the dynamic library file
    /// - `overwrite`: If true, allows re-registering a plugin with the same name
    ///
    /// # Overwrite Behavior Warning
    /// When `overwrite=true`, only the logical plugin registration is updated.
    /// The physical library file from the old path remains loaded in memory.
    /// This is a safety tradeoff to prevent crashes but leads to memory accumulation.
    ///
    /// # Returns
    /// Tuple of (plugin_name, plugin_description) on success
    async fn load_path(
        &mut self,
        name: Option<&str>,
        path: &Path,
        overwrite: bool,
    ) -> Result<(String, String)>;

    /// Unload a plugin by name (logical removal only)
    ///
    /// NOTE: This only removes the logical registration. The physical library
    /// remains in memory to prevent crashes from dangling references.
    fn unload(&mut self, name: &str) -> Result<bool>;

    /// Clear all logical plugin registrations
    ///
    /// NOTE: Physical libraries remain in memory
    #[allow(dead_code)]
    fn clear(&mut self) -> Result<()>;
}

#[derive(Debug)]
enum PluginType {
    Runner,
}

#[derive(Debug)]
pub struct Plugins {
    runner_loader: Arc<TokioRwLock<RunnerPluginLoader>>,
}
impl Default for Plugins {
    fn default() -> Self {
        Self::new()
    }
}
impl Plugins {
    pub fn new() -> Self {
        Plugins {
            runner_loader: Arc::new(TokioRwLock::new(RunnerPluginLoader::new())),
        }
    }
    pub async fn load_plugin_files(&self, runner_dir_str: &str) -> Vec<PluginMetadata> {
        // default: current dir
        let runner_dirs: Vec<&str> = runner_dir_str.split(',').collect_vec();
        let mut loaded = Vec::new();
        for runner_dir in runner_dirs {
            match fs::read_dir(runner_dir) {
                Ok(runner_path) => {
                    loaded.extend(
                        self.load_plugin_files_from(runner_path, PluginType::Runner)
                            .await,
                    );
                }
                _ => {
                    tracing::warn!("runner plugin dir not found: {}", runner_dir);
                }
            }
        }
        loaded
    }
    pub async fn load_plugin_file(
        &self,
        name: Option<&str>,
        file: &str,
        overwrite: bool,
    ) -> Result<PluginMetadata> {
        let path = Path::new(file);
        self.load_plugin_from_path(name, path, &PluginType::Runner, overwrite)
            .await
    }

    fn get_library_extension() -> &'static str {
        if cfg!(target_os = "windows") {
            ".dll"
        } else if cfg!(target_os = "macos") {
            ".dylib"
        } else if cfg!(target_os = "linux") {
            ".so"
        } else {
            tracing::error!("Unsupported operating system");
            ".so"
        }
    }

    // return: (name, file_name)
    async fn load_plugin_files_from(&self, dir: ReadDir, ptype: PluginType) -> Vec<PluginMetadata> {
        let mut loaded = Vec::new();
        for file in dir.flatten() {
            if file.path().is_file()
                && file
                    .file_name()
                    .to_str()
                    .is_some_and(|n| n.ends_with(Self::get_library_extension()))
            {
                // use plugin name defined in plugin as plugin name
                match self
                    .load_plugin_from_path(None, &file.path(), &ptype, false)
                    .await
                {
                    Ok(plugin) => {
                        loaded.push(plugin);
                    }
                    Err(e) => {
                        // Surface the underlying cause (libloading errors include
                        // unresolved symbols, missing shared libs, ABI mismatches).
                        tracing::warn!("cannot load plugin: {:?}: {:#}", file.path(), e);
                    }
                }
            } else if !file.path().exists() {
                tracing::warn!("file not found: {:?}", file.path());
            } else if !file.path().is_file() {
                tracing::warn!("not a file: {:?}", file.path());
            } else if file.file_name().to_str().is_none() {
                tracing::warn!("cannot convert file name to str: {:?}", file.path());
            } else if !file
                .file_name()
                .to_str()
                .unwrap()
                .ends_with(Self::get_library_extension())
            {
                tracing::warn!(
                    "not a plugin file (wrong extension): expect: {}, real:{:?}",
                    Self::get_library_extension(),
                    file.path()
                );
            } else {
                tracing::warn!("not a file: {:?}", file.path());
            }
        }
        loaded
    }
    async fn load_plugin_from_path(
        &self,
        name: Option<&str>,
        path: &Path,
        ptype: &PluginType,
        overwrite: bool,
    ) -> Result<PluginMetadata> {
        if path.is_file()
            && path
                .file_name()
                .and_then(|f| f.to_str())
                .is_some_and(|n| n.ends_with(Self::get_library_extension()))
        {
            tracing::info!("load {:?} plugin file: {}", ptype, path.display());
            match ptype {
                PluginType::Runner => self
                    .runner_loader
                    .write()
                    .await
                    .load_path(name, path, overwrite)
                    .await
                    .map(|(name, description)| PluginMetadata {
                        name,
                        description,
                        filename: path.to_string_lossy().to_string(),
                    }),
            }
        } else {
            Err(JobWorkerError::InvalidParameter(format!("not a file: {path:?}")).into())
        }
    }
    pub fn runner_plugins(&self) -> Arc<TokioRwLock<RunnerPluginLoader>> {
        self.runner_loader.clone()
    }
}

//TODO function load, run, cancel to async
/// Legacy PluginRunner trait (origin/main compatible)
///
/// This trait maintains binary compatibility with plugins compiled against origin/main.
/// DO NOT modify this trait definition - it will break existing plugin binaries!
pub trait PluginRunner: Send + Sync {
    fn name(&self) -> String;
    fn description(&self) -> String;
    fn load(&mut self, settings: Vec<u8>) -> Result<()>;
    fn run(
        &mut self,
        args: Vec<u8>,
        metadata: HashMap<String, String>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>);
    // run for generating stream
    fn begin_stream(&mut self, arg: Vec<u8>, metadata: HashMap<String, String>) -> Result<()> {
        // default implementation (return empty)
        let (_, _) = (arg, metadata);
        Err(anyhow::anyhow!("not implemented"))
    }
    /// Receive the next chunk from the stream started by `begin_stream()`.
    ///
    /// Returns `Ok(Some(data))` for each chunk, `Ok(None)` when the stream ends.
    ///
    /// **Blocking constraint**: This method runs inside `spawn_blocking`, so it MAY block
    /// the calling thread. However, it MUST NOT block indefinitely — it should return
    /// promptly (within seconds) when no more data is available or when the plugin is
    /// cancelled. Blocking for extended periods delays cancellation detection, since the
    /// `is_canceled()` check only runs between `receive_stream()` calls.
    ///
    /// Recommended pattern: use a channel (`mpsc::Receiver`) internally and block on
    /// `recv()`, which returns `None` when the sender is dropped (e.g., on cancellation
    /// or feed channel close).
    fn receive_stream(&mut self) -> Result<Option<Vec<u8>>> {
        // default implementation (return empty)
        Err(anyhow::anyhow!("not implemented"))
    }
    fn cancel(&self) -> bool;
    fn is_canceled(&self) -> bool;
    fn runner_settings_proto(&self) -> String;
    fn job_args_proto(&self) -> String;
    fn result_output_proto(&self) -> Option<String>;
    fn output_type(&self) -> proto::jobworkerp::data::StreamingOutputType {
        proto::jobworkerp::data::StreamingOutputType::NonStreaming
    }
    fn settings_schema(&self) -> String {
        schema_to_json_string!(crate::jobworkerp::runner::Empty, "settings_schema")
    }
    fn arguments_schema(&self) -> String {
        schema_to_json_string!(crate::jobworkerp::runner::Empty, "arguments_schema")
    }
    fn output_json_schema(&self) -> Option<String> {
        None
    }
}

/// Multi-method PluginRunner trait (new plugins)
///
/// This trait is for new plugins that support multiple methods via method_proto_map().
/// Use this for plugins that need to expose multiple callable methods.
pub trait MultiMethodPluginRunner: Send + Sync {
    fn name(&self) -> String;
    fn description(&self) -> String;
    fn load(&mut self, settings: Vec<u8>) -> Result<()>;
    /// Execute with optional method selection
    ///
    /// # Arguments
    /// * `args` - Protobuf binary arguments
    /// * `metadata` - Job metadata
    /// * `using` - Optional method name for multi-method plugins
    fn run(
        &mut self,
        args: Vec<u8>,
        metadata: HashMap<String, String>,
        using: Option<&str>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>);
    /// Begin streaming execution with optional method selection
    ///
    /// # Arguments
    /// * `arg` - Protobuf binary arguments
    /// * `metadata` - Job metadata
    /// * `using` - Optional method name for multi-method plugins
    fn begin_stream(
        &mut self,
        arg: Vec<u8>,
        metadata: HashMap<String, String>,
        using: Option<&str>,
    ) -> Result<()> {
        // default implementation (return empty)
        let (_, _, _) = (arg, metadata, using);
        Err(anyhow::anyhow!("not implemented"))
    }
    /// Receive the next chunk from the stream started by `begin_stream()`.
    ///
    /// Returns `Ok(Some(data))` for each chunk, `Ok(None)` when the stream ends.
    ///
    /// **Blocking constraint**: This method runs inside `spawn_blocking`, so it MAY block
    /// the calling thread. However, it MUST NOT block indefinitely — it should return
    /// promptly (within seconds) when no more data is available or when the plugin is
    /// cancelled. See `PluginRunner::receive_stream()` for full details.
    fn receive_stream(&mut self) -> Result<Option<Vec<u8>>> {
        // default implementation (return empty)
        Err(anyhow::anyhow!("not implemented"))
    }
    /// Cancel the running task.
    /// Unlike PluginRunner (legacy), this takes &mut self for simpler plugin implementation.
    fn cancel(&mut self) -> bool;
    fn is_canceled(&self) -> bool;
    fn runner_settings_proto(&self) -> String;

    /// Key: method name, Value: MethodSchema (input and output schemas)
    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema>;

    /// Optional: Provide custom JSON schemas
    /// If None, automatic conversion from method_proto_map() will be used
    fn method_json_schema_map(
        &self,
    ) -> Option<HashMap<String, proto::jobworkerp::data::MethodJsonSchema>> {
        None
    }

    fn settings_schema(&self) -> String {
        schema_to_json_string!(crate::jobworkerp::runner::Empty, "settings_schema")
    }

    /// Whether this plugin supports client streaming input for the given method
    fn supports_client_stream(&self, _using: Option<&str>) -> bool {
        false
    }

    /// Proto definition for client streaming data of the given method
    fn client_stream_data_proto(&self, _using: Option<&str>) -> Option<String> {
        None
    }

    /// Set up a client stream channel for receiving raw bytes during streaming execution.
    /// The wrapper layer bridges this to FeedData by spawning an adapter task.
    fn setup_client_stream_channel(
        &mut self,
        _using: Option<&str>,
    ) -> Option<tokio::sync::mpsc::Sender<Vec<u8>>> {
        None
    }

    /// Collect streaming output into a single result
    ///
    /// Default implementation: keeps only the last data chunk
    /// (protobuf binary concatenation produces invalid data)
    /// Plugins should override this for custom collection logic (e.g., merging proto messages)
    fn collect_stream(
        &self,
        stream: futures::stream::BoxStream<'static, proto::jobworkerp::data::ResultOutputItem>,
        _using: Option<&str>,
    ) -> crate::runner::CollectStreamFuture {
        use futures::StreamExt;
        use proto::jobworkerp::data::result_output_item;

        Box::pin(async move {
            let mut last_data: Option<Vec<u8>> = None;
            let mut metadata = HashMap::new();
            let mut stream = stream;
            // Separate variable for FinalCollected to avoid being overwritten by late Data chunks
            let mut final_collected: Option<Vec<u8>> = None;

            while let Some(item) = stream.next().await {
                match item.item {
                    Some(result_output_item::Item::Data(data)) => {
                        last_data = Some(data);
                    }
                    Some(result_output_item::Item::FinalCollected(data)) => {
                        final_collected = Some(data);
                    }
                    Some(result_output_item::Item::End(trailer)) => {
                        metadata = trailer.metadata;
                        break;
                    }
                    None => {}
                }
            }
            Ok((final_collected.or(last_data).unwrap_or_default(), metadata))
        })
    }
}

/// V2 multi-method plugin trait: async-ffi based plugin interface with
/// cooperative cancellation. Loaded via the FFI symbol
/// `load_multi_method_plugin_v2`.
///
/// V2 is **independent** of the legacy `MultiMethodPluginRunner` trait — the
/// vtable layout is separate, and existing V1/legacy plugin binaries are not
/// affected by changes here.
///
/// # FFI safety
/// - Async methods return `async_ffi::FfiFuture<T>` (a `#[repr(C)]` wrapper
///   over `Box<dyn Future<Output=T> + Send + 'static>`). Plugin authors
///   construct them with `async move { ... }.into_ffi()`.
/// - `metadata` crosses the boundary as `Vec<(String, String)>` because
///   `HashMap`'s layout is not ABI-stable across separately-compiled crates.
///   The host converts to/from `HashMap<String, String>` at the wrapper.
/// - Errors cross as `Result<T, String>`; `anyhow::Error` is not ABI-stable.
///   The host re-wraps `String` into `anyhow::Error` for upstream callers.
/// - `CancellationToken` crosses the boundary safely only because host and
///   plugin link the exact same `tokio_util` version (workspace pin).
/// - The `load_multi_method_plugin_v2` loader still returns
///   `Box<dyn MultiMethodPluginRunnerV2 + Send + Sync>`, so the trait's
///   vtable layout itself is Rust-ABI: host and plugin MUST share the same
///   `rustc` version. Pinning `async-ffi` only makes the returned futures
///   ABI-stable, not the trait dispatch.
///
/// # Constraints from `FfiFuture<T> = Send + 'static`
/// The returned future is `'static` and cannot borrow `&mut self`. Plugin
/// implementations MUST move any needed state into the `async move { ... }`
/// block (typically by `clone()` on cheap handles like `CancellationToken`,
/// or via `Arc<Mutex<_>>` for shared mutable state). The host expects this:
/// after the trait method returns the `FfiFuture`, the host may release its
/// borrow on `self` and even drop the future at any time (e.g., on timeout).
///
/// # Tokio runtime (dylib constraint)
/// Plugins are loaded as `cdylib`, so the `tokio` crate linked into the
/// plugin has its own `thread_local!` runtime context that is **invisible
/// to the host**. Calling `tokio::time::sleep`, `tokio::spawn`, or any
/// other `Handle::current()`-using API directly inside the `async move {}`
/// block driven by the host will panic with `there is no reactor running`.
///
/// To make async work runnable, the plugin MUST:
/// 1. Own a `tokio::runtime::Runtime` built with `Builder::new_multi_thread()`
///    and **at least one worker thread**. (`new_current_thread()` does not
///    progress tasks unless someone calls `block_on(...)`, so `handle.spawn`
///    queues but never executes — defeating cooperative cancellation.)
/// 2. `handle.spawn(...)` the actual async work onto that runtime.
/// 3. Return an `FfiFuture` that awaits the resulting `JoinHandle` and
///    bridges the result back to the host.
///
/// See `plugins/cancel_test/src/lib.rs` for the canonical implementation
/// pattern and `manual/en/src/plugin-development-v2.md` (or the Japanese
/// edition at `manual/ja/src/plugin-development-v2.md`) for the full
/// rationale.
///
/// # Drop safety
/// On timeout the host drops the returned `FfiFuture`. Dropping a
/// `JoinHandle` does NOT cancel the spawned task; the plugin-side task
/// keeps running on the plugin runtime until it observes the
/// `CancellationToken` or completes naturally. Always `select!` on
/// `token.cancelled()` inside long-running spawned work.
///
/// Avoid tokio I/O inside `Drop` impls — they may run during shutdown of
/// either runtime and panic outside a runtime context.
pub trait MultiMethodPluginRunnerV2: Send + Sync {
    // === Sync metadata methods ===
    fn name(&self) -> String;
    fn description(&self) -> String;
    fn runner_settings_proto(&self) -> String;

    /// Key: method name, Value: MethodSchema (input and output schemas).
    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema>;

    /// Optional: custom JSON schemas. Returning `None` triggers automatic
    /// conversion from `method_proto_map()`.
    fn method_json_schema_map(
        &self,
    ) -> Option<HashMap<String, proto::jobworkerp::data::MethodJsonSchema>> {
        None
    }

    fn settings_schema(&self) -> String {
        schema_to_json_string!(crate::jobworkerp::runner::Empty, "settings_schema")
    }

    /// Whether this plugin supports client streaming input for the given method.
    fn supports_client_stream(&self, _using: Option<&str>) -> bool {
        false
    }

    /// Proto definition for client streaming data of the given method.
    fn client_stream_data_proto(&self, _using: Option<&str>) -> Option<String> {
        None
    }

    /// Set up a client stream channel for receiving raw bytes during streaming
    /// execution. The wrapper bridges this to FeedData by spawning an adapter.
    fn setup_client_stream_channel(
        &mut self,
        _using: Option<&str>,
    ) -> Option<tokio::sync::mpsc::Sender<Vec<u8>>> {
        None
    }

    // === Cancellation ===

    /// Host calls this BEFORE each job's `run()`/`run_stream()` (once per
    /// job; pooled plugin instances are reused across jobs and receive a fresh
    /// token each time). The plugin should simply replace any previously
    /// stored token — old tokens belong to completed or cancelled jobs and are
    /// no longer relevant.
    ///
    /// Plugins await `token.cancelled().await` inside the future returned by
    /// `run()`/`run_stream()` to cooperatively abort.
    fn set_cancellation_token(&mut self, token: CancellationToken);

    // === Async surface (async-ffi) ===

    /// Initialize the plugin instance with settings. Errors cross the FFI as
    /// `String` (re-wrapped to `anyhow::Error` by the host).
    fn load(&mut self, settings: Vec<u8>) -> FfiFuture<Result<(), String>>;

    /// Execute one unary job.
    ///
    /// # Arguments
    /// * `args` - protobuf-encoded binary
    /// * `metadata` - job metadata as `Vec<(String, String)>` (HashMap form
    ///   on the host side is converted at the wrapper boundary)
    /// * `using` - optional method selector for multi-method plugins
    ///
    /// # Return
    /// A future yielding `(Result<output, error_message>, metadata)`. The
    /// metadata is passed back so plugins can attach trace/telemetry fields.
    fn run(
        &mut self,
        args: Vec<u8>,
        metadata: V2Metadata,
        using: Option<String>,
    ) -> FfiFuture<V2RunResult>;

    /// Execute one streaming job.
    ///
    /// Unlike V1's pull-based `begin_stream`/`receive_stream` split, V2 takes
    /// a push-based design: the host creates an `mpsc::Sender<Vec<u8>>` and
    /// hands it to the plugin. The plugin sends each output chunk via
    /// `output.send(chunk).await` and returns the final metadata when done.
    /// Dropping the sender (the plugin returning, or the host dropping the
    /// future) signals end of stream to the host's receiver.
    ///
    /// # Arguments
    /// * `args` - protobuf-encoded binary
    /// * `metadata` - job metadata as `V2Metadata`
    /// * `using` - optional method selector for multi-method plugins
    /// * `output` - host-provided sink for stream chunks. The plugin sends
    ///   `Vec<u8>` chunks here; the host wraps them into `ResultOutputItem`
    ///   and forwards downstream. The sender is `tokio::sync::mpsc::Sender`,
    ///   whose ABI is stable as long as host and plugin pin the same `tokio`
    ///   workspace version (the same constraint as `CancellationToken`).
    ///
    /// # Return
    /// A future yielding `Result<final_metadata, error_message>`. The
    /// metadata is attached to the End trailer of the host-side stream.
    ///
    /// # Cancellation
    /// If the host drops the returned `FfiFuture` (e.g., on timeout) the
    /// plugin's spawned task continues until it observes the cancellation
    /// token or completes naturally. Plugins that perform long-running work
    /// MUST select on `token.cancelled()` to abort early.
    ///
    /// Default impl returns `Err("not implemented")` immediately.
    fn run_stream(
        &mut self,
        args: Vec<u8>,
        metadata: V2Metadata,
        using: Option<String>,
        output: tokio::sync::mpsc::Sender<Vec<u8>>,
    ) -> FfiFuture<Result<V2Metadata, String>> {
        let _ = (args, metadata, using, output);
        async move { Err("not implemented".to_string()) }.into_ffi()
    }

    /// Collect streaming output into a single result.
    ///
    /// Default implementation: keeps only the last data chunk (concatenating
    /// protobuf binaries is invalid). Plugins should override this for custom
    /// collection logic (e.g., merging proto messages).
    fn collect_stream(
        &self,
        stream: futures::stream::BoxStream<'static, proto::jobworkerp::data::ResultOutputItem>,
        _using: Option<&str>,
    ) -> crate::runner::CollectStreamFuture {
        use futures::StreamExt;
        use proto::jobworkerp::data::result_output_item;

        Box::pin(async move {
            let mut last_data: Option<Vec<u8>> = None;
            let mut metadata = HashMap::new();
            let mut stream = stream;
            let mut final_collected: Option<Vec<u8>> = None;

            while let Some(item) = stream.next().await {
                match item.item {
                    Some(result_output_item::Item::Data(data)) => {
                        last_data = Some(data);
                    }
                    Some(result_output_item::Item::FinalCollected(data)) => {
                        final_collected = Some(data);
                    }
                    Some(result_output_item::Item::End(trailer)) => {
                        metadata = trailer.metadata;
                        break;
                    }
                    None => {}
                }
            }
            Ok((final_collected.or(last_data).unwrap_or_default(), metadata))
        })
    }
}

/// Enum to wrap legacy and multi-method plugins
pub enum PluginRunnerVariant {
    /// Legacy plugins (origin/main compatible, no collect_stream)
    Legacy(Box<dyn PluginRunner + Send + Sync>),
    /// Multi-method plugins (multiple methods with collect_stream support)
    MultiMethod(Box<dyn MultiMethodPluginRunner + Send + Sync>),
    /// V2 multi-method plugins: superset of MultiMethod with cooperative
    /// cancellation. Loaded via `load_multi_method_plugin_v2`.
    MultiMethodV2(Box<dyn MultiMethodPluginRunnerV2 + Send + Sync>),
}

impl PluginRunnerVariant {
    /// Access the V1 `MultiMethodPluginRunner` surface. Returns `Some` only
    /// for V1 plugins.
    ///
    /// Note: V2 plugins do NOT share a supertrait with V1 (V2 is async-ffi
    /// based with a different vtable layout). Callers that need to dispatch
    /// uniformly across V1/V2 should match on the enum directly or use the
    /// sync accessors below (`name()`, `method_proto_map()`, etc.).
    pub fn as_multi_method_v1(&self) -> Option<&(dyn MultiMethodPluginRunner + Send + Sync)> {
        match self {
            PluginRunnerVariant::MultiMethod(p) => Some(p.as_ref()),
            PluginRunnerVariant::MultiMethodV2(_) | PluginRunnerVariant::Legacy(_) => None,
        }
    }
    pub fn as_multi_method_v1_mut(
        &mut self,
    ) -> Option<&mut (dyn MultiMethodPluginRunner + Send + Sync)> {
        match self {
            PluginRunnerVariant::MultiMethod(p) => Some(p.as_mut()),
            PluginRunnerVariant::MultiMethodV2(_) | PluginRunnerVariant::Legacy(_) => None,
        }
    }
    /// Hand the cancellation token to a v2 plugin; no-op for v1/legacy.
    pub fn set_cancellation_token(&mut self, token: CancellationToken) {
        if let PluginRunnerVariant::MultiMethodV2(p) = self {
            p.set_cancellation_token(token);
        }
    }

    // === Sync metadata accessors (3-way dispatch) ===
    //
    // V2 plugins drop the supertrait relation to `MultiMethodPluginRunner`, so
    // `as_multi_method_v1()` cannot upcast V2. These accessors let callers
    // read metadata uniformly without caring which variant they hold.

    pub fn name(&self) -> String {
        match self {
            PluginRunnerVariant::Legacy(p) => p.name(),
            PluginRunnerVariant::MultiMethod(p) => p.name(),
            PluginRunnerVariant::MultiMethodV2(p) => p.name(),
        }
    }

    pub fn description(&self) -> String {
        match self {
            PluginRunnerVariant::Legacy(p) => p.description(),
            PluginRunnerVariant::MultiMethod(p) => p.description(),
            PluginRunnerVariant::MultiMethodV2(p) => p.description(),
        }
    }

    pub fn runner_settings_proto(&self) -> String {
        match self {
            PluginRunnerVariant::Legacy(p) => p.runner_settings_proto(),
            PluginRunnerVariant::MultiMethod(p) => p.runner_settings_proto(),
            PluginRunnerVariant::MultiMethodV2(p) => p.runner_settings_proto(),
        }
    }

    pub fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        match self {
            PluginRunnerVariant::Legacy(plugin) => {
                // Legacy plugins have a single implicit method; synthesize a
                // DEFAULT_METHOD_NAME entry from job_args_proto/result_output_proto.
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
                map
            }
            PluginRunnerVariant::MultiMethod(p) => p.method_proto_map(),
            PluginRunnerVariant::MultiMethodV2(p) => p.method_proto_map(),
        }
    }

    pub fn method_json_schema_map(
        &self,
    ) -> HashMap<String, proto::jobworkerp::data::MethodJsonSchema> {
        match self {
            PluginRunnerVariant::Legacy(plugin) => {
                let mut map = HashMap::new();
                map.insert(
                    proto::DEFAULT_METHOD_NAME.to_string(),
                    proto::jobworkerp::data::MethodJsonSchema {
                        args_schema: plugin.arguments_schema(),
                        result_schema: plugin.output_json_schema(),
                        client_stream_data_schema: None,
                    },
                );
                map
            }
            PluginRunnerVariant::MultiMethod(p) => {
                p.method_json_schema_map().unwrap_or_else(|| {
                    proto::jobworkerp::data::MethodJsonSchema::from_proto_map(
                        self.method_proto_map(),
                    )
                })
            }
            PluginRunnerVariant::MultiMethodV2(p) => {
                p.method_json_schema_map().unwrap_or_else(|| {
                    proto::jobworkerp::data::MethodJsonSchema::from_proto_map(
                        self.method_proto_map(),
                    )
                })
            }
        }
    }

    pub fn settings_schema(&self) -> String {
        match self {
            PluginRunnerVariant::Legacy(p) => p.settings_schema(),
            PluginRunnerVariant::MultiMethod(p) => p.settings_schema(),
            PluginRunnerVariant::MultiMethodV2(p) => p.settings_schema(),
        }
    }

    pub fn supports_client_stream(&self, using: Option<&str>) -> bool {
        match self {
            PluginRunnerVariant::Legacy(_) => false,
            PluginRunnerVariant::MultiMethod(p) => p.supports_client_stream(using),
            PluginRunnerVariant::MultiMethodV2(p) => p.supports_client_stream(using),
        }
    }

    pub fn client_stream_data_proto(&self, using: Option<&str>) -> Option<String> {
        match self {
            PluginRunnerVariant::Legacy(_) => None,
            PluginRunnerVariant::MultiMethod(p) => p.client_stream_data_proto(using),
            PluginRunnerVariant::MultiMethodV2(p) => p.client_stream_data_proto(using),
        }
    }

    /// Whether the wrapper instance must be discarded (not pooled) on timeout.
    ///
    /// V1/legacy plugins block on a synchronous `run()` while holding the
    /// variant write lock, so a timeout cannot reclaim the lock — the
    /// instance is unusable for the next job.
    ///
    /// V2 plugins return an `FfiFuture` that is awaited inside the wrapper.
    /// On timeout the caller drops the future, the guard drops with it, and
    /// the lock is reclaimed immediately, so the instance can be reused.
    pub fn should_detach_on_timeout(&self) -> bool {
        match self {
            PluginRunnerVariant::Legacy(_) | PluginRunnerVariant::MultiMethod(_) => true,
            PluginRunnerVariant::MultiMethodV2(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_plugins_initialization() {
        let plugins = Plugins::new();

        // Test that runner loader is initialized
        let runner_loader = plugins.runner_plugins();
        let loader_guard = runner_loader.read().await;

        // Should not panic and should be empty initially
        drop(loader_guard);
    }

    #[tokio::test]
    async fn test_plugin_loader_directory_scan() {
        let plugins = Plugins::new();

        // Test scanning a non-existent directory
        let loaded = plugins.load_plugin_files("/non_existent_path").await;

        // Should return empty vec for non-existent directory
        assert!(loaded.is_empty());
    }

    #[tokio::test]
    async fn test_plugin_metadata() {
        let metadata = PluginMetadata {
            name: "test_plugin".to_string(),
            description: "Test plugin for unit testing".to_string(),
            filename: "test_plugin.so".to_string(),
        };

        assert_eq!(metadata.name, "test_plugin");
        assert_eq!(metadata.description, "Test plugin for unit testing");
        assert_eq!(metadata.filename, "test_plugin.so");
    }

    #[tokio::test]
    async fn test_plugin_system_initialization() {
        // Test basic plugin system initialization without loading actual plugins
        let plugins = Plugins::new();

        // Test that we can access the runner loader
        let loader = plugins.runner_plugins();
        let loader_guard = loader.read().await;
        drop(loader_guard);

        // Test that plugin files loading works with invalid path
        let loaded = plugins
            .load_plugin_files("invalid/path/that/does/not/exist")
            .await;
        assert!(
            loaded.is_empty(),
            "Should return empty vec for non-existent directory"
        );
    }
}
