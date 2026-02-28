# JobWorkerP Plugin Development Guide

This guide explains how to create a plugin for JobWorkerP using Rust. Plugins allow you to extend JobWorkerP's functionality by implementing custom runners that can execute jobs.

## Overview

JobWorkerP plugins are dynamic libraries (`.so`, `.dylib`, or `.dll`) that implement the `MultiMethodPluginRunner` trait. They are loaded at runtime by the JobWorkerP runner.

> [!CAUTION]
> **Server Stability Warning**
> Since plugins are loaded as dynamic libraries into the main JobWorkerP process, a **panic** in your plugin will cause the **entire server to crash**.
> *   Avoid `unwrap()` or `expect()`. Always handle errors gracefully (e.g., return `Result`).
> *   Validate all inputs carefully.
> *   If `tokio::runtime::Runtime::new()` fails, handle it in a method that returns `Result` (like `load()`), not in `new()`.
>
> [!TIP]
> **Performance & Initialization (Static Loading)**
> When static worker loading is enabled in JobWorkerP (specifically when `WorkerData.use_static` is set to `true`), plugins are loaded (`load()` is called) once and kept in memory (pooled).
> *   **Heavy Initialization**: Perform expensive setup (e.g., loading large ML models, establishing shared database connections) in `load()`.
> *   **Per-Execution Setup**: Since plugin instances are pooled and reused, state stored in struct fields (`self`) persists across executions. Always reset these fields at the beginning of `run()` or `begin_stream()` to prevent data leakage.

## Prerequisites

-   Rust (stable)
-   Protoc (Protocol Buffers compiler)

## Project Structure

A typical plugin project has the following structure:

```
my-plugin/
├── Cargo.toml          # dependencies and lib declaration
├── build.rs            # protobuf compilation
├── protobuf/           # your proto definitions
│   ├── my_plugin.proto
│   └── ...
└── src/
    └── lib.rs          # implementation
```

## Step-by-Step Guide

### 1. Configure Cargo.toml

You need to configure your crate as a dynamic library (`cdylib`) and add necessary dependencies.

```toml
[package]
name = "my_plugin"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["dylib"]

[dependencies]
# Client library containing plugin traits and types
jobworkerp-client = { git = "https://github.com/jobworkerp-rs/jobworkerp-client-rs" }

# Async runtime and utilities (Optional: only if you need async tasks)
anyhow = "1.0.95"
futures = "0.3.31"
tokio = { version = "1.43", features = ["full"] }
tokio-stream = "0.1.17"
async-stream = "0.3.6"
tracing = "0.1.41"
serde = { version = "1.0.217", features = ["derive"] }
serde_json = "1.0.138"
schemars = "0.8.21"

# Protobuf support
prost = "0.13.4"
uuid = "1.13.0"

[build-dependencies]
prost-build = "0.13.4"
```

### 2. Define Protobufs

Define your plugin's configuration, input arguments, and output results using Protocol Buffers.

**Example `protobuf/my_runner.proto` (Configuration):**
```protobuf
syntax = "proto3";
package my_runner;

message MyRunnerSettings {
  string some_config = 1;
}
```

**Example `protobuf/my_job_args.proto` (Input):**
```protobuf
syntax = "proto3";
package my_runner;

message MyJobArgs {
  string input_data = 1;
}
```

**Example `protobuf/my_result.proto` (Output):**
```protobuf
syntax = "proto3";
package my_runner;

message MyResult {
  string output_data = 1;
}
```

### 3. Setup build.rs

Configure `build.rs` to compile your proto files.

```rust
use std::env;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    let mut config = prost_build::Config::new();
    config
        .protoc_arg("--experimental_allow_proto3_optional")
        // derive necessary traits
        .type_attribute(
            ".",
            "#[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema)]",
        )
        .compile_protos(
            &[
                "protobuf/my_runner.proto",
                "protobuf/my_job_args.proto",
                "protobuf/my_result.proto",
            ],
            &["protobuf"],
        )?;

    Ok(())
}
```

### 4. Implement the Plugin

In `src/lib.rs`, allow users to include the generated proto code and implement the `MultiMethodPluginRunner` trait.

