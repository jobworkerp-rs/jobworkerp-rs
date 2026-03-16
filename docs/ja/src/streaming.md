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
- `ResponseType::Direct`のWorkerの場合、エンキュー呼び出しはブロックし、ストリームを返します
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
| Response | Direct | ブロック、ストリーム返却 | pub/sub経由ストリーム |
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
- **End**: オプションのメタデータを含むストリーム終了マーカー
- **FinalCollected**: `collect_stream()`による集約結果（Internalモードのみ）

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
- `EnqueueWithClientStream`はジョブのエンキューとfeedデータ配信を単一の双方向ストリームに統合し、`FeedToStream`のタイミング制約を解消します。

## Workerプーリングとuse_static

Workerで`use_static=true`を使用する場合（例：ローカルLLM向け）、Runnerインスタンスはプールされ、ジョブ実行間で再利用されます。初期化コストが高いリソースにとって重要な機能です。

`StreamingType::Internal`は以下の方法でこのプーリング動作を維持します：
1. エンキュー時に既存の`worker_id`を使用（一時Workerを作成しない）
2. エンキューから即座に返る（Directレスポンスでブロックしない）
3. 呼び出し側が独立してストリーム購読を管理可能

これにより、ローカルLLMモデルなどの重いリソースがジョブごとに再初期化されることを防ぎます。

## EnqueueWithClientStream: クライアントストリーミング

`EnqueueWithClientStream` RPCは、ジョブのエンキューとクライアントストリーミングデータの配信を単一の双方向gRPCストリームに統合します。非推奨の`FeedToStream` RPCの制約を解消します。

### FeedToStreamに対する利点

| FeedToStreamの制約 | EnqueueWithClientStream |
|-------------------|------------------------|
| `use_static=true`が必須 | 不要 |
| チャネル concurrency = 1 が必須 | 不要 |
| feed前にジョブが`Running`状態である必要 | 自動：エンキューとfeedが単一ストリーム |
| Enqueue + Feed が別RPC（レースコンディションリスク） | 単一の統合ストリーム |
| Scalableモード：Redis Pub/Sub（メッセージロスリスク） | Scalableモード：Redis List + BLPOP（メッセージロスなし） |

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
  │<── [headers: x-job-id-bin, x-job-result-bin]
  │<── ResultOutputItem(Data) ──────────────│  初期出力
  │─── ClientStreamRequest(feed_data) ──────>│  feedデータ
  │<── ResultOutputItem(Data) ──────────────│  中間出力
  │─── ClientStreamRequest(feed_data,        │
  │         is_final=true) ─────────────────>│  最終feedデータ
  │<── ResultOutputItem(End(trailer)) ──────│  ストリーム終了
```

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

- **Standaloneモード**: feedデータはプロセス内`mpsc`チャネル（`ChanFeedSenderStore`）で直接配信。gRPCハンドラは`tokio::sync::Notify`を使用してDispatcherのfeedチャネル登録を待機します。
- **Scalableモード**: feedデータはRedis List（`job_feed_buf:{job_id}`）にRPUSHで送信。Worker側のfeedブリッジがBLPOPで読み取ります。Listがバッファとして機能するため、メッセージロスは発生しません。

### エラーケース

| ケース | gRPCステータス | タイミング |
|--------|---------------|----------|
| 最初のメッセージが`job_request`でない | `INVALID_ARGUMENT` | ストリーム開始時 |
| Workerが見つからない | `NOT_FOUND` | ストリーム開始時 |
| Runnerがクライアントストリーミング非対応 | `INVALID_ARGUMENT` | ストリーム開始時 |
| Runnerがストリーミング出力非対応 | `INVALID_ARGUMENT` | ストリーム開始時 |
| feedチャネルディスパッチタイムアウト | `DEADLINE_EXCEEDED` | feed開始前 |
| 最初のメッセージ後に`job_request`送信 | `INVALID_ARGUMENT` | feed中 |
| Runner実行エラー | `ResultStatus`に応じたコード | 実行中 |
| クライアント切断（half-closeなし） | Runnerにキャンセル通知 | 任意 |

### 設定

| 環境変数 | デフォルト | 説明 |
|---------|----------|------|
| `JOB_QUEUE_FEED_DISPATCH_TIMEOUT` | `5000` | feedチャネル準備完了の最大待機時間（Standaloneモード）。Scalableモードでは Redis List の TTL ベースとして使用。 |

## [非推奨] FeedToStream: 実行中ストリーミングジョブへのデータ送信

> **⚠ 非推奨**: `FeedToStream`は非推奨です。代わりに`EnqueueWithClientStream`を使用してください。`FeedToStream`は将来のバージョンで削除される予定です。
>
> **重要**: Scalableモードでは`FeedToStream`は動作しません。基盤のRedis Pub/Subメカニズムは`EnqueueWithClientStream`向けにRedis List + BLPOPに置き換えられました。Standaloneモードでは引き続き動作します。

`FeedToStream` RPCを使用すると、実行中のストリーミングジョブに対してクライアントから追加データを送信できます。リアルタイム音声処理など、クライアントが音声チャンクを送信しながらRunnerが処理・結果を返すインタラクティブなストリーミングシナリオが可能になります。

### 前提条件

ジョブがfeedデータを受け取るには、以下の条件すべてを満たす必要があります：

| 条件 | 理由 |
|------|------|
| ジョブが `Running` 状態 | feedは実行中にのみ有効 |
| `streaming_type != None` | Runnerが`run_stream()`を使用している必要がある |
| Workerが `use_static=true` | Runnerインスタンスがプールされ永続化される必要がある |
| チャネル concurrency = 1 | feed先のRunnerが一意に特定される必要がある（単一ホストであること） |
| Runnerメソッドが `require_client_stream=true` | Runnerが明示的にクライアントストリーミングをサポートしている必要がある |

### プロトコル

```protobuf
// JobService 内（非推奨）
rpc FeedToStream(FeedToStreamRequest) returns (FeedToStreamResponse) {
  option deprecated = true;
}

