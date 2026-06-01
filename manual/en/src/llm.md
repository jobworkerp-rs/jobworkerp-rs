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

### GenAI Provider Configuration

GenAI uses the [rust-genai](https://github.com/jeremychone/rust-genai) crate. The provider is automatically detected from the model name (e.g. `gpt-*` → OpenAI, `claude-*` → Anthropic, `gemini-*` → Gemini). Authentication is done via environment variables — set the appropriate API key before starting the worker.

#### Provider Auto-Detection by Model Name Prefix

The provider is determined by the model name prefix:

| Prefix | Provider | Environment Variable | Example |
|--------|----------|---------------------|---------|
| `gpt*` | OpenAI | `OPENAI_API_KEY` | `gpt-4o` |
| `claude*` | Anthropic | `ANTHROPIC_API_KEY` | `claude-sonnet-4-20250514` |
| `gemini*` | Google Gemini | `GEMINI_API_KEY` | `gemini-2.5-flash` |
| `command*` | Cohere | `COHERE_API_KEY` | `command-r-plus` |
| `deepseek*` | DeepSeek | `DEEPSEEK_API_KEY` | `deepseek-chat` |
| `glm*` | ZAI | `ZAI_API_KEY` | `glm-4-plus` |
| (other) | Ollama | *(none — local)* | `llama3.3` |

For providers that cannot be inferred from the prefix, use a namespace prefix `provider::model-name`:

| Namespace Prefix | Provider | Environment Variable | Example |
|-----------------|----------|---------------------|---------|
| `groq::` | Groq | `GROQ_API_KEY` | `groq::llama-3.3-70b-versatile` |
| `xai::` | xAI | `XAI_API_KEY` | `xai::grok-3-mini` |
| `open_router::` | OpenRouter | `OPEN_ROUTER_API_KEY` | `open_router::meta-llama/llama-3.3-70b-instruct` |
| `ollama_cloud::` | Ollama Cloud | `OLLAMA_API_KEY` | `ollama_cloud::llama3.3` |

> For the full and up-to-date mapping rules, see [rust-genai examples/c00-readme.rs](https://github.com/jeremychone/rust-genai/blob/main/examples/c00-readme.rs).

#### Environment Variable Setup

Set the API key for the provider you want to use in `.env` file or as system environment variables:

```bash
# .env
OPENAI_API_KEY="sk-..."
ANTHROPIC_API_KEY="sk-ant-..."
GEMINI_API_KEY="AIza..."
```

#### Using Custom / OpenAI-Compatible Endpoints

Set `base_url` in `GenaiRunnerSettings` to point to a custom endpoint. This is useful for self-hosted OpenAI-compatible servers (e.g. vLLM, LocalAI) or proxy services.

```json
{
  "genai": {
    "model": "gpt-4o",
    "base_url": "https://your-proxy.example.com/v1/"
  }
}
```

If the path is empty or `/`, it is automatically normalized to `/v1/`.

> **Note:** When using an OpenAI-compatible proxy (e.g. LiteLLM) for non-OpenAI models, add the `openai::` prefix to the model name to force the OpenAI adapter (e.g. `openai::claude-sonnet-4-20250514`). Without the prefix, the provider is inferred from the model name and a non-OpenAI protocol will be used, which will not work with the proxy.

## Tool Calling

The chat method supports providing tools to the LLM via FunctionSets. The `is_auto_calling` option controls automatic/manual mode:

- `is_auto_calling: true` - Automatically execute tools when LLM returns tool calls
- `is_auto_calling: false` (default) - Return tool calls to client for review/modification before execution

### FunctionSets

Tools are organized by functionality using FunctionSets. Available built-in function sets correspond to runner types:
- COMMAND, HTTP_REQUEST, GRPC, DOCKER, etc.
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
