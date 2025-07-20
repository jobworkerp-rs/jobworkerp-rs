//! JSON Schema format support test for Ollama integration
//! This test requires an Ollama server running at http://ollama.ollama.svc.cluster.local:11434

#![allow(clippy::uninlined_format_args)]
#![allow(clippy::collapsible_match)]

use anyhow::Result;
use app::module::test::create_hybrid_test_app;
use app_wrapper::llm::chat::ollama::OllamaChatService;
use app_wrapper::llm::completion::ollama::OllamaService;
use futures::StreamExt;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
    message_content, ChatRole, LlmOptions as ChatLlmOptions,
};
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{ChatMessage, MessageContent};
use jobworkerp_runner::jobworkerp::runner::llm::llm_completion_args::LlmOptions as CompletionLlmOptions;
use jobworkerp_runner::jobworkerp::runner::llm::llm_completion_result;
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::OllamaRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::{LlmChatArgs, LlmCompletionArgs};
use std::collections::HashMap;
use tokio::time::{timeout, Duration};
use tokio_util::sync::CancellationToken;

/// Test configuration
const OLLAMA_HOST: &str = "http://ollama.ollama.svc.cluster.local:11434";
const TEST_MODEL: &str = "qwen3:30b"; // Use a model that supports structured output
const TEST_TIMEOUT: Duration = Duration::from_secs(300);

/// Create test chat service
async fn create_test_chat_service() -> Result<OllamaChatService> {
    let app_module = create_hybrid_test_app().await?;

    let settings = OllamaRunnerSettings {
        model: TEST_MODEL.to_string(),
        base_url: Some(OLLAMA_HOST.to_string()),
        system_prompt: Some(
            "You are a helpful assistant. Always respond in valid JSON format when a schema is provided."
                .to_string(),
        ),
        pull_model: Some(false),
    };

    OllamaChatService::new(app_module.function_app.clone(), settings)
}

/// Create test completion service
async fn create_test_completion_service() -> Result<OllamaService> {
    let settings = OllamaRunnerSettings {
        base_url: Some(OLLAMA_HOST.to_string()),
        model: TEST_MODEL.to_string(),
        system_prompt: Some(
            "You are a helpful assistant. Always respond in valid JSON format when instructed."
                .to_string(),
        ),
        pull_model: Some(false),
    };

    OllamaService::new(settings).await
}

#[ignore = "need to run with ollama server"]
#[tokio::test]
async fn test_chat_with_json_schema() -> Result<()> {
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

    // 結果検証
    assert!(result.content.is_some());
    if let Some(content) = result.content {
        if let Some(jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(text)) = content.content {
            // JSON形式かどうか確認
            let parsed: serde_json::Value = serde_json::from_str(&text)?;
            assert!(parsed.get("answer").is_some());
            assert!(parsed.get("confidence").is_some());

            // confidence値の範囲確認
            if let Some(confidence) = parsed.get("confidence").and_then(|v| v.as_f64()) {
                assert!((0.0..=1.0).contains(&confidence));
            }

            println!("Chat JSON Schema test passed. Response: {}", text);
        }
    }

    Ok(())
}

#[ignore = "need to run with ollama server"]
#[tokio::test]
async fn test_completion_with_json_schema() -> Result<()> {
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

    let cancellation_token = CancellationToken::new();
    let result = timeout(
        TEST_TIMEOUT,
        service.request_generation_with_cancellation(
            args,
            cancellation_token,
            opentelemetry::Context::current(),
            HashMap::new(),
        ),
    )
    .await??;

    // 結果検証
    assert!(result.content.is_some());
    if let Some(content) = result.content {
        if let Some(llm_completion_result::message_content::Content::Text(text)) = content.content {
            // JSON形式かどうか確認
            let parsed: serde_json::Value = serde_json::from_str(&text)?;
            assert!(parsed.get("translation").is_some());
            assert!(parsed.get("source_language").is_some());
            assert!(parsed.get("target_language").is_some());

            // 内容の妥当性確認
            if let Some(translation) = parsed.get("translation").and_then(|v| v.as_str()) {
                assert!(!translation.is_empty());
            }

            println!("Completion JSON Schema test passed. Response: {}", text);
        }
    }

    Ok(())
}

#[ignore = "need to run with ollama server"]
#[tokio::test]
async fn test_invalid_json_schema_handling() -> Result<()> {
    let service = create_test_chat_service().await?;

    let invalid_schema = r#"{ invalid json schema }"#;

    let args = LlmChatArgs {
        json_schema: Some(invalid_schema.to_string()),
        messages: vec![ChatMessage {
            role: ChatRole::User.into(),
            content: Some(MessageContent {
                content: Some(message_content::Content::Text("Test message".to_string())),
            }),
        }],
        ..Default::default()
    };

    // 無効なスキーマでもエラーにならず正常に動作することを確認
    let result = timeout(
        TEST_TIMEOUT,
        service.request_chat(args, opentelemetry::Context::current(), HashMap::new()),
    )
    .await??;

    assert!(result.content.is_some());
    println!("Invalid JSON schema gracefully handled");
    Ok(())
}

#[ignore = "need to run with ollama server"]
#[tokio::test]
async fn test_completion_stream_with_json_schema() -> Result<()> {
    let service = create_test_completion_service().await?;

    let schema = r#"{
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {
            "count": {"type": "integer"},
            "items": {"type": "array", "items": {"type": "string"}}
        },
        "required": ["count", "items"]
    }"#;

    let args = LlmCompletionArgs {
        json_schema: Some(schema.to_string()),
        prompt: "List 3 programming languages and count them".to_string(),
        options: Some(CompletionLlmOptions {
            max_tokens: Some(256),
            temperature: Some(0.1),
            ..Default::default()
        }),
        ..Default::default()
    };

    // Request the streaming response
    let stream_result = timeout(TEST_TIMEOUT, service.request_stream_generation(args)).await??;

    // Collect all chunks to verify the streaming functionality
    let responses = stream_result.collect::<Vec<_>>().await;

    // Make sure we got some responses
    assert!(!responses.is_empty(), "No streaming responses received");

    // Combine all text chunks
    let combined_text = responses
        .iter()
        .filter_map(|res| {
            res.content.as_ref().and_then(|c| {
                c.content.as_ref().map(|content| match content {
                    llm_completion_result::message_content::Content::Text(text) => text.clone(),
                })
            })
        })
        .collect::<Vec<_>>()
        .join("");

    // Verify the combined response is valid JSON matching the schema
    if !combined_text.trim().is_empty() {
        let parsed: serde_json::Value = serde_json::from_str(&combined_text)?;
        assert!(parsed.get("count").is_some());
        assert!(parsed.get("items").is_some());

        println!(
            "Streaming JSON Schema test passed. Response: {}",
            combined_text
        );
    }

    Ok(())
}
