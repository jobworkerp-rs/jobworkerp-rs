# 詳細設定

[基本設定](./configuration.md)に記載されていない環境変数の一覧です。具体的な設定例は [dot.env](https://github.com/jobworkerp-rs/jobworkerp-rs/blob/main/dot.env) を参照してください。

## データベース接続設定

### SQLite

| 環境変数名 | 説明 | デフォルト値 |
|-----------|------|-------------|
| SQLITE_URL | SQLite接続URL | sqlite://jobworkerp.sqlite3?mode=rwc |
| SQLITE_MAX_CONNECTIONS | 最大接続数 | 20 |

### MySQL（Scalableモード用）

| 環境変数名 | 説明 | デフォルト値 |
|-----------|------|-------------|
| MYSQL_HOST | MySQLホスト | 127.0.0.1 |
| MYSQL_PORT | MySQLポート | 3306 |
| MYSQL_USER | ユーザー名 | - |
| MYSQL_PASSWORD | パスワード | - |
| MYSQL_DBNAME | データベース名 | - |
| MYSQL_MAX_CONNECTIONS | 最大接続数 | 20 |

## Redis接続設定（Scalableモード用）

| 環境変数名 | 説明 | デフォルト値 |
|-----------|------|-------------|
| REDIS_URL | Redis接続URL | redis://redis:6379 |
| REDIS_USERNAME | Redisユーザー名 | - |
| REDIS_PASSWORD | Redisパスワード | - |
| REDIS_POOL_SIZE | コネクションプールサイズ | 20 |
| REDIS_POOL_MIN_IDLE | 最小アイドル接続数 | 1 |
| REDIS_POOL_CONNECTION_TIMEOUT_MSEC | 接続タイムアウト（ミリ秒） | 5000 |
| REDIS_POOL_IDLE_TIMEOUT_MSEC | アイドルタイムアウト（ミリ秒） | 60000 |
| REDIS_POOL_MAX_LIFETIME_MSEC | 接続最大生存時間（ミリ秒） | 300000 |

## キャッシュ設定

stretto/mokaキャッシュの設定です。

| 環境変数名 | 説明 | デフォルト値 |
|-----------|------|-------------|
| MEMORY_CACHE_NUM_COUNTERS | strettoカウンター数 | 12960 |
| MEMORY_CACHE_MAX_COST | stretto最大コスト | 12960 |
| MEMORY_CACHE_USE_METRICS | メトリクス有効化 | true |
| MEMORY_CACHE_TTL_SEC | mokaキャッシュTTL（秒） | 3600 |

## ジョブキュー詳細設定（Standaloneモード）

| 環境変数名 | 説明 | デフォルト値 |
|-----------|------|-------------|
| JOB_QUEUE_CHANNEL_CAPACITY | インメモリチャンネルの最大メッセージ数 | 10000 |
| JOB_QUEUE_PUBSUB_CHANNEL_CAPACITY | 結果通知用pubsubチャンネルの容量 | 128 |
| JOB_QUEUE_MAX_CHANNELS | strettoキャッシュが保持する最大チャンネル数 | 10000 |
| JOB_QUEUE_CANCEL_CHANNEL_CAPACITY | キャンセルブロードキャストチャンネルの容量 | 1000 |

## ログ・トレーシング詳細設定

| 環境変数名 | 説明 | デフォルト値 |
|-----------|------|-------------|
| RUST_LOG | Rustログフィルタ（envfilter形式） | info |
| LOG_APP_NAME | アプリケーション名 | jobworkerp-rs |
| ZIPKIN_ADDR | Zipkinエンドポイント（現在無効） | - (未設定で無効) |

## プラグイン・MCP詳細設定

| 環境変数名 | 説明 | デフォルト値 |
|-----------|------|-------------|
| PLUGIN_INSTANTIATE_TIMEOUT_SECS | プラグインインスタンス化タイムアウト（秒） | 5 |
| MCP_TRANSPORT_START_TIMEOUT_SECS | MCP SSE/Stdioトランスポート起動タイムアウト（秒） | 15 |
| MCP_TOOLS_LOAD_TIMEOUT_SECS | MCPツール読み込みタイムアウト（秒） | 10 |
| PLUGIN_SCHEMA_LOAD_TIMEOUT_SECS | プラグインスキーマ読み込みタイムアウト（秒） | 5 |

## LLM APIキー設定

LLMランナーで外部LLMサービスを利用する場合に設定します。

| 環境変数名 | 説明 |
|-----------|------|
| OPENAI_API_KEY | OpenAI APIキー |
| ANTHROPIC_API_KEY | Anthropic APIキー |
| COHERE_API_KEY | Cohere APIキー |
| GEMINI_API_KEY | Google Gemini APIキー |
| GROQ_API_KEY | Groq APIキー |
| XAI_API_KEY | xAI (Grok) APIキー |
| DEEPSEEK_API_KEY | DeepSeek APIキー |

## その他

| 環境変数名 | 説明 | デフォルト値 |
|-----------|------|-------------|
| TZ_OFFSET_HOURS | タイムゾーンオフセット（時間） | 9 |
| MAX_FRAME_SIZE | gRPC最大フレームサイズ（バイト） | None（tonicデフォルト）。dot.envでは16777215を設定例として記載 |
| WORKFLOW_HTTP_USER_AGENT | ワークフローHTTPリクエストのUser-Agent | simple-workflow/1.0 |
