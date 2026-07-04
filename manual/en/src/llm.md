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

### embedding

Text embedding generation (vector representation of text).

- Converts one or more text inputs into embedding vectors for semantic search, RAG, clustering, etc.
- Long inputs are automatically split into chunks (see [Text Chunking](#text-chunking-embedding)); each chunk produces its own vector tagged with the source input index and character range
- Batch input supported (multiple texts in one job)
- Streaming is **not** supported for this method

See [Embedding Usage](#embedding-usage) below for details.

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

## Embedding Usage

The `embedding` method generates embedding vectors. Set `using` to `"embedding"` at job execution time. The runner settings (provider / model) are shared with `completion`/`chat`; use an embedding-capable model.

### Embedding-Capable Models

| Provider | Example model | API key |
|----------|---------------|---------|
| Ollama | `nomic-embed-text`, `mxbai-embed-large` | *(none — local)* |
| OpenAI (GenAI) | `text-embedding-3-small`, `text-embedding-3-large` | `OPENAI_API_KEY` |
| Cohere (GenAI) | `embed-english-v3.0`, `embed-multilingual-v3.0` | `COHERE_API_KEY` |
| Gemini (GenAI) | `gemini-embedding-001` | `GEMINI_API_KEY` |

Provider auto-detection and environment-variable setup are the same as for completion/chat (see [GenAI Provider Configuration](#genai-provider-configuration)).

> **Note:** The provider is inferred from the model-name prefix, so use a name the target adapter recognizes. For Gemini embeddings use the `gemini-*` prefix (e.g. `gemini-embedding-001`). A name starting with `text-embedding-` (e.g. Gemini's `text-embedding-004`) is routed to the **OpenAI** adapter — it will use `OPENAI_API_KEY` and fail against Gemini. When needed, force an adapter with a namespace prefix (e.g. `gemini::<model>`).

### Job Arguments (`job.args`)

| Field | Description |
|-------|-------------|
| `inputs` | Batch of inputs (at least one). Each input carries `text`. Image/media variants are reserved for future multimodal support and currently return an unsupported error. |
| `model` | Optional per-job override of the settings model. |
| `options` | Optional embedding options (see below). |
| `chunking` | Optional per-job chunking override. When unset, uses the settings-level `embedding_chunking`. |

#### `options`

| Option | Description | Providers |
|--------|-------------|-----------|
| `dimensions` | Desired output dimensionality (if the model supports it). | GenAI (OpenAI/Cohere/Gemini), Ollama |
| `truncate` | How to handle over-length input: `"NONE"` / `"START"` / `"END"`. GenAI (Cohere) forwards it as-is; Ollama maps it to a boolean (`NONE`→false, `START`/`END`→true; Ollama has no START/END distinction). Unset → provider default. | GenAI (Cohere), Ollama |
| `embedding_type` | Purpose/type: `"search_document"`/`"search_query"` (Cohere) or `"RETRIEVAL_DOCUMENT"`/`"RETRIEVAL_QUERY"` (Gemini). Ignored by Ollama. | GenAI (Cohere/Gemini) |
| `encoding_format` | Output vector encoding. Only float-family values (`"float"`/`"float32"`) are accepted — vectors are always decoded to `f32`, so non-float encodings (base64/binary/int8/…) are rejected with a warning and the provider default (float) is used. Ignored by Ollama. | GenAI (OpenAI/Cohere) |
| `user` | End-user identifier for abuse detection / rate management (OpenAI `user`). Ignored by Cohere/Gemini/Ollama. | GenAI (OpenAI) |

### Result (`LLMEmbeddingResult`)

- `embeddings`: one entry per text chunk (or per non-text input), ordered by input index then chunk order. Each entry has:
  - `vector`: the embedding (`repeated float`)
  - `input_index`: 0-based index into the request `inputs`
  - `begin_position` / `end_position`: half-open character range `[begin, end)` (Unicode scalar indices, not byte offsets) of the covered substring within the input text
  - `content`: the exact substring covered by this chunk
  - `dimensions`: length of `vector`
- `usage` (optional): `model`, `prompt_tokens`, `total_tokens`. Ollama reports no token counts (only the model name); GenAI fills token fields when the provider returns usage. Token counts are summed across retry batches.

### Text Chunking (embedding)

Long inputs are split so each chunk stays within the model's context budget. Chunking is configured via `embedding_chunking` in the runner settings (default for all jobs) or `chunking` in `job.args` (per-job override).

| Field | Description | Default |
|-------|-------------|---------|
| `max_chunk_tokens` | Upper bound on tokens per chunk (must be > 0). | 512 |
| `min_chunk_tokens` | Lower bound used when merging small adjacent chunks (kept below `max_chunk_tokens`). | 0 |
| `token_estimation` | How token length is estimated: `CHARACTER_ESTIMATION` (1, ~4 chars/token, no tokenizer), `TIKTOKEN` (2, OpenAI BPE), `HF_TOKENIZER` (3, HuggingFace). Unspecified → character estimation. | character estimation |
| `tiktoken_encoding` | tiktoken encoding when `token_estimation = TIKTOKEN`: `"cl100k_base"` (text-embedding-3 / GPT-3.5/4) or `"o200k_base"` (GPT-4o). | `cl100k_base` |
| `tokenizer_hf_repo` | HuggingFace repo id whose `tokenizer.json` is used when `token_estimation = HF_TOKENIZER` (e.g. `"nomic-ai/nomic-embed-text-v1.5"`). Downloaded/cached via hf-hub. Ollama model names are **not** auto-mapped to HF repos — specify the repo explicitly. | — |
| `tokenizer_file_path` | Absolute path to a local `tokenizer.json` for `HF_TOKENIZER` (offline use). Takes precedence over `tokenizer_hf_repo`. | — |

Notes:
- **TIKTOKEN** gives exact token counts for OpenAI/GenAI models; char boundaries fall back to string search (character-estimation precision).
- **HF_TOKENIZER** is intended for Ollama models where the matching HuggingFace tokenizer is known; it produces exact token counts and char spans. Private repos need `HF_TOKEN` in the environment (public repos need no key).
- If a batch exceeds the model's context length at request time, the runner automatically shrinks `max_chunk_tokens` and re-chunks, then falls back to single-item requests.

### Example (gRPC / jobworkerp-client)

Worker settings (Ollama, embedding model):

```json
{
  "ollama": {
    "base_url": "http://localhost:11434",
    "model": "nomic-embed-text"
  },
  "embedding_chunking": {
    "max_chunk_tokens": 512,
    "token_estimation": "HF_TOKENIZER",
    "tokenizer_hf_repo": "nomic-ai/nomic-embed-text-v1.5"
  }
}
```

Job args (`using = "embedding"`):

```json
{
  "inputs": [
    { "text": "The quick brown fox jumps over the lazy dog." },
    { "text": "jobworkerp-rs is a scalable job worker system." }
  ],
  "options": { "dimensions": 768 }
}
```

OpenAI example (worker settings + args):

```json
{ "genai": { "model": "text-embedding-3-small" } }
```

```json
{
  "inputs": [{ "text": "Semantic search query." }],
  "options": { "dimensions": 256, "encoding_format": "float", "user": "tenant-42" },
  "chunking": { "max_chunk_tokens": 256, "token_estimation": "TIKTOKEN", "tiktoken_encoding": "cl100k_base" }
}
```

## Runner Settings

| Field | Description |
|-------|-------------|
| `using` | Method to use: `"completion"`, `"chat"`, or `"embedding"`. Specified via `JobRequest.using` at job execution time (not a Runner Settings field) |
| worker.runner_settings | Model configuration (provider, model name, parameters). For embedding, also `embedding_chunking` |
| job.args | Prompts (completion), messages (chat), or inputs (embedding) |

## Related Documentation

- [Function / FunctionSet](function.md) - Unified Function abstraction, FunctionSet management, and AutoSelection
- [Streaming](streaming.md) - Streaming execution for LLM token generation
