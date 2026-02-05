# JobWorkerP プラグイン開発ガイド

このガイドでは、Rustを使用してJobWorkerPのプラグインを作成する方法を説明します。プラグインを使用することで、独自のジョブ実行ロジック（ランナー）を実装し、JobWorkerPの機能を拡張することができます。

## 概要

JobWorkerPのプラグインは、`MultiMethodPluginRunner`トレイトを実装した動的ライブラリ（`.so`, `.dylib`, または `.dll`）です。これらはJobWorkerPランナーによって実行時にロードされます。

> [!CAUTION]
> **サーバーの安定性に関する警告**
> プラグインはメインのJobWorkerPプロセスに動的ライブラリとしてロードされるため、プラグイン内で **panic（パニック）** が発生すると、**サーバー全体がクラッシュ** します。
> *   `unwrap()` や `expect()` の使用は避け、常に `Result` を返してエラーを適切に処理してください。
> *   入力の検証を慎重に行ってください。
> *   `tokio::runtime::Runtime::new()` などの初期化処理も失敗する可能性があるため、`new()` ではなく `Result` を返す `load()` メソッド内で行うなどして、panicを回避してください。
>
> [!TIP]
> **パフォーマンスと初期化について (Static Loading)**
> JobWorkerPでWorkerのstaticロード設定が有効な場合（具体的には `WorkerData.use_static` が `true` の場合）、プラグインは一度だけロードされ（`load()`が呼ばれ）、メモリ上にプールされた状態で維持されます。
> *   **重い初期化処理**: モデルのロードや共有DB接続の確立などのコストの高い処理は `load()` で実施してください。
> *   **実行ごとの初期化**: Runnerのインスタンスはプールされ再利用されるため、構造体のフィールド（`self`が保持する状態）は以前の実行時の状態を引き継ぎます。`run()` や `begin_stream()` の冒頭では、必ず実行に使用するフィールドの状態を初期化（リセット）してください。

## 前提条件

-   Rust (安定版)
-   Protoc (Protocol Buffers コンパイラ)

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

クレートの種類を動的ライブラリ（`cdylib`）として設定し、必要な依存関係を追加します。

```toml
[package]
name = "my_plugin"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["dylib"]

[dependencies]
# クライアントライブラリ（プラグイン用トレイトと型を含む）
jobworkerp-client = { git = "https://github.com/jobworkerp-rs/jobworkerp-client-rs" }

# 非同期ランタイムとユーティリティ（任意: 非同期処理が必要な場合のみ）
anyhow = "1.0.95"
futures = "0.3.31"
tokio = { version = "1.43", features = ["full"] }
tokio-stream = "0.1.17"
async-stream = "0.3.6"
tracing = "0.1.41"
serde = { version = "1.0.217", features = ["derive"] }
serde_json = "1.0.138"
schemars = "0.8.21"

# Protobuf サポート
prost = "0.13.4"
uuid = "1.13.0"

[build-dependencies]
prost-build = "0.13.4"
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

`build.rs`を設定してprotoファイルをコンパイルします。

```rust
use std::env;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    let mut config = prost_build::Config::new();
    config
        .protoc_arg("--experimental_allow_proto3_optional")
        // 必要なトレイトの自動導出
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

### 4. プラグインの実装

`src/lib.rs`で生成されたコードを読み込み、`MultiMethodPluginRunner`トレイトを実装します。

