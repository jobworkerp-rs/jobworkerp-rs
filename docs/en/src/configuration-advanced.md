# Advanced Configuration

Environment variables not listed in the [basic configuration](./configuration.md). See [dot.env](https://github.com/jobworkerp-rs/jobworkerp-rs/blob/main/dot.env) for examples.

## Database Connection Settings

### SQLite

| Environment Variable | Description | Default |
|---------------------|-------------|---------|
| SQLITE_URL | SQLite connection URL | sqlite://jobworkerp.sqlite3?mode=rwc |
| SQLITE_MAX_CONNECTIONS | Maximum connections | 20 |

### MySQL (for Scalable mode)

| Environment Variable | Description | Default |
|---------------------|-------------|---------|
| MYSQL_HOST | MySQL host | 127.0.0.1 |
| MYSQL_PORT | MySQL port | 3306 |
| MYSQL_USER | Username | - |
| MYSQL_PASSWORD | Password | - |
| MYSQL_DBNAME | Database name | - |
| MYSQL_MAX_CONNECTIONS | Maximum connections | 20 |

## Redis Connection Settings (for Scalable mode)

| Environment Variable | Description | Default |
|---------------------|-------------|---------|
| REDIS_URL | Redis connection URL | redis://redis:6379 |
| REDIS_USERNAME | Redis username | - |
| REDIS_PASSWORD | Redis password | - |
| REDIS_POOL_SIZE | Connection pool size | 20 |
| REDIS_POOL_MIN_IDLE | Minimum idle connections | 1 |
| REDIS_POOL_CONNECTION_TIMEOUT_MSEC | Connection timeout (milliseconds) | 5000 |
| REDIS_POOL_IDLE_TIMEOUT_MSEC | Idle timeout (milliseconds) | 60000 |
| REDIS_POOL_MAX_LIFETIME_MSEC | Connection max lifetime (milliseconds) | 300000 |

## Cache Settings

Settings for stretto/moka cache.

| Environment Variable | Description | Default |
|---------------------|-------------|---------|
| MEMORY_CACHE_NUM_COUNTERS | stretto counter count | 12960 |
| MEMORY_CACHE_MAX_COST | stretto max cost | 12960 |
| MEMORY_CACHE_USE_METRICS | Enable metrics | true |
| MEMORY_CACHE_TTL_SEC | moka cache TTL (seconds) | 3600 |

## Job Queue Advanced Settings (Standalone mode)

| Environment Variable | Description | Default |
|---------------------|-------------|---------|
| JOB_QUEUE_CHANNEL_CAPACITY | Max messages per in-memory channel | 10000 |
| JOB_QUEUE_PUBSUB_CHANNEL_CAPACITY | Capacity of result notification pubsub channels | 128 |
| JOB_QUEUE_MAX_CHANNELS | Max channels in stretto cache | 10000 |
| JOB_QUEUE_CANCEL_CHANNEL_CAPACITY | Cancellation broadcast channel capacity | 1000 |

## Log and Tracing Advanced Settings

| Environment Variable | Description | Default |
|---------------------|-------------|---------|
| RUST_LOG | Rust log filter (env_filter format) | info |
| LOG_APP_NAME | Application name | jobworkerp-rs |
| ZIPKIN_ADDR | Zipkin endpoint (currently disabled) | - (disabled when unset) |

## Plugin and MCP Advanced Settings

| Environment Variable | Description | Default |
|---------------------|-------------|---------|
| PLUGIN_INSTANTIATE_TIMEOUT_SECS | Plugin instantiation timeout (seconds) | 5 |
| MCP_TRANSPORT_START_TIMEOUT_SECS | MCP SSE/Stdio transport startup timeout (seconds) | 15 |
| MCP_TOOLS_LOAD_TIMEOUT_SECS | MCP tools loading timeout (seconds) | 10 |
| PLUGIN_SCHEMA_LOAD_TIMEOUT_SECS | Plugin schema loading timeout (seconds) | 5 |

## LLM API Key Settings

Required when using external LLM services with the LLM runner.

| Environment Variable | Description |
|---------------------|-------------|
| OPENAI_API_KEY | OpenAI API key |
| ANTHROPIC_API_KEY | Anthropic API key |
| COHERE_API_KEY | Cohere API key |
| GEMINI_API_KEY | Google Gemini API key |
| GROQ_API_KEY | Groq API key |
| XAI_API_KEY | xAI (Grok) API key |
| DEEPSEEK_API_KEY | DeepSeek API key |

## Other Settings

| Environment Variable | Description | Default |
|---------------------|-------------|---------|
| TZ_OFFSET_HOURS | Timezone offset (hours) | 9 |
| MAX_FRAME_SIZE | gRPC max frame size (bytes) | None (tonic default). dot.env uses 16777215 as example |
| WORKFLOW_HTTP_USER_AGENT | User-Agent for workflow HTTP requests | simple-workflow/1.0 |
