# jobworkerp-rs のストリーミング機能

このドキュメントでは、jobworkerp-rsのストリーミング機能と、各ストリーミングタイプの動作の違いについて説明します。

## 概要

jobworkerp-rsは、`run_stream()`メソッドを実装したRunnerに対してストリーミング実行をサポートしています。ストリーミングにより、実行終了時に単一の結果を返すのではなく、結果を逐次的に出力できます。以下のようなユースケースで特に有用です：

- トークンを逐次生成するLLMレスポンス
- 長時間実行され、時間経過とともに出力を生成するコマンド
- リアルタイムの進捗更新

## StreamingType

`StreamingType`列挙型は、ジョブ実行時のストリーミング処理方法を制御します。ジョブをエンキューする際に指定します。

### STREAMING_TYPE_NONE (0)

デフォルトモード。ストリーミングを使用しません。

- Runnerの`run()`メソッドが呼ばれます（`run_stream()`ではない）
- ジョブ完了後、単一の`JobResult`として結果が返されます
- `ResponseType::Direct`のWorkerの場合、エンキュー呼び出しは完了までブロックします

### STREAMING_TYPE_RESPONSE (1)

クライアント向けの完全ストリーミングモード。

- Runnerの`run_stream()`メソッドが呼ばれます
- 結果はpub/sub経由で`ResultOutputItem`メッセージとしてクライアントにストリーミングされます
- クライアントは生成された`Data`チャンクを受信し、最後に`End`トレーラーを受信します
- `ResponseType::Direct`のWorkerの場合、`EnqueueForStream`および`EnqueueWithClientStream`はenqueue成功後に`x-job-id-bin`レスポンスヘッダーを返し、実行完了をサーバーストリームで待機します
- 最終`JobResult`が存在する実行失敗・キャンセルはストリームエラーで終了し、gRPC trailerの`x-job-result-bin`からその結果を取得できます。Redisタイムアウトなどの結果待機失敗では最終結果が存在しないため、`x-job-result-bin` trailerは送信されません
- `queue_type=DbOnly`と`response_type=Direct`の組み合わせは`INVALID_ARGUMENT`で拒否されます
- `request_streaming=true`（レガシーのbooleanフィールド）と互換性があります

### STREAMING_TYPE_INTERNAL (2)

ワークフローオーケストレーション用の内部ストリーミングモード。

- Runnerの`run_stream()`メソッドが内部的に呼ばれます
- 結果は`RunnerSpec::collect_stream()`を使用して収集されます
- 最終的な集約結果は`ResultOutputItem::FinalCollected`として送信されます
- **重要な動作**: `ResponseType::Direct`のWorkerでもエンキューは即座に返ります
- 呼び出し側がストリームの購読と結果の収集を担当します

このモードは以下のようなワークフローステップ向けに設計されています：
1. ストリーミング対応Runner（例：逐次トークン生成するLLM）の結果をworkflow上でもリアルタイムに活用したい
2. 次のワークフローステップ用に最終集約結果を単一チャンクとして取得したい
3. ローカルLLMなどの重いリソースに対してWorkerプーリング（`use_static=true`）を維持したい

## 動作マトリクス

| StreamingType | ResponseType | エンキュー動作 | 結果配信 |
|---------------|--------------|----------------|----------|
| None | Direct | 完了までブロック | 単一JobResult |
| None | NoResult | 即座に返る | Listen/store経由 |
| Response | Direct | JobId headerを即時返却 | pub/sub経由ストリーム |
| Response | NoResult | 即座に返る | pub/sub経由ストリーム |
| Internal | Direct | **即座に返る** | ストリーム + FinalCollected |
| Internal | NoResult | 即座に返る | ストリーム + FinalCollected |

注意: `Internal`モードは`ResponseType`に関係なく常に即座に返ります。これにより、データが発行される前に呼び出し側がストリームを購読できます。

## ResultOutputItemメッセージタイプ

ストリーミングモードを使用する場合、結果は`ResultOutputItem`メッセージとして配信されます：

```protobuf
message ResultOutputItem {
  oneof item {
    bytes data = 1;           // 逐次データチャンク
    Trailer end = 2;          // ストリーム終了マーカー
    bytes final_collected = 3; // 集約結果（Internalモード用）
  }
}
```

