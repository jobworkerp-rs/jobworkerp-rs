# MCP Server for jobworkerp

jobworkerp の機能を [Model Context Protocol (MCP)](https://modelcontextprotocol.io/) 経由で公開するサーバー実装です。

## 概要

MCP Server は jobworkerp の Runner と Worker を MCP ツールとして公開し、Claude Desktop などの MCP クライアントから直接ジョブを実行できるようにします。

## デプロイメント構成

### All-in-One モード（開発・単体テスト向け）

単一プロセスで MCP Server と Worker を起動します：

```bash
MCP_ENABLED=true ./all-in-one
```

環境変数：
- `MCP_ENABLED`: `true` または `1` で MCP Server モードを有効化（デフォルト: `false`、gRPC を使用）
- `MCP_ADDR`: HTTP サーバーのバインドアドレス（デフォルト: `127.0.0.1:8000`）

### Scalable モード（本番向け）

MCP Server と Worker を別プロセスで起動し、Redis/MySQL 経由で通信します：

```bash
# Worker プロセス（ジョブ処理）
./worker

# MCP Server（HTTP transport）
./mcp-http

# または MCP Server（stdio transport）
./mcp-stdio
```

**重要**: `mcp-http` / `mcp-stdio` は単独では動作しません。必ず Worker プロセスを別途起動してください。

## バイナリ

### mcp-http

HTTP transport を使用する MCP Server です。ブラウザベースのクライアントや HTTP プロキシ経由での接続に適しています。

環境変数：
- `MCP_ADDR`: バインドアドレス（デフォルト: `127.0.0.1:8000`）
- `MCP_AUTH_ENABLED`: Bearer 認証を有効化（デフォルト: `false`）
- `MCP_AUTH_TOKENS`: 有効なトークン（カンマ区切り、デフォルト: `demo-token`）

### mcp-stdio

stdio transport を使用する MCP Server です。Claude Desktop など、stdin/stdout で通信する MCP クライアント向けです。

Claude Desktop での設定例：

```json
{
  "mcpServers": {
    "jobworkerp": {
      "command": "/path/to/mcp-stdio",
      "env": {
        "DATABASE_URL": "sqlite://./jobworkerp.db",
        "STORAGE_TYPE": "Scalable",
        "REDIS_URL": "redis://localhost:6379"
      }
    }
  }
}
```

## 共通環境変数

| 変数名 | 説明 | デフォルト |
|--------|------|-----------|
| `STORAGE_TYPE` | `Standalone` または `Scalable` | `Standalone` |
| `DATABASE_URL` | データベース接続 URL | `sqlite://./jobworkerp.db` |
| `REDIS_URL` | Redis 接続 URL（Scalable 時必須） | - |
| `MCP_SET_NAME` | 公開する FunctionSet の名前 | - |
| `MCP_EXCLUDE_RUNNER` | Runner をツールから除外 | `false` |
| `MCP_EXCLUDE_WORKER` | Worker をツールから除外 | `false` |
| `MCP_STREAMING` | ストリーミング実行を有効化 | `false` |
| `MCP_TIMEOUT_SEC` | ツール実行のタイムアウト秒数 | - |

## アーキテクチャ

```
┌─────────────────┐     ┌─────────────────┐     ┌─────────────────┐
│   MCP Client    │────▶│   MCP Server    │────▶│     Worker      │
│ (Claude Desktop)│     │ (mcp-http/stdio)│     │  (job executor) │
└─────────────────┘     └────────┬────────┘     └────────┬────────┘
                                 │                       │
                                 ▼                       ▼
                        ┌─────────────────┐     ┌─────────────────┐
                        │  Redis (Queue)  │◀───▶│   MySQL/SQLite  │
                        └─────────────────┘     │  (Persistence)  │
                                                └─────────────────┘
```

## 公開されるツール

MCP Server は以下の jobworkerp Runner をツールとして公開します：

- `COMMAND`: シェルコマンド実行
- `HTTP_REQUEST`: HTTP リクエスト
- `PYTHON_COMMAND`: Python スクリプト実行
- `DOCKER`: Docker コンテナ実行
- `LLM_COMPLETION`: LLM テキスト生成
- `INLINE_WORKFLOW` / `REUSABLE_WORKFLOW`: ワークフロー実行
- カスタムプラグイン

## 認証

`mcp-http` は Bearer トークン認証をサポートします：

```bash
export MCP_AUTH_ENABLED=true
export MCP_AUTH_TOKENS="token1,token2,token3"
./mcp-http
```

**注意**: 本番環境では TLS/HTTPS の使用を強く推奨します。

## プロトコルバージョン

MCP プロトコルバージョン `2025-03-26` (LATEST) を使用しています。
