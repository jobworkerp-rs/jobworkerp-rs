# Function / FunctionSet

## What is a Function?

Function is an abstraction layer that unifies **Runner** and **Worker** under a single interface. Instead of dealing with Runner settings, Worker creation, and Job enqueueing separately, you can specify a function name and JSON arguments to execute tasks.

There are two ways to reference a Function:

- **By runner name**: Specify a runner name (and optionally settings) to execute directly. A temporary Worker is created internally.
- **By worker name**: Specify an existing Worker name to execute with its pre-configured settings.

For MCP/Plugin runners that expose multiple tools, you can further narrow the scope to a specific tool. For multi-method runners (e.g., LLM), you can select a specific method (e.g., `"chat"`).

## What is a FunctionSet?

FunctionSet groups multiple Functions into a named collection. This is a key concept because it controls **which tools are available** in LLM and MCP contexts:

- **LLM tool calling**: Specify a FunctionSet name to limit which tools the LLM can use, keeping the context focused and efficient
- **MCP server**: Specify a FunctionSet name in MCP server config to control which tools are exposed to external clients
- **AutoSelection**: When many FunctionSets exist, the LLM can automatically select the most relevant set before executing tools (see [AutoSelection](#autoselection-hierarchical-functionset-selection))

A FunctionSet is defined by a name, description, category, and a list of target Functions.

### FunctionSetService

FunctionSetService provides CRUD operations for managing FunctionSets:

| RPC | Description |
|-----|-------------|
| `Create` / `Update` / `Delete` | Manage FunctionSets |
| `Find` / `FindByName` | Look up a FunctionSet by ID or name |
| `FindDetail` / `FindDetailByName` | Look up with expanded metadata (FunctionSpecs) for each target |
| `FindList` | List all FunctionSets |
| `Count` | Get total count of FunctionSets |

## Executing Functions

### FunctionService.Call

The primary way to execute a Function is through the `Call` RPC, which accepts a function name and JSON arguments and returns a stream of results.

The behavior depends on which name you specify:

| Name Type | Behavior |
|-----------|----------|
| `runner_name` | Creates a temporary Worker with the specified runner. You can optionally provide `runner_parameters` (settings JSON, WorkerOptions) to configure execution. |
| `worker_name` | Looks up the existing Worker by name and enqueues a job using its pre-configured settings. |

Other request fields:

| Field | Description |
|-------|-------------|
| `args_json` | Job arguments as JSON string |
| `options` | Optional: timeout_ms, streaming, metadata |
| `uniq_key` | Optional: deduplication key |

### Searching Functions

| RPC | Description |
|-----|-------------|
| `FindList` | List all functions (optionally excluding runners or workers) |
| `FindListBySet` | List functions belonging to a specific FunctionSet |
| `FindByName` | Find a function by runner_name or worker_name |
| `Find` | Find a function by FunctionUsing (FunctionId + optional using) |

### Creating Workers

| RPC | Description |
|-----|-------------|
| `CreateWorker` | Create a Worker from a runner name/ID with settings |

> **Note**: To create a workflow Worker, use `WORKFLOW.create` method via `JobService.Enqueue` with `using="create"`. See [Workflow Runner](workflow.md) for details.

## LLM Function Calling

When using the LLM runner's `chat` method, you can specify a FunctionSet to provide tools to the LLM. Each Function is converted to a tool definition that the LLM can invoke.

### Auto-calling mode

- **`is_auto_calling: true`**: The system automatically executes tool calls returned by the LLM and feeds results back (up to 10 rounds)
- **`is_auto_calling: false`** (default): Tool calls are returned to the client for review before execution

### Tool naming convention

- Single-method runners: runner name as-is (e.g., `"http_request"`)
- Multi-method runners: `"runner_name___method_name"` (triple underscore separator)

For details on LLM configuration and providers, see [LLM Runner](llm.md).

### AutoSelection (Hierarchical FunctionSet Selection)

When many tools are registered, providing all of them to the LLM wastes context and can degrade performance. AutoSelection solves this with a two-phase approach:

1. **Phase 1 - Set Selection**: All FunctionSets are presented to the LLM as pseudo-tools named `select_toolset_{set_name}`. The LLM selects the most appropriate set for the task.
2. **Phase 2 - Tool Execution**: Only the tools from the selected FunctionSet are provided to the LLM for actual execution.

To enable AutoSelection, set `auto_select_function_set: true` in the chat request.

## MCP Server Backend

With `MCP_ENABLED=true`, jobworkerp-rs exposes its Workers and Runners as MCP (Model Context Protocol) tools, allowing external LLM agents and MCP clients to use them.

- Function metadata is converted to MCP tool definitions
- Tool arguments follow the combined schema: `{ "settings": {...}, "arguments": {...} }`
- FunctionSet name can be specified in MCP server config to limit exposed tools

For MCP proxy configuration (using external MCP servers as runners), see [MCP Proxy](runners/mcp-proxy.md).

## Related Documentation

- [LLM Runner](llm.md) - LLM provider configuration and chat methods
- [gRPC-Web API](grpc-web-api.md) - Browser-accessible JSON API for Function calls
- [MCP Proxy](runners/mcp-proxy.md) - Using external MCP servers as runners
- [Streaming](streaming.md) - Streaming execution for Function calls