- **Data**: ストリーミング出力の個別チャンク
- **End**: ストリーム終了マーカー。メタデータと、失敗した場合のエラー（`Trailer.error`）を持ちます
- **FinalCollected**: `collect_stream()`による集約結果（Internalモードのみ）

## ストリームエラー契約

ストリームが失敗で終わったかどうかは、End の `Trailer.error` で判定します。すべての Runner と、worker・app 層が送る End が同じ契約に従います。

```protobuf
message Trailer {
  map<string, string> metadata = 1;
  StreamError error = 2;  // 失敗したときだけ設定される
}

message StreamError {
  StreamErrorCode code = 1;
  string message = 2;  // 最大 4096 UTF-8 bytes
  string origin = 3;   // 失敗を検出したコンポーネント
  ProblemDetails problem = 4;  // コンポーネントが報告した構造化されたエラー
}

message ProblemDetails {  // RFC 9457 の形。ワークフローのエラーと同じ
  string type = 1;
  int32 status = 2;
  optional string title = 3;
  optional string detail = 4;  // 最大 4096 UTF-8 bytes
  optional string instance = 5;
}
```

- **`error` のない End は正常完了**です。`error` があれば、`code` の値に関係なく失敗として扱います（未知の値も失敗です）。
- **判定基準は非ストリーム実行と同じ**です。非ストリームの実行が失敗（エラー）を返す条件のときだけ、End に `error` が設定されます。結果データとしての失敗は対象外で、従来どおり Data で返ります。例: HTTP の 4xx/5xx、GRPC の下流サービスの非 OK status、COMMAND の非 0 終了（`treat_nonzero_as_error=false` のとき）、MCP ツールの `is_error`、WORKFLOW の Faulted / Cancelled の結果。
- 失敗時も、Runner がこれまで送っていた Data（エラー内容を入れた `CommandResult` や Faulted の `WorkflowResult` など）は引き続き送られます。
- grpc-front は End を加工せずにストリームの中で配信します。gRPC のエラー status には変換しません。
- `problem` は、WORKFLOW のジョブがワークフローのエラーで失敗したときに設定されます。そのエラーの type、status、title、detail、instance が入り、親のワークフローは同じエラーを送出します。`problem` を使わない利用者は `code` だけで判定できます。
- 非ストリームのジョブが失敗した場合も、同じ `StreamError` が `JobResultData.error` に入ります（[gRPC Direct API](grpc-direct-api.md) を参照）。

### code

| `StreamErrorCode` | 意味 |
|---|---|
| `STREAM_ERROR_CODE_EXECUTION_FAILED` | Runner の実行失敗（コマンドの起動失敗、エラー扱いの非 0 終了、plugin の失敗、workflow の実行エラーなど） |
| `STREAM_ERROR_CODE_TIMEOUT` | 時間制限の超過（ストリームの idle timeout など） |
| `STREAM_ERROR_CODE_CANCELLED` | jobworkerp によるキャンセル（ジョブのキャンセル要求） |
| `STREAM_ERROR_CODE_TRANSPORT_FAILED` | 下流サービスとの通信の途中失敗 |
| `STREAM_ERROR_CODE_INVALID_ARGUMENT` | 実行開始時に検出した不正な入力 |
| `STREAM_ERROR_CODE_PROVIDER_STREAM_FAILED` | LLM プロバイダのストリームの中断 |
| `STREAM_ERROR_CODE_UNSPECIFIED` | 分類できない失敗（失敗として扱う） |

### origin

| 生成元 | origin |
|---|---|
| Runner（idle timeout など Runner 固有の処理を含む） | Runner 名（`COMMAND`、`HTTP_REQUEST`、`GRPC`、`LLM`、`WORKFLOW`、`SANDBOX`、plugin 名、MCP サーバ名） |
| worker / app 層（ストリーム開始前に失敗したジョブの End、worker 自身の job timeout、実行前やリトライ中に取り消されたジョブなど） | `WORKER` |

