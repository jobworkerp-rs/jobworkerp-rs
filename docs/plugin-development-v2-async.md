# V2 Plugin 開発ガイド (async-ffi ベース)

V2 Plugin trait (`MultiMethodPluginRunnerV2`) は async-ffi クレートをベース
にした非同期プラグインインタフェースです。本ドキュメントは V2 プラグインを
書く外部開発者向けに、必須の制約と推奨パターンをまとめます。

## 概要

- **trait**: `jobworkerp_runner::runner::plugins::MultiMethodPluginRunnerV2`
- **FFI ローダシンボル**: `load_multi_method_plugin_v2`
- **戻り値型**: 各 async メソッドは `async_ffi::FfiFuture<T>` を返す
- **キャンセル**: `tokio_util::sync::CancellationToken` を host から受け取り、
  プラグイン側で `select!` で監視する
- **V1 との関係**: V2 trait は V1 (`MultiMethodPluginRunner`) と独立。
  既存 V1 プラグイン(`hello_runner`, `test_runner` 等)はそのまま動作

## 重要な制約 (必読)

### 1. 自前の tokio runtime を持つこと

プラグインは `crate-type = ["dylib"]` で動的にロードされます。dylib として
ビルドされたプラグイン内の `tokio` は host とは別個に `thread_local!` の
runtime context を保持するため、**プラグインの async 関数を host runtime 上
で直接 await すると `Handle::current()` が失敗し、"there is no reactor
running" で panic します**。

そのためプラグインは:

1. 自前の `tokio::runtime::Runtime` を保持する
2. `run()` などの async メソッドでは、その runtime に `handle.spawn(...)` で
   task を投入する
3. `FfiFuture` の中身は spawn された `JoinHandle` を `.await` する形にする

```rust
pub struct MyPlugin {
    rt: tokio::runtime::Runtime,
    token: Option<CancellationToken>,
}

impl MyPlugin {
    pub fn new() -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("failed to build plugin tokio runtime");
        Self { rt, token: None }
    }
}
```

### 2. multi-thread runtime を使うこと

`new_current_thread()` 版のランタイムは `rt.block_on(...)` の内側でしか
タスクを進めません。`handle.spawn(...)` だけではタスクが進行せず、結果として
キャンセルにも応答しません。**`new_multi_thread()` を最低 1 worker で使う
必要があります**。CPU バウンドな並列処理が不要なプラグインなら 1 worker で
十分です。

### 3. `FfiFuture<T>` は `Send + 'static` 制約

`FfiFuture<T>` は `Pin<Box<dyn Future<Output=T> + Send + 'static>>` 相当の
ABI 安定なラッパーです。そのため async block 内で `&mut self` を保持できま
せん。必要な状態は `async move { ... }` ブロックに **clone で move する**
か、`Arc<Mutex<_>>` 経由で共有してください。

```rust
fn run(
    &mut self,
    args: Vec<u8>,
    metadata: Vec<(String, String)>,
    _using: Option<String>,
) -> FfiFuture<(Result<Vec<u8>, String>, Vec<(String, String)>)> {
    // self の状態は事前に clone で取り出す
    let token = self.token.clone();
    let handle = self.rt.handle().clone();

    let join = handle.spawn(async move {
        // ここでは self を借りていない
        match token {
            Some(t) => tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(5)) =>
                    (Ok(Vec::new()), metadata),
                _ = t.cancelled() =>
                    (Err("cancelled".to_string()), metadata),
            },
            None => (Ok(Vec::new()), metadata),
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
```

### 4. CancellationToken でキャンセル

V2 では `cancel()` / `is_canceled()` メソッドは廃止されました。代わりに
host が `set_cancellation_token(token)` でキャンセルトークンを渡し、
プラグインは spawn したタスク内で `token.cancelled().await` を `select!`
で監視します。

```rust
fn set_cancellation_token(&mut self, token: CancellationToken) {
    self.token = Some(token);
}
```

**重要**: `CancellationToken` は内部的に `Arc<AtomicBool> + Notify` で
構成されており、tokio runtime に依存しません。よって host runtime で
`token.cancel()` を呼び、plugin runtime で `token.cancelled().await` で
待機する構成は正しく動作します。

### 5. timeout 時の挙動

host は wrapper に対する timeout を実装しています。timeout が発火すると
host 側の `FfiFuture` は drop され、内部の `JoinHandle` も drop されます。
ただし **plugin runtime 上で spawn された task はそのまま走り続け**、
自然完了か `token.cancelled()` を観測するまで継続します。

そのため、host は timeout と同時に `token.cancel()` を呼ぶことで、plugin
側のタスクも早期に終了させることができます。逆に、token をキャンセル
しないと plugin 側のタスクは指定された処理を完遂してから終了します。

これは設計上の選択です — 強制 abort は plugin に予期せぬリソースリークを
引き起こす可能性があり、cooperative cancellation のほうが安全です。

### 6. Drop で tokio I/O を呼ばない

プラグインの構造体の `Drop` 内では `tokio::time::sleep` 等の tokio I/O を
呼ばないでください。host runtime が shutdown 中に drop が走るケースがあり、
runtime context 外で tokio API を呼ぶと panic します。

### 7. ワークスペース依存を host と揃える

以下のクレートは FFI 境界をまたぐ型を提供するため、**host が pin している
バージョンと完全一致**させてください:

- `async-ffi` — `FfiFuture<T>` の表現
- `tokio` — `JoinHandle` 等(プラグイン内部でしか使わないが、host から
  渡される `CancellationToken` の trait bound を満たすために必要)
