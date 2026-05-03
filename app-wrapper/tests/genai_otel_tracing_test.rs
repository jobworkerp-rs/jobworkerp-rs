//! Diagnostic test for genai LLM tracing reaching the OTLP endpoint.
//!
//! Mirror of `ollama_otel_tracing_test.rs` but exercising the genai-backed services
//! (chat + completion, both streaming and non-streaming). Each test must be inspected
//! manually in Langfuse afterwards — assertions only confirm the call returned a
//! response, not that the trace landed.
//!
//! Run with (single-threaded so OTLP_ADDR / OPENAI_API_KEY env mutations are safe):
//! ```sh
//! cargo test -p app-wrapper --test genai_otel_tracing_test \
//!   -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Notes:
//! - Uses Ollama's OpenAI-compatible `/v1/` endpoint via the genai OpenAI adapter so
//!   no real OpenAI key is needed (a dummy value is injected). Aligns with
//!   `genai_json_schema_test.rs`.
//! - As with the ollama variant, `tracing_init` (NOT `tracing_init_test`) is used so
//!   the OTLP exporter is actually wired up, and `shutdown_tracer_provider` is called
//!   at the end to flush BatchSpanProcessor before the process exits.

#![allow(clippy::uninlined_format_args)]

use anyhow::Result;
use app::module::test::create_hybrid_test_app;
use app_wrapper::llm::chat::genai::GenaiChatService;
use app_wrapper::llm::completion::genai::GenaiCompletionService;
use command_utils::util::tracing::LoggingConfig;
use futures::StreamExt;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
    ChatMessage, ChatRole, LlmOptions as ChatLlmOptions, MessageContent, message_content,
};
use jobworkerp_runner::jobworkerp::runner::llm::llm_completion_args::LlmOptions as CompletionLlmOptions;
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::GenaiRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::{LlmChatArgs, LlmCompletionArgs};
use std::collections::HashMap;
use tokio::time::{Duration, timeout};

// Default to Ollama's OpenAI-compatible endpoint; the path is normalised to `/v1/`
// inside GenaiChatService::new so the bare host URL is enough. Same pattern as the
// existing genai_json_schema_test.rs — keeps the test runnable in a local k8s setup.
const GENAI_BASE_URL: &str = "http://ollama.ollama.svc.cluster.local:11434";
// Force the OpenAI adapter (genai 0.5.x excludes `gpt-oss:*` from auto-detection,
// without the namespace it falls back to the Ollama adapter which does NOT exercise
// the genai code path we want to trace).
const TEST_MODEL: &str = "openai::gpt-oss:20b";
const OTLP_ADDR: &str = "http://otel-collector.default.svc.cluster.local:4317";
const TEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Wire up the OTLP-aware tracing pipeline (NOT `tracing_init_test`) so spans
/// actually leave the process. Returns whether OTLP_ADDR was set.
async fn init_real_tracing() -> Result<bool> {
    // SAFETY: tests run single-threaded; this is the only writer here.
    unsafe { std::env::set_var("OTLP_ADDR", OTLP_ADDR) };
    let otlp_set = std::env::var("OTLP_ADDR").is_ok();
    command_utils::util::tracing::tracing_init(LoggingConfig::new()).await?;
    Ok(otlp_set)
}

/// genai's OpenAI adapter aborts up-front if `OPENAI_API_KEY` is unset; Ollama's
/// OpenAI-compatible endpoint accepts any non-empty bearer.
fn ensure_dummy_openai_key() {
    if std::env::var_os("OPENAI_API_KEY").is_none() {
        // SAFETY: tests run single-threaded (--test-threads=1).
        unsafe { std::env::set_var("OPENAI_API_KEY", "ollama") };
    }
}

async fn build_chat_service() -> Result<GenaiChatService> {
    ensure_dummy_openai_key();
    let app_module = create_hybrid_test_app().await?;
    let settings = GenaiRunnerSettings {
        model: TEST_MODEL.to_string(),
        base_url: Some(GENAI_BASE_URL.to_string()),
        system_prompt: Some("You are a helpful assistant.".to_string()),
    };
    GenaiChatService::new(
        app_module.function_app.clone(),
        app_module.function_set_app.clone(),
        settings,
    )
    .await
}

async fn build_completion_service() -> Result<GenaiCompletionService> {
    ensure_dummy_openai_key();
    let settings = GenaiRunnerSettings {
        model: TEST_MODEL.to_string(),
        base_url: Some(GENAI_BASE_URL.to_string()),
        system_prompt: Some("You are a helpful assistant.".to_string()),
    };
    GenaiCompletionService::new(settings).await
}