ストリーム開始前に失敗したジョブの End には、worker が記録した失敗の分類（`JobResultData.error`）があればそれがそのまま入ります。この場合の origin は、失敗を検出した Runner 名です。

### ResultStatus への対応

`StreamError` から `ResultStatus` を求める場合（FunctionService の結果、client の結果など）は次の対応を使います。ストリーム開始後はデータが届いているためリトライできないので、リトライ対象の失敗はリトライ上限到達と同じ `MAX_RETRY` になります。

| `StreamErrorCode` | `ResultStatus` |
|---|---|
| `CANCELLED` | `CANCELLED` |
| `INVALID_ARGUMENT` | `OTHER_ERROR` |
| それ以外（`UNSPECIFIED`・未知の値を含む） | `MAX_RETRY` |

### 互換用の metadata（deprecated）

移行期間中は、`error` に加えて次の metadata も書き込まれます。どちらも deprecated で、将来削除されます。新しい client は `Trailer.error` を使ってください。

- `metadata["stream_error"]`: JSON v1（`{"version":1,"code":"EXECUTION_FAILED","message":...,"origin":...}`、最大 4096 bytes）。`code` は enum 名から `STREAM_ERROR_CODE_` を除いた文字列です。すべての失敗 End に書き込まれます。
- `metadata["jobworkerp.plugin.run_stream_error"]`: plugin の失敗と idle timeout（SANDBOX 以外）で書き込まれます。値はエラーメッセージです。

読み取り側は次の優先順で判定します。

1. `error` があれば失敗。
2. `metadata["stream_error"]` があれば失敗。不正な形式でも失敗（`UNSPECIFIED`）。
3. `metadata["jobworkerp.plugin.run_stream_error"]` があれば失敗（`EXECUTION_FAILED`、origin `PLUGIN`）。
4. どれもなければ正常完了。

この2つのキーは予約キーです。ジョブの request metadata などから End の metadata に入っても、正常 End では除去され、失敗 End では正しい値で上書きされます。そのため plugin が正常終了時に返す metadata に予約キーを入れても失敗にはなりません。plugin が失敗を伝える手段は `run_stream` の `Err` だけです。

## 使用例

### クライアント向けストリーミング（Responseモード）

```rust
// Responseストリーミングでエンキュー
let (job_id, _result, stream) = job_app.enqueue_job(
    metadata,
    Some(&worker_id),
    None,
    args,
    None,
    0,
    Priority::Medium as i32,
    timeout,
    None,
    StreamingType::Response,
    None,
).await?;

// ストリームを消費
while let Some(item) = stream.next().await {
    match item.item {
        Some(Item::Data(data)) => { /* チャンクを処理 */ }
        Some(Item::End(_)) => break,
        _ => {}
    }
}
```

### ワークフロー内部ストリーミング（Internalモード）

```rust
// Internalストリーミングでエンキュー - 即座に返る
let (job_id, _result, _stream) = job_app.enqueue_job(
    metadata,
    Some(&worker_id),
    None,
    args,
    None,
    0,
    Priority::Medium as i32,
    timeout,
    None,
    StreamingType::Internal,
    None,
).await?;

// 別途ストリームを購読
let stream = pubsub_repo.subscribe_result_stream(&job_id, timeout_ms).await?;

// 結果を収集
let mut final_result = None;
while let Some(item) = stream.next().await {
    match item.item {
        Some(Item::Data(data)) => { /* UIに転送または収集 */ }
        Some(Item::FinalCollected(data)) => {
            final_result = Some(data);
        }
        Some(Item::End(_)) => break,
        _ => {}
    }
}

// final_resultを次のワークフローステップで使用
```

### gRPCによるストリーミング結果の取得（ListenStream）

Workerで`broadcast_results`が有効で、ジョブがストリーミング（`streaming_type` = ResponseまたはInternal）を使用している場合、`JobResultService.ListenStream`を通じてストリーミング出力を取得できます。このメソッドは`Listen`と同じ`ListenRequest`を使用しますが、単一の`JobResult`ではなく`stream ResultOutputItem`を返します。

