# Agent Server

Agent Server is a separate process for chat orchestration, Skills discovery and activation, and registered Worker tool execution. It talks to jobworkerp over gRPC; it does not register its tools in jobworkerp `FunctionSet`. The existing FunctionSet endpoints remain independent.

## Start

From the repository root, build and run `cargo run -p agent-server`. A local-only configuration, for example:

```sh
export AGENT_SERVER_GRPC_ENDPOINT=http://127.0.0.1:9000
# When jobworkerp has AUTH_TOKEN configured, set this to the same secret.
# export AGENT_SERVER_GRPC_AUTH_TOKEN="$JOBWORKERP_AUTH_TOKEN"
export AGENT_SERVER_AUTH_MODE=local-no-token
export AGENT_SERVER_ADDR=127.0.0.1:8181
export AGENT_SERVER_SKILLS_ROOTS=/path/to/skills
export AGENT_SERVER_TOOL_REGISTRY_PATH=/path/to/tool-registry.json
cargo run -p agent-server
```

`local-no-token` must be selected explicitly and is restricted to loopback. `local-shared-token` instead requires `AGENT_SERVER_TOKEN`. For a non-loopback `AGENT_SERVER_ADDR`, select `AGENT_SERVER_AUTH_MODE=external` and supply distinct `AGENT_SERVER_CHAT_TOKEN` and `AGENT_SERVER_ADMIN_TOKEN`, plus `AGENT_SERVER_ALLOWED_HOSTS` and `AGENT_SERVER_ALLOWED_ORIGINS` allowlists. **Protect external HTTP Bearer tokens in transit with TLS** (for example, a trusted TLS-terminating proxy); an external plain-HTTP listener is not suitable over untrusted networks. Protect the gRPC link and the on-disk tool registry according to your deployment's trust boundary. Do not expose the no-token listener through a reverse proxy.

`AGENT_SERVER_GRPC_AUTH_TOKEN` is optional for jobworkerp deployments without gRPC token authentication. When jobworkerp has `AUTH_TOKEN` configured, set this variable to the same non-empty secret; Agent Server sends it as `jobworkerp-auth` metadata on every Worker, Runner, and Job RPC, including enqueue, result streaming, and Delete. The token is bounded and rejected if empty or whitespace-containing. Agent Server does not log the token or gRPC endpoint.

The default `STORAGE_TYPE=Standalone` keeps pending approvals in process memory. `STORAGE_TYPE=Scalable` requires `REDIS_URL`; `REDIS_USERNAME` and `REDIS_PASSWORD` can supply separate Redis ACL credentials. `REDIS_POOL_*` options are rejected because this release uses a Redis client directly rather than a configurable connection pool. Pending approvals have a fixed TTL (default 15 minutes, configurable with `AGENT_SERVER_APPROVAL_TTL_SECS`) and are consumed once. Idle approval sessions are swept after that TTL; sessions still owning Worker jobs remain available for explicit cancellation. Redis stores chat continuation text: secure the Redis network and access controls. Approval state is transient, not an audit trail. The first release operates as a single Agent Server instance, including when Redis is configured: running-job ownership and SSE delivery remain process-local.

## HTTP interface

For the loopback-only example above, a local chat request and Skills inspection look like:

```sh
curl -sS http://127.0.0.1:8181/v1/skills
curl -sS -X POST http://127.0.0.1:8181/v1/chats \
  -H 'Content-Type: application/json' \
  -d '{"llmWorkerId":123,"messages":[{"role":"USER","content":{"text":"Hello"}}]}'
```

Replace `123` with a configured LLM Worker ID. For token modes, add `Authorization: Bearer <chat-or-admin-token>` according to the route. The local no-token example is for a trusted workstation only.

