# AG-UI Front

An HTTP SSE server compliant with AG-UI 1.0. Registered endpoints select either real-time streaming from an LLM worker or guarded workflow execution.

> **Note:** This is an alpha version implemented by a coding agent. APIs and behavior may change without notice.

## Overview

AG-UI Front implements the [AG-UI protocol](https://docs.ag-ui.com/llms-full.txt) and provides the following features:

- **Workflow Execution**: Execute jobworkerp-rs workflows via HTTP API
- **Real-time Streaming**: Event delivery via SSE (Server-Sent Events)
- **LLM Integration**: Integration with LLM_CHAT runner, streaming text output
- **Human-in-the-Loop (HITL)**: Pause and resume workflow/LLM tool calls
- **Session Management**: Session persistence with in-memory or Redis backend
- **Reconnection Support**: Prevent event loss with Last-Event-ID

## Architecture

```text
┌─────────────┐     ┌──────────────────┐     ┌─────────────────┐
│   Client    │────>│  AG-UI Front     │────>│  jobworkerp-rs  │
│  (Browser/  │<────│  (HTTP SSE)      │<────│  (Workflow)     │
│   CLI)      │ SSE │                  │     │                 │
└─────────────┘     └──────────────────┘     └─────────────────┘
                            │
                    ┌───────┴───────┐
                    │               │
              ┌─────┴─────┐   ┌─────┴─────┐
              │ Session   │   │  Event    │
              │ Manager   │   │  Store    │
              │(Memory/   │   │(Memory/   │
              │ Redis)    │   │ Redis)    │
              └───────────┘   └───────────┘
```

## Module Structure

| Module | Description |
|--------|-------------|
| `config` | Server configuration (loading from environment variables) |
| `error` | Error types and HTTP status mapping |
| `events` | AG-UI event types, encoders, adapters |
| `events/adapter` | jobworkerp events → AG-UI events conversion |
| `events/encoder` | SSE format encoder |
| `events/llm` | LLM streaming result conversion, tool call extraction |
| `events/state_diff` | State diff via RFC 6902 JSON Patch |
| `events/types` | `AgUiEvent` enum and related utilities |
| `handler` | Core logic for workflow execution and HITL management |
| `pubsub` | LLM job result subscription and conversion |
| `server/auth` | Bearer Token authentication middleware |
| `server/http` | Axum HTTP server, routing, SSE |
| `session/manager` | Session management trait and in-memory implementation |
| `session/redis_manager` | Redis session management implementation |
| `session/store` | Event store trait and in-memory implementation |
| `session/redis_store` | Redis event store implementation |
| `types` | Input/output type definitions (Context, Message, Tool, etc.) |

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `AG_UI_BIND_ADDR` | `0.0.0.0:8080` | Server bind address |
| `AG_UI_AUTH_TOKENS` | (none) | Comma-separated authentication tokens |
| `AG_UI_SESSION_TTL_SEC` | `3600` | Session TTL (seconds) |
| `AG_UI_MAX_EVENTS_PER_SESSION` | `1000` | Maximum events per session |
| `AG_UI_PROFILES` | `[]` | JSON array of registered endpoint profiles |
| `STORAGE_TYPE` | `Standalone` | `Standalone` or `Scalable` |
| `REDIS_URL` | (none) | Redis URL (for Scalable mode) |

## API Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/health` | GET | Health check |
| `/ag-ui/agents/<name>/run` | POST | Run a registered LLM streaming or workflow profile |
| `/ag-ui/stream/{run_id}` | GET | Reconnect to existing execution stream |
| `/ag-ui/run/{run_id}` | DELETE | Cancel workflow |
| `/ag-ui/state/{run_id}` | GET | Get workflow state |

See the [Client Guide](docs/CLIENT_GUIDE.md) for details.

Each `AG_UI_PROFILES` item has `path`, `workerName`, and `mode` (`streaming` or `workflow`). Paths use `/ag-ui/agents/<name>/run` and must be unique. Example:

```json
[
  {"path":"/ag-ui/agents/live/run","workerName":"chat-live","mode":"streaming"},
  {"path":"/ag-ui/agents/guarded/run","workerName":"chat-guarded","mode":"workflow"}
]
```

Register an `LLM/chat` worker for `streaming` and a `WORKFLOW/run` worker for `workflow`. Workers that are enqueued as jobs (the `streaming` profile worker, and the `workflow` profile worker under `ag-ui-http-async`) must use the `Direct` response type on a `Normal` or `WithBackup` queue, with no periodic interval. Workers that do not meet these conditions cannot provide a live response stream, so runs and resumes are rejected before the job is enqueued. Point each AG-UI client at the profile path for its use case. The legacy `/ag-ui/run` route is not registered automatically.

### Get Started

1. **Register workers:** Configure the `LLM/chat` worker for client tools with server-side auto execution disabled. The `WORKFLOW/run` worker owns the trusted LLM and guardrail settings. See the jobworkerp [worker creation examples](../../README.md#execution-examples-using-jobworkerp-client).
2. **Set endpoint profiles:**

   ```sh
   export AG_UI_PROFILES='[{"path":"/ag-ui/agents/live/run","workerName":"chat-live","mode":"streaming"},{"path":"/ag-ui/agents/guarded/run","workerName":"chat-guarded","mode":"workflow"}]'
   ```

3. **Start the server:** Set `AG_UI_ENABLED=true` for the embedded all-in-one server. Or run `cargo run -p ag-ui-front --bin ag-ui-http-async`; the async binary requires `STORAGE_TYPE=Scalable` and running worker processes.
4. **Connect a client:** Set the AG-UI client's endpoint URL to a profile path such as `/ag-ui/agents/live/run`. When authentication is enabled, configure `Authorization: Bearer <token>`.
5. **Send a test request:**

   ```sh
   curl -N http://localhost:8080/ag-ui/agents/live/run \
     -H 'Content-Type: application/json' \
     -H 'Authorization: Bearer <token>' \
     --data '{"threadId":"thread-1","runId":"run-1","messages":[{"id":"msg-1","role":"user","content":"Hello"}]}'
   ```

   Confirm `RUN_STARTED`, followed by text or client-tool events, then `RUN_FINISHED` or `RUN_ERROR`. The client executes client tool calls and continues the conversation in the next run with the tool result.

## SSE Events

### Lifecycle Events

| Event | Description |
|-------|-------------|
| `RUN_STARTED` | Workflow started |
| `RUN_FINISHED` | Workflow completed successfully |
| `RUN_ERROR` | Workflow ended with error |
| `STEP_STARTED` | Task started |
| `STEP_FINISHED` | Task finished |

### Message Events (LLM Streaming)

| Event | Description |
|-------|-------------|
| `TEXT_MESSAGE_START` | Message start |
| `TEXT_MESSAGE_CONTENT` | Message content (delta) |
| `TEXT_MESSAGE_END` | Message end |

### Tool Call Events

| Event | Description |
|-------|-------------|
| `TOOL_CALL_START` | Tool call start |
| `TOOL_CALL_ARGS` | Tool arguments (delta) |
| `TOOL_CALL_END` | Tool call end |
| `TOOL_CALL_RESULT` | Tool execution result |

### State Events

| Event | Description |
|-------|-------------|
| `STATE_SNAPSHOT` | State snapshot |
| `STATE_DELTA` | State delta (RFC 6902 JSON Patch) |

## Tools and Resume


To resume a workflow checkpoint, send a standard `resume` entry array to the same thread and profile URL with a new `runId`. The checkpoint is not continued while interrupts remain unanswered. Configure jobworkerp FunctionSets as a separate LLM task inside the workflow; do not combine server FunctionSets and client tools in one LLM call.

**Tool Name Format:**

LLM tool call `toolCallName` follows the format `RUNNER___method`, where the runner name and method name are concatenated with triple underscores.
Examples: `COMMAND___run`, `HTTP_REQUEST___get`

It is recommended to convert to `/` separator for client display.

## Usage

### Starting the Server

```bash
# Set environment variables
export AG_UI_BIND_ADDR=0.0.0.0:8080
export AG_UI_AUTH_TOKENS=secret-token-1,secret-token-2

# Start
cargo run --bin ag_ui_http
```

### Client Implementation

See the following for client implementation details:

- [Client Guide (English)](docs/CLIENT_GUIDE.md)
- [Client Guide (Japanese)](docs/CLIENT_GUIDE_ja.md)

## Sequence Diagram

```mermaid
sequenceDiagram
    participant Client
    participant HTTP as AG-UI HTTP Server
    participant Handler as AgUiHandler
    participant Session as SessionManager
    participant EventStore as EventStore
    participant JobApp as JobResultApp/Runner

    Note over Client,HTTP: Run using a registered profile
    Client->>HTTP: POST /ag-ui/agents/<name>/run (RunAgentInput)
    HTTP->>Handler: dispatch by registered profile
    Handler->>Session: create_session(run_id, thread_id)
    Session-->>Handler: Session
    Handler->>EventStore: store_event(RUN_STARTED)
    Handler-->>HTTP: SSE stream and session headers

    par Live execution
        Handler->>JobApp: Run registered LLM worker or workflow
        JobApp-->>Handler: Workflow/job result stream
        Handler->>EventStore: store_event(each event)
        Handler->>HTTP: SSE event delivery
    end

    Note over Client,HTTP: Reconnection (Last-Event-ID)
    Client->>HTTP: GET /stream (Last-Event-ID)
    HTTP->>Handler: resume_stream(run_id, last_event_id)
    Handler->>EventStore: get_events_since(run_id, last_event_id)
    EventStore-->>Handler: Historical events
    Handler-->>HTTP: Historical + live events
```

## License

Follows the jobworkerp-rs project license.
