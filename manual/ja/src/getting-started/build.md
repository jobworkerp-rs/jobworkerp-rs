# ビルドと起動

## 前提条件

- **Rust toolchain** (stable、最新推奨)
- **(オプション) Docker** - DockerRunnerやコンテナベースのデプロイに必要
- **(オプション) Redis + MySQL** - Scalableモードで必要

## ビルド

### デフォルト (SQLite)

```shell
$ cargo build --release
```

### MySQL使用

```shell
$ cargo build --release --features mysql
```

## デプロイモードの選択

jobworkerp-rsは2つのデプロイモードをサポートしています。用途に応じて選択してください。

### Standalone（推奨: 開発・単一インスタンス）

- **ストレージ**: インメモリチャンネル (mpsc/mpmc) + SQLite
- **外部依存**: なし
- **用途**: ローカル開発、単一サーバーデプロイ

主要な `.env` 設定:

```shell
STORAGE_TYPE=Standalone
SQLITE_URL="sqlite://jobworkerp.sqlite3?mode=rwc"
```

### Scalable（推奨: 本番・複数インスタンス）

- **ストレージ**: Redis (ジョブキュー) + MySQL (永続データ)
- **外部依存**: Redis, MySQL
- **用途**: マルチワーカー本番環境デプロイ
- **ビルド**: `--features mysql` が必要

主要な `.env` 設定:

```shell
STORAGE_TYPE=Scalable
REDIS_URL="redis://localhost:6379"
MYSQL_HOST="127.0.0.1"
MYSQL_PORT="3306"
MYSQL_USER="mysql"
MYSQL_PASSWORD="mysqlpw"
MYSQL_DBNAME="test"
```

## 起動

### 1. 環境ファイルの準備

```shell
$ cp dot.env .env
# 環境に合わせて .env を編集
```

### 2. サーバーの起動

**All-in-one**（単一プロセス、最もシンプル）:

```shell
$ ./target/release/all-in-one
```

**分離構成**（フロントエンド + ワーカー）:

```shell
$ ./target/release/worker &
$ ./target/release/grpc-front &
```

**Docker Compose**（Standalone）:

```shell
$ docker-compose up
```

**Docker Compose**（Scalable、マルチワーカー）:

```shell
$ docker-compose -f docker-compose-scalable.yml up --scale jobworkerp-worker=3
```

## 主要な機能有効化オプション

以下の機能は環境変数で有効化できます。詳細は[設定と環境変数](../configuration.md)を参照してください。

| 環境変数 | 説明 | デフォルト |
|---------|------|-----------|
| `MCP_ENABLED` | MCPサーバーモードの有効化 (all-in-oneモード時) | `false` |
| `AG_UI_ENABLED` | AG-UIサーバーの有効化 (all-in-oneモード時) | `false` |
| `JOB_STATUS_RDB_INDEXING` | ジョブステータスのRDB永続化（検索用） | `false` |
| `OTLP_ADDR` | OpenTelemetry gRPCエンドポイント（分散トレーシング・メトリクス） | - (無効) |
| `MCP_CONFIG` | MCPサーバー設定ファイルパス | `mcp-settings.toml` |
