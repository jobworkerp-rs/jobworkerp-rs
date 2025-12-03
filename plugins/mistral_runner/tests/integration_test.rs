//! Integration tests for MistralRS plugin
//!
//! These tests load an actual LLM model and execute inference.
//! Corresponds to app-wrapper/tests/mistral_json_schema_test.rs functionality.
//!
//! # Environment Variables
//! - `MISTRAL_TEST_MODEL_PATH`: Path/ID for model directory (default: "Qwen/Qwen3-14B-GGUF")
//! - `MISTRAL_TEST_GGUF_FILE`: GGUF model file name (default: "Qwen3-14B-Q4_K_M.gguf")
//! - `MISTRAL_TEST_TOK_MODEL`: Tokenizer model ID (optional, default: "Qwen/Qwen3-14B")
//! - `MISTRAL_TEST_TIMEOUT`: Test timeout in seconds (default: 300)
//!
//! # Example Usage
//! ```bash
//! # Run with default Qwen model (requires GPU or sufficient RAM)
//! cargo test --package plugin_runner_mistral --test integration_test -- --ignored --nocapture
//!
//! # Run with custom model
//! MISTRAL_TEST_MODEL_PATH="/path/to/models" \
//! MISTRAL_TEST_GGUF_FILE="model.gguf" \
//! cargo test --package plugin_runner_mistral --test integration_test -- --ignored --nocapture
//! ```

#![allow(clippy::uninlined_format_args)]

use anyhow::Result;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
    message_content, ChatMessage, ChatRole, LlmOptions as ChatLlmOptions, MessageContent,
};
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result;
use jobworkerp_runner::jobworkerp::runner::llm::llm_completion_args::LlmOptions as CompletionLlmOptions;
use jobworkerp_runner::jobworkerp::runner::llm::llm_completion_result;
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::{
    local_runner_settings::{GgufModelSettings, ModelSettings},
    LocalRunnerSettings,
};
use jobworkerp_runner::jobworkerp::runner::llm::{
    LlmChatArgs, LlmChatResult, LlmCompletionArgs, LlmCompletionResult,
};
use jobworkerp_runner::runner::plugins::MultiMethodPluginRunner;
use plugin_runner_mistral::plugin::MistralPlugin;
use prost::Message;
use std::collections::HashMap;
use std::time::{Duration, Instant};

const DEFAULT_TIMEOUT_SECS: u64 = 300;
const DEFAULT_MODEL_PATH: &str = "Qwen/Qwen3-14B-GGUF";
const DEFAULT_GGUF_FILE: &str = "Qwen3-14B-Q4_K_M.gguf";
const DEFAULT_TOK_MODEL: &str = "Qwen/Qwen3-14B";

