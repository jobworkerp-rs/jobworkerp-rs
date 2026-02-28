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

## Workerプーリングとuse_static

Workerで`use_static=true`を使用する場合（例：ローカルLLM向け）、Runnerインスタンスはプールされ、ジョブ実行間で再利用されます。初期化コストが高いリソースにとって重要な機能です。

`StreamingType::Internal`は以下の方法でこのプーリング動作を維持します：
1. エンキュー時に既存の`worker_id`を使用（一時Workerを作成しない）
2. エンキューから即座に返る（Directレスポンスでブロックしない）
3. 呼び出し側が独立してストリーム購読を管理可能

これにより、ローカルLLMモデルなどの重いリソースがジョブごとに再初期化されることを防ぎます。

## FeedToStream: 実行中ストリーミングジョブへのデータ送信

`FeedToStream` RPC を使用すると、実行中のストリーミングジョブに対してクライアントから追加データを送信できます。リアルタイム音声処理など、クライアントが音声チャンクを送信しながら Runner が処理・結果を返すインタラクティブなストリーミングシナリオが可能になります。

### 前提条件

ジョブが feed データを受け取るには、以下の条件すべてを満たす必要があります：

| 条件 | 理由 |
|------|------|
| ジョブが `Running` 状態 | feed は実行中にのみ有効 |
| `streaming_type != None` | Runner が `run_stream()` を使用している必要がある |
| Worker が `use_static=true` | Runner インスタンスがプールされ永続化される必要がある |
| チャネル concurrency = 1 | feed 先の Runner が一意に特定される必要がある |
| Runner メソッドが `need_feed=true` | Runner が明示的に feed をサポートしている必要がある |

### プロトコル

```protobuf
// JobService 内
rpc FeedToStream(FeedToStreamRequest) returns (FeedToStreamResponse);

message FeedToStreamRequest {
  jobworkerp.data.JobId job_id = 1;  // 対象ジョブ（EnqueueForStream レスポンスヘッダーから取得）
  bytes data = 2;                     // feed データペイロード
  bool is_final = 3;                  // feed の終了を通知
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
   ↓（Runner が最終データを処理し、出力ストリーム終了）
5. クライアントが残りの出力と End トレーラーを受信
```

### データ転送メカニズム

- **Scalable モード (Redis)**: feed データは Redis Pub/Sub (`job_feed:{job_id}`) で publish され、Worker 側のブリッジタスクが subscribe して Runner の `mpsc` チャネルに転送します。
- **Standalone モード (Channel)**: feed データは `ChanFeedSenderStore` に保持されたプロセス内 `mpsc` チャネルを通じて直接送信されます。

### エラーケース

| ケース | gRPC ステータス |
|--------|----------------|
| ジョブが存在しない | `NOT_FOUND` |
| ジョブが実行中でない | `FAILED_PRECONDITION` |
| ジョブがストリーミングでない | `FAILED_PRECONDITION` |
| Runner メソッドに `need_feed=true` がない | `FAILED_PRECONDITION` |
| feed チャネルが利用不可（ジョブ完了済み） | `INTERNAL` |

詳細な仕様は `docs/feed-stream-spec.md` を参照してください。

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
