# Built-in Runners

The following features are built into the runner definition.
Each feature requires setting necessary values in protobuf format for worker.runner_settings and job.args. The protobuf definitions can be obtained from runner.data.runner_settings_proto. Per-method argument and result schemas can be obtained from runner.data.method_proto_map.

| Runner ID | Name | Description | Settings |
|----------|------|-------------|---------|
| COMMAND | Command execution | Executes shell commands | worker.runner_settings: none, job.args: command name and argument array |
| SLACK_POST_MESSAGE | Slack message posting | Posts messages to Slack channels | worker.runner_settings: Slack token, job.args: channel, message text, etc. |
| PYTHON_COMMAND | Python execution | Executes Python scripts using uv | worker.runner_settings: uv environment settings, job.args: python script, input, etc. |
| HTTP_REQUEST | HTTP requests | HTTP communication using reqwest | worker.runner_settings: base URL, job.args: headers, method, body, path, etc. |
| GRPC | gRPC communication (multi-method) | gRPC unary and server streaming requests | worker.runner_settings: host, port, TLS settings, default method/metadata/timeout/as_json/proto (all optional), etc., job.args: method, request, metadata, timeout, as_json, proto (all optional if set in settings). With proto, JSON/protobuf conversion works without reflection. worker.using: "unary" or "streaming" |
| DOCKER | Docker container execution | Equivalent to docker run | worker.runner_settings: FromImage/Tag, job.args: Image/Cmd, etc. |
| LLM | LLM execution (multi-method) | Uses various LLMs via external servers | See [LLM](../llm.md) for details |
| WORKFLOW | Workflow execution (multi-method) | Executes multiple jobs in defined order | See [Workflow](../workflow.md) for details |
| MCP_SERVER | MCP server tool execution | Executes MCP server tools | See [MCP Proxy](mcp-proxy.md) for details |
| FUNCTION_SET_SELECTOR | FunctionSet listing | Lists available FunctionSets with tool summaries for LLM tool selection | - |

## GRPC runner proto sources

The GRPC runner resolves the schema of the target service and method in one of two ways:

- **Server reflection** (default): when the target server has gRPC reflection enabled, the schema is obtained without specifying any proto.
- **External proto source**: setting `worker.runner_settings.proto` (per connection) or `job.args.proto` (per request) resolves the schema without reflection. The `source` may be an inline string, a `file://` path, or an `http(s)://` URL.

### What a proto source must contain

A proto source does not need the full implementation of a gRPC service, but it must satisfy the following:

- **It must be a syntactically complete proto that `protoc` can compile.** It is compiled internally as a single file, so **a proto that references other files via `import` cannot be resolved**. The proto must be self-contained.
- **It must include the `service { rpc ... }` declaration of the method being called.** The method is resolved from the service definition, so a proto that defines only message types and omits the service/RPC declaration fails to resolve.
- **It must include the definitions of that method's input/output message types (and any message types they depend on).** These are used for JSON ⇔ protobuf conversion. Definitions of other services or unused methods are not required.

### When a proto source is not needed

If requests and responses are exchanged as raw protobuf bytes, neither a proto source nor reflection is required:

- Request: pass `job.args.request` as `body` (pre-serialized protobuf bytes) instead of `json_body` (a JSON string).
- Response: do not set `as_json` (receive raw bytes).

Schema resolution (reflection or a proto source) is required only when sending `json_body` or returning the response as JSON via `as_json`.

### Proto source security constraints

- `http://` (plaintext) is rejected by default and allowed only when `allow_insecure_http` is explicitly enabled.
- `file://` is usable only when the allowed directory is configured via the `GRPC_PROTO_ALLOWED_DIR` environment variable; paths outside the allowed directory are rejected.
- The fetched size is capped.