fn get_test_timeout() -> Duration {
    let secs = std::env::var("MISTRAL_TEST_TIMEOUT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

fn create_model_settings() -> LocalRunnerSettings {
    let model_path =
        std::env::var("MISTRAL_TEST_MODEL_PATH").unwrap_or_else(|_| DEFAULT_MODEL_PATH.to_string());
    let gguf_file =
        std::env::var("MISTRAL_TEST_GGUF_FILE").unwrap_or_else(|_| DEFAULT_GGUF_FILE.to_string());
    let tok_model_id = std::env::var("MISTRAL_TEST_TOK_MODEL")
        .ok()
        .or_else(|| Some(DEFAULT_TOK_MODEL.to_string()));

    LocalRunnerSettings {
        model_settings: Some(ModelSettings::GgufModel(GgufModelSettings {
            model_name_or_path: model_path,
            gguf_files: vec![gguf_file],
            tok_model_id,
            with_logging: true,
            chat_template: None,
            with_paged_attn: true,
        })),
        auto_device_map: None,
    }
}

fn create_plugin_with_model() -> Result<MistralPlugin> {
    let mut plugin = MistralPlugin::new();
    let settings = create_model_settings();
    let settings_bytes = settings.encode_to_vec();

    plugin.load(settings_bytes)?;
    Ok(plugin)
}

fn create_text_message(role: ChatRole, text: &str) -> ChatMessage {
    ChatMessage {
        role: role as i32,
        content: Some(MessageContent {
            content: Some(message_content::Content::Text(text.to_string())),
        }),
    }
}

fn extract_chat_text(result: &LlmChatResult) -> Option<String> {
    result.content.as_ref().and_then(|c| {
        c.content.as_ref().and_then(|content| match content {
            llm_chat_result::message_content::Content::Text(text) => Some(text.clone()),
            _ => None,
        })
    })
}

fn extract_completion_text(result: &LlmCompletionResult) -> Option<String> {
    result.content.as_ref().and_then(|c| {
        c.content.as_ref().map(|content| match content {
            llm_completion_result::message_content::Content::Text(text) => text.clone(),
        })
    })
}

// ============================================================================
// Chat Tests
// ============================================================================

/// Test simple chat without JSON schema
/// Corresponds to: test_simple_chat_without_schema in mistral_json_schema_test.rs
#[ignore = "requires LLM model - run with --ignored"]
#[test]
fn test_simple_chat_without_schema() -> Result<()> {
    let mut plugin = create_plugin_with_model()?;

    let args = LlmChatArgs {
        model: None,
        messages: vec![create_text_message(
            ChatRole::User,
            "What is 2+2? Answer with just the number.",
        )],
        options: Some(ChatLlmOptions {
            max_tokens: Some(50),
            temperature: Some(0.1),
            ..Default::default()
        }),
        function_options: None,
        json_schema: None,
    };

    let start = Instant::now();
    let (result, _metadata) = plugin.run(args.encode_to_vec(), HashMap::new(), Some("chat"));
    let elapsed = start.elapsed();

    let result = result?;
    let chat_result = LlmChatResult::decode(result.as_slice())?;

    println!("Response time: {:?}", elapsed);
    println!("Done: {}", chat_result.done);

    let text = extract_chat_text(&chat_result);
    println!("Response text: {:?}", text);

    assert!(
        chat_result.content.is_some(),
        "Response should have content"
    );
    assert!(text.is_some(), "Response should have text content");
    assert!(
        elapsed < get_test_timeout(),
        "Request should complete within timeout"
    );

    println!("test_simple_chat_without_schema PASSED");
    Ok(())
}

/// Test chat with JSON schema for structured output
/// Corresponds to: test_chat_with_json_schema in mistral_json_schema_test.rs
#[ignore = "requires LLM model - run with --ignored"]
#[test]
fn test_chat_with_json_schema() -> Result<()> {
    let mut plugin = create_plugin_with_model()?;

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
        model: None,
        messages: vec![create_text_message(
            ChatRole::User,
            "What is 2+2? Respond with your answer and confidence level.",
        )],
        options: Some(ChatLlmOptions {
            max_tokens: Some(256),
            temperature: Some(0.2),
            extract_reasoning_content: Some(true),
            ..Default::default()
        }),
        function_options: None,
        json_schema: Some(schema.to_string()),
    };

    let start = Instant::now();
    let (result, _metadata) = plugin.run(args.encode_to_vec(), HashMap::new(), Some("chat"));
    let elapsed = start.elapsed();

    let result = result?;
    let chat_result = LlmChatResult::decode(result.as_slice())?;

    println!("Response time: {:?}", elapsed);

    let text = extract_chat_text(&chat_result);
    println!("Response text: {:?}", text);

    if let Some(text) = text {
        // Try to parse as JSON
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(parsed) => {
                assert!(parsed.get("answer").is_some(), "Should have 'answer' field");
                assert!(
                    parsed.get("confidence").is_some(),
                    "Should have 'confidence' field"
                );

                if let Some(confidence) = parsed.get("confidence").and_then(|v| v.as_f64()) {
                    assert!(
                        (0.0..=1.0).contains(&confidence),
                        "Confidence should be between 0 and 1"
                    );
                }

                println!("Parsed JSON: {}", serde_json::to_string_pretty(&parsed)?);
            }
            Err(e) => {
                println!("Warning: Response is not valid JSON: {}", e);
                println!("Raw response: {}", text);
            }
        }
    }

    assert!(
        chat_result.content.is_some(),
        "Response should have content"
    );
    assert!(
        elapsed < get_test_timeout(),
        "Request should complete within timeout"
    );

    println!("test_chat_with_json_schema PASSED");
    Ok(())
}

