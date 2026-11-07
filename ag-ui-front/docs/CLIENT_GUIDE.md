# AG-UI Client Guide

This guide describes the standard AG-UI 1.0 RunAgentInput and SSE interface. AG-UI Front serves endpoint profiles registered by the server administrator. The URL selects the execution mode; the request body cannot select a worker, workflow, or guardrail configuration.

## Choose an endpoint

The administrator registers a path, worker name, and mode in `AG_UI_PROFILES`. `streaming` runs a registered LLM/chat worker in real time. `workflow` publishes the output of a guardrail workflow after it completes. See the README profile table for the deployed configuration.

```http
POST <registered profile path>
Content-Type: application/json
Authorization: Bearer <token>
```

## RunAgentInput

The body follows the AG-UI 1.0 schema. `threadId`, `runId`, and `messages` are required. Optional fields follow the standard definition. Context entries use `{ "description": "...", "value": "..." }`; `tools` contains standard Tool definitions and `resume` is an array of standard ResumeEntry values. Do not send proprietary `workflow_inline`, worker selection, or configuration overrides.

```json
{
  "threadId": "thread-123",
  "runId": "run-456",
  "messages": [{ "id": "msg-1", "role": "user", "content": "Hello" }],
  "tools": []
}
```

`state` and `forwardedProps` are client data. They cannot change server settings or checkpoints. A workflow profile returns only shared state explicitly mapped by the workflow.

A workflow worker returns an output matching the public contract. `messages` is required and contains additional assistant messages; `state` and `result` are optional. Do not merge intermediate task output or internal context into this object without an explicit projection.

```json
{
  "messages": [
    { "id": "assistant-1", "role": "assistant", "content": "Guardrail-checked answer" }
  ],
  "state": { "lastAnswer": "Guardrail-checked answer" },
  "result": { "approved": true }
}
```

## Event stream

The POST response uses `text/event-stream`. The stream starts with `RUN_STARTED` and ends with `RUN_FINISHED` or `RUN_ERROR`. Events use fields from the AG-UI 1.0 schema. Reconnect with `GET /ag-ui/stream/{run_id}` and the `Last-Event-ID` header. Cancelling a `streaming` profile run with `DELETE /ag-ui/run/{run_id}` ends both the POST response and any reconnected stream with `RUN_ERROR` code `CANCELLED`. A slow reader may miss events after the cancellation on the POST response; if the stream closes without a terminal event, reconnect with `Last-Event-ID`. A `DELETE` for a run that has already ended changes nothing and returns success. State retrieval never exposes internal workflow variables.

### Tool ownership

The client executes the client tools it passes in `RunAgentInput.tools`. Front emits `TOOL_CALL_START`, `TOOL_CALL_ARGS`, and `TOOL_CALL_END`, then ends the run. The client includes the result in the next run as a `role: "tool"` message with the same `toolCallId`. In a `streaming` profile, a ToolMessage `error` reaches the LLM as a failed tool result, keeping any partial result in its content, and ToolMessage content accepts text only; a request with media parts such as images is rejected.

jobworkerp FunctionSets are server tools, configured as a separate LLM task inside a workflow profile. Client tools and server FunctionSets cannot be supplied to the same LLM call. A server tool result is added to history and emitted as `TOOL_CALL_RESULT` before the run ends.

## Resume

To resume a HITL/checkpoint, use the same `threadId` and profile URL, a new `runId`, and an array of standard `resume` entries. The server keeps checkpoint identifiers internally. It does not continue the checkpoint while interrupts remain unanswered. A failure confirmed before the queue write starts (for example, request serialization or Redis connection acquisition) leaves the interrupts pending, so the same answers can be resent. If Redis accepts the queue write but its reply is lost, the outcome is unknown: the job may already be queued, running, or complete. The response run ends with `RUN_STARTED` followed by `RUN_ERROR` with code `ENQUEUE_OUTCOME_UNKNOWN`; do not automatically resend the same interrupt. Before deciding how to recover, ask an operator to find the job ID in the structured server log for that run. Continue client tools with a ToolMessage in the next run instead of resume.