```rust
use anyhow::Result;
use futures::stream::BoxStream;
// Use types from the client library
use jobworkerp_client::plugin::MultiMethodPluginRunner;
// Use StreamingOutputType to define execution mode
use jobworkerp_client::data::{MethodSchema, StreamingOutputType};
use std::{collections::HashMap, sync::Arc};
// tokio is used here to run async code within the synchronous trait method
use tokio::sync::Mutex;
use futures::StreamExt; // Trait for stream.next()
use futures::stream::BoxStream; // Type alias for boxed stream

// Include generated proto code
pub mod my_runner {
    // With prost-build, include the generated file directly.
    // The filename is typically based on the package name in the proto file.
    include!(concat!(env!("OUT_DIR"), "/my_runner.rs"));
}
use my_runner::{MyRunnerSettings, MyJobArgs, MyResult};

// 1. Export FFI functions
#[allow(improper_ctypes_definitions)]
#[no_mangle]
pub extern "C" fn load_multi_method_plugin() -> Box<dyn MultiMethodPluginRunner + Send + Sync> {
    Box::new(MyPlugin::new())
}

#[no_mangle]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn free_multi_method_plugin(ptr: Box<dyn MultiMethodPluginRunner + Send + Sync>) {
    drop(ptr);
}

// 2. Define your plugin struct
pub struct MyPlugin {
    // We hold a runtime to execute async code in synchronous trait methods
    // Use Option to allow lazy initialization and avoid unwrap() in new()
    rt: Option<tokio::runtime::Runtime>,
    // State for streaming
    running: Arc<Mutex<bool>>,
    stream: Arc<Mutex<BoxStream<'static, Vec<u8>>>>,
    args: Option<MyJobArgs>,
}

impl MyPlugin {
    pub fn new() -> Self {
        Self {
            rt: None, // Initialize in load() to handle errors safely
            running: Arc::new(Mutex::new(false)),
            stream: Arc::new(Mutex::new(futures::stream::empty().boxed())),
            args: None,
        }
    }
    
    // Helper functionality to generate stream
    async fn async_run_stream(input: String) -> BoxStream<'static, Vec<u8>> {
        // Example: generate 5 messages with 100ms interval
        let stream = async_stream::stream! {
            for i in 0..5 {
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                let res = MyResult {
                     output_data: format!("Stream chunk {} for {}", i, input),
                };
                yield res.encode_to_vec();
            }
        };
        Box::pin(stream)
    }
}

// 3. Implement the trait
impl MultiMethodPluginRunner for MyPlugin {
    fn name(&self) -> String {
        "MyPlugin".to_string()
    }

    fn description(&self) -> String {
        "A description of my plugin".to_string()
    }

    fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        use prost::Message;
        // Decode settings
        let settings = MyRunnerSettings::decode(settings.as_slice())?;
        
        // Initialize runtime if needed
        if self.rt.is_none() {
             self.rt = Some(tokio::runtime::Runtime::new()?);
        }
        
        // Initialize your plugin with settings
        println!("Loaded with config: {:?}", settings);
        Ok(())
    }

    fn run(
        &mut self,
        arg: Vec<u8>,
        _metadata: HashMap<String, String>,
        _using: Option<&str>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        use prost::Message;
        let metadata = HashMap::new();
        
        // Ensure runtime is available
        let rt = match self.rt.as_ref() {
            Some(rt) => rt,
            None => {
                // Try to initialize if not already done (fallback)
                 match tokio::runtime::Runtime::new() {
                    Ok(r) => {
                        self.rt = Some(r);
                        self.rt.as_ref().unwrap()
                    }
                    Err(e) => return (Err(e.into()), metadata),
                 }
            }
        };

        // Execute your logic inside the runtime
        // Since run() is synchronous, we use block_on to bridge to async code.
        let result = rt.block_on(async {
            let args = MyJobArgs::decode(arg.as_slice())?;
            
            // Your business logic here
            let response = MyResult {
                output_data: format!("Processed: {}", args.input_data),
            };
            
            Ok(response.encode_to_vec())
        });

        (result, metadata)
    }

    // Implement streaming methods
    // Called when output_type is Streaming(1) or Both(2) (and client requested stream)
    fn begin_stream(
        &mut self,
        arg: Vec<u8>,
        _metadata: HashMap<String, String>,
        _using: Option<&str>,
    ) -> Result<()> {
        let args = MyJobArgs::decode(arg.as_slice())?;
        self.args = Some(args);
        
        // Reset state
        let rt = self.rt.as_ref().unwrap(); // Safe: initialized in load/run/here
        rt.block_on(async {
             *self.running.lock().await = false;
        });
        
        Ok(())
    }

    fn receive_stream(&mut self) -> Result<Option<Vec<u8>>> {
        let rt = self.rt.as_ref().unwrap();
        rt.block_on(async {
            // 1. Initialize stream if not running
            {
                let mut running = self.running.lock().await;
                if !*running {
                    if let Some(args) = &self.args {
                         let new_stream = Self::async_run_stream(args.input_data.clone()).await;
                         *self.stream.lock().await = new_stream;
                         *running = true;
                    }
                }
            }
            
            // 2. Poll next item
            let mut stream_lock = self.stream.lock().await;
            let res = stream_lock.next().await;
            
            if res.is_none() {
                 // Stream finished
                 *self.running.lock().await = false;
            }
            Ok(res)
        })
    }

    fn cancel(&mut self) -> bool {
        false
    }

    fn is_canceled(&self) -> bool {
        false
    }
    
    // Schema definitions
    fn runner_settings_proto(&self) -> String {
        include_str!("../protobuf/my_runner.proto").to_string()
    }

    fn method_proto_map(&self) -> HashMap<String, jobworkerp_client::data::MethodSchema> {
        use jobworkerp_client::data::{MethodSchema, StreamingOutputType};
        
        let mut schemas = HashMap::new();
        schemas.insert(
            "run".to_string(),
            MethodSchema {
                args_proto: include_str!("../protobuf/my_job_args.proto").to_string(),
                result_proto: include_str!("../protobuf/my_result.proto").to_string(),
                description: Some("Main execution method".to_string()),
                // CRITICAL: output_type determines the execution method
                // - Some(0) (NonStreaming) -> calls run()
                // - Some(1) (Streaming)    -> calls begin_stream() / receive_stream()
                // - Some(2) (Both)         -> supports both
                // - None                   -> Method acts as a dummy (no execution)
                output_type: Some(StreamingOutputType::Both as i32),
            },
        );
        schemas
    }
}
```