fn simple_chat_args(message: &str) -> LlmChatArgs {
    LlmChatArgs {
        messages: vec![ChatMessage {
            role: ChatRole::User as i32,
            content: Some(MessageContent {
                content: Some(message_content::Content::Text(message.to_string())),
            }),
        }],
        options: Some(ChatLlmOptions {
            temperature: Some(0.0),
            max_tokens: Some(200),
            top_p: None,
            repeat_penalty: None,
            repeat_last_n: None,
            seed: Some(42),
            extract_reasoning_content: Some(false),
        }),
        model: Some(TEST_MODEL.to_string()),
        function_options: None,
        json_schema: None,
    }
}

fn simple_completion_args(prompt: &str) -> LlmCompletionArgs {
    LlmCompletionArgs {
        prompt: prompt.to_string(),
        options: Some(CompletionLlmOptions {
            temperature: Some(0.0),
            max_tokens: Some(200),
            top_p: None,
            repeat_penalty: None,
            repeat_last_n: None,
            seed: Some(42),
            extract_reasoning_content: Some(false),
        }),
        model: Some(TEST_MODEL.to_string()),
        context: None,
        json_schema: None,
        ..Default::default()
    }
}

/// Non-streaming chat baseline. Matches the ollama variant — the existing
/// `request_chat` path was already known to emit a span; this guards against a
/// regression introduced while wiring the streaming paths.
#[tokio::test]
#[ignore = "Requires an OpenAI-compatible endpoint and OTLP collector. Inspect Langfuse afterwards."]
async fn genai_simple_greeting_emits_otlp_span() -> Result<()> {
    let otlp_active = init_real_tracing().await?;
    eprintln!("OTLP exporter active: {} (genai chat)", otlp_active);

    let service = build_chat_service().await?;
    let args = simple_chat_args("Hi");
    let cx = opentelemetry::Context::current();
    let metadata: HashMap<String, String> = HashMap::new();

    let result = timeout(TEST_TIMEOUT, service.request_chat(args, cx, metadata)).await??;
    eprintln!(
        "genai chat replied (done={}, has_content={})",
        result.done,
        result.content.is_some()
    );

    command_utils::util::tracing::shutdown_tracer_provider();
    tokio::time::sleep(Duration::from_millis(500)).await;
    Ok(())
}

/// Streaming chat. This is the case that did NOT emit traces before the streaming
/// instrumentation was added. The expected outcome is a generation span in Langfuse
/// with `service.name=command-utils` and `name=genai.chat.completions`.
#[tokio::test]
#[ignore = "Requires an OpenAI-compatible endpoint and OTLP collector. Inspect Langfuse afterwards."]
async fn genai_simple_greeting_streaming_emits_otlp_span() -> Result<()> {
    let otlp_active = init_real_tracing().await?;
    eprintln!("OTLP exporter active: {} (genai chat stream)", otlp_active);

    let service = build_chat_service().await?;
    let args = simple_chat_args("Hi");
    let metadata: HashMap<String, String> = HashMap::new();

    let mut stream = timeout(
        TEST_TIMEOUT,
        service.request_chat_stream(args, metadata, None),
    )
    .await??;

    let mut chunks: u32 = 0;
    while let Some(_chunk) = timeout(TEST_TIMEOUT, stream.next()).await? {
        chunks += 1;
    }
    eprintln!("genai chat stream finished (chunks={})", chunks);

    command_utils::util::tracing::shutdown_tracer_provider();
    tokio::time::sleep(Duration::from_millis(500)).await;
    Ok(())
}

/// Streaming completion via the genai service. Verifies the new streaming tracing
/// path on the completion side. Inspect Langfuse for a `genai.completion.completions`
/// generation span.
#[tokio::test]
#[ignore = "Requires an OpenAI-compatible endpoint and OTLP collector. Inspect Langfuse afterwards."]
async fn genai_completion_streaming_emits_otlp_span() -> Result<()> {
    let otlp_active = init_real_tracing().await?;
    eprintln!(
        "OTLP exporter active: {} (genai completion stream)",
        otlp_active
    );

    let service = build_completion_service().await?;
    let args = simple_completion_args("Say hi in one short sentence.");
    let metadata: HashMap<String, String> = HashMap::new();

    let mut stream = timeout(
        TEST_TIMEOUT,
        service.request_chat_stream(args, metadata, None),
    )
    .await??;

    let mut chunks: u32 = 0;
    while let Some(_item) = timeout(TEST_TIMEOUT, stream.next()).await? {
        chunks += 1;
    }
    eprintln!("genai completion stream finished (chunks={})", chunks);

    command_utils::util::tracing::shutdown_tracer_provider();
    tokio::time::sleep(Duration::from_millis(500)).await;
    Ok(())
}