message FeedToStreamRequest {
  jobworkerp.data.JobId job_id = 1;
  bytes data = 2;
  bool is_final = 3;
}

message FeedToStreamResponse {
  bool accepted = 1;
}
```

### 利用フロー

```text
1. EnqueueForStream(worker_id, args) → job_id（レスポンスヘッダー x-job-id-bin から取得）
   ↓（出力ストリーム開始）
2. FeedToStream(job_id, data_chunk_1, is_final=false)
3. FeedToStream(job_id, data_chunk_2, is_final=false)
4. FeedToStream(job_id, last_chunk, is_final=true)
   ↓（Runnerが最終データを処理し、出力ストリーム終了）
5. クライアントが残りの出力とEndトレーラーを受信
```

### データ転送メカニズム

- **Standaloneモード (Channel)**: feedデータは`ChanFeedSenderStore`に保持されたプロセス内`mpsc`チャネルを通じて直接送信されます。
- **Scalableモード**: 動作しません。代わりに`EnqueueWithClientStream`を使用してください。

### エラーケース

| ケース | gRPCステータス |
|--------|----------------|
| ジョブが存在しない | `NOT_FOUND` |
| ジョブが実行中でない | `FAILED_PRECONDITION` |
| ジョブがストリーミングでない | `FAILED_PRECONDITION` |
| Runnerメソッドに`require_client_stream=true`がない | `FAILED_PRECONDITION` |
| feedチャネルが利用不可（ジョブ完了済み） | `INTERNAL` |

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
3. 最終的な収集バイト列を返却

例えば、LLM Runnerはすべてのトークンチャンクを連結し、コマンドRunnerはstdout/stderrを適切にマージします。

## FeedToStreamからEnqueueWithClientStreamへの移行

### 移行理由

`FeedToStream`には`EnqueueWithClientStream`で解消される制約があります：

- **`use_static=true`が不要に**: feedチャネルはRunnerインスタンスではなく`job_id`単位で紐付けられます
- **チャネル concurrency=1 が不要に**: ジョブレベルのfeed分離により単一ホスト制約が不要になります
- **レースコンディションリスクなし**: エンキューとfeedが単一ストリームで行われ、タイミング調整が不要です
- **Scalableモード完全対応**: Redis List + BLPOPがPub/Subを置き換え、メッセージロスリスクを排除します

### 重要な変更

1. **Scalableモード**: `FeedToStream`はScalableモードでは動作しなくなりました。Redis Pub/SubはRedis List + BLPOPに置き換えられています。
2. **フィールドリネーム**: `need_feed` → `require_client_stream`（protoフィールド番号は変更なし、ワイヤ互換）
3. **トレイトリネーム**: `supports_feed()` → `supports_client_stream()`、`setup_feed_channel()` → `setup_client_stream_channel()`

### 移行手順

1. Runnerの`need_feed=true` → `require_client_stream=true`に更新（同一protoフィールド番号、ワイヤ互換）
2. 2ステップのEnqueue + FeedToStreamフローを単一の`EnqueueWithClientStream`ストリームに置き換え
3. 必要に応じて`use_static=true`やconcurrency=1の制約を緩和

### コード例

**変更前 (FeedToStream)**:
```text
1. EnqueueForStream(worker_id, args)
   → x-job-id-binヘッダーからjob_idを取得
   → 出力ストリームの受信開始
2. FeedToStream(job_id, data_chunk_1, is_final=false)
3. FeedToStream(job_id, data_chunk_2, is_final=false)
4. FeedToStream(job_id, last_chunk, is_final=true)
5. 残りの出力 + Endトレーラーを受信
```

**変更後 (EnqueueWithClientStream)**:
```text
1. EnqueueWithClientStreamストリームを開く
2. ClientStreamRequest(job_request)を送信
   → x-job-id-binヘッダー受信 + 出力の受信開始
3. ClientStreamRequest(feed_data: chunk_1, is_final=false)を送信
4. ClientStreamRequest(feed_data: chunk_2, is_final=false)を送信
5. ClientStreamRequest(feed_data: last_chunk, is_final=true)を送信
6. 残りの出力 + Endトレーラーを受信
```
