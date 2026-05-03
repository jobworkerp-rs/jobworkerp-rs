//! Diagnostic test: verify whether LLM (ollama) tracing actually reaches the OTLP endpoint.
//!
//! Background: production observation shows that LLM spans never appear in Langfuse,
//! while ordinary gRPC spans (Tracing::trace_request) DO appear. This test isolates the
//! LLM path so that we can iterate hypotheses against a real OTLP collector.
//!
//! Requirements to run:
//! - Ollama server reachable at OLLAMA_HOST
//! - OTLP collector reachable at OTLP_ADDR (forwarding to Langfuse)
//! - Run with: `OTLP_ADDR=http://...:4317 cargo test -p app-wrapper --test ollama_otel_tracing_test -- --ignored --nocapture --test-threads=1`
//!
//! After the test runs, manually inspect Langfuse / collector logs to confirm whether
//! the span actually arrived.
//!
//! IMPORTANT: this test uses the real `tracing_init_from_env` (NOT `tracing_init_test`)
//! so that the OTLP exporter is actually initialised. It also calls
//! `shutdown_tracer_provider()` at the end to flush the BatchSpanProcessor queue.
//! Without that flush, the test process exits before the queue is drained and nothing
//! is sent regardless of the code under test.

#![allow(clippy::uninlined_format_args)]

use anyhow::Result;
use app::module::test::create_hybrid_test_app;
use app_wrapper::llm::chat::ollama::OllamaChatService;
use app_wrapper::llm::completion::ollama::OllamaService;
use command_utils::util::tracing::LoggingConfig;
use futures::StreamExt;
use jobworkerp_runner::jobworkerp::runner::llm::LlmChatArgs;
use jobworkerp_runner::jobworkerp::runner::llm::LlmCompletionArgs;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::Content as ArgsContent;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
    ChatMessage, ChatRole, LlmOptions, MessageContent,
};
use jobworkerp_runner::jobworkerp::runner::llm::llm_completion_args::LlmOptions as CompletionLlmOptions;
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::OllamaRunnerSettings;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::time::{Duration, timeout};
use tokio_util::sync::CancellationToken;

const OLLAMA_HOST: &str = "http://ollama.ollama.svc.cluster.local:11434";
const OTLP_ADDR: &str = "http://otel-collector.default.svc.cluster.local:4317";
const TEST_MODEL: &str = "gpt-oss:20b";
const TEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Initialise OTLP-aware tracing.
///
/// Returns Ok(true) when the OTLP exporter has been wired up via the env (i.e. OTLP_ADDR
/// was set), Ok(false) when no OTLP endpoint was configured (still useful for verifying
/// that the test harness itself works without crashing).
async fn init_real_tracing() -> Result<bool> {
    unsafe { std::env::set_var("OTLP_ADDR", OTLP_ADDR) };
    let otlp_set = std::env::var("OTLP_ADDR").is_ok();

    // Use tracing_init with an explicit LoggingConfig instead of tracing_init_from_env:
    // the latter calls envy::from_env::<LoggingConfig>() which fails when LOG_USE_JSON /
    // LOG_USE_STDOUT are unset. We still go through tracing_init -> setup_layer_from_logging_config
    // -> set_otlp_tracer_provider_from_env, so the OTLP exporter is wired up exactly the
    // same way as the production path.
    command_utils::util::tracing::tracing_init(LoggingConfig::new()).await?;

    Ok(otlp_set)
}

fn simple_chat_args(message: &str) -> LlmChatArgs {
    LlmChatArgs {
        messages: vec![ChatMessage {
            role: ChatRole::User as i32,
            content: Some(MessageContent {
                content: Some(ArgsContent::Text(message.to_string())),
            }),
        }],
        options: Some(LlmOptions {
            temperature: Some(0.0),
            max_tokens: Some(200),
            top_p: None,
            repeat_penalty: None,
            repeat_last_n: None,
            seed: Some(42),
            extract_reasoning_content: Some(false),
        }),
        model: Some(TEST_MODEL.to_string()),
        function_options: None, // No tools — the smallest possible chat.
        json_schema: None,
    }
}

async fn build_service() -> Result<OllamaChatService> {
    let app_module = create_hybrid_test_app().await?;
    let settings = OllamaRunnerSettings {
        model: TEST_MODEL.to_string(),
        base_url: Some(OLLAMA_HOST.to_string()),
        system_prompt: Some("You are a helpful assistant.".to_string()),
        pull_model: Some(false),
    };
    OllamaChatService::new(
        app_module.function_app.clone(),
        app_module.function_set_app.clone(),
        settings,
    )
}

/// Send a single short user message ("Hi") and return.
///
/// Inspect Langfuse or the collector logs after this test runs to confirm whether the
/// span actually shows up. The test itself only verifies that the call to ollama
/// succeeded — observability of the trace must be checked out-of-band.
#[tokio::test]
#[ignore = "Requires Ollama server and an OTLP collector. Inspect Langfuse afterwards."]
async fn ollama_simple_greeting_emits_otlp_span() -> Result<()> {
    let otlp_active = init_real_tracing().await?;
    eprintln!(
        "OTLP exporter active: {} (OTLP_ADDR env var was {})",
        otlp_active,
        if otlp_active { "set" } else { "NOT SET" }
    );

    let service = build_service().await?;
    let args = simple_chat_args("Hi");

    // Mirror the production call site: an empty parent context, so any LLM span
    // created downstream becomes a root span (this matches the runner entry path
    // when no upstream gRPC trace context is propagated through `metadata`).
    let cx = opentelemetry::Context::current();
    let metadata: HashMap<String, String> = HashMap::new();

    let result = timeout(TEST_TIMEOUT, service.request_chat(args, cx, metadata)).await??;

    eprintln!(
        "ollama replied (done={}, has_content={})",
        result.done,
        result.content.is_some()
    );

    // Critical: flush the BatchSpanProcessor queue. Without this, the test process
    // exits before the exporter drains and nothing actually leaves the box.
    command_utils::util::tracing::shutdown_tracer_provider();

    // Give the exporter a brief moment to finish in-flight gRPC writes after shutdown
    // returns. The shutdown call is synchronous-on-completion in opentelemetry-sdk
    // 0.31, but a short sleep is cheap insurance for diagnostic runs.
    tokio::time::sleep(Duration::from_millis(500)).await;

    Ok(())
}