## Feed Support (FeedToStream)

Plugins can accept additional data from clients during streaming execution via the `FeedToStream` gRPC RPC. This is useful for scenarios such as real-time audio processing where the client sends audio chunks while the runner processes and returns results.

### Prerequisites

For a plugin to support feed:

- The worker must have `use_static=true` (runner instances are pooled)
- The worker's channel concurrency must be 1
- The plugin's `MethodSchema` must have `need_feed=true`

### Implementation

Override the following methods in `MultiMethodPluginRunner`:

```rust
impl MultiMethodPluginRunner for MyFeedPlugin {
    fn supports_feed(&self, _using: Option<&str>) -> bool {
        true
    }

    fn feed_data_proto(&self, _using: Option<&str>) -> Option<String> {
        // Optional: protobuf schema for feed data
        // Return None to treat feed data as raw bytes
        Some(r#"syntax = "proto3"; message AudioChunk { bytes pcm = 1; }"#.to_string())
    }

    fn setup_feed_channel(
        &mut self,
        _using: Option<&str>,
    ) -> Option<tokio::sync::mpsc::Sender<Vec<u8>>> {
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        self.feed_rx = Some(rx);
        Some(tx)
    }

    fn method_proto_map(&self) -> HashMap<String, MethodSchema> {
        let mut schemas = HashMap::new();
        schemas.insert("run".to_string(), MethodSchema {
            args_proto: "...".to_string(),
            result_proto: "...".to_string(),
            description: Some("Feed-capable streaming method".to_string()),
            output_type: Some(StreamingOutputType::Streaming as i32),
            need_feed: Some(true),        // Enables FeedToStream RPC
            feed_data_proto: Some("...".to_string()), // Optional schema
        });
        schemas
    }

    // In receive_stream(), read feed data non-blockingly:
    fn receive_stream(&mut self) -> Result<Option<Vec<u8>>> {
        let rt = self.rt.as_ref().unwrap();
        rt.block_on(async {
            // Check for feed data
            if let Some(ref mut rx) = self.feed_rx {
                while let Ok(data) = rx.try_recv() {
                    self.buffer.extend_from_slice(&data);
                }
            }
            // Process buffered data and return output
            // Return None when stream is complete
            Ok(self.process_buffer())
        })
    }
    // ...
}
```

### How It Works

1. `setup_feed_channel()` is called before `begin_stream()` — the plugin stores the `Receiver`
2. Client sends feed data via `FeedToStream` RPC with the `job_id`
3. Data arrives at the plugin's `mpsc::Receiver` (via direct channel or Redis bridge)
4. Plugin reads from the receiver in `receive_stream()` using `try_recv()` (non-blocking)
5. When `is_final=true` is sent, the channel's `Sender` is dropped, and `try_recv()` returns `Disconnected`

> [!IMPORTANT]
> Feed data is delivered as `Vec<u8>` (not `FeedData`). The `is_final` flag is handled by the bridge layer — when `is_final=true`, the `Sender` is dropped, signaling end-of-feed to the plugin.

For detailed specification, see `docs/feed-stream-spec.md`.

## Building

Build your plugin in release mode:

```bash
cargo build --release
```

The resulting library will be in `target/release/libmy_plugin.so` (or `.dylib`, `.dll`).

## Deployment

1.  Place the compiled library file in the directory configured for JobWorkerP plugins.
2.  Register the runner in JobWorkerP settings, specifying the plugin file name and settings.
