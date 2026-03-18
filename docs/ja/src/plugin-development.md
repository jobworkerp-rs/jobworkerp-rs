# JobWorkerP プラグイン開発ガイド

このガイドでは、Rustを使用してJobWorkerPのプラグインを作成する方法を説明します。プラグインを使用することで、独自のジョブ実行ロジック（ランナー）を実装し、JobWorkerPの機能を拡張することができます。

## 概要

JobWorkerPのプラグインは、`MultiMethodPluginRunner`トレイトを実装した動的ライブラリ（`.so`, `.dylib`, または `.dll`）です。これらはJobWorkerPランナーによって実行時にロードされます。

> **注意**: **サーバーの安定性に関する警告**
> プラグインはメインのJobWorkerPプロセスに動的ライブラリとしてロードされるため、プラグイン内で **panic（パニック）** が発生すると、**サーバー全体がクラッシュ** します。
> - `unwrap()` や `expect()` の使用は避け、常に `Result` を返してエラーを適切に処理してください。
> - 入力の検証を慎重に行ってください。
> - `tokio::runtime::Runtime::new()` などの初期化処理も失敗する可能性があるため、`new()` ではなく `Result` を返す `load()` メソッド内で行うなどして、panicを回避してください。

> **ヒント**: **パフォーマンスと初期化について (Static Loading)**
> JobWorkerPでWorkerのstaticロード設定が有効な場合（具体的には `WorkerData.use_static` が `true` の場合）、プラグインは一度だけロードされ（`load()`が呼ばれ）、メモリ上にプールされた状態で維持されます。
> - **重い初期化処理**: モデルのロードや共有DB接続の確立などのコストの高い処理は `load()` で実施してください。
> - **実行ごとの初期化**: Runnerのインスタンスはプールされ再利用されるため、構造体のフィールド（`self`が保持する状態）は以前の実行時の状態を引き継ぎます。`run()` や `begin_stream()` の冒頭では、必ず実行に使用するフィールドの状態を初期化（リセット）してください。

## 前提条件

- Rust (安定版)
- Protoc (Protocol Buffers コンパイラ)

## プロジェクト構成

典型的なプラグインプロジェクトの構成は以下の通りです：

```
my-plugin/
├── Cargo.toml          # 依存関係とライブラリ設定
├── build.rs            # protobufのコンパイル設定
├── protobuf/           # proto定義ファイル
│   ├── my_plugin.proto
│   └── ...
└── src/
    └── lib.rs          # 実装コード
```

## ステップ・バイ・ステップ ガイド

### 1. Cargo.toml の設定

クレートの種類を動的ライブラリ（`dylib`）として設定し、必要な依存関係を追加します。

```toml
[package]
name = "my_plugin"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["dylib"]

[dependencies]
# jobworkerp-rs のランナートレイトと型
jobworkerp-runner = { git = "https://github.com/jobworkerp-rs/jobworkerp-rs", path = "runner" }
# protobuf定義（MethodSchema等の共通型）
proto = { git = "https://github.com/jobworkerp-rs/jobworkerp-rs", path = "proto" }

# 非同期ランタイムとユーティリティ
anyhow = "1"
async-trait = "0.1"
futures = "0.3"
tokio = { version = "1", features = ["full"] }
async-stream = "0.3"
tracing = "0.1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
schemars = "1"

# Protobuf サポート
prost = "0.14"
tonic = "0.14"
tokio-stream = "0.1"

[build-dependencies]
tonic-prost-build = "0.14"
```

### 2. Protobuf の定義

プラグインの設定、入力引数、出力結果をProtocol Buffersで定義します。

**例 `protobuf/my_runner.proto` (設定):**
```protobuf
syntax = "proto3";
package my_runner;

message MyRunnerSettings {
  string some_config = 1;
}
```

**例 `protobuf/my_job_args.proto` (ジョブ引数):**
```protobuf
syntax = "proto3";
package my_runner;

message MyJobArgs {
  string input_data = 1;
}
```

**例 `protobuf/my_result.proto` (実行結果):**
```protobuf
syntax = "proto3";
package my_runner;

message MyResult {
  string output_data = 1;
}
```

### 3. build.rs のセットアップ

`build.rs`を設定してprotoファイルをコンパイルします。`tonic_prost_build`を使用します。