/// Streaming counterpart of the greeting test.
///
/// Hypothesis: `OllamaChatService::create_streaming_chat` (chat/ollama.rs:1270) does NOT
/// invoke any of the GenericLLMTracingHelper / `with_chat_response_tracing` machinery —
/// only the non-streaming `request_chat_internal_with_tracing` path does. If this test
/// produces no Langfuse trace while the non-streaming test above does, that confirms the
/// streaming path is missing instrumentation entirely.
#[tokio::test]
#[ignore = "Requires Ollama server and an OTLP collector. Inspect Langfuse afterwards."]
async fn ollama_simple_greeting_streaming_emits_otlp_span() -> Result<()> {
    let otlp_active = init_real_tracing().await?;
    eprintln!(
        "OTLP exporter active: {} (OTLP_ADDR env var was {})",
        otlp_active,
        if otlp_active { "set" } else { "NOT SET" }
    );

    let service = Arc::new(build_service().await?);
    let args = simple_chat_args("Hi");
    let metadata: HashMap<String, String> = HashMap::new();

    // Drive the streaming entry point used by run_stream in production
    // (chat.rs:246-249 -> request_stream_chat_ref -> request_stream_chat -> create_streaming_chat).
    let stream_fut = service.clone().request_stream_chat(args, metadata, None);
    let mut stream = timeout(TEST_TIMEOUT, stream_fut).await??;

    let mut chunk_count: u32 = 0;
    let mut saw_done = false;
    while let Some(chunk) = timeout(TEST_TIMEOUT, stream.next()).await? {
        chunk_count += 1;
        if chunk.done {
            saw_done = true;
        }
    }
    eprintln!(
        "streaming finished (chunks={}, saw_done={})",
        chunk_count, saw_done
    );

    command_utils::util::tracing::shutdown_tracer_provider();
    tokio::time::sleep(Duration::from_millis(500)).await;

    Ok(())
}

async fn build_completion_service() -> Result<OllamaService> {
    let settings = OllamaRunnerSettings {
        model: TEST_MODEL.to_string(),
        base_url: Some(OLLAMA_HOST.to_string()),
        system_prompt: Some("You are a helpful assistant.".to_string()),
        pull_model: Some(false),
    };
    OllamaService::new(settings).await
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

/// Streaming completion test for the ollama completion runner. Verifies that the new
/// streaming tracing path produces a Langfuse generation span just like the chat
/// streaming test does.
#[tokio::test]
#[ignore = "Requires Ollama server and an OTLP collector. Inspect Langfuse afterwards."]
async fn ollama_completion_streaming_emits_otlp_span() -> Result<()> {
    let otlp_active = init_real_tracing().await?;
    eprintln!("OTLP exporter active: {} (completion stream)", otlp_active);

    let service = build_completion_service().await?;
    let args = simple_completion_args("Say hi in one short sentence.");
    let metadata: HashMap<String, String> = HashMap::new();

    let mut stream = timeout(
        TEST_TIMEOUT,
        service.request_stream_generation(args, metadata, None),
    )
    .await??;

    let mut chunks: u32 = 0;
    let mut saw_done = false;
    while let Some(chunk) = timeout(TEST_TIMEOUT, stream.next()).await? {
        chunks += 1;
        if chunk.done {
            saw_done = true;
        }
    }
    eprintln!(
        "completion stream finished (chunks={}, saw_done={})",
        chunks, saw_done
    );

    command_utils::util::tracing::shutdown_tracer_provider();
    tokio::time::sleep(Duration::from_millis(500)).await;
    Ok(())
}

/// Non-streaming cancellable completion test. The previous implementation ignored its
/// `_cx` / `_metadata` arguments; this exercises the newly wired tracing and ensures a
/// generation span shows up.
#[tokio::test]
#[ignore = "Requires Ollama server and an OTLP collector. Inspect Langfuse afterwards."]
async fn ollama_completion_with_cancellation_emits_otlp_span() -> Result<()> {
    let otlp_active = init_real_tracing().await?;
    eprintln!(
        "OTLP exporter active: {} (completion non-stream)",
        otlp_active
    );

    let service = build_completion_service().await?;
    let args = simple_completion_args("Say hi in one short sentence.");
    let metadata: HashMap<String, String> = HashMap::new();
    let token = CancellationToken::new();
    let cx = opentelemetry::Context::current();

    let result = timeout(
        TEST_TIMEOUT,
        service.request_generation_with_cancellation(args, token, cx, metadata),
    )
    .await??;
    eprintln!(
        "completion non-stream finished (done={}, content={})",
        result.done,
        result.content.is_some()
    );

    command_utils::util::tracing::shutdown_tracer_provider();
    tokio::time::sleep(Duration::from_millis(500)).await;
    Ok(())
}
