# LLM Runner

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
- Streaming token generation supported (see [Streaming](streaming.md))

## Supported LLM Providers

- **Ollama**: Local LLM server with full tool calling support
- **GenAI**: Multi-provider LLM client (supports OpenAI, Anthropic, Gemini, Cohere, Groq, xAI, DeepSeek, Ollama, and other backends). OpenAI API-compatible servers can also be used by configuring base_url

## Tool Calling

The chat method supports providing tools to the LLM via FunctionSets. The `is_auto_calling` option controls automatic/manual mode:

- `is_auto_calling: true` - Automatically execute tools when LLM returns tool calls
- `is_auto_calling: false` (default) - Return tool calls to client for review/modification before execution

### FunctionSets

Tools are organized by functionality using FunctionSets. Available built-in function sets correspond to runner types:
- COMMAND, HTTP_REQUEST, GRPC_UNARY, DOCKER, etc.
- MCP server tools

For details on FunctionSet definition, management, and AutoSelection (automatic FunctionSet selection by LLM to reduce context usage), see [Function / FunctionSet](function.md).

## Runner Settings

| Field | Description |
|-------|-------------|
| `using` | Method to use: `"completion"` or `"chat"`. Specified via `JobRequest.using` at job execution time (not a Runner Settings field) |
| worker.runner_settings | Model configuration (provider, model name, parameters) |
| job.args | Prompts (completion) or messages (chat) |

## Related Documentation

- [Function / FunctionSet](function.md) - Unified Function abstraction, FunctionSet management, and AutoSelection
- [Streaming](streaming.md) - Streaming execution for LLM token generation