// ============================================================================
// Completion Tests
// ============================================================================

/// Test simple completion without JSON schema
#[ignore = "requires LLM model - run with --ignored"]
#[test]
fn test_completion_without_schema() -> Result<()> {
    let mut plugin = create_plugin_with_model()?;

    let args = LlmCompletionArgs {
        model: None,
        system_prompt: None,
        prompt: "Once upon a time in a land far away".to_string(),
        context: None,
        options: Some(CompletionLlmOptions {
            max_tokens: Some(100),
            temperature: Some(0.7),
            ..Default::default()
        }),
        function_options: None,
        json_schema: None,
    };

    let start = Instant::now();
    let (result, _metadata) = plugin.run(args.encode_to_vec(), HashMap::new(), Some("completion"));
    let elapsed = start.elapsed();

    let result = result?;
    let completion_result = LlmCompletionResult::decode(result.as_slice())?;

    println!("Response time: {:?}", elapsed);
    println!("Done: {}", completion_result.done);

    let text = extract_completion_text(&completion_result);
    println!("Response text: {:?}", text);

    assert!(
        completion_result.content.is_some(),
        "Response should have content"
    );
    assert!(text.is_some(), "Response should have text content");
    assert!(
        elapsed < get_test_timeout(),
        "Request should complete within timeout"
    );

    println!("test_completion_without_schema PASSED");
    Ok(())
}

/// Test completion with JSON schema
/// Corresponds to: test_completion_with_json_schema in mistral_json_schema_test.rs
#[ignore = "requires LLM model - run with --ignored"]
#[test]
fn test_completion_with_json_schema() -> Result<()> {
    let mut plugin = create_plugin_with_model()?;

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
        model: None,
        system_prompt: None,
        prompt: "Translate 'Hello world' to Japanese".to_string(),
        context: None,
        options: Some(CompletionLlmOptions {
            max_tokens: Some(256),
            temperature: Some(0.3),
            ..Default::default()
        }),
        function_options: None,
        json_schema: Some(schema.to_string()),
    };

    let start = Instant::now();
    let (result, _metadata) = plugin.run(args.encode_to_vec(), HashMap::new(), Some("completion"));
    let elapsed = start.elapsed();

    let result = result?;
    let completion_result = LlmCompletionResult::decode(result.as_slice())?;

    println!("Response time: {:?}", elapsed);

    let text = extract_completion_text(&completion_result);
    println!("Response text: {:?}", text);

    if let Some(text) = text {
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(parsed) => {
                assert!(
                    parsed.get("translation").is_some(),
                    "Should have 'translation' field"
                );
                println!("Parsed JSON: {}", serde_json::to_string_pretty(&parsed)?);
            }
            Err(e) => {
                println!("Warning: Response is not valid JSON: {}", e);
                println!("Raw response: {}", text);
            }
        }
    }

    assert!(
        completion_result.content.is_some(),
        "Response should have content"
    );
    assert!(
        elapsed < get_test_timeout(),
        "Request should complete within timeout"
    );

    println!("test_completion_with_json_schema PASSED");
    Ok(())
}

// ============================================================================
// Streaming Tests
// ============================================================================

