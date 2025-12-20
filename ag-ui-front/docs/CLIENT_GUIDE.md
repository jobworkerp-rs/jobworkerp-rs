# AG-UI Client Guide

This document is a guide for client implementers using the AG-UI Front HTTP API.

## Table of Contents

1. [Overview](#overview)
2. [Authentication](#authentication)
3. [API Endpoints](#api-endpoints)
4. [SSE Events](#sse-events)
5. [Human-in-the-Loop (HITL)](#human-in-the-loop-hitl)
6. [LLM Tool Calling HITL](#llm-tool-calling-hitl)
7. [Error Handling](#error-handling)
8. [Implementation Examples](#implementation-examples)

---

## Overview

AG-UI Front provides an HTTP API compliant with the [AG-UI protocol](https://docs.ag-ui.com/), enabling jobworkerp-rs workflow execution and real-time event streaming.

### Basic Flow

```text
Client                         AG-UI Server
    |                              |
    |-- POST /ag-ui/run ---------->|  Start workflow execution
    |<-------- SSE stream ---------|  Event stream
    |                              |
    |-- GET /ag-ui/stream/{id} --->|  Reconnect (Last-Event-ID)
    |<-------- SSE stream ---------|
    |                              |
    |-- DELETE /ag-ui/run/{id} --->|  Cancel
    |<-------- 200 OK -------------|
```

---

## Authentication

### Bearer Token Authentication

When tokens are configured via the `AG_UI_AUTH_TOKENS` environment variable, authentication is required for all `/ag-ui/*` endpoints.

```http
Authorization: Bearer <your-token>
```

If authentication is disabled or not configured, endpoints are accessible without authentication.

### Authentication Error

```json
{
  "error": {
    "code": "UNAUTHORIZED",
    "message": "Invalid or missing authentication token"
  }
}
```

---

## API Endpoints

### Health Check

```http
GET /api/health
```

**Response (200 OK):**
```json
{
  "status": "ok",
  "service": "ag-ui-front"
}
```

---

### Run Workflow

```http
POST /ag-ui/run
Content-Type: application/json
Authorization: Bearer <token>
```

**Request Body:**

```json
{
  "threadId": "thread-123",
  "runId": "run-456",
  "messages": [
    {
      "role": "user",
      "content": "Hello, please process this request"
    }
  ],
  "tools": [],
  "context": [
    {
      "type": "workflow_definition",
      "workflowName": "my_workflow"
    }
  ],
  "forwardedProps": {
    "workerId": 12345,
    "priority": 10,
    "timeout": 300
  }
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `threadId` | string | No | Conversation thread ID (auto-generated if not specified) |
| `runId` | string | No | Execution ID (auto-generated if not specified) |
| `messages` | Message[] | No | Input messages |
| `tools` | Tool[] | No | Client-defined tools (for HITL) |
| `context` | Context[] | Yes | Context (including workflow name) |
| `forwardedProps` | object | No | jobworkerp-rs specific properties |

**Context Types:**

```json
// Workflow definition specification
{
  "type": "workflow_definition",
  "workflowName": "my_workflow"
}

// Inline workflow
{
  "type": "workflow_inline",
  "workflow": { /* Serverless Workflow YAML/JSON */ }
}

// Resume from checkpoint
{
  "type": "checkpoint_resume",
  "executionId": "exec-123",
  "position": "/tasks/task1"
}
```

**Response:**

- Content-Type: `text/event-stream`
- Custom Headers:
  - `x-ag-ui-run-id`: Execution ID
  - `x-ag-ui-session-id`: Session ID

Events are delivered via SSE stream.

---

### Stream Reconnection

```http
GET /ag-ui/stream/{run_id}
Authorization: Bearer <token>
Last-Event-ID: 42
```

Endpoint for reconnection after connection drops. Resumes from events after the ID specified in the `Last-Event-ID` header.

**Response:** SSE stream

---

### HITL Message Submission

```http
POST /ag-ui/message
Content-Type: application/json
Authorization: Bearer <token>
```

**Request Body:**

```json
{
  "runId": "run-456",
  "toolCallResults": [
    {
      "toolCallId": "wait_run-456",
      "result": {
        "userInput": "approved",
        "comment": "Looks good to me"
      }
    }
  ]
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `runId` | string | Yes | Target execution ID |
| `toolCallResults` | ToolCallResult[] | Yes | Tool call results (**exactly 1 item**) |

**Response:** SSE stream (events after resume)

---

### Cancel Workflow

```http
DELETE /ag-ui/run/{run_id}
Authorization: Bearer <token>
```

**Response (200 OK):**
```json
{
  "status": "cancelled",
  "runId": "run-456"
}
```

---

### Get State

```http
GET /ag-ui/state/{run_id}
Authorization: Bearer <token>
```

**Response (200 OK):**
```json
{
  "status": "running",
  "completedTasks": ["task1", "task2"],
  "currentTask": "task3",
  "contextVariables": {
    "result": "intermediate value"
  }
}
```

---

## SSE Events

### Event Format

```text
event: EVENT_TYPE
data: {"field": "value", ...}
id: 1
```

### Lifecycle Events

| Event | Description |
|-------|-------------|
| `RUN_STARTED` | Workflow execution started |
| `RUN_FINISHED` | Workflow completed successfully |
| `RUN_ERROR` | Workflow ended with error |
| `STEP_STARTED` | Task started |
| `STEP_FINISHED` | Task completed |

**RUN_STARTED:**
```json
{
  "type": "RUN_STARTED",
  "runId": "run-456",
  "threadId": "thread-123",
  "timestamp": 1702345678000
}
```

**RUN_FINISHED:**
```json
{
  "type": "RUN_FINISHED",
  "runId": "run-456",
  "timestamp": 1702345679000,
  "result": { "output": "completed" }
}
```

**RUN_ERROR:**
```json
{
  "type": "RUN_ERROR",
  "runId": "run-456",
  "message": "Task execution failed",
  "code": "TASK_FAILED",
  "timestamp": 1702345679000
}
```

### Message Events (LLM Streaming)

| Event | Description |
|-------|-------------|
| `TEXT_MESSAGE_START` | Message start |
| `TEXT_MESSAGE_CONTENT` | Message content (delta) |
| `TEXT_MESSAGE_END` | Message end |

**Sequence Example:**
```text
event: TEXT_MESSAGE_START
data: {"type":"TEXT_MESSAGE_START","messageId":"msg-1","role":"assistant","timestamp":1702345678000}
id: 5

event: TEXT_MESSAGE_CONTENT
data: {"type":"TEXT_MESSAGE_CONTENT","messageId":"msg-1","delta":"Hello, ","timestamp":1702345678001}
id: 6

event: TEXT_MESSAGE_CONTENT
data: {"type":"TEXT_MESSAGE_CONTENT","messageId":"msg-1","delta":"how can I help?","timestamp":1702345678002}
id: 7

event: TEXT_MESSAGE_END
data: {"type":"TEXT_MESSAGE_END","messageId":"msg-1","timestamp":1702345678003}
id: 8
```

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
| `STATE_SNAPSHOT` | State snapshot (full state) |
| `STATE_DELTA` | State delta (RFC 6902 JSON Patch) |

---

## Human-in-the-Loop (HITL)

HITL is a feature that pauses workflow execution to wait for user input.

### HITL Flow

```text
Client                         AG-UI Server
    |                              |
    |-- POST /ag-ui/run ---------->|
    |<-- RUN_STARTED --------------|
    |<-- STEP_STARTED -------------|
    |<-- TOOL_CALL_START ----------|  toolCallName: "HUMAN_INPUT"
    |<-- TOOL_CALL_ARGS -----------|  Current output data
    |<-- TOOL_CALL_END ------------|
    |   (Stream paused)            |
    |                              |
    |   [User provides input]      |
    |                              |
    |-- POST /ag-ui/message ------>|  toolCallResults
    |<-- TOOL_CALL_RESULT ---------|
    |<-- STEP_FINISHED ------------|
    |<-- ... (continues) ----------|
    |<-- RUN_FINISHED -------------|
```

### HITL Event Sequence

1. **TOOL_CALL_START** - `toolCallName: "HUMAN_INPUT"`
2. **TOOL_CALL_ARGS** - Current workflow output (JSON string)
3. **TOOL_CALL_END** - End of tool call information

At this point, the stream pauses and waits for client input.

### Submitting User Input

```http
POST /ag-ui/message
Content-Type: application/json

{
  "runId": "run-456",
  "toolCallResults": [
    {
      "toolCallId": "wait_run-456",
      "result": {
        "approved": true,
        "comment": "User approved the action"
      }
    }
  ]
}
```

**Important:**
- `toolCallId` must **exactly match** the value received in `TOOL_CALL_START`
- `toolCallResults` must contain **exactly 1 item**
- `result` can be any JSON value (used by the workflow)

### Events After HITL Resume

```text
event: TOOL_CALL_RESULT
data: {"type":"TOOL_CALL_RESULT","toolCallId":"wait_run-456","result":{"approved":true},"timestamp":...}

event: STEP_FINISHED
data: {"type":"STEP_FINISHED","stepId":"task1","timestamp":...}

event: STEP_STARTED
data: {"type":"STEP_STARTED","stepId":"task2","stepName":"next_task","timestamp":...}
...
```

---

## LLM Tool Calling HITL

When using the LLM_CHAT runner with `isAutoCalling: false`, the LLM can request tool calls that require user approval before execution. This is different from workflow HITL (checkpoint-based) as it handles LLM function calling.

### Overview

LLM Tool Calling HITL allows users to:
- Review tool calls requested by the LLM before execution
- Approve, modify, or reject tool call arguments
- Provide tool execution results back to the LLM

### Enabling LLM Tool Calling HITL

Set `isAutoCalling: false` in the workflow's `functionOptions`:

```json
{
  "context": [{
    "type": "workflow_inline",
    "data": {
      "workflow": {
        "do": [{
          "ChatTask": {
            "run": {
              "runner": {
                "name": "LLM_CHAT",
                "arguments": {
                  "functionOptions": {
                    "useFunctionCalling": true,
                    "functionSetName": "command-functions",
                    "isAutoCalling": false
                  }
                }
              }
            }
          }
        }]
      }
    }
  }]
}
```

### LLM Tool Calling Flow

```text
Client                         AG-UI Server                    LLM
   |                              |                             |
   |-- POST /ag-ui/run ---------->|                             |
   |                              |-- Execute LLM_CHAT -------->|
   |                              |<-- Tool call request -------|
   |<-- TOOL_CALL_START ----------|   (pending_tool_calls)      |
   |<-- TOOL_CALL_ARGS -----------|                             |
   |   (Stream paused)            |                             |
   |                              |                             |
   |   [User approves tool]       |                             |
   |                              |                             |
   |-- POST /ag-ui/message ------>|                             |
   |   (toolCallResults)          |                             |
   |<-- TOOL_CALL_RESULT ---------|                             |
   |<-- TOOL_CALL_END ------------|                             |
   |<-- RUN_FINISHED -------------|                             |
   |                              |                             |
   |   [Client sends follow-up    |                             |
   |    with tool result in       |                             |
   |    messages]                 |                             |
   |                              |                             |
   |-- POST /ag-ui/run ---------->|                             |
   |   (messages with TOOL role)  |-- Execute LLM_CHAT -------->|
   |                              |   (with tool result)        |
   |<-- TEXT_MESSAGE_* -----------|<-- LLM response ------------|
```

### Tool Call Event Sequence

1. **TOOL_CALL_START** - Indicates a tool call request from LLM

```json
{
  "type": "TOOL_CALL_START",
  "toolCallId": "call_abc123",
  "toolCallName": "COMMAND___run",
  "parentMessageId": "msg-1",
  "timestamp": 1702345678000
}
```

2. **TOOL_CALL_ARGS** - Tool arguments (streamed as delta)

```json
{
  "type": "TOOL_CALL_ARGS",
  "toolCallId": "call_abc123",
  "delta": "{\"command\": \"date\"}",
  "timestamp": 1702345678001
}
```

At this point, if `requires_tool_execution` is true (HITL mode), the stream pauses and waits for user approval.

### Tool Name Format

Tool names follow the pattern `RUNNER___method` where:
- `RUNNER` is the runner name (e.g., `COMMAND`, `HTTP_REQUEST`)
- `___` (triple underscore) is the separator
- `method` is the method name (e.g., `run`, `get`)

Examples:
- `COMMAND___run` - Execute a shell command
- `HTTP_REQUEST___get` - Make an HTTP GET request

**Recommendation:** When displaying to users, convert `___` to `/` for readability:
- `COMMAND___run` → `COMMAND/run`

### Submitting Tool Call Result

```http
POST /ag-ui/message
Content-Type: application/json

{
  "runId": "run-456",
  "toolCallResults": [
    {
      "toolCallId": "call_abc123",
      "result": {
        "command": "date"
      }
    }
  ]
}
```

**Response Events:**

```text
event: TOOL_CALL_RESULT
data: {"type":"TOOL_CALL_RESULT","toolCallId":"call_abc123","result":{"command":"date"},"timestamp":...}

event: TOOL_CALL_END
data: {"type":"TOOL_CALL_END","toolCallId":"call_abc123","timestamp":...}

event: RUN_FINISHED
data: {"type":"RUN_FINISHED","runId":"run-456","timestamp":...}
```

### Continuing the Conversation

After receiving `RUN_FINISHED`, send a new `/ag-ui/run` request with the tool result included in the messages:

```json
{
  "context": [{ "type": "workflow_inline", "data": { ... } }],
  "messages": [
    { "role": "user", "content": "Run the date command" },
    { "role": "assistant", "content": "" },
    { "role": "tool", "content": "Tool: COMMAND/run\nArguments: {\"command\":\"date\"}\nResult: {\"command\":\"date\"}" }
  ]
}
```

The LLM will process the tool result and continue the conversation.

### Client Implementation Example

```typescript
// Track pending tool calls
const toolCallsRef = useRef<Map<string, ToolCall>>(new Map());
let pendingToolCalls: ToolCall[] = [];

function handleEvent(event: AgUiEvent) {
  switch (event.type) {
    case 'TOOL_CALL_START':
      toolCallsRef.current.set(event.toolCallId, {
        id: event.toolCallId,
        name: event.toolCallName,
        arguments: {},
        argumentsRaw: '',
        status: 'pending'
      });
      break;

    case 'TOOL_CALL_ARGS':
      const toolCall = toolCallsRef.current.get(event.toolCallId);
      if (toolCall) {
        toolCall.argumentsRaw += event.delta;
        try {
          toolCall.arguments = JSON.parse(toolCall.argumentsRaw);
        } catch {
          // Still accumulating JSON
        }
      }
      break;

    case 'RUN_FINISHED':
      // Check for pending tool calls requiring approval
      pendingToolCalls = Array.from(toolCallsRef.current.values())
        .filter(tc => tc.status === 'pending');

      if (pendingToolCalls.length > 0) {
        // Show approval UI
        showToolApprovalUI(pendingToolCalls);
      }
      break;
  }
}

// Approve a tool call
async function approveToolCall(toolCallId: string, result: unknown) {
  const response = await fetch(`${baseUrl}/ag-ui/message`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      runId: currentRunId,
      toolCallResults: [{ toolCallId, result }]
    })
  });

  // Process SSE response, then send follow-up
  // ...

  // After RUN_FINISHED, continue conversation with tool result
  await sendFollowUpWithToolResult(toolCallId, result);
}

// Format tool name for display
function formatToolName(name: string): string {
  return name.replace(/___/g, '/');
}
```

### Rejecting a Tool Call

To reject a tool call, send a result indicating rejection:

```json
{
  "runId": "run-456",
  "toolCallResults": [
    {
      "toolCallId": "call_abc123",
      "result": {
        "rejected": true,
        "reason": "User declined to execute this command"
      }
    }
  ]
}
```

The LLM will receive this rejection and can respond accordingly in the follow-up request.

---

## Error Handling

### HTTP Error Response

```json
{
  "error": {
    "code": "ERROR_CODE",
    "message": "Human-readable error message"
  }
}
```

### Error Code Reference

| Code | HTTP Status | Description |
|------|-------------|-------------|
| `SESSION_NOT_FOUND` | 404 | Session not found |
| `SESSION_EXPIRED` | 410 | Session expired |
| `INVALID_INPUT` | 400 | Invalid input |
| `WORKFLOW_NOT_FOUND` | 404 | Workflow not found |
| `WORKFLOW_INIT_FAILED` | 500 | Workflow initialization failed |
| `TIMEOUT` | 504 | Timeout |
| `CANCELLED` | 200 | Cancelled |
| `INVALID_SESSION_STATE` | 409 | Invalid session state (HITL) |
| `INVALID_TOOL_CALL_ID` | 400 | Invalid tool_call_id (HITL) |
| `CHECKPOINT_NOT_FOUND` | 404 | Checkpoint not found |
| `HITL_INFO_NOT_FOUND` | 404 | HITL waiting info not found |
| `INTERNAL_ERROR` | 500 | Internal error |

### Errors in SSE Stream

Delivered as `RUN_ERROR` events:

```text
event: RUN_ERROR
data: {"type":"RUN_ERROR","runId":"run-456","message":"Task failed","code":"TASK_FAILED","timestamp":...}
```

---

## Implementation Examples

### TypeScript (fetch + EventSource)

```typescript
interface RunAgentInput {
  threadId?: string;
  runId?: string;
  messages?: Message[];
  context: Context[];
  forwardedProps?: JobworkerpFwdProps;
}

interface AgUiEvent {
  type: string;
  [key: string]: unknown;
}

class AgUiClient {
  private baseUrl: string;
  private token?: string;

  constructor(baseUrl: string, token?: string) {
    this.baseUrl = baseUrl;
    this.token = token;
  }

  private headers(): HeadersInit {
    const headers: HeadersInit = {
      'Content-Type': 'application/json',
    };
    if (this.token) {
      headers['Authorization'] = `Bearer ${this.token}`;
    }
    return headers;
  }

  // Run workflow
  async runWorkflow(
    input: RunAgentInput,
    onEvent: (event: AgUiEvent) => void,
    onError?: (error: Error) => void
  ): Promise<{ runId: string; sessionId: string }> {
    const response = await fetch(`${this.baseUrl}/ag-ui/run`, {
      method: 'POST',
      headers: this.headers(),
      body: JSON.stringify(input),
    });

    if (!response.ok) {
      const error = await response.json();
      throw new Error(error.error?.message || 'Request failed');
    }

    const runId = response.headers.get('x-ag-ui-run-id') || '';
    const sessionId = response.headers.get('x-ag-ui-session-id') || '';

    // Process SSE stream
    const reader = response.body?.getReader();
    const decoder = new TextDecoder();

    if (reader) {
      this.processStream(reader, decoder, onEvent, onError);
    }

    return { runId, sessionId };
  }

  private async processStream(
    reader: ReadableStreamDefaultReader<Uint8Array>,
    decoder: TextDecoder,
    onEvent: (event: AgUiEvent) => void,
    onError?: (error: Error) => void
  ) {
    let buffer = '';

    try {
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;

        buffer += decoder.decode(value, { stream: true });
        const lines = buffer.split('\n');
        buffer = lines.pop() || '';

        let eventType = '';
        let eventData = '';

        for (const line of lines) {
          if (line.startsWith('event: ')) {
            eventType = line.slice(7);
          } else if (line.startsWith('data: ')) {
            eventData = line.slice(6);
          } else if (line === '' && eventData) {
            try {
              const event = JSON.parse(eventData) as AgUiEvent;
              onEvent(event);
            } catch (e) {
              console.warn('Failed to parse event:', eventData);
            }
            eventType = '';
            eventData = '';
          }
        }
      }
    } catch (error) {
      onError?.(error as Error);
    }
  }

  // Send HITL message
  async sendMessage(
    runId: string,
    toolCallId: string,
    result: unknown,
    onEvent: (event: AgUiEvent) => void,
    onError?: (error: Error) => void
  ): Promise<void> {
    const response = await fetch(`${this.baseUrl}/ag-ui/message`, {
      method: 'POST',
      headers: this.headers(),
      body: JSON.stringify({
        runId,
        toolCallResults: [{ toolCallId, result }],
      }),
    });

    if (!response.ok) {
      const error = await response.json();
      throw new Error(error.error?.message || 'Request failed');
    }

    const reader = response.body?.getReader();
    const decoder = new TextDecoder();

    if (reader) {
      this.processStream(reader, decoder, onEvent, onError);
    }
  }

  // Cancel workflow
  async cancel(runId: string): Promise<void> {
    const response = await fetch(`${this.baseUrl}/ag-ui/run/${runId}`, {
      method: 'DELETE',
      headers: this.headers(),
    });

    if (!response.ok) {
      const error = await response.json();
      throw new Error(error.error?.message || 'Cancel failed');
    }
  }

  // Get state
  async getState(runId: string): Promise<unknown> {
    const response = await fetch(`${this.baseUrl}/ag-ui/state/${runId}`, {
      headers: this.headers(),
    });

    if (!response.ok) {
      const error = await response.json();
      throw new Error(error.error?.message || 'Get state failed');
    }

    return response.json();
  }
}
```

### Usage Example

```typescript
const client = new AgUiClient('http://localhost:8080', 'your-token');

// Event handler
let pendingToolCall: { id: string; args: string } | null = null;

function handleEvent(event: AgUiEvent) {
  switch (event.type) {
    case 'RUN_STARTED':
      console.log('Workflow started:', event.runId);
      break;

    case 'TEXT_MESSAGE_CONTENT':
      process.stdout.write(event.delta as string);
      break;

    case 'TOOL_CALL_START':
      if (event.toolCallName === 'HUMAN_INPUT') {
        pendingToolCall = { id: event.toolCallId as string, args: '' };
        console.log('\n[Waiting for user input...]');
      }
      break;

    case 'TOOL_CALL_ARGS':
      if (pendingToolCall) {
        pendingToolCall.args += event.delta as string;
      }
      break;

    case 'TOOL_CALL_END':
      if (pendingToolCall) {
        console.log('Current data:', pendingToolCall.args);
        // Collect user input here and call sendMessage
      }
      break;

    case 'RUN_FINISHED':
      console.log('\nWorkflow completed');
      break;

    case 'RUN_ERROR':
      console.error('\nWorkflow error:', event.message);
      break;
  }
}

// Execute
async function main() {
  const { runId } = await client.runWorkflow(
    {
      context: [{ type: 'workflow_definition', workflowName: 'my_workflow' }],
      messages: [{ role: 'user', content: 'Process this request' }],
    },
    handleEvent,
    (error) => console.error('Stream error:', error)
  );

  // If HITL input is needed
  if (pendingToolCall) {
    const userInput = await promptUser('Enter your response:');
    await client.sendMessage(
      runId,
      pendingToolCall.id,
      { userInput },
      handleEvent
    );
  }
}
```

### Python (requests + sseclient)

```python
import requests
import sseclient
import json
from typing import Callable, Optional, Any

class AgUiClient:
    def __init__(self, base_url: str, token: Optional[str] = None):
        self.base_url = base_url
        self.token = token

    def _headers(self) -> dict:
        headers = {"Content-Type": "application/json"}
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        return headers

    def run_workflow(
        self,
        workflow_name: str,
        messages: list = None,
        on_event: Callable[[dict], None] = None,
    ) -> tuple[str, str]:
        """Execute workflow and process SSE stream"""
        payload = {
            "context": [{"type": "workflow_definition", "workflowName": workflow_name}],
            "messages": messages or [],
        }

        response = requests.post(
            f"{self.base_url}/ag-ui/run",
            headers=self._headers(),
            json=payload,
            stream=True,
        )
        response.raise_for_status()

        run_id = response.headers.get("x-ag-ui-run-id", "")
        session_id = response.headers.get("x-ag-ui-session-id", "")

        if on_event:
            client = sseclient.SSEClient(response)
            for event in client.events():
                if event.data:
                    data = json.loads(event.data)
                    on_event(data)

        return run_id, session_id

    def send_message(
        self,
        run_id: str,
        tool_call_id: str,
        result: Any,
        on_event: Callable[[dict], None] = None,
    ):
        """Send HITL message"""
        payload = {
            "runId": run_id,
            "toolCallResults": [{"toolCallId": tool_call_id, "result": result}],
        }

        response = requests.post(
            f"{self.base_url}/ag-ui/message",
            headers=self._headers(),
            json=payload,
            stream=True,
        )
        response.raise_for_status()

        if on_event:
            client = sseclient.SSEClient(response)
            for event in client.events():
                if event.data:
                    data = json.loads(event.data)
                    on_event(data)

    def cancel(self, run_id: str):
        """Cancel workflow"""
        response = requests.delete(
            f"{self.base_url}/ag-ui/run/{run_id}",
            headers=self._headers(),
        )
        response.raise_for_status()
        return response.json()

    def get_state(self, run_id: str) -> dict:
        """Get workflow state"""
        response = requests.get(
            f"{self.base_url}/ag-ui/state/{run_id}",
            headers=self._headers(),
        )
        response.raise_for_status()
        return response.json()


# Usage example
if __name__ == "__main__":
    client = AgUiClient("http://localhost:8080", "your-token")
    pending_tool_call = None

    def handle_event(event: dict):
        global pending_tool_call
        event_type = event.get("type")

        if event_type == "RUN_STARTED":
            print(f"Workflow started: {event['runId']}")

        elif event_type == "TEXT_MESSAGE_CONTENT":
            print(event["delta"], end="", flush=True)

        elif event_type == "TOOL_CALL_START":
            if event["toolCallName"] == "HUMAN_INPUT":
                pending_tool_call = {"id": event["toolCallId"], "args": ""}
                print("\n[Waiting for user input...]")

        elif event_type == "TOOL_CALL_ARGS":
            if pending_tool_call:
                pending_tool_call["args"] += event["delta"]

        elif event_type == "TOOL_CALL_END":
            if pending_tool_call:
                print(f"Current data: {pending_tool_call['args']}")

        elif event_type == "RUN_FINISHED":
            print("\nWorkflow completed")

        elif event_type == "RUN_ERROR":
            print(f"\nWorkflow error: {event['message']}")

    run_id, _ = client.run_workflow("my_workflow", on_event=handle_event)

    if pending_tool_call:
        user_input = input("Enter your response: ")
        client.send_message(
            run_id,
            pending_tool_call["id"],
            {"userInput": user_input},
            on_event=handle_event,
        )
```

---

## Reconnection and Recovery

### Reconnection with Last-Event-ID

Record the SSE `id` field and use the `Last-Event-ID` header when reconnecting to prevent event loss.

```typescript
let lastEventId = 0;

function handleEvent(event: AgUiEvent, eventId: number) {
  lastEventId = eventId;
  // ... event processing
}

async function reconnect(runId: string) {
  const response = await fetch(`${baseUrl}/ag-ui/stream/${runId}`, {
    headers: {
      ...headers,
      'Last-Event-ID': lastEventId.toString(),
    },
  });
  // Resume stream processing
}
```

### Checking Session State

It is recommended to check the workflow state via `/ag-ui/state/{run_id}` before reconnecting.

---

## Notes

1. **Single SSE Stream Connection**: Multiple stream connections to the same run_id are not recommended
2. **HITL Timeout**: Sessions have a TTL; long waits may result in session expiration
3. **Event Ordering**: Events are ordered by the `id` field
4. **RUN_FINISHED and RUN_ERROR are Mutually Exclusive**: Both will never occur simultaneously
