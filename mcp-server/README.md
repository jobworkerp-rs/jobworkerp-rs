# MCP Server for jobworkerp

A server implementation that exposes jobworkerp functionality via the [Model Context Protocol (MCP)](https://modelcontextprotocol.io/).

## Overview

MCP Server exposes jobworkerp Runners and Workers as MCP tools, enabling direct job execution from MCP clients like Claude Desktop.

## Deployment Configurations

### All-in-One Mode (Development / Single-node)

Runs MCP Server and Worker in a single process:

```bash
MCP_ENABLED=true ./all-in-one
```

Environment variables:
- `MCP_ENABLED`: Set to `true` or `1` to enable MCP Server mode (default: `false`, uses gRPC)
- `MCP_ADDR`: HTTP server bind address (default: `127.0.0.1:8000`)

### Scalable Mode (Production)

Runs MCP Server and Worker as separate processes, communicating via Redis/MySQL:

```bash
# Worker process (job execution)
./worker

# MCP Server (HTTP transport)
./mcp-http

# Or MCP Server (stdio transport)
./mcp-stdio
```

**Important**: `mcp-http` / `mcp-stdio` do not work standalone. A Worker process must be running separately.

## Binaries

### mcp-http

MCP Server using HTTP transport. Suitable for browser-based clients or HTTP proxy connections.

Environment variables:
- `MCP_ADDR`: Bind address (default: `127.0.0.1:8000`)
- `MCP_AUTH_ENABLED`: Enable Bearer authentication (default: `false`)
- `MCP_AUTH_TOKENS`: Valid tokens, comma-separated (default: `demo-token`)

### mcp-stdio

MCP Server using stdio transport. For MCP clients that communicate via stdin/stdout, such as Claude Desktop.

Claude Desktop configuration example:

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

## Common Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `STORAGE_TYPE` | `Standalone` or `Scalable` | `Standalone` |
| `DATABASE_URL` | Database connection URL | `sqlite://./jobworkerp.db` |
| `REDIS_URL` | Redis connection URL (required for Scalable) | - |
| `MCP_SET_NAME` | FunctionSet name to expose | - |
| `MCP_EXCLUDE_RUNNER` | Exclude Runners from tools | `false` |
| `MCP_EXCLUDE_WORKER` | Exclude Workers from tools | `false` |
| `MCP_STREAMING` | Enable streaming execution | `false` |
| `MCP_TIMEOUT_SEC` | Tool execution timeout in seconds | - |

## Architecture

```
+------------------+     +------------------+     +------------------+
|    MCP Client    |---->|    MCP Server    |---->|      Worker      |
| (Claude Desktop) |     | (mcp-http/stdio) |     |  (job executor)  |
+------------------+     +---------+--------+     +---------+--------+
                                   |                        |
                                   v                        v
                         +------------------+     +------------------+
                         |  Redis (Queue)   |<--->|   MySQL/SQLite   |
                         +------------------+     |  (Persistence)   |
                                                  +------------------+
```

## Exposed Tools

MCP Server exposes the following jobworkerp Runners as tools:

- `COMMAND`: Shell command execution
- `HTTP_REQUEST`: HTTP requests
- `PYTHON_COMMAND`: Python script execution
- `DOCKER`: Docker container execution
- `LLM_COMPLETION`: LLM text generation
- `INLINE_WORKFLOW` / `REUSABLE_WORKFLOW`: Workflow execution
- Custom plugins

## Authentication

`mcp-http` supports Bearer token authentication:

```bash
export MCP_AUTH_ENABLED=true
export MCP_AUTH_TOKENS="token1,token2,token3"
./mcp-http
```

**Note**: TLS/HTTPS is strongly recommended for production environments.

## Protocol Version

Uses MCP protocol version `2025-03-26` (LATEST).

## Documentation

- [Japanese documentation (README_ja.md)](./README_ja.md)