- `tokio-util` — `CancellationToken` の構造体レイアウト

Cargo.toml の例:

```toml
[dependencies]
proto = { path = "../../proto" }
jobworkerp-runner = { path = "../../runner" }

anyhow = { workspace = true }
async-ffi = { workspace = true }
tokio = { workspace = true }
tokio-util = { workspace = true }
tracing = { workspace = true }

[lib]
crate-type = ["dylib"]
```

## ストリーミング: `run_stream(output_sender)`

V2 は V1 の pull 型 (`begin_stream` → `receive_stream` を host が loop で
poll) ではなく、push 型のストリーミング API を採用しています。

```rust
fn run_stream(
    &mut self,
    args: Vec<u8>,
    metadata: V2Metadata,
    using: Option<String>,
    /// host が作成して plugin に渡す出力 channel。plugin は send() で
    /// チャンクを送信する。送信せず future を完了すれば自動的に
    /// sender が drop され、host 側で End trailer が生成される。
    output: tokio::sync::mpsc::Sender<Vec<u8>>,
) -> FfiFuture<Result<V2Metadata, String>>;
```

- **チャンク送信**: plugin は `output.send(chunk).await?` で逐次送信
- **終了**: plugin の future が `Ok(final_metadata)` で resolve すると、
  host は `final_metadata` を `End` trailer に詰めて stream を閉じる
- **エラー時**: `Err(message)` を返すと host は元の入力 metadata を End
  trailer に使う
- **キャンセル**: host が future を drop すると plugin 側の `output` の
  受信側も dropped 状態になり、`output.send` が Err を返すか
  `token.cancelled()` の発火を観測することで cooperative cancel が成立

実装例(`plugins/cancel_test/src/lib.rs`):

```rust
fn run_stream(
    &mut self,
    args: Vec<u8>,
    metadata: Vec<(String, String)>,
    _using: Option<String>,
    output: tokio::sync::mpsc::Sender<Vec<u8>>,
) -> FfiFuture<Result<Vec<(String, String)>, String>> {
    let token = self.token.clone();
    let total_ms = parse_sleep_ms(&args);
    let handle = self.rt.handle().clone();

    let join = handle.spawn(async move {
        let chunk_count = (total_ms / 100).max(1);
        for i in 0..chunk_count {
            let tick = tokio::time::sleep(Duration::from_millis(100));
            let cancelled = match &token {
                Some(t) => tokio::select! {
                    _ = tick => false,
                    _ = t.cancelled() => true,
                },
                None => { tick.await; false }
            };
            if cancelled { return Err("cancelled".to_string()); }
            if output.send(format!("{i}").into_bytes()).await.is_err() {
                return Err("output channel closed".to_string());
            }
        }
        Ok(metadata)
    });

    async move {
        match join.await {
            Ok(r) => r,
            Err(e) => Err(format!("plugin task join error: {e}")),
        }
    }
    .into_ffi()
}
```

### 互換性ノート

`tokio::sync::mpsc::Sender<T>` は内部実装が `Semaphore` ベースで
**tokio runtime 非依存** (`Handle::current()` を呼ばない) なので、
host runtime で作成した sender を plugin runtime 上で `.send().await`
しても問題ありません。これは `CancellationToken` と同じく
「FFI 境界を越えて渡せる tokio の同期プリミティブ」のひとつです。

ただし `tokio` クレートのバージョンは host と plugin で **完全一致**
させる必要があります(workspace pin)。

## 完全な実装例

`plugins/cancel_test/src/lib.rs` を参照してください。最小構成の V2 プラグ
インのリファレンス実装で、`run` (unary) と `run_stream` (streaming) の
両方の実装例を含みます。

## FFI シンボル定義

ローダ関数は `extern "C"` で `Box<dyn MultiMethodPluginRunnerV2 + Send + Sync>`
を返します:

```rust
use jobworkerp_runner::runner::plugins::MultiMethodPluginRunnerV2;

#[unsafe(no_mangle)]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn load_multi_method_plugin_v2()
    -> Box<dyn MultiMethodPluginRunnerV2 + Send + Sync>
{
    Box::new(MyPlugin::new())
}

#[unsafe(no_mangle)]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn free_multi_method_plugin_v2(
    ptr: Box<dyn MultiMethodPluginRunnerV2 + Send + Sync>,
) {
    drop(ptr);
}
```

`#[allow(improper_ctypes_definitions)]` は `Box<dyn Trait>` を C ABI 関数の
戻り値にすることへの警告抑制です。トレイトの vtable レイアウト自体は
Rust ABI に依存するため、host とプラグインは **完全に同一の rustc バー
ジョン・同一の workspace dependency** でビルドする必要があります。

## V1 から V2 への移行

V1 (`MultiMethodPluginRunner`) は維持されており、V2 への移行は強制では
ありません。V2 を選ぶ動機:

| 動機 | V2 の利点 |
|------|----------|
| **timeout 時のリソース回収** | host 側の write lock が即解放され、wrapper インスタンスが再利用可能 |
| **明示的 cancel API** | `CancellationToken` ベースの一貫したキャンセル経路 |
| **将来の async-ffi 改善** | 例: future drop 時の挙動が C ABI で定義されているため、安定性が高い |

V2 では `cancel()` / `is_canceled()` が廃止されているため、V1 で同名メソッ
ドを実装していた場合は `set_cancellation_token` + `tokio::select!` の
パターンに書き換える必要があります。