```rust
use std::env;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    tonic_prost_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .file_descriptor_set_path(out_dir.join("my_plugin.bin"))
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
        )
        .unwrap_or_else(|e| panic!("Failed to compile protos {e:?}"));

    Ok(())
}
```

### 4. プラグインの実装

`src/lib.rs`で生成されたコードを読み込み、`MultiMethodPluginRunner`トレイトを実装します。

```rust
use anyhow::Result;
use futures::{StreamExt, stream::BoxStream};
use jobworkerp_runner::runner::plugins::MultiMethodPluginRunner;
use prost::Message;
use std::{alloc::System, collections::HashMap, sync::Arc};
use tokio::sync::Mutex;

// tonic::include_proto! マクロで生成されたコードを読み込み
pub mod my_runner {
    tonic::include_proto!("my_runner");
}
use my_runner::{MyRunnerSettings, MyJobArgs, MyResult};

// System allocatorの指定（プラグインでは必須）
#[global_allocator]
static ALLOCATOR: System = System;

// 1. FFI関数をエクスポート
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn load_multi_method_plugin() -> Box<dyn MultiMethodPluginRunner + Send + Sync> {
    Box::new(MyPlugin::new())
}

#[unsafe(no_mangle)]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn free_multi_method_plugin(ptr: Box<dyn MultiMethodPluginRunner + Send + Sync>) {
    drop(ptr);
}

// 2. プラグイン構造体の定義
pub struct MyPlugin {
    rt: tokio::runtime::Runtime,
    running: Arc<Mutex<bool>>,
    stream: Arc<Mutex<BoxStream<'static, Vec<u8>>>>,
    args: MyJobArgs,
}

impl MyPlugin {
    pub fn new() -> Self {
        Self {
            rt: tokio::runtime::Runtime::new().unwrap(),
            running: Arc::new(Mutex::new(false)),
            stream: Arc::new(Mutex::new(futures::stream::empty().boxed())),
            args: MyJobArgs {
                input_data: String::new(),
            },
        }
    }

    // ストリームを生成するヘルパーメソッド
    async fn async_run_stream(input: String) -> BoxStream<'static, Vec<u8>> {
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

// 3. トレイトの実装
impl MultiMethodPluginRunner for MyPlugin {
    fn name(&self) -> String {
        "MyPlugin".to_string()
    }

    fn description(&self) -> String {
        "プラグインの説明".to_string()
    }

    fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        let settings = MyRunnerSettings::decode(settings.as_slice())?;
        println!("Loaded with config: {:?}", settings);
        Ok(())
    }

    fn run(
        &mut self,
        arg: Vec<u8>,
        metadata: HashMap<String, String>,
        _using: Option<&str>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        let result = self.rt.block_on(async {
            let args = MyJobArgs::decode(arg.as_slice())?;

            let response = MyResult {
                output_data: format!("Processed: {}", args.input_data),
            };

            Ok(response.encode_to_vec())
        });

        (result, metadata)
    }

    // ストリーミングメソッドの実装
    fn begin_stream(
        &mut self,
        arg: Vec<u8>,
        _metadata: HashMap<String, String>,
        _using: Option<&str>,
    ) -> Result<()> {
        self.args = MyJobArgs::decode(arg.as_slice())?;
        // 状態のリセット
        self.rt.block_on(async {
             *self.running.lock().await = false;
        });
        Ok(())
    }

    fn receive_stream(&mut self) -> Result<Option<Vec<u8>>> {
        self.rt.block_on(async {
            {
                let mut running = self.running.lock().await;
                if !*running {
                    let new_stream = Self::async_run_stream(self.args.input_data.clone()).await;
                    *self.stream.lock().await = new_stream;
                    *running = true;
                }
            }

            let mut stream_lock = self.stream.lock().await;
            let res = stream_lock.next().await;

            if res.is_none() {
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

    // スキーマ定義
    fn runner_settings_proto(&self) -> String {
        include_str!("../protobuf/my_runner.proto").to_string()
    }

    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        let mut schemas = HashMap::new();
        schemas.insert(
            "run".to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: include_str!("../protobuf/my_job_args.proto").to_string(),
                result_proto: include_str!("../protobuf/my_result.proto").to_string(),
                description: Some("メイン実行メソッド".to_string()),
                // output_type は実行メソッドを決定します
                // - NonStreaming (0) -> run() を呼び出し
                // - Streaming (1)   -> begin_stream() / receive_stream() を呼び出し
                // - Both (2)        -> 両方をサポート
                output_type: proto::jobworkerp::data::StreamingOutputType::Both as i32,
                ..Default::default()
            },
        );
        schemas
    }
}
```

