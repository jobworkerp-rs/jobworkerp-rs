# JobWorkerP プラグイン開発ガイド (V2 / async-ffi)

このガイドでは **V2 プラグイン trait (`MultiMethodPluginRunnerV2`)** を使った
JobWorkerP プラグインの作成方法を説明します。V2 はキャンセル時の挙動と非同期
処理の書き味を改善した新しい trait で、`load_multi_method_plugin_v2` という
独立した FFI シンボル経由でロードされます。V1
([プラグイン開発 (V1)](./plugin-development.md)) と V2 は同一ホスト上で並存
でき、既存 V1 プラグインのバイナリ互換性は維持されます。

> **注意**: V1 ガイドの
> [サーバー安定性に関する警告](./plugin-development.md#概要) は V2 でも
> そのまま適用されます (プラグイン内の panic がホスト全体をクラッシュさ
> せます)。V2 で 1 点追加: `tokio::runtime::Runtime` の構築は `new()` で
> はなく `Err` を返せる `load()` 内で行ってください。

## なぜ V2 か

V1 (`MultiMethodPluginRunner`) は同期 trait で、長時間ブロックする `run()` の
途中にタイムアウトが発生してもホスト側のロックを回収できず、wrapper インスタ
ンスを破棄するしかありませんでした。V2 では:

| 観点 | V1 | V2 |
|------|----|----|
| async サーフェス | `fn` (同期) | `async_ffi::FfiFuture<T>` |
| キャンセル | `cancel()` / `is_canceled()` | `CancellationToken` を `select!` で監視 |
| ストリーミング | `begin_stream` + pull 型 `receive_stream` | `run_stream(args, ..., output: Sender<Vec<u8>>)` で push 型 |
| タイムアウト時の lock 解放 | ロック保持で wrapper を破棄 | future drop で即解放、wrapper 再利用可 |
| FFI シンボル | `load_multi_method_plugin` | `load_multi_method_plugin_v2` |

新規プラグインでは V2 を選択することを推奨します。

## V2 固有の制約 (必読)

### 1. 自前の tokio runtime を持つこと

プラグインは `cdylib` としてロードされるため、プラグイン内でリンクされた
`tokio` は **ホスト側とは別の `thread_local!` ランタイムコンテキスト** を
持ちます。`async move {}` ブロックをホスト runtime 上で直接 await すると
`tokio::time::sleep` や `Handle::current()` を使う API が
"there is no reactor running" で panic します。

そのため V2 プラグインは:

1. 自前の `tokio::runtime::Runtime` を `Builder::new_multi_thread()` で構築
   し、**最低 1 worker thread** を確保すること。
   - `new_current_thread()` は `block_on()` の内側でしかタスクを進めない
     ため `handle.spawn()` したタスクが永久に進まず、cooperative cancel
     も成立しません。
2. `handle.spawn(...)` で実際の async work を投入すること。
3. `FfiFuture` の中身は `JoinHandle::await` で結果を host に橋渡しすること。

### 2. `FfiFuture<T>` は `Send + 'static`

返り値の future は `'static` のため `&mut self` 借用を持ち越せません。必要な
状態は `async move {}` ブロックに **clone して move** するか、`Arc<Mutex<_>>`
で共有してください。

### 3. キャンセルは CancellationToken のみ

V2 では `cancel()` / `is_canceled()` メソッドはありません。代わりに host が
`set_cancellation_token(token)` で `tokio_util::sync::CancellationToken` を
渡してきます。プラグインは spawn したタスク内で `tokio::select!` を使って
`token.cancelled().await` を監視してください。

`CancellationToken` は内部実装が `Arc<AtomicBool> + Notify` で
**tokio runtime 非依存**のため、host runtime で `cancel()`、plugin runtime で
`cancelled().await` という構成が正しく動作します。

### 4. timeout 時の挙動

host は wrapper の `should_detach_on_timeout()` が `false` を返すので、
timeout 発火時に host 側の `FfiFuture` は drop されますが wrapper インスタンス
は破棄されず再利用されます。一方、**プラグイン runtime 上で spawn したタスク
は引き続き走り続け**、自然完了か `token.cancelled()` 観測まで継続します。

そのため、host は timeout と同時に `token.cancel()` を呼び出すことで、
プラグイン側のタスクも早期終了させます。token を cancel しなければ
プラグイン側タスクは指定された処理を完遂してから終了します。

### 5. `Drop` で tokio I/O を呼ばない

プラグイン構造体の `Drop` 内で `tokio::time::sleep` 等の tokio I/O を呼ばな
いでください。host runtime が shutdown 中に drop が走るケースがあり、
runtime context 外で tokio API を呼ぶと panic します。

### 6. ワークスペース依存を host と揃える

FFI 境界をまたぐ型を持つため、以下のクレートは **host が pin している
バージョンと完全一致**させてください:

- `async-ffi` — `FfiFuture<T>` の表現
- `tokio` — `JoinHandle` 等(`run_stream` の `Sender<Vec<u8>>` も含む)
- `tokio-util` — `CancellationToken` の構造体レイアウト

## ステップ・バイ・ステップ ガイド

### 1. Cargo.toml

V1 の [Cargo.toml ブロック](./plugin-development.md#1-cargotoml-の設定) を
ベースに、以下のクレートを追加します(バージョンは host の workspace pin
と **完全一致** させてください):

```toml
[dependencies]
async-ffi = "0.5"
tokio-util = { version = "0.7", features = ["full"] }
```

### 2. Protobuf 定義と build.rs

V1 と同じです([V1 ガイド](./plugin-development.md#2-protobuf-の定義)を参照)。
`runner_settings_proto` / `method_proto_map` 等の sync メタデータメソッドの
シグネチャは V1 と同じであり、proto 定義の制約も同じです。

### 3. プラグイン実装

```rust
use async_ffi::{FfiFuture, FutureExt};
use jobworkerp_runner::runner::plugins::{
    CancellationToken, MultiMethodPluginRunnerV2,
};
use std::collections::HashMap;
use std::time::Duration;

// 1. FFI ローダ関数(V2 用シンボル: load_multi_method_plugin_v2)
#[unsafe(no_mangle)]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn load_multi_method_plugin_v2()
    -> Box<dyn MultiMethodPluginRunnerV2 + Send + Sync>
{
    Box::new(MyV2Plugin::new())
}

#[unsafe(no_mangle)]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn free_multi_method_plugin_v2(
    ptr: Box<dyn MultiMethodPluginRunnerV2 + Send + Sync>,
) {
    drop(ptr);
}

// 2. プラグイン構造体
pub struct MyV2Plugin {
    /// プラグイン専用 tokio runtime。host とは独立した tokio thread_local
    /// コンテキストを持つため、async work は必ずこの runtime に spawn する。
    rt: tokio::runtime::Runtime,
    token: Option<CancellationToken>,
}

impl MyV2Plugin {
    pub fn new() -> Self {
        // multi_thread + 最低 1 worker が必須(current_thread だと spawn
        // したタスクが進まない)。プラグイン内で並列度が要らないなら
        // worker_threads(1) で十分。
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("failed to build plugin tokio runtime");
        Self { rt, token: None }
    }
}

// 3. trait 実装
impl MultiMethodPluginRunnerV2 for MyV2Plugin {
    fn name(&self) -> String { "MyV2Plugin".to_string() }
    fn description(&self) -> String { "V2 sample".to_string() }
    fn runner_settings_proto(&self) -> String {
        include_str!("../protobuf/my_runner.proto").to_string()
    }

    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        HashMap::from([(
            "run".to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: include_str!("../protobuf/my_job_args.proto").to_string(),
                result_proto: include_str!("../protobuf/my_result.proto").to_string(),
                description: Some("メイン実行メソッド".to_string()),
                output_type: proto::jobworkerp::data::StreamingOutputType::Both as i32,
                ..Default::default()
            },
        )])
    }

    fn set_cancellation_token(&mut self, token: CancellationToken) {
        // host が job ごとに新しい token を渡してくる。古い token は
        // 既に完了/キャンセルされた job のものなので上書きしてよい。
        self.token = Some(token);
    }

    fn load(&mut self, _settings: Vec<u8>) -> FfiFuture<Result<(), String>> {
        async move { Ok(()) }.into_ffi()
    }

    fn run(
        &mut self,
        _args: Vec<u8>,
        metadata: Vec<(String, String)>,
        _using: Option<String>,
    ) -> FfiFuture<(Result<Vec<u8>, String>, Vec<(String, String)>)> {
        // self の状態は事前に clone で取り出す(future が 'static のため)
        let token = self.token.clone();
        let handle = self.rt.handle().clone();

        // plugin runtime に spawn → JoinHandle を await して host に橋渡し
        let join = handle.spawn(async move {
            match token {
                Some(t) => tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {
                        (Ok(b"done".to_vec()), metadata)
                    }
                    _ = t.cancelled() => {
                        (Err("cancelled".to_string()), metadata)
                    }
                },
                None => {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    (Ok(b"done".to_vec()), metadata)
                }
            }
        });

        async move {
            match join.await {
                Ok(out) => out,
                Err(e) => (Err(format!("join error: {e}")), Vec::new()),
            }
        }
        .into_ffi()
    }
}
```

## ストリーミング: `run_stream(output_sender)`

V2 は V1 の pull 型 (`begin_stream` → `receive_stream` を host が loop で poll)
ではなく、push 型のストリーミング API を採用しています。

```rust
fn run_stream(
    &mut self,
    args: Vec<u8>,
    metadata: Vec<(String, String)>,
    using: Option<String>,
    // host が作成して plugin に渡す出力 channel。plugin は send() で
    // チャンクを送信する。送信せず future を完了すれば自動的に
    // sender が drop され、host 側で End trailer が生成される。
    output: tokio::sync::mpsc::Sender<Vec<u8>>,
) -> FfiFuture<Result<Vec<(String, String)>, String>>;
```

- **チャンク送信**: plugin は `output.send(chunk).await?` で逐次送信
- **終了**: plugin の future が `Ok(final_metadata)` で resolve すると、
  host は `final_metadata` を `End` trailer に詰めて stream を閉じる
- **エラー時**: チャンクを 1 つも送信していなければ `Err(message)` は
  `run_stream` 全体の `Err` として host に伝播し、ジョブは失敗扱いになる。
  既に 1 つ以上送信した後の `Err` は End trailer を流して終了し、エラーは
  ログに残る(`ResultOutputItem` に in-band エラー型が無いため)
- **キャンセル**: host が future を drop すると plugin 側の `output` の
  受信側も drop された状態になり、`output.send` が Err を返すか
  `token.cancelled()` の発火を観測することで cooperative cancel が成立

### 実装例

```rust
fn run_stream(
    &mut self,
    _args: Vec<u8>,
    metadata: Vec<(String, String)>,
    _using: Option<String>,
    output: tokio::sync::mpsc::Sender<Vec<u8>>,
) -> FfiFuture<Result<Vec<(String, String)>, String>> {
    let token = self.token.clone();
    let handle = self.rt.handle().clone();

    let join = handle.spawn(async move {
        for i in 0..5 {
            let tick = tokio::time::sleep(Duration::from_millis(100));
            let cancelled = match &token {
                Some(t) => tokio::select! {
                    _ = tick => false,
                    _ = t.cancelled() => true,
                },
                None => { tick.await; false }
            };
            if cancelled { return Err("cancelled".to_string()); }
            if output.send(format!("chunk-{i}").into_bytes()).await.is_err() {
                // host が受信を中断した
                return Err("output channel closed".to_string());
            }
        }
        Ok(metadata)
    });

    async move {
        match join.await {
            Ok(r) => r,
            Err(e) => Err(format!("join error: {e}")),
        }
    }
    .into_ffi()
}
```

### 互換性ノート

`tokio::sync::mpsc::Sender<T>` は `CancellationToken` と同じ理由で
runtime 非依存です(詳細は[制約 #3](#3-キャンセルは-cancellationtoken-のみ))。
host runtime で作成した sender を plugin runtime 上で `.send().await`
しても問題ありません。`tokio` クレートのバージョンは host と plugin で
完全一致させてください。

## ビルドとデプロイ

V1 と同じです。`cargo build --release` で `.so` を生成し、
`PLUGINS_RUNNER_DIR` 環境変数で指定したディレクトリに配置します。
host はロード時にまず V2 シンボル (`load_multi_method_plugin_v2`) を探し、
存在しなければ V1 (`load_multi_method_plugin`)、それも無ければ legacy
(`load_plugin`) を試します。

## V1 から V2 への移行

V1 ([プラグイン開発 (V1)](./plugin-development.md)) は維持されており、
移行は強制ではありません。V2 を選ぶ動機は前述の比較表のとおりです。
移行時の主な書き換え:

| V1 | V2 |
|----|----|
| `fn run(&mut self, ...) -> (Result<Vec<u8>>, HashMap<String, String>)` | `fn run(&mut self, ...) -> FfiFuture<(Result<Vec<u8>, String>, Vec<(String, String)>)>` |
| `metadata: HashMap<String, String>` | `metadata: Vec<(String, String)>` (ABI 安定のため) |
| `Result<Vec<u8>>` (anyhow) | `Result<Vec<u8>, String>` (ABI 安定のため) |
| `fn begin_stream` + `fn receive_stream` | `fn run_stream(.., output: Sender<Vec<u8>>) -> FfiFuture<Result<Vec<(String, String)>, String>>` |
| `fn cancel(&mut self) -> bool` | `fn set_cancellation_token(&mut self, token)` + `tokio::select!` で監視 |
| `self.rt.block_on(async {...})` | `self.rt.handle().spawn(async {...})` + `JoinHandle::await` |

リファレンス実装: [`plugins/cancel_test/src/lib.rs`](https://github.com/jobworkerp-rs/jobworkerp-rs/blob/main/plugins/cancel_test/src/lib.rs)
