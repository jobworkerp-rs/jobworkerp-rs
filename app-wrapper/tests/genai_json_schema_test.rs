//! JSON Schema (structured output) test for the GenAI integration.
//!
//! These tests are #[ignore]d by default because they need an OpenAI-compatible
//! endpoint (typically Ollama's `/v1/` endpoint) and a model name that the
//! GenAI crate resolves to the OpenAI adapter (e.g. `gpt-*`). Set
//! `OPENAI_API_KEY` to any value (Ollama accepts `ollama` as a dummy key).
//!
//! Run with:
//! ```sh
//! OPENAI_API_KEY=ollama cargo test --package app-wrapper --test genai_json_schema_test \
//!   -- --ignored --nocapture --test-threads=1
//! ```

use anyhow::Result;
use app::module::test::create_hybrid_test_app;
use app_wrapper::llm::chat::genai::GenaiChatService;
use app_wrapper::llm::completion::genai::GenaiCompletionService;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
    ChatMessage, ChatRole, LlmOptions as ChatLlmOptions, MessageContent, message_content,
};
use jobworkerp_runner::jobworkerp::runner::llm::llm_completion_args::LlmOptions as CompletionLlmOptions;
use jobworkerp_runner::jobworkerp::runner::llm::llm_completion_result;
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::GenaiRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::{LlmChatArgs, LlmCompletionArgs};
use std::collections::HashMap;
use tokio::time::{Duration, timeout};

// Use the OpenAI-compatible endpoint of a local Ollama server. The path is
// normalized to `/v1/` by GenaiChatService::new, so the bare base URL is fine.
const GENAI_BASE_URL: &str = "http://ollama.ollama.svc.cluster.local:11434";
// Force the OpenAI adapter via the `openai::` namespace so that genai sends the
// `response_format` payload. Without the namespace, `gpt-oss:*` is explicitly
// excluded from the OpenAI auto-detection in genai 0.5.x and falls back to the
// Ollama adapter, which does not implement structured output. The OpenAI
// adapter strips the namespace before sending the model name on the wire, so
// Ollama still receives `gpt-oss:20b`.
const TEST_MODEL: &str = "openai::gpt-oss:20b";
const TEST_TIMEOUT: Duration = Duration::from_secs(300);

// Ollama's OpenAI-compatible endpoint accepts any non-empty bearer token, but
// genai's OpenAI adapter aborts up-front if `OPENAI_API_KEY` is unset. Provide
// a dummy value so #[ignore]d tests work out of the box.
fn ensure_dummy_openai_key() {
    if std::env::var_os("OPENAI_API_KEY").is_none() {
        // SAFETY: tests run single-threaded (--test-threads=1) and this is the
        // only writer of the env var in this test binary.
        unsafe { std::env::set_var("OPENAI_API_KEY", "ollama") };
    }
}

async fn create_test_chat_service() -> Result<GenaiChatService> {
    ensure_dummy_openai_key();
    let app_module = create_hybrid_test_app().await?;

    let settings = GenaiRunnerSettings {
        model: TEST_MODEL.to_string(),
        base_url: Some(GENAI_BASE_URL.to_string()),
        system_prompt: Some(
            "You are a helpful assistant. Always respond in valid JSON format when a schema is provided."
                .to_string(),
        ),
    };

    GenaiChatService::new(
        app_module.function_app.clone(),
        app_module.function_set_app.clone(),
        settings,
    )
    .await
}

async fn create_test_completion_service() -> Result<GenaiCompletionService> {
    ensure_dummy_openai_key();
    let settings = GenaiRunnerSettings {
        model: TEST_MODEL.to_string(),
        base_url: Some(GENAI_BASE_URL.to_string()),
        system_prompt: Some(
            "You are a helpful assistant. Always respond in valid JSON format when instructed."
                .to_string(),
        ),
    };

    GenaiCompletionService::new(settings).await
}

#[ignore = "need to run with OpenAI-compatible LLM server (e.g. Ollama)"]
#[tokio::test]
async fn test_chat_with_json_schema() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    let service = create_test_chat_service().await?;

    let schema = r#"{
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {
            "answer": {"type": "string"},
            "confidence": {"type": "number", "minimum": 0, "maximum": 1}
        },
        "required": ["answer", "confidence"]
    }"#;

    let args = LlmChatArgs {
        json_schema: Some(schema.to_string()),
        messages: vec![ChatMessage {
            role: ChatRole::User.into(),
            content: Some(MessageContent {
                content: Some(message_content::Content::Text(
                    "What is 2+2? Respond with your answer and confidence level.".to_string(),
                )),
            }),
        }],
        options: Some(ChatLlmOptions {
            max_tokens: Some(512),
            temperature: Some(0.1),
            ..Default::default()
        }),
        ..Default::default()
    };

    let result = timeout(
        TEST_TIMEOUT,
        service.request_chat(args, opentelemetry::Context::current(), HashMap::new()),
    )
    .await??;

    assert!(result.content.is_some());
    if let Some(content) = result.content
        && let Some(jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(text)) = content.content {
            let parsed: serde_json::Value = serde_json::from_str(&text)?;
            assert!(parsed.get("answer").is_some(), "missing 'answer' field");
            assert!(parsed.get("confidence").is_some(), "missing 'confidence' field");

            if let Some(confidence) = parsed.get("confidence").and_then(|v| v.as_f64()) {
                assert!((0.0..=1.0).contains(&confidence));
            }

            println!("GenAI chat JSON Schema test passed. Response: {text}");
        }

    Ok(())
}

#[ignore = "need to run with OpenAI-compatible LLM server (e.g. Ollama)"]
#[tokio::test]
async fn test_completion_with_json_schema() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    let service = create_test_completion_service().await?;

    let schema = r#"{
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {
            "translation": {"type": "string"},
            "source_language": {"type": "string"},
            "target_language": {"type": "string"}
        },
        "required": ["translation", "source_language", "target_language"]
    }"#;

    let args = LlmCompletionArgs {
        json_schema: Some(schema.to_string()),
        prompt: "Translate 'Hello world' to Japanese".to_string(),
        options: Some(CompletionLlmOptions {
            max_tokens: Some(256),
            temperature: Some(0.1),
            ..Default::default()
        }),
        ..Default::default()
    };

    let result = timeout(
        TEST_TIMEOUT,
        service.request_chat(args, opentelemetry::Context::current(), HashMap::new()),
    )
    .await??;

    assert!(result.content.is_some());
    if let Some(content) = result.content
        && let Some(llm_completion_result::message_content::Content::Text(text)) = content.content
    {
        let parsed: serde_json::Value = serde_json::from_str(&text)?;
        assert!(parsed.get("translation").is_some(), "missing 'translation'");
        assert!(
            parsed.get("source_language").is_some(),
            "missing 'source_language'"
        );
        assert!(
            parsed.get("target_language").is_some(),
            "missing 'target_language'"
        );

        if let Some(translation) = parsed.get("translation").and_then(|v| v.as_str()) {
            assert!(!translation.is_empty());
        }

        println!("GenAI completion JSON Schema test passed. Response: {text}");
    }

    Ok(())
}
