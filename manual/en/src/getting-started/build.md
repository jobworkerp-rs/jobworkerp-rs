# Build and Launch

## Prerequisites

- **Rust toolchain** (stable, latest recommended)
- **(Optional) Docker** - for DockerRunner or container-based deployment
- **(Optional) Redis + MySQL** - required for Scalable mode

## Build

### Default (SQLite)

```shell
$ cargo build --release
```

### With MySQL support

```shell
$ cargo build --release --features mysql
```

## Choosing a Deploy Mode

jobworkerp-rs supports two deployment modes. Choose based on your requirements:

### Standalone (recommended for development / single instance)

- **Storage**: In-memory channels (mpsc/mpmc) + SQLite
- **External dependencies**: None
- **Use case**: Local development, single-server deployment

Key `.env` settings:

```shell
STORAGE_TYPE=Standalone
SQLITE_URL="sqlite://jobworkerp.sqlite3?mode=rwc"
```

### Scalable (recommended for production / multiple instances)

- **Storage**: Redis (job queue) + MySQL (persistent data)
- **External dependencies**: Redis, MySQL
- **Use case**: Multi-worker production deployment
- **Build**: Must use `--features mysql`

Key `.env` settings:

```shell
STORAGE_TYPE=Scalable
REDIS_URL="redis://localhost:6379"
MYSQL_HOST="127.0.0.1"
MYSQL_PORT="3306"
MYSQL_USER="mysql"
MYSQL_PASSWORD="mysqlpw"
MYSQL_DBNAME="test"
```

## Launch

### 1. Prepare the environment file

```shell
$ cp dot.env .env
# Edit .env to match your environment
```

### 2. Start the server

**All-in-one** (single process, simplest):

```shell
$ ./target/release/all-in-one
```

**Separate processes** (frontend + worker):

```shell
$ ./target/release/worker &
$ ./target/release/grpc-front &
```

**Docker Compose** (Standalone):

```shell
$ docker-compose up
```

**Docker Compose** (Scalable, multi-worker):

```shell
$ docker-compose -f docker-compose-scalable.yml up --scale jobworkerp-worker=3
```

## Key Feature Options

The following features can be enabled via environment variables. For full details, see [Configuration](../configuration.md).

| Environment Variable | Description | Default |
|---------------------|-------------|---------|
| `MCP_ENABLED` | Enable MCP server mode (all-in-one only) | `false` |
| `AG_UI_ENABLED` | Enable AG-UI server (all-in-one only) | `false` |
| `JOB_STATUS_RDB_INDEXING` | Persist job status to RDB for querying | `false` |
| `OTLP_ADDR` | OpenTelemetry gRPC endpoint for distributed tracing/metrics | - (disabled) |
| `MCP_CONFIG` | Path to MCP server configuration file | `mcp-settings.toml` |
