# 設定と環境変数

- 特に単位を明記していない時間項目の単位はミリ秒

## worker/job設定パラメータ

- **job.run_after_time**: ジョブの実行時刻 (epoch time)
- **job.timeout**：タイムアウト時間
- **worker.periodic_interval**: 繰り返しジョブ実行 (1以上の指定)
- **worker.retry_policy**: job実行失敗時のリトライ方式(RetryType: CONSTANT、LINEAR、EXPONENTIAL)、最大回数(max_retry)、最大時間間隔(max_interval)などを指定
- **worker.use_static**: runnerプロセスを並列度の分だけstaticに確保することが可能 (実行runerをpoolingして初期化を都度行わない)
- **worker.broadcast_results**: ジョブ実行結果をリアルタイムに通知する機能を有効化 (true/false)
  - 複数のクライアントが同時に結果を取得可能になる (Redis pubsubを使用)

## RDB設定

データベーススキーマ：
- [MySQL schema](https://github.com/jobworkerp-rs/jobworkerp-rs/blob/main/infra/sql/mysql/002_worker.sql)
- [SQLite schema](https://github.com/jobworkerp-rs/jobworkerp-rs/blob/main/infra/sql/sqlite/002_schema.sql)

(runnerテーブルには組み込み機能としての固定レコードが存在します)

## 環境変数一覧

(具体例は [dot.env](https://github.com/jobworkerp-rs/jobworkerp-rs/blob/main/dot.env) ファイルを参照)

| カテゴリ | 環境変数名 | 説明 | デフォルト値 |
|---------|------------|------|-------------|
| **実行runner設定** | PLUGINS_RUNNER_DIR | プラグイン格納ディレクトリ | ./ |
| | DOCKER_GID | DockerグループID (DockerRunner用) | - |
| **ジョブキュー設定** | WORKER_DEFAULT_CONCURRENCY | デフォルトチャンネルの並列度。`0`で無効化 | CPUコア数（`num_cpus::get()`）。dot.envでは8を設定例として記載 |
| | WORKER_DEFAULT_CHANNEL_DISABLED | デフォルトチャンネルを明示的に無効化。`true`は並列度より優先され、追加チャンネルには影響しない | false |
| | WORKER_CHANNELS | 追加ジョブキューチャンネルの名称(カンマ区切り) | - |
| | WORKER_CHANNEL_CONCURRENCIES | 追加ジョブキューチャンネルの並列度(カンマ区切り) | - |
| **ログ設定** | LOG_LEVEL | ログレベル(trace, debug, info, warn, error) | info |
| | LOG_FILE_DIR | ログ出力ディレクトリ | - |
| | LOG_USE_JSON | ログ出力をJSON形式で実施するか(boolean) | false |
| | LOG_USE_STDOUT | ログ出力を標準出力するか(boolean) | true |
| | OTLP_ADDR | OpenTelemetry gRPCエンドポイント（分散トレーシング・メトリクス） | - (未設定で無効) |
| **ストレージ設定** | STORAGE_TYPE | Standalone: 単一インスタンス、Scalable: 複数インスタンス | Standalone |
| | JOB_QUEUE_EXPIRE_JOB_RESULT_SECONDS | worker.broadcast_results=trueの場合の完了済み結果の保持期間。アクティブな結果ストリームおよびDirect応答の待機上限には使用しない | 86400（dot.envでは3600を推奨） |
| | JOB_QUEUE_FETCH_INTERVAL | rdbに格納されたjobの定期fetch間隔 | 1000 |
| | STORAGE_RESTORE_AT_STARTUP | クラッシュ後のジョブ復旧フラグ | false |
| **GRPC設定** | GRPC_ADDR | grpcサーバアドレス:ポート | 0.0.0.0:9000 |
| | USE_GRPC_WEB | grpcサーバでgRPC webを利用するか(boolean) | false |
| | AUTH_TOKEN | 設定時、保護対象 gRPC RPC で `jobworkerp-auth` metadata として要求されるトークン | - |
| **機能有効化** | MCP_ENABLED | MCPサーバーモードの有効化 (all-in-oneモード時) | false |
| | MCP_ADDR | MCPサーバーバインドアドレス | 127.0.0.1:8000 |
| | AG_UI_ENABLED | AG-UIサーバーの有効化 (all-in-oneモード時) | false |
| | AG_UI_ADDR | AG-UIサーバーバインドアドレス | 127.0.0.1:8001 |
| **ジョブステータス** | JOB_STATUS_RDB_INDEXING | ジョブステータスのRDBインデックス有効化（検索可能にする） | false |
| | JOB_STATUS_CLEANUP_INTERVAL_HOURS | クリーンアップ間隔（時間） | 1 |
| | JOB_STATUS_RETENTION_HOURS | 削除済みレコード保持期間（時間） | 24 |
| **MCP設定** | MCP_CONFIG | MCPサーバー設定ファイルパス | mcp-settings.toml |
| **ワーカーインスタンス設定** | WORKER_INSTANCE_ENABLED | ワーカーインスタンス登録の有効/無効 | true |
| | WORKER_INSTANCE_HEARTBEAT_INTERVAL_SEC | ハートビート間隔 | 30 |
| | WORKER_INSTANCE_TIMEOUT_SEC | 非アクティブタイムアウト (Scalableモードのみ) | 90 |
| | WORKER_INSTANCE_CLEANUP_INTERVAL_SEC | クリーンアップ間隔（秒） | 300 |
| **ワークフロー設定** | WORKFLOW_TASK_DEFAULT_TIMEOUT_SEC | タスクデフォルトタイムアウト（秒） | None（制限なし）。dot.envでは3600を設定例として記載 |
| | WORKFLOW_HTTP_TIMEOUT_SEC | HTTPリソース取得タイムアウト（秒） | 120（dot.envでは20を設定例として記載） |
| | WORKFLOW_CHECKPOINT_EXPIRE_SEC | チェックポイント有効期限（秒） | None（制限なし）。dot.envでは86400を設定例として記載 |
| | WORKFLOW_CHECKPOINT_MAX_COUNT | チェックポイント最大保持数 | None（制限なし）。dot.envでは100000を設定例として記載 |
| | WORKFLOW_SKIP_SCHEMA_VALIDATION | スキーマバリデーションスキップ | false |
| **Runner/Plugin設定** | PLUGIN_LOAD_TIMEOUT_SECS | プラグインロードタイムアウト（秒） | 10 |
| | MCP_CONNECTION_TIMEOUT_SECS | MCP接続タイムアウト（秒） | 15 |

## Scalableモードのステータス形式更新

Redis の `JOB_STATUS` に保存するジョブステータスは、リトライ回数を含む `status:retried` 形式です。旧バージョンが保存した整数形式とは互換性がないため、同じ Redis を旧バージョンと新バージョンで併用してはいけません。

Scalableモードをこの形式へ更新する場合は、次の順序で実施してください。

1. 新規ジョブの受付を停止し、通常キューと遅延実行キューをdrainします。実行中ジョブは完了またはキャンセルまで待機します。
2. 旧バージョンの worker をすべて停止します。`STORAGE_RESTORE_AT_STARTUP` を有効にしている場合も、未完了ジョブを残したまま更新しません。
3. すべてのジョブが完了またはキャンセル済みであることを確認してから、Redis の `JOB_STATUS` ハッシュを削除します。この操作は保存済みのステータスを復元できないため、drain 完了前には実行しないでください。
4. 新バージョンを一斉にデプロイし、ジョブ受付と worker を再開します。

その他の詳細な環境変数（データベース接続、Redis、キャッシュ、LLM APIキー等）については[詳細設定](./configuration-advanced.md)を参照してください。
