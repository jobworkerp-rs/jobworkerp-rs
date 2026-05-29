# JobWorkerP プラグイン開発ガイド (V2 / async-ffi)

このガイドでは **V2 プラグイン (`PluginV2` trait と `register_plugin_v2!`
proc マクロ)** を使った JobWorkerP プラグインの作成方法を説明します。V2
プラグインは `load_multi_method_plugin_v2` という FFI シンボル経由でロード
されます。V1 ([プラグイン開発 (V1)](./plugin-development.md)) と V2 は同一
ホスト上で並存できます。

> **注意**: V1 ガイドの
> [サーバー安定性に関する警告](./plugin-development.md#概要) は V2 でも
> そのまま適用されます (プラグイン内の panic がホスト全体をクラッシュさ
> せます)。V2 で 1 点追加: `tokio::runtime::Runtime` の構築は `new()` で
> はなく `Err` を返せる `load()` 内で行ってください。

## なぜ V2 か

V1 (`MultiMethodPluginRunner`) は同期 trait で、長時間ブロックする `run()`
の途中にタイムアウトが発生してもホスト側のロックを回収できず、wrapper
インスタンスを破棄するしかありませんでした。V2 では:

| 観点 | V1 | V2 |
|------|----|----|
| async サーフェス | `fn` (同期) | `async fn` (`PluginV2` トレイト、マクロ展開で `FfiFuture<T>` に変換) |
| キャンセル | `cancel()` / `is_canceled()` | `CancelToken::cancelled().await` を `select!` で監視 |
| ストリーミング | `begin_stream` + pull 型 `receive_stream` | `run_stream(args, ..., output: HighLevelSink)` で push 型 |
| タイムアウト時の lock 解放 | wrapper を破棄 | future drop で即解放、wrapper 再利用可 |
| FFI シンボル | `load_multi_method_plugin` | `load_multi_method_plugin_v2` |

新規プラグインでは V2 を選択することを推奨します。

## アーキテクチャ: 2 層構造の API

プラグイン作成者は **高レベル** `PluginV2` トレイトを通常の Rust 型
(`Vec<u8>`, `HashMap<String, String>`, `Result<_, String>`) で実装します。
`register_plugin_v2!` proc マクロが **低レベル** `#[repr(C)]` FFI サーフェス
を自動生成します:

```text
impl PluginV2 for MyPlugin { async fn run(...) -> ... }
        |
        | register_plugin_v2!(MyPlugin, MyPlugin::new());
        v
static PLUGIN_VTABLE: PluginVtable = PluginVtable {
    name:  __thunk_name,   // extern "C" fn(*mut ()) -> FfiBytes
    run:   __thunk_run,    // extern "C" fn(...) -> FfiFuture<V2RunOutcome>
    ...
};
load_multi_method_plugin_v2() -> PluginInstanceRaw { state, vtable }
```

すべて `#[repr(C)]` で揃えたため:

- ホストとプラグインは **rustc バージョンを独立に選択可能** (trait オブ
  ジェクトの vtable は FFI 境界を超えません)
- ホストとプラグインは **`tokio` / `tokio-util` バージョンを独立に選択可
  能** (それらの型はランタイム内に閉じ、`FfiCancellationToken` /
  `OutputSink` のみが境界を渡る)
- exact pin が必要なのは `async-ffi` のみ (`FfiFuture<T>` レイアウトの
  一致が必要)

## V2 固有の制約 (必読)

### 1. 自前の tokio runtime を持つこと

プラグインは `cdylib` としてロードされるため、リンクされた `tokio` は
**ホスト側とは別の `thread_local!` ランタイムコンテキスト** を持ちます。
ホスト future 内で直接 `tokio::time::sleep` / `Handle::current()` を呼ぶと
`there is no reactor running` で panic します。

V2 プラグインは以下を行ってください:

1. 自前の `tokio::runtime::Runtime` を `Builder::new_multi_thread()` で
   構築し、**最低 1 worker thread** を確保する
   - `new_current_thread()` は `block_on()` 内でしかタスクを進めないため
     `handle.spawn()` で投入したタスクが永久に進まず、cooperative cancel
     も成立しません
2. `handle.spawn(...)` で実際の async work を投入する
3. spawned `JoinHandle` を await することで結果を高レベル trait の戻り値
   経由でホストへ橋渡しする (`cancel_test` プラグインが手本)

### 2. `async fn run/run_stream` の戻り値 future は `Send + 'static`

マクロが内部で `async fn` ボディを `FfiFuture<T>` (`Send + 'static`) に
変換します。状態は `async move {}` に **clone して move** するか、
`Arc<Mutex<...>>` で共有してください。`&mut self` 借用は持ち越せません。

### 3. キャンセルは `CancelToken` のみ

V2 では `cancel()` / `is_canceled()` メソッドはありません。代わりにホスト
が `set_cancellation_token(token)` で `CancelToken` を 1 ジョブにつき 1 回
(run/run_stream の前に) 渡します。プラグインは spawn したタスクで
`token.cancelled().await` を `tokio::select!` で監視してください。

`CancelToken` は内部に `FfiCancellationToken` を持ち、その内部は
`Arc<AtomicBool> + waker map` です。**tokio runtime には依存しない** ため、
ホストはどのスレッドからでも `cancel()` を呼べ、プラグイン側の
`cancelled().await` は即座に解決します。

### 4. timeout 時の挙動

ホストの `should_detach_on_timeout()` は V2 プラグインで `false` を返すた
め、timeout 時にホスト側の `FfiFuture` は drop されますが wrapper は再利
用されます。**プラグイン runtime 上で spawn したタスクは引き続き走り**、
自然完了か `token.cancelled()` 観測まで継続します。

ホストは timeout と同時に `token.cancel()` を呼び、プラグイン側リソース
を早期解放するよう設計されています。

### 5. `Drop` で tokio I/O を呼ばない

プラグイン構造体の `Drop` 内で `tokio::time::sleep` 等の tokio I/O を
呼ばないでください。host runtime が shutdown 中に drop が走るケースが
あり、runtime context 外で tokio API を呼ぶと panic します。

### 6. pin が必要なのは `async-ffi` のみ

exact pin が必要な依存は `async-ffi = "=0.5.0"` だけです。tokio /
tokio-util / rustc はホストとプラグインで独立に動かせます。以前の
Constraint 6 はこの 1 行に集約されました。

## FFI 安全性契約

proc マクロが unsafe な詳細を隠蔽しますが、その下にある契約は変わりません:

1. **panic は FFI 境界で捕捉される** — 各 thunk は同期部分を
   `std::panic::catch_unwind` で、async 部分を
   `futures::FutureExt::catch_unwind` で包みます。プラグイン作成者の
   panic は `FfiResult::Err` に変換されます。foreign-exception
   handling 経由の panic は catch できず、Rust 1.81+ では abort される
   ため、panicking パスは短く明示的に保ってください。
2. **`mem::forget` は禁止** — `FfiBytes` / `FfiVec` / `OutputSink` /
   `FfiCancellationToken` / `PluginInstance` は Drop で対応する
   allocator フックが起動するため、所有を捨てるときは move のみで。
3. **Sink の並行性** — `OutputSink::send` (`HighLevelSink::send` 経由)
   は 1 プラグインタスク内で順次呼んでください。in-flight な `send`
   future がすべて完了してから sink を drop してください (spawn した
   future が末尾で drop する形なら自然に守れます)。
4. **アロケータ** — `FfiBytes` は自前の解放関数を埋め込んでいるため、
   プラグインが `#[global_allocator] = mimalloc`、ホストが system
   allocator という構成でも安全に動作します。性能面では揃える方が
   望ましいです。

## ステップ・バイ・ステップ ガイド

### 1. Cargo.toml

```toml
[package]
name = "my_plugin"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
# V2 プラグイン ABI 定義は jobworkerp-rs ホストと jobworkerp-client SDK
# で共有される `jobworkerp-plugin-abi` crate に集約されています。
# jobworkerp-rs リポジトリから git 経由で取得してください。
jobworkerp-plugin-abi = { git = "https://gitea.sutr.app/jobworkerp-rs/jobworkerp-rs.git", branch = "main", package = "jobworkerp-plugin-abi" }
jobworkerp-plugin-abi-macros = { git = "https://gitea.sutr.app/jobworkerp-rs/jobworkerp-rs.git", branch = "main", package = "jobworkerp-plugin-abi-macros" }

# `MethodSchema` を encode するための proto crate (host runtime proto、
# jobworkerp-client proto、自前 proto のいずれでも可)。
proto       = { path = "../../proto" }
prost       = "0.14"

anyhow      = "1.0"
async-trait = "0.1"
tokio       = { version = "1", features = ["full"] }
```

`jobworkerp-plugin-abi` は `async-ffi` / `async-trait` / `futures` /
`prost` を re-export しているため、proc マクロは
`::jobworkerp_plugin_abi::*` 経由でこれらを参照します。プラグイン作者は
上記以外を直接依存に追加する必要はありません。`register_plugin_v2!`
マクロは `jobworkerp-plugin-abi-macros` に含まれます。

### 2. Protobuf 定義と build.rs

V1 と同じです (`runner_settings_proto` / `method_proto_map` の payload
は同一)。[V1 ガイドの「Protobuf 定義」](./plugin-development.md#2-protobuf-の定義)
を参照してください。

### 3. プラグイン実装

```rust
use jobworkerp_plugin_abi::v2::{CancelToken, HighLevelSink, PluginV2};
use jobworkerp_plugin_abi_macros::register_plugin_v2;
use prost::Message;
use proto::DEFAULT_METHOD_NAME;
use proto::jobworkerp::data::MethodSchema;
use std::collections::HashMap;
use std::time::Duration;

pub struct MyPlugin {
    rt: tokio::runtime::Runtime,
    token: Option<CancelToken>,
}

impl Default for MyPlugin {
    fn default() -> Self { Self::new() }
}

impl MyPlugin {
    pub fn new() -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("plugin runtime");
        Self { rt, token: None }
    }
}

#[async_trait::async_trait]
impl PluginV2 for MyPlugin {
    fn name(&self) -> String { "MyPlugin".to_string() }
    fn description(&self) -> String { "Sample V2 plugin".to_string() }
    fn settings_schema(&self) -> String { String::new() }
    fn method_proto_map(&self) -> HashMap<String, Vec<u8>> {
        // `PluginV2` は schema を protobuf-encoded bytes でやり取りする
        // ため、ABI crate は proto に依存しません。`prost::Message` の
        // `encode_to_vec()` で encode してください。
        HashMap::from([(
            DEFAULT_METHOD_NAME.to_string(),
            MethodSchema::default().encode_to_vec(),
        )])
    }
    // `method_json_schema_map` のデフォルトは `None`、ホスト側で
    // `method_proto_map` から JSON schema を自動生成します。

    fn set_cancellation_token(&mut self, token: CancelToken) {
        self.token = Some(token);
    }

    async fn load(&mut self, _settings: Vec<u8>) -> Result<(), String> { Ok(()) }

    async fn run(
        &mut self,
        _args: Vec<u8>,
        metadata: HashMap<String, String>,
        _using: Option<String>,
    ) -> (Result<Vec<u8>, String>, HashMap<String, String>) {
        let token = self.token.clone();
        let handle = self.rt.handle().clone();
        let join = handle.spawn(async move {
            match token {
                Some(t) => tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(500)) => {
                        (Ok(b"done".to_vec()), metadata)
                    }
                    _ = t.cancelled() => {
                        (Err("cancelled".to_string()), metadata)
                    }
                },
                None => {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    (Ok(b"done".to_vec()), metadata)
                }
            }
        });
        join.await.unwrap_or_else(|e| (Err(format!("join: {e}")), HashMap::new()))
    }

    async fn run_stream(
        &mut self,
        _args: Vec<u8>,
        metadata: HashMap<String, String>,
        _using: Option<String>,
        output: HighLevelSink,
    ) -> Result<HashMap<String, String>, String> {
        let token = self.token.clone();
        let handle = self.rt.handle().clone();
        let join = handle.spawn(async move {
            for i in 0..5u32 {
                if let Some(t) = &token {
                    if t.is_cancelled() { return Err("cancelled".to_string()); }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                output.send(format!("chunk {i}").into_bytes()).await
                    .map_err(|e| format!("sink closed: {e}"))?;
            }
            Ok(metadata)
        });
        join.await.unwrap_or_else(|e| Err(format!("join: {e}")))
    }
}

register_plugin_v2!(MyPlugin, MyPlugin::new());
```

マクロの形式は `register_plugin_v2!(PluginType, init_expr)` です。
init 式は `load_multi_method_plugin_v2` 呼び出し時 (= プラグイン論理
インスタンス生成時) に 1 回ずつ評価されます。環境変数読み込みなどの
fallible な初期化もそのまま書けます。

## 旧 V2 ビルドからの移行

旧 V2 トレイトオブジェクトインターフェース
(`Box<dyn MultiMethodPluginRunnerV2>`) に対してビルドされたプラグインは、
新 API に対して再ビルドする必要があります。FFI シンボル名は変わりません
が、戻り値型が変わりました: ホストは `PluginInstanceRaw { state, vtable }`
を期待し、旧 `Box<dyn Trait>` ペイロードは vtable ヘッダの sanity check で
reject されます。V2 プラグインリポジトリは以下の対応をしてください:

1. `jobworkerp-runner` (および `jobworkerp-client`) 依存を本作業を含む
   commit に更新する
2. `impl MultiMethodPluginRunnerV2 for ...` を `impl PluginV2 for ...`
   に書き換える (上記コード例参照)
3. `extern "C" fn load_multi_method_plugin_v2(...)` を
   `register_plugin_v2!(MyPlugin, MyPlugin::new());` に置換する

`llama-cpp-plugin` は現状唯一の V2 プラグインなので、本変更とリリースを
連動させる必要があります。

## 性能

ストリーミングのホットパスは
`cargo run --release --bin sink_throughput -p jobworkerp-runner`
(`runner/benches/sink_throughput.rs`) で計測しました:

| 経路 | 10000 × 10 bytes |
|------|------------------|
| Baseline `tokio::mpsc::Sender::send` | 約 2.7 ms |
| `OutputSink::send_raw` (V2) | 約 2.5 ms |

`OutputSink::send_raw` は FFI 境界用に定めた 1.5x 予算内に収まっており、
今回の計測では tokio ベースラインよりも僅かに高速でした。将来この比率が
予算を超えた場合は、毎回 `FfiFuture` を allocate する現在の設計を
`try_send` + 書き込み可能通知方式に切り替えてください。

## 参考実装

`plugins/cancel_test/src/lib.rs` に完全な動作例があります: 同期メタデータ
の実装、cooperative cancel、Drop 契約を守ったストリーミング、
`register_plugin_v2!` の使用がすべて含まれています。