- `POST /v1/chats` and `POST /v1/chats/stream` accept a selected `llmWorkerId`, typed `messages`, and limited model `options` (the stream variant uses SSE).
- A `USER` message may carry bounded base64 image data, for example `{"image":{"contentType":"image/png","source":{"base64":"..."}}}`, instead of text. Image content is passed to the selected LLM Worker and preserved in returned chat history and approval continuations; use a model/provider that supports image input. URL image sources are explicitly rejected in this release: fetching arbitrary client-selected URLs from a Worker would allow access to internal services and unbounded downloads. Other unknown or unsupported content fields are rejected rather than silently dropped.
- SSE sends `started` before execution and, while connected, live `text_delta`, `tool_call`, and `tool_result` events before its final event. Text deltas are delivered while the model generates, with any missing suffix sent at completion; validated final chat history remains authoritative. Progress uses a bounded queue: a slow receiver may miss intermediate tool events; it is not a durable event log. SSE disconnect does not stop orchestration, and replay after reconnect is not supported in this release. A later stream error can follow already delivered text; consumers must treat the terminal event as authoritative.
- Prefer SSE for long-running jobs that may need cancellation: a synchronous `/v1/chats` caller receives its `cancelCapability` only with a completed or recoverable-error response. If that HTTP request is abandoned before any response, its caller cannot use the cancellation API for a job already started.
- `POST /v1/chats/{chat_id}/resume` accepts the issued `resumeCapability` and **exactly one** decision for the pending `callId`. Zero or multiple decisions receive HTTP 400. The server revalidates the stored Worker ID, method, arguments, and schema; a resumed caller cannot provide a replacement target.
- If a valid resume starts a Worker job after a process restart but cleanup fails, its error response includes a **new** `cancelCapability` for retrying `POST /v1/chats/{chat_id}/cancel`. A claimed approval is single-use: do not retry the resume request to repeat the job.
- A restored `/resume` request uses a fresh cancellation capability. Since resume currently returns one synchronous JSON response, the new capability cannot be delivered before that response: the old capability cannot cancel an in-flight restored resume. If the HTTP request is abandoned, the server attempts to cancel its owned jobs and retains uncertain ownership until cleanup succeeds. Prefer short Worker timeouts and a reliable connection for approval continuations.
- If a non-streaming chat fails after starting a job whose result is still uncertain, its error response includes the `chatId` and `cancelCapability` needed to retry cancellation. Other backend errors remain opaque. Responses carrying capabilities use `Cache-Control: no-store`.
- `POST /v1/chats/{chat_id}/cancel` accepts the issued `cancelCapability`, and may cancel only jobs owned by that chat. Closing an SSE connection does not cancel its jobs or replay its events.
- A `completed` chat can still have a Worker whose result stream failed before a terminal result was confirmed. In that case its `cancelCapability` remains usable until `Delete` succeeds; a completed chat response alone does not prove every Worker stopped.
- The admin routes `GET /v1/skills`, `GET /v1/skills/{name}`, `POST /v1/skills/reload`, `GET /v1/tools`, and `PUT`/`DELETE /v1/tools/{name}` expose the local Skills catalog and Agent Server's own tool registry.

Use an LLM Worker configured with manual client-tool support. Externally registered Worker tools must support immediate Direct results; unsupported delayed, periodic, or client-stream-only jobs are rejected. A tool's schema revision is checked against the version offered to the model, again when resuming approval, and again before encoding the Worker job. A schema change **after the final check but before jobworkerp accepts the enqueue** cannot be made atomic without a conditional-revision RPC on jobworkerp; coordinate live Runner schema deployments accordingly. Agent Server holds a newly enqueued job's ID **after the enqueue response arrives** and before waiting for completion so that an explicit cancellation can reach jobworkerp `Delete`. If the enqueue succeeded but its response or job-ID header never arrives, the client cannot know that ID and automatic cancellation is impossible; investigate the jobworkerp job separately. An approval decision is not a guarantee of exactly-once execution if enqueue or the connection fails ambiguously.

`Delete.is_success=false` is **not** proof that a job completed: a result or cancellation may have raced with the request, or cancellation may be unavailable. When Agent Server cannot verify a terminal result or a successful Delete, it keeps the job's local ownership and cancellation capability instead of silently dropping them. Repeated Delete after a lost response may still return `false`; this release has no durable, idempotent per-job terminal-reconciliation protocol. Inspect jobworkerp independently when repeated cancellation cannot be confirmed. A terminated Agent Server process cannot recover process-local job ownership after restart.

The `ai-docs/` design documents are internal and intentionally excluded from version control.