- `JobResult`メタデータ（ステータス、タイムスタンプ、出力等）はgRPCレスポンスヘッダー`x-job-result-bin`に格納されます（protobufエンコードされたバイナリ）。
- ストリーム本体では`Data`チャンク、`End`トレーラー、`FinalCollected`（Internalモードのみ）が配信されます。

```protobuf
// JobResultService内
rpc ListenStream(ListenRequest) returns (stream ResultOutputItem);
```

利用フロー:

```text
1. ジョブをエンキュー（例：JobService.Enqueue経由）
2. JobResultService.ListenStream(job_id, worker_id/worker_name, timeout) を呼び出し
3. レスポンスヘッダーの x-job-result-bin から JobResult メタデータを取得
4. ストリームを消費: Data チャンク → End トレーラー（Internalモードの場合は FinalCollected）
```

### ストリーミング/非ストリーミングのRPC対応関係

ストリーミングの有無に応じて使用するRPCの対応関係は以下の通りです：

| シナリオ | エンキューRPC | 結果取得RPC | 関連設定 |
|---------|-------------|------------|-----------|
| 非ストリーミング | `JobService.Enqueue`（`StreamingType::None`使用） | `JobResultService.Listen` または `FindListByJobId` | - |
| ストリーミング（単一クライアント） | `JobService.EnqueueForStream`（`StreamingType::Response`使用） | `EnqueueForStream`のレスポンスとして直接ストリーム返却 | (Job) `streaming_type` = Response |
| ストリーミング（複数クライアント） | `JobService.Enqueue` または `EnqueueForStream` | `JobResultService.ListenStream` | (Worker) `broadcast_results=true`, (Job) `streaming_type` = Response or Internal |
| **Client streaming + Direct** | **`JobService.EnqueueWithClientStream`** | **レスポンスストリーム直接** | Runner `require_client_stream=true` |
| **Client streaming + NoResult** | **`JobService.EnqueueWithClientStream`** | **`JobResultService.ListenStream`（別クライアント）** | Runner `require_client_stream=true`, (Worker) `broadcast_results=true` |

- ストリーミングジョブに対して`Listen`を呼び出すと、ストリーミング結果を単一の`JobResult`レスポンスにまとめられないため、エラーが返されます。代わりに`ListenStream`を使用してください。
- `EnqueueForStream`はレスポンスとして直接ストリームを返すため、リクエスト元クライアントは別途`ListenStream`を呼ぶ必要はありません。ただし、`broadcast_results=true`の場合は、他のクライアントが`ListenStream`を使って同じストリーミング結果を購読できます。
- `EnqueueWithClientStream`はジョブのエンキューとfeedデータ配信を単一の双方向ストリームに統合します。

## Workerプーリングとuse_static

Workerで`use_static=true`を使用する場合（例：ローカルLLM向け）、Runnerインスタンスはプールされ、ジョブ実行間で再利用されます。初期化コストが高いリソースにとって重要な機能です。

`StreamingType::Internal`は以下の方法でこのプーリング動作を維持します：
1. エンキュー時に既存の`worker_id`を使用（一時Workerを作成しない）
2. エンキューから即座に返る（Directレスポンスでブロックしない）
3. 呼び出し側が独立してストリーム購読を管理可能

これにより、ローカルLLMモデルなどの重いリソースがジョブごとに再初期化されることを防ぎます。

## EnqueueWithClientStream: クライアントストリーミング

`EnqueueWithClientStream` RPCは、ジョブのエンキューとクライアントストリーミングデータの配信を単一の双方向gRPCストリームに統合します。

### プロトコル定義

```protobuf
message ClientStreamRequest {
  oneof request {
    JobRequest job_request = 1;                   // 最初のメッセージ：ジョブエンキューリクエスト
    jobworkerp.data.FeedDataTransport feed_data = 2;  // 以降：feedデータチャンク
  }
}

// JobService 内
rpc EnqueueWithClientStream(stream ClientStreamRequest)
    returns (stream jobworkerp.data.ResultOutputItem);
```

### 利用フロー

#### Directモード（`response_type=Direct`）

送信クライアント自身が結果ストリームを直接受信します。