## クライアントストリーミングサポート (EnqueueWithClientStream)

プラグインは `EnqueueWithClientStream` gRPC RPC を通じて、ストリーミング実行中にクライアントから追加データを受け取ることができます。リアルタイム音声処理など、クライアントが音声チャンクを送信しながら Runner が処理・結果を返すようなシナリオで有用です。

### 前提条件

プラグインでクライアントストリーミングを使用するには以下の条件が必要です：

- プラグインの `MethodSchema` で `require_client_stream=true` が設定されていること

### 実装方法

`MultiMethodPluginRunner` の以下のメソッドをオーバーライドします：

```rust
impl MultiMethodPluginRunner for MyFeedPlugin {
    fn supports_client_stream(&self, _using: Option<&str>) -> bool {
        true
    }

    fn client_stream_data_proto(&self, _using: Option<&str>) -> Option<String> {
        // 任意: クライアントストリームデータ用の protobuf スキーマ
        // None を返すとデータは生バイト列として扱われます
        Some(r#"syntax = "proto3"; message AudioChunk { bytes pcm = 1; }"#.to_string())
    }

    fn setup_client_stream_channel(
        &mut self,
        _using: Option<&str>,
    ) -> Option<tokio::sync::mpsc::Sender<Vec<u8>>> {
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        self.feed_rx = Some(rx);
        Some(tx)
    }

    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        let mut schemas = HashMap::new();
        schemas.insert("run".to_string(), proto::jobworkerp::data::MethodSchema {
            args_proto: "...".to_string(),
            result_proto: "...".to_string(),
            description: Some("クライアントストリーミング対応メソッド".to_string()),
            output_type: proto::jobworkerp::data::StreamingOutputType::Streaming as i32,
            require_client_stream: true,        // EnqueueWithClientStream RPC を有効化
            client_stream_data_proto: Some("...".to_string()), // 任意のスキーマ
        });
        schemas
    }

    // receive_stream() 内で feed データを非ブロッキングで読み取り:
    fn receive_stream(&mut self) -> Result<Option<Vec<u8>>> {
        let rt = self.rt.as_ref().unwrap();
        rt.block_on(async {
            // feed データを確認
            if let Some(ref mut rx) = self.feed_rx {
                while let Ok(data) = rx.try_recv() {
                    self.buffer.extend_from_slice(&data);
                }
            }
            // バッファのデータを処理して出力を返す
            // ストリーム完了時は None を返す
            Ok(self.process_buffer())
        })
    }
    // ...
}
```

### 動作の仕組み

1. `begin_stream()` の前に `setup_client_stream_channel()` が呼ばれ、プラグインは `Receiver` を保持します
2. クライアントが `EnqueueWithClientStream` RPC でデータを送信します
3. データはプラグインの `mpsc::Receiver` に到達します（直接チャネルまたは Redis ブリッジ経由）
4. プラグインは `receive_stream()` 内で `try_recv()` を使い非ブロッキングで受信します
5. `is_final=true` が送信されると、チャネルの `Sender` が drop され、`try_recv()` が `Disconnected` を返します

> **重要**: クライアントストリームデータは `Vec<u8>` として配信されます。`is_final` フラグはブリッジ層で処理され、`is_final=true` 時に `Sender` が drop されることでストリームの終了をプラグインに通知します。

## ビルド

リリースモードでビルドします：

```bash
cargo build --release
```

生成されたライブラリは `target/release/libmy_plugin.so` (または `.dylib`, `.dll`) に配置されます。

## デプロイ

1. コンパイルされたライブラリファイルを環境変数 `PLUGINS_RUNNER_DIR` で指定されたディレクトリに配置します（デフォルト: `./`）。ディレクトリ内に配置するだけでRunnerとして自動登録されます。
2. 実装例: [HelloPlugin](https://github.com/jobworkerp-rs/jobworkerp-rs/blob/main/plugins/hello_runner/src/lib.rs)
