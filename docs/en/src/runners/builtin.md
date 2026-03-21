# Built-in Runners

The following features are built into the runner definition.
Each feature requires setting necessary values in protobuf format for worker.runner_settings and job.args. The protobuf definitions can be obtained from runner.data.runner_settings_proto. Per-method argument and result schemas can be obtained from runner.data.method_proto_map.

| Runner ID | Name | Description | Settings |
|----------|------|-------------|---------|
| COMMAND | Command execution | Executes shell commands | worker.runner_settings: none, job.args: command name and argument array |
| SLACK_POST_MESSAGE | Slack message posting | Posts messages to Slack channels | worker.runner_settings: Slack token, job.args: channel, message text, etc. |
| PYTHON_COMMAND | Python execution | Executes Python scripts using uv | worker.runner_settings: uv environment settings, job.args: python script, input, etc. |
| HTTP_REQUEST | HTTP requests | HTTP communication using reqwest | worker.runner_settings: base URL, job.args: headers, method, body, path, etc. |
| GRPC | gRPC communication (multi-method) | gRPC unary and server streaming requests | worker.runner_settings: host, port, TLS settings, default method/metadata/timeout/as_json (all optional), etc., job.args: method, request, metadata, timeout, as_json (all optional if set in settings). worker.using: "unary" or "streaming" |
| DOCKER | Docker container execution | Equivalent to docker run | worker.runner_settings: FromImage/Tag, job.args: Image/Cmd, etc. |
| LLM | LLM execution (multi-method) | Uses various LLMs via external servers | See [LLM](../llm.md) for details |
| WORKFLOW | Workflow execution (multi-method) | Executes multiple jobs in defined order | See [Workflow](../workflow.md) for details |
| MCP_SERVER | MCP server tool execution | Executes MCP server tools | See [MCP Proxy](mcp-proxy.md) for details |
| FUNCTION_SET_SELECTOR | FunctionSet listing | Lists available FunctionSets with tool summaries for LLM tool selection | - |