```text
Client                                    Server
  │                                          │
  │─── ClientStreamRequest(job_request) ────>│  ジョブ受付
  │<── [headers: x-job-id-bin]
  │<── ResultOutputItem(Data) ──────────────│  初期出力
  │─── ClientStreamRequest(feed_data) ──────>│  feedデータ
  │<── ResultOutputItem(Data) ──────────────│  中間出力
  │─── ClientStreamRequest(feed_data,        │
  │         is_final=true) ─────────────────>│  最終feedデータ
  │<── ResultOutputItem(End(trailer)) ──────│  ストリーム終了
```

初期ヘッダーの`x-job-id-bin`はenqueue成功を示します。成功時の出力とrunner metadataは`Data`および`End.metadata`で受信します。実行失敗・キャンセルでは、最終`JobResult`が存在する場合にのみストリームエラーのgRPC trailerへ`x-job-result-bin`が設定されます。Redisタイムアウトなどの結果待機失敗では、このtrailerは送信されません。

#### NoResultモード（`response_type=NoResult`）

送信クライアントはデータ送信のみ。別クライアントが`ListenStream`で結果を受信します。

```text
Feed Client                               Server
  │                                          │
  │─── ClientStreamRequest(job_request) ────>│  ジョブ受付
  │<── [headers: x-job-id-bin]               │
  │─── ClientStreamRequest(feed_data) ──────>│  feedデータ
  │─── ClientStreamRequest(feed_data,        │
  │         is_final=true) ─────────────────>│  最終feedデータ
  │<── ResultOutputItem(End(trailer)) ──────│  feed完了、ストリーム終了

Listener Client                            Server
  │─── ListenStream(job_id, worker_id) ────>│  結果を購読
  │<── ResultOutputItem(Data) ──────────────│
  │<── ResultOutputItem(End(trailer)) ──────│
```

> **注意**: NoResultモードでは、feedクライアント側のgRPCレスポンスストリームは、すべてのfeedデータの送信が完了する（クライアントが`is_final=true`を送信する）まで終了しません。結果データは受信しませんが、feedクライアントはすべてのデータ送信を完了する必要があります。

### `require_client_stream` フラグ

クライアントストリーミング入力を必要とするRunnerは、`MethodSchema`で`require_client_stream=true`を設定する必要があります。このフラグは`check_worker_streaming`で検証されます：

- **EnqueueWithClientStream**: Runnerメソッドに`require_client_stream=true`が必要。falseの場合は拒否。
- **Enqueue / EnqueueForStream**: `require_client_stream=true`のRunnerを拒否（逆方向ガード）。これらのRunnerは`EnqueueWithClientStream`経由で呼び出す必要があります。

### データ転送メカニズム

- **Standaloneモード**: feedデータはプロセス内`mpsc`チャネル（`ChanFeedSenderStore`）で直接配信。gRPCハンドラは Dispatcher による feed チャネル登録を**待ちません**。クライアントストリームを即座に読み始めて `publish_feed` を呼び、Runner 未登録時はジョブごとの `JobFeedState.buffer` にチャンクをバッファします。登録時にホストは `drain_loop` で到着順に buffer を流すため、Runner プールが空くより前に送られた feed もロスしません。
- **Scalableモード**: feedデータはRedis List（`job_feed_buf:{job_id}`）にRPUSHで送信。Worker側のfeedブリッジがBLPOPで読み取ります。Listがバッファとして機能するため、メッセージロスは発生しません。

いずれのモードでも gRPC フロントエンドは、V2 ホストが `BoxStream` を返した時点(現在は plugin が最初の chunk を出す前)でレスポンスストリームを返します。feed forwarder は並行で動作するため、`use_static=true, concurrency=1` の Worker pool が前のジョブで占有されていても、レスポンス受信後に feed を送り始めるクライアントとデッドロックすることはありません。

### Plugin への in-band `is_final` 伝達 (ABI minor 1 以降)

V2 plugin ABI minor 1 で `OutputSinkWithFinal` が追加され、ホストは feed 由来の `is_final` フラグを plugin の feed receiver までそのまま運べるようになりました。新方式に乗りたい plugin は `PluginV2::setup_client_stream_channel_v2` を実装し、`(Vec<u8>, bool)` のチャンクを受け取ります。これにより `is_final == true` の到着を観測した時点で `run_stream` を決定論的に終了できます。

