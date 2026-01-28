# LLM Runner

[Japanese ver.](LLM_ja.md)

The LLM runner is a multi-method runner that provides LLM (Large Language Model) capabilities through jobworkerp-rs. It uses the `using` parameter to specify the execution method.

## Methods

### completion

Text completion (prompt-based).

- Sends a prompt to the LLM and receives a generated text response
- Suitable for simple text generation tasks

### chat

Chat conversation with message history and tool calling support.

- Supports multi-turn conversations with message history
- Tool calling via FunctionSets (see [Tool Calling](#tool-calling) below)
- Streaming token generation supported (see [STREAMING.md](STREAMING.md))

## Supported LLM Providers

- **Ollama**: Local LLM server with full tool calling support
- **OpenAI API-compatible servers**: Any server implementing the OpenAI API interface
- **GenAI**: Multi-provider LLM client (supports Ollama and other backends)

## Tool Calling

The chat method supports providing tools to the LLM via FunctionSets. The `is_auto_calling` option controls automatic/manual mode:

- `is_auto_calling: true` - Automatically execute tools when LLM returns tool calls
- `is_auto_calling: false` (default) - Return tool calls to client for review/modification before execution

### FunctionSets

Tools are organized by functionality using FunctionSets. Available built-in function sets correspond to runner types:
- COMMAND, HTTP_REQUEST, GRPC_UNARY, DOCKER, etc.
- MCP server tools

## Runner Settings

| Field | Description |
|-------|-------------|
| `using` | Method to use: `"completion"` or `"chat"` |
| worker.runner_settings | Model configuration (provider, model name, parameters) |
| job.args | Prompts (completion) or messages (chat) |

## Deprecated Runners

`LLM_COMPLETION` and `LLM_CHAT` are deprecated. Use the `LLM` runner with the `using` parameter instead.

## Related Documentation

- [Streaming](STREAMING.md) - Streaming execution for LLM token generation
