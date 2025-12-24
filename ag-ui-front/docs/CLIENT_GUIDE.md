# AG-UI Client Guide

This document is a guide for client implementers using the AG-UI Front HTTP API.

## Table of Contents

1. [Overview](#overview)
2. [Authentication](#authentication)
3. [API Endpoints](#api-endpoints)
4. [SSE Events](#sse-events)
5. [Human-in-the-Loop (HITL)](#human-in-the-loop-hitl)
6. [LLM Tool Calling HITL](#llm-tool-calling-hitl)
7. [AG-UI Interrupts (Resume)](#ag-ui-interrupts-resume)
8. [Error Handling](#error-handling)
9. [Implementation Examples](#implementation-examples)

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
| `resume` | ResumeInfo | No | Resume info for interrupted runs ([AG-UI Interrupts](#ag-ui-interrupts-resume)) |

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
  "result": { "output": "completed" },
  "outcome": "success"
}
```

**RUN_FINISHED with Interrupt (AG-UI Interrupts):**

When a workflow requires user approval (e.g., LLM tool calling with HITL), `RUN_FINISHED` includes interrupt information:

```json
{
  "type": "RUN_FINISHED",
  "runId": "run-456",
  "timestamp": 1702345679000,
  "outcome": "interrupt",
  "interrupt": {
    "id": "int_abc123",
    "reason": "tool_approval_required",
    "payload": {
      "pendingToolCalls": [
        {
          "callId": "call_xyz",
          "fnName": "COMMAND___run",
          "fnArguments": "{\"command\":\"date\"}"
        }
      ],
      "checkpointPosition": "/tasks/ChatTask",
      "workflowName": "copilot-chat"
    }
  }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `outcome` | string | `"success"` or `"interrupt"` |
| `interrupt` | object | Present when `outcome` is `"interrupt"` |
| `interrupt.id` | string | Unique interrupt ID for resuming |
| `interrupt.reason` | string | Reason for interrupt (e.g., `"tool_approval_required"`) |
| `interrupt.payload` | object | Context-specific payload |

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
- Server-side tool execution with results passed back to LLM automatically

### Why AG-UI Interrupts Instead of Standard AG-UI Tools?

The standard [AG-UI Tools specification](https://docs.ag-ui.com/concepts/tools) assumes that tools are defined and executed on the **client side**. However, jobworkerp-rs has a different architecture:

| Aspect | Standard AG-UI Tools | jobworkerp-rs |
|--------|---------------------|---------------|
| Tool Definition | Client defines tools and sends to server | Server defines tools (Runners: COMMAND, HTTP_REQUEST, etc.) |
| Tool Execution | Client executes tools locally | Server executes tools via job workers |
| Tool Discovery | Client knows available tools | Client discovers tools from server's function sets |
| Result Handling | Client sends results back to server | Server handles results internally and continues LLM |

**jobworkerp-rs specific characteristics:**

1. **Server-side Tool Registry**: Tools are registered as "Runners" (COMMAND, HTTP_REQUEST, PYTHON_COMMAND, etc.) or loaded from plugins on the server
2. **Function Sets**: Tools are organized into function sets (e.g., `command-functions`) configured on the server
3. **Job Worker Execution**: Tool execution is handled by jobworkerp's distributed job worker system
4. **Unified Execution**: Both tool execution and LLM continuation happen server-side in a single workflow

Because of this architecture, we use [AG-UI Interrupts](https://docs.ag-ui.com/drafts/interrupts) instead of standard AG-UI Tools:

- **Interrupt**: When LLM requests a tool call, the workflow pauses and returns `RUN_FINISHED` with `outcome: "interrupt"`
- **Resume**: Client approves/rejects via `resume` field in `/ag-ui/run`, and the server executes tools and continues the LLM conversation

This approach provides Human-in-the-Loop (HITL) control while keeping tool execution on the server where the tools are defined and have access to system resources.

### Enabling LLM Tool Calling HITL

Set `isAutoCalling: false` in the workflow's `functionOptions`:

```json
{
  "context": [{
    "type": "workflow_inline",
    "workflow": {
      "document": {
        "dsl": "1.0.0-jobworkerp",
        "namespace": "default",
        "name": "copilot-chat"
      },
      "do": [{
        "ChatTask": {
          "useStreaming": true,
          "run": {
            "runner": {
              "name": "LLM_CHAT",
              "arguments": {
                "messages": "${ $runnerMessages }",
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
  }]
}
```

### LLM Tool Calling Flow (AG-UI Interrupts)

This flow follows the [AG-UI Interrupts specification](https://docs.ag-ui.com/drafts/interrupts):

```text
Client                         AG-UI Server                    LLM
   |                              |                             |
   |-- POST /ag-ui/run ---------->|                             |
   |                              |-- Execute LLM_CHAT -------->|
   |                              |<-- Tool call request -------|
   |<-- TOOL_CALL_START ----------|   (pending_tool_calls)      |
   |<-- TOOL_CALL_ARGS -----------|                             |
   |<-- RUN_FINISHED -------------|   outcome: "interrupt"      |
   |   (with interrupt info)      |   interrupt: {...}          |
   |                              |                             |
   |   [User approves tool]       |                             |
   |                              |                             |
   |-- POST /ag-ui/run ---------->|                             |
   |   (with resume field)        |-- Execute tools ----------->|
   |                              |<-- Tool results ------------|
   |                              |-- Continue LLM_CHAT ------->|
   |<-- TOOL_CALL_RESULT ---------|                             |
   |<-- TEXT_MESSAGE_* -----------|<-- LLM response ------------|
   |<-- RUN_FINISHED -------------|   outcome: "success"        |
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

3. **RUN_FINISHED with interrupt** - Workflow pauses for approval

```json
{
  "type": "RUN_FINISHED",
  "runId": "run-456",
  "outcome": "interrupt",
  "interrupt": {
    "id": "int_xyz789",
    "reason": "tool_approval_required",
    "payload": {
      "pendingToolCalls": [
        {
          "callId": "call_abc123",
          "fnName": "COMMAND___run",
          "fnArguments": "{\"command\":\"date\"}"
        }
      ],
      "checkpointPosition": "/tasks/ChatTask",
      "workflowName": "copilot-chat"
    }
  },
  "timestamp": 1702345678002
}
```

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

---

## AG-UI Interrupts (Resume)

The AG-UI Interrupts specification provides a standardized way to handle tool approval. When `RUN_FINISHED` has `outcome: "interrupt"`, use the `resume` field in `/ag-ui/run` to continue.

Reference: [AG-UI Interrupts Draft](https://docs.ag-ui.com/drafts/interrupts)

### Approving Tool Calls

Send a new `/ag-ui/run` request with the `resume` field:

```http
POST /ag-ui/run
Content-Type: application/json

{
  "threadId": "thread-123",
  "messages": [...],
  "context": [{ "type": "workflow_inline", "workflow": {...} }],
  "resume": {
    "interruptId": "int_xyz789",
    "payload": {
      "type": "approve"
    }
  }
}
```

**ResumeInfo Fields:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `interruptId` | string | Yes | The interrupt ID from `RUN_FINISHED.interrupt.id` |
| `payload.type` | string | Yes | `"approve"` or `"reject"` |
| `payload.toolResults` | array | No | Optional client-side tool results (for approve) |
| `payload.reason` | string | No | Optional rejection reason (for reject) |

**Response Events (on approval):**

The server executes the pending tools and continues the LLM conversation:

```text
event: TOOL_CALL_RESULT
data: {"type":"TOOL_CALL_RESULT","toolCallId":"call_abc123","result":{"output":"Wed Dec 25 10:30:00 JST 2024"},"timestamp":...}

event: TEXT_MESSAGE_START
data: {"type":"TEXT_MESSAGE_START","messageId":"msg-2","role":"assistant","timestamp":...}

event: TEXT_MESSAGE_CONTENT
data: {"type":"TEXT_MESSAGE_CONTENT","messageId":"msg-2","delta":"The current date is...","timestamp":...}

event: TEXT_MESSAGE_END
data: {"type":"TEXT_MESSAGE_END","messageId":"msg-2","timestamp":...}

event: RUN_FINISHED
data: {"type":"RUN_FINISHED","runId":"run-456","outcome":"success","timestamp":...}
```

### Rejecting Tool Calls

To reject a tool call:

```json
{
  "threadId": "thread-123",
  "messages": [...],
  "context": [...],
  "resume": {
    "interruptId": "int_xyz789",
    "payload": {
      "type": "reject",
      "reason": "User declined to execute this command"
    }
  }
}
```

The LLM will receive the rejection and respond accordingly.

### Client Implementation Example

```typescript
interface InterruptInfo {
  id: string;
  reason: string;
  payload: {
    pendingToolCalls: Array<{
      callId: string;
      fnName: string;
      fnArguments: string;
    }>;
    checkpointPosition: string;
    workflowName: string;
  };
}

// State
let pendingInterrupt: InterruptInfo | null = null;
const toolCallsRef = useRef<Map<string, ToolCall>>(new Map());

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
      if (event.outcome === 'interrupt' && event.interrupt) {
        // Store interrupt info for resume
        pendingInterrupt = event.interrupt;
        // Show approval UI with pending tool calls
        showToolApprovalUI(event.interrupt.payload.pendingToolCalls);
      } else {
        // Normal completion
        pendingInterrupt = null;
      }
      break;
  }
}

// Approve tool calls
async function approveToolCall() {
  if (!pendingInterrupt) return;

  const response = await fetch(`${baseUrl}/ag-ui/run`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      threadId: currentThreadId,
      messages: conversationMessages,
      context: [{ type: 'workflow_inline', workflow: CHAT_WORKFLOW }],
      resume: {
        interruptId: pendingInterrupt.id,
        payload: { type: 'approve' }
      }
    })
  });

  // Process SSE response - tools are executed server-side
  // and LLM continues automatically
  await processSSEStream(response);
}

// Reject tool calls
async function rejectToolCall(reason: string) {
  if (!pendingInterrupt) return;

  const response = await fetch(`${baseUrl}/ag-ui/run`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      threadId: currentThreadId,
      messages: conversationMessages,
      context: [{ type: 'workflow_inline', workflow: CHAT_WORKFLOW }],
      resume: {
        interruptId: pendingInterrupt.id,
        payload: { type: 'reject', reason }
      }
    })
  });

  await processSSEStream(response);
}

// Format tool name for display
function formatToolName(name: string): string {
  return name.replace(/___/g, '/');
}
```

### Legacy Approach (Deprecated)

The `/ag-ui/message` endpoint is still supported for backward compatibility but is deprecated for LLM tool calling HITL. The new AG-UI Interrupts approach using `resume` in `/ag-ui/run` is recommended as it:
- Follows the AG-UI specification
- Executes tools server-side automatically
- Continues LLM conversation in a single request
- Simplifies client implementation

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