/// Test streaming chat
#[ignore = "requires LLM model - run with --ignored"]
#[test]
fn test_streaming_chat() -> Result<()> {
    let mut plugin = create_plugin_with_model()?;

    let args = LlmChatArgs {
        model: None,
        messages: vec![create_text_message(
            ChatRole::User,
            "Count from 1 to 5, one number per line.",
        )],
        options: Some(ChatLlmOptions {
            max_tokens: Some(100),
            temperature: Some(0.1),
            ..Default::default()
        }),
        function_options: None,
        json_schema: None,
    };

    let start = Instant::now();

    // Start streaming
    plugin.begin_stream(args.encode_to_vec(), HashMap::new(), Some("chat"))?;

    let mut chunks = Vec::new();
    let mut combined_text = String::new();

    // Receive stream chunks
    while let Some(chunk_bytes) = plugin.receive_stream()? {
        let chunk_result = LlmChatResult::decode(chunk_bytes.as_slice())?;
        if let Some(text) = extract_chat_text(&chunk_result) {
            combined_text.push_str(&text);
            print!("{}", text);
        }
        chunks.push(chunk_result);

        // Safety timeout
        if start.elapsed() > get_test_timeout() {
            break;
        }
    }
    println!();

    let elapsed = start.elapsed();
    println!("Streaming completed in {:?}", elapsed);
    println!("Received {} chunks", chunks.len());
    println!("Combined text: {}", combined_text);

    assert!(!chunks.is_empty(), "Should receive at least one chunk");
    assert!(!combined_text.is_empty(), "Should receive text content");
    assert!(
        elapsed < get_test_timeout(),
        "Request should complete within timeout"
    );

    println!("test_streaming_chat PASSED");
    Ok(())
}

/// Test streaming completion
/// Corresponds to: test_completion_stream_with_json_schema in mistral_json_schema_test.rs
#[ignore = "requires LLM model - run with --ignored"]
#[test]
fn test_streaming_completion() -> Result<()> {
    let mut plugin = create_plugin_with_model()?;

    let args = LlmCompletionArgs {
        model: None,
        system_prompt: None,
        prompt: "List 3 programming languages:".to_string(),
        context: None,
        options: Some(CompletionLlmOptions {
            max_tokens: Some(100),
            temperature: Some(0.1),
            ..Default::default()
        }),
        function_options: None,
        json_schema: None,
    };

    let start = Instant::now();

    // Start streaming
    plugin.begin_stream(args.encode_to_vec(), HashMap::new(), Some("completion"))?;

    let mut chunks = Vec::new();
    let mut combined_text = String::new();

    // Receive stream chunks
    while let Some(chunk_bytes) = plugin.receive_stream()? {
        let chunk_result = LlmCompletionResult::decode(chunk_bytes.as_slice())?;
        if let Some(text) = extract_completion_text(&chunk_result) {
            combined_text.push_str(&text);
            print!("{}", text);
        }
        chunks.push(chunk_result);

        // Safety timeout
        if start.elapsed() > get_test_timeout() {
            break;
        }
    }
    println!();

    let elapsed = start.elapsed();
    println!("Streaming completed in {:?}", elapsed);
    println!("Received {} chunks", chunks.len());
    println!("Combined text: {}", combined_text);

    assert!(!chunks.is_empty(), "Should receive at least one chunk");
    assert!(!combined_text.is_empty(), "Should receive text content");
    assert!(
        elapsed < get_test_timeout(),
        "Request should complete within timeout"
    );

    println!("test_streaming_completion PASSED");
    Ok(())
}

// ============================================================================
// Error Handling Tests
// ============================================================================

/// Test handling of invalid JSON schema
/// Corresponds to: test_invalid_json_schema_handling in mistral_json_schema_test.rs
#[ignore = "requires LLM model - run with --ignored"]
#[test]
fn test_invalid_json_schema_handling() -> Result<()> {
    let mut plugin = create_plugin_with_model()?;

    let invalid_schema = r#"{ invalid json schema }"#;

    let args = LlmChatArgs {
        model: None,
        messages: vec![create_text_message(ChatRole::User, "Say hello.")],
        options: Some(ChatLlmOptions {
            max_tokens: Some(50),
            temperature: Some(0.1),
            ..Default::default()
        }),
        function_options: None,
        json_schema: Some(invalid_schema.to_string()),
    };

    let start = Instant::now();
    let (result, _metadata) = plugin.run(args.encode_to_vec(), HashMap::new(), Some("chat"));
    let elapsed = start.elapsed();

    // The model should handle invalid schema gracefully
    // Either by ignoring it or returning an error
    match result {
        Ok(bytes) => {
            let chat_result = LlmChatResult::decode(bytes.as_slice())?;
            let text = extract_chat_text(&chat_result);
            println!("Response (schema ignored): {:?}", text);
            println!("Invalid JSON schema was handled gracefully");
        }
        Err(e) => {
            println!("Error (expected for invalid schema): {:?}", e);
            println!("Invalid JSON schema caused an error (also acceptable)");
        }
    }

    println!("Response time: {:?}", elapsed);
    println!("test_invalid_json_schema_handling PASSED");
    Ok(())
}