- **minor 0 で構築した plugin**(または v2 setup から `None` を返した場合): ホストは従来の `setup_client_stream_channel` 経路にフォールバック。plugin が EOF を認識するのは mpsc `Receiver` が `None` を返したときだけで、これは `is_final=true` を観測したホストが sink を drop したタイミングです。
- **minor 1 以降で構築した plugin で v2 setup が `Some` を返す**: ホストは各 feed チャンクを `OutputSinkWithFinal::send_raw_with_final(bytes, is_final)` で送信。plugin は `mpsc::Receiver<(Vec<u8>, bool)>` でフラグを観測し、receiver の close を待たずに `run_stream` から return できます。

minor 1 経路でも最後のチャンク送信後にホストは sink を drop するため、フラグを無視する plugin にも従来の EOF シグナルが届きます — 採用は完全にオプトインです。minor bump は前方互換なので、既存の `.so` ファイルはそのまま動きます。plugin 作者向けの詳細は [Plugin 開発 (V2) — feed チャンクへの in-band `is_final` 伝達](./plugin-development-v2.md) を参照してください。

### エラーケース

| ケース | gRPCステータス | タイミング |
|--------|---------------|----------|
| 最初のメッセージが`job_request`でない | `INVALID_ARGUMENT` | ストリーム開始時 |
| Workerが見つからない | `NOT_FOUND` | ストリーム開始時 |
| Runnerがクライアントストリーミング非対応 | `INVALID_ARGUMENT` | ストリーム開始時 |
| Runnerがストリーミング出力非対応 | `INVALID_ARGUMENT` | ストリーム開始時 |
| 最初のメッセージ後に`job_request`送信 | `INVALID_ARGUMENT` | feed中 |
| Runner 登録前に保留 feed が `PENDING_BUFFER_MAX` (1024) を超過 | 最古の非 final チャンクを破棄し warn ログ | feed中 |
| Runner ストリームの drain 中、`JobData.timeout` ms 間チャンクが流れない | Runner のキャンセルトークンが発火し、`error`（`TIMEOUT`）付きの End でストリーム終了 | 実行中 |
| ストリーム開始後の Runner の失敗 | gRPC status は OK のまま、`error` 付きの End で終了（[ストリームエラー契約](#ストリームエラー契約)） | 実行中 |
| Runner実行エラー | `ResultStatus`に応じたコード | 実行中 |
| クライアント切断（half-closeなし） | Runnerにキャンセル通知 | 任意 |

### 設定

| 環境変数 | デフォルト | 説明 |
|---------|----------|------|
| `JOB_QUEUE_FEED_DISPATCH_TIMEOUT` | `5000` | **Scalable** モードでは Redis List の TTL ベース (`ttl_secs = feed_dispatch_timeout * 2`)。**Standalone** モードでは feed 配信に使用しません — 事前登録バッファ (`PENDING_BUFFER_MAX = 1024`) でメモリを抑え、ストリーム drain の停止検出はジョブごとの `JobData.timeout` を idle window として扱います。 |

## 実装上の注意

### レースコンディションの防止

`Internal`モードでは、Workerがデータを発行する前に呼び出し側がpub/subストリームを購読できるよう、エンキューは即座に返ります。これにより以下のレースコンディションを防止します：
1. ジョブがエンキューされ、Workerが処理を開始
2. Workerが完了し、ストリームデータを発行
3. 呼び出し側が購読しようとするが、データは既に消失（pub/subはバッファリングしない）

### collect_stream()

ストリーミングを実装する各Runnerは、`RunnerSpec`トレイト実装で`collect_stream()`メソッドを提供する必要があります。このメソッドは：
1. `ResultOutputItem`のストリームを受信
2. Runnerタイプに適した方法でデータチャンクを集約/マージ
3. End が失敗を示していれば（[ストリームエラー契約](#ストリームエラー契約)）エラーを返却
4. 最終的な収集バイト列を返却

例えば、LLM Runnerはすべてのトークンチャンクを連結し、コマンドRunnerはstdout/stderrを適切にマージします。