```rust
use anyhow::Result;
use futures::stream::BoxStream;
// クライアントライブラリから型を使用
use jobworkerp_client::plugin::MultiMethodPluginRunner;
// StreamingOutputTypeを使用して実行モードを定義します
use jobworkerp_client::data::{MethodSchema, StreamingOutputType};
use std::{collections::HashMap, sync::Arc};
// 同期トレイトメソッド内で非同期コードを実行するためにtokioを使用
use tokio::sync::Mutex;
use futures::StreamExt; // ストリーム操作用トレイト
use futures::stream::BoxStream; // Box化されたストリーム型

// 生成されたprotoコードをインクルード
pub mod my_runner {
    // prost-buildを使用する場合、生成されたファイルを直接includeします。
    // ファイル名は通常、protoファイル内のパッケージ名に基づきます。
    include!(concat!(env!("OUT_DIR"), "/my_runner.rs"));
}
use my_runner::{MyRunnerSettings, MyJobArgs, MyResult};

// 1. FFI関数をエクスポート
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

// 2. プラグイン構造体の定義
pub struct MyPlugin {
    // 同期トレイトメソッド内で非同期コードを実行するためのランタイムを保持します
    // Optionを使用して遅延初期化を行い、new()でのunwrap()を回避します
    rt: Option<tokio::runtime::Runtime>,
    // ストリーミング用の状態
    running: Arc<Mutex<bool>>,
    stream: Arc<Mutex<BoxStream<'static, Vec<u8>>>>,
    args: Option<MyJobArgs>,
}

impl MyPlugin {
    pub fn new() -> Self {
        Self {
            // load()で安全に初期化するため、ここはNoneにしておきます
            rt: None,
            running: Arc::new(Mutex::new(false)),
            stream: Arc::new(Mutex::new(futures::stream::empty().boxed())),
            args: None,
        }
    }

    // ストリームを生成するヘルパーメソッド
    async fn async_run_stream(input: String) -> BoxStream<'static, Vec<u8>> {
        // 例: 100msごとに5つのメッセージを生成
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
        use prost::Message;
        // 設定のデコード
        let settings = MyRunnerSettings::decode(settings.as_slice())?;
        
        // ランタイムの初期化（まだなければ）
        if self.rt.is_none() {
             self.rt = Some(tokio::runtime::Runtime::new()?);
        }

        // 設定を使った初期化処理
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
        
        // ランタイムが利用可能か確認
        let rt = match self.rt.as_ref() {
            Some(rt) => rt,
            None => {
                // フォールバック: 初期化を試みる
                 match tokio::runtime::Runtime::new() {
                    Ok(r) => {
                        self.rt = Some(r);
                        self.rt.as_ref().unwrap()
                    }
                    Err(e) => return (Err(e.into()), metadata),
                 }
            }
        };
        
        // ランタイム内でのロジック実行
        // run()は同期メソッドなので、block_onを使用して非同期コードをブリッジします
        let result = rt.block_on(async {
            let args = MyJobArgs::decode(arg.as_slice())?;
            
            // ビジネスロジック
            let response = MyResult {
                output_data: format!("Processed: {}", args.input_data),
            };
            
            Ok(response.encode_to_vec())
        });

        (result, metadata)
    }

    // ストリーミングメソッドの実装
    // output_type が Streaming(1) または Both(2) の場合に呼び出されます
    fn begin_stream(
        &mut self,
        arg: Vec<u8>,
        _metadata: HashMap<String, String>,
        _using: Option<&str>,
    ) -> Result<()> {
        let args = MyJobArgs::decode(arg.as_slice())?;
        self.args = Some(args);
        
        // 状態のリセット
        let rt = self.rt.as_ref().unwrap(); // load/runなどで初期化済み
        rt.block_on(async {
             *self.running.lock().await = false;
        });
        
        Ok(())
    }

    fn receive_stream(&mut self) -> Result<Option<Vec<u8>>> {
        let rt = self.rt.as_ref().unwrap();
        rt.block_on(async {
            // 1. 実行中でなければストリームを初期化
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
            
            // 2. 次のアイテムをポール
            let mut stream_lock = self.stream.lock().await;
            let res = stream_lock.next().await;
            
            if res.is_none() {
                 // ストリーム終了
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

    fn method_proto_map(&self) -> HashMap<String, jobworkerp_client::data::MethodSchema> {
        // クライアントライブラリから型をインポート
        use jobworkerp_client::data::{MethodSchema, StreamingOutputType};
        
        let mut schemas = HashMap::new();
        schemas.insert(
            "run".to_string(),
            MethodSchema {
                args_proto: include_str!("../protobuf/my_job_args.proto").to_string(),
                result_proto: include_str!("../protobuf/my_result.proto").to_string(),
                description: Some("メイン実行メソッド".to_string()),
                // 重要: output_type は実行メソッドを決定します
                // - Some(0) (NonStreaming) -> run() を呼び出し
                // - Some(1) (Streaming)    -> begin_stream() / receive_stream() を呼び出し
                // - Some(2) (Both)         -> 両方をサポート
                // - None                   -> メソッドは何もしない（実行されない）
                output_type: Some(StreamingOutputType::Both as i32),
            },
        );
        schemas
    }
}
```

## ビルド

リリースモードでビルドします：

```bash
cargo build --release
```

生成されたライブラリは `target/release/libmy_plugin.so` (または `.dylib`, `.dll`) に配置されます。

## デプロイ

1.  コンパイルされたライブラリファイルをJobWorkerPプラグイン用に設定されたディレクトリに配置します。
2.  JobWorkerPの設定でランナーを登録し、プラグインファイル名と設定を指定します。