// ============================================================================
// Cancellation Tests
// ============================================================================

/// Test cancellation during inference
#[ignore = "requires LLM model - run with --ignored"]
#[test]
fn test_cancel_during_inference() -> Result<()> {
    let mut plugin = create_plugin_with_model()?;

    let args = LlmChatArgs {
        model: None,
        messages: vec![create_text_message(
            ChatRole::User,
            "Write a very long essay about the history of computing. Make it at least 1000 words.",
        )],
        options: Some(ChatLlmOptions {
            max_tokens: Some(2000),
            temperature: Some(0.7),
            ..Default::default()
        }),
        function_options: None,
        json_schema: None,
    };

    // Start streaming
    plugin.begin_stream(args.encode_to_vec(), HashMap::new(), Some("chat"))?;

    // Receive a few chunks
    let mut chunk_count = 0;
    let mut combined_text = String::new();

    while let Some(chunk_bytes) = plugin.receive_stream()? {
        let chunk_result = LlmChatResult::decode(chunk_bytes.as_slice())?;
        if let Some(text) = extract_chat_text(&chunk_result) {
            combined_text.push_str(&text);
        }
        chunk_count += 1;

        // Cancel after receiving a few chunks
        if chunk_count >= 3 {
            let cancelled = plugin.cancel();
            println!("Cancellation requested: {}", cancelled);
            assert!(
                cancelled || plugin.is_canceled(),
                "Should be able to cancel"
            );
            break;
        }
    }

    // Drain remaining chunks (if any)
    let mut remaining_chunks = 0;
    while (plugin.receive_stream()?).is_some() {
        remaining_chunks += 1;
        if remaining_chunks > 100 {
            break; // Safety limit
        }
    }

    println!("Received {} chunks before cancel", chunk_count);
    println!("Received {} chunks after cancel", remaining_chunks);
    println!(
        "Partial text: {}...",
        &combined_text[..combined_text.len().min(100)]
    );
    println!("Is canceled: {}", plugin.is_canceled());

    println!("test_cancel_during_inference PASSED");
    Ok(())
}

// ============================================================================
// Plugin Lifecycle Tests
// ============================================================================

/// Test plugin name and description
#[test]
fn test_plugin_metadata() {
    let plugin = MistralPlugin::new();

    assert_eq!(plugin.name(), "MistralLocalLLM");
    assert!(!plugin.description().is_empty());
    assert!(plugin.description().contains("LLM"));

    println!("Plugin name: {}", plugin.name());
    println!("Plugin description: {}", plugin.description());
    println!("test_plugin_metadata PASSED");
}

/// Test method schemas
#[test]
fn test_method_schemas() {
    let plugin = MistralPlugin::new();

    let schemas = plugin.method_proto_map();

    assert!(schemas.contains_key("chat"), "Should have 'chat' method");
    assert!(
        schemas.contains_key("completion"),
        "Should have 'completion' method"
    );

    let chat_schema = &schemas["chat"];
    assert!(!chat_schema.args_proto.is_empty());
    assert!(!chat_schema.result_proto.is_empty());

    let completion_schema = &schemas["completion"];
    assert!(!completion_schema.args_proto.is_empty());
    assert!(!completion_schema.result_proto.is_empty());

    println!("Chat schema: {:?}", chat_schema);
    println!("Completion schema: {:?}", completion_schema);
    println!("test_method_schemas PASSED");
}

/// Test settings schema
#[test]
fn test_settings_schema() {
    let plugin = MistralPlugin::new();

    let schema = plugin.settings_schema();
    assert!(!schema.is_empty());

    let parsed: serde_json::Value = serde_json::from_str(&schema).expect("Should be valid JSON");
    assert!(parsed.is_object());

    println!(
        "Settings schema (truncated): {}...",
        &schema[..schema.len().min(200)]
    );
    println!("test_settings_schema PASSED");
}
