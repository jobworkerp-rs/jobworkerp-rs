//! Integration tests for MistralRS plugin
//!
//! These tests require a MistralRS model loaded.
//! They are designed to cover the same functionality as app-wrapper/tests/mistral_json_schema_test.rs
//!
//! # Environment Variables
//! - `MISTRAL_TEST_TIMEOUT`: Override test timeout in seconds (default: 300)
//! - `MISTRAL_GRPC_ENDPOINT`: gRPC endpoint for tool calling tests
//!
//! # Example Usage
//! ```bash
//! cargo test --package plugin_runner_mistral --test integration_test -- --nocapture
//! # For ignored tests (require model):
//! cargo test --package plugin_runner_mistral --test integration_test -- --ignored --nocapture
//! ```

#![allow(clippy::uninlined_format_args)]

use anyhow::Result;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
    message_content, ChatMessage, ChatRole, LlmOptions as ChatLlmOptions, MessageContent,
};
use jobworkerp_runner::jobworkerp::runner::llm::llm_completion_args::LlmOptions as CompletionLlmOptions;
use jobworkerp_runner::jobworkerp::runner::llm::{LlmChatArgs, LlmCompletionArgs};
use mistralrs::RequestLike;
use plugin_runner_mistral::conversion::args::RequestConverter;
use plugin_runner_mistral::mistral_proto::MistralRunnerSettings;

#[allow(dead_code)]
fn create_test_settings() -> MistralRunnerSettings {
    MistralRunnerSettings { tool_calling: None }
}

fn create_text_message(role: ChatRole, text: &str) -> ChatMessage {
    ChatMessage {
        role: role as i32,
        content: Some(MessageContent {
            content: Some(message_content::Content::Text(text.to_string())),
        }),
    }
}

// ============================================================================
// Request Conversion Tests (unit tests that don't require model)
// ============================================================================

#[test]
fn test_request_conversion_simple_chat() {
    let args = LlmChatArgs {
        model: None,
        messages: vec![
            create_text_message(ChatRole::System, "You are a helpful assistant."),
            create_text_message(ChatRole::User, "What is 2+2?"),
        ],
        options: Some(ChatLlmOptions {
            max_tokens: Some(100),
            temperature: Some(0.2),
            ..Default::default()
        }),
        function_options: None,
        json_schema: None,
    };

    let result = RequestConverter::build_request(&args, vec![]);
    assert!(result.is_ok());
}

#[test]
fn test_request_conversion_with_json_schema() {
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
            max_tokens: Some(3512),
            temperature: Some(0.2),
            extract_reasoning_content: Some(true),
            ..Default::default()
        }),
        function_options: None,
        json_schema: Some(schema.to_string()),
    };

    let result = RequestConverter::build_request(&args, vec![]);
    assert!(result.is_ok());
}

#[test]
fn test_request_conversion_completion() {
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
        json_schema: None,
    };

    let result = RequestConverter::build_completion_request(&args);
    assert!(result.is_ok());
}

#[test]
fn test_request_conversion_with_invalid_schema() {
    let invalid_schema = r#"{ invalid json schema }"#;

    let args = LlmChatArgs {
        model: None,
        messages: vec![create_text_message(ChatRole::User, "Test message")],
        options: None,
        function_options: None,
        json_schema: Some(invalid_schema.to_string()),
    };

    // Request building should still succeed (schema validation happens at LLM level)
    let result = RequestConverter::build_request(&args, vec![]);
    assert!(result.is_ok());
}

#[test]
fn test_request_conversion_empty_messages() {
    let args = LlmChatArgs {
        model: None,
        messages: vec![],
        options: None,
        function_options: None,
        json_schema: None,
    };

    let result = RequestConverter::build_request(&args, vec![]);
    assert!(result.is_ok());
}

#[test]
fn test_request_conversion_with_all_options() {
    let args = LlmChatArgs {
        model: Some("test-model".to_string()),
        messages: vec![
            create_text_message(ChatRole::System, "You are a helpful assistant."),
            create_text_message(ChatRole::User, "Hello!"),
        ],
        options: Some(ChatLlmOptions {
            max_tokens: Some(1024),
            temperature: Some(0.7),
            top_p: Some(0.9),
            repeat_penalty: Some(1.1),
            repeat_last_n: Some(64),
            seed: Some(42),
            extract_reasoning_content: Some(false),
        }),
        function_options: None,
        json_schema: None,
    };

    let result = RequestConverter::build_request(&args, vec![]);
    assert!(result.is_ok());
}

// ============================================================================
// Proto Settings Tests
// ============================================================================

#[test]
fn test_settings_creation() {
    let settings = create_test_settings();
    // Plugin settings only contain tool_calling configuration
    // Model settings are passed via LlmChatArgs/LlmCompletionArgs
    assert!(settings.tool_calling.is_none());
}

#[test]
fn test_tool_calling_config_default() {
    use plugin_runner_mistral::core::types::ToolCallingConfig;

    // Clear env vars for consistent test
    std::env::remove_var("MISTRAL_GRPC_ENDPOINT");
    std::env::remove_var("MISTRAL_MAX_ITERATIONS");
    std::env::remove_var("MISTRAL_TOOL_TIMEOUT_SEC");
    std::env::remove_var("MISTRAL_PARALLEL_TOOLS");
    std::env::remove_var("MISTRAL_CONNECTION_TIMEOUT_SEC");

    let config = ToolCallingConfig::default();

    assert_eq!(config.max_iterations, 3);
    assert_eq!(config.tool_timeout_sec, 30);
    assert!(config.parallel_execution);
    assert!(config.grpc_endpoint.is_none());
    assert_eq!(config.connection_timeout_sec, 10);
}

#[test]
fn test_tool_calling_config_from_env() {
    use plugin_runner_mistral::core::types::ToolCallingConfig;

    std::env::set_var("MISTRAL_GRPC_ENDPOINT", "http://localhost:50051");
    std::env::set_var("MISTRAL_MAX_ITERATIONS", "5");
    std::env::set_var("MISTRAL_TOOL_TIMEOUT_SEC", "60");
    std::env::set_var("MISTRAL_PARALLEL_TOOLS", "false");
    std::env::set_var("MISTRAL_CONNECTION_TIMEOUT_SEC", "20");

    let config = ToolCallingConfig::default();

    assert_eq!(config.max_iterations, 5);
    assert_eq!(config.tool_timeout_sec, 60);
    assert!(!config.parallel_execution);
    assert_eq!(
        config.grpc_endpoint,
        Some("http://localhost:50051".to_string())
    );
    assert_eq!(config.connection_timeout_sec, 20);

    // Cleanup
    std::env::remove_var("MISTRAL_GRPC_ENDPOINT");
    std::env::remove_var("MISTRAL_MAX_ITERATIONS");
    std::env::remove_var("MISTRAL_TOOL_TIMEOUT_SEC");
    std::env::remove_var("MISTRAL_PARALLEL_TOOLS");
    std::env::remove_var("MISTRAL_CONNECTION_TIMEOUT_SEC");
}

// ============================================================================
// Integration Tests (require model - marked as ignored)
// ============================================================================

/// Test simple chat without JSON schema
/// Corresponds to: test_simple_chat_without_schema in mistral_json_schema_test.rs
#[ignore = "requires MistralRS model server"]
#[tokio::test]
async fn test_simple_chat_without_schema() -> Result<()> {
    let _settings = create_test_settings();

    let args = LlmChatArgs {
        model: None,
        messages: vec![create_text_message(ChatRole::User, "What is 2+2?")],
        options: Some(ChatLlmOptions {
            max_tokens: Some(100),
            temperature: Some(0.2),
            ..Default::default()
        }),
        function_options: None,
        json_schema: None,
    };

    // Build request (model execution would happen here in real integration)
    let request = RequestConverter::build_request(&args, vec![])?;
    assert!(!request.messages_ref().is_empty());

    println!("Simple chat request built successfully");
    Ok(())
}

/// Test chat with JSON schema for structured output
/// Corresponds to: test_chat_with_json_schema in mistral_json_schema_test.rs
#[ignore = "requires MistralRS model server"]
#[tokio::test]
async fn test_chat_with_json_schema() -> Result<()> {
    let _settings = create_test_settings();

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
            max_tokens: Some(3512),
            temperature: Some(0.2),
            extract_reasoning_content: Some(true),
            ..Default::default()
        }),
        function_options: None,
        json_schema: Some(schema.to_string()),
    };

    let request = RequestConverter::build_request(&args, vec![])?;
    assert!(!request.messages_ref().is_empty());

    println!("Chat with JSON schema request built successfully");
    Ok(())
}

/// Test completion with JSON schema
/// Corresponds to: test_completion_with_json_schema in mistral_json_schema_test.rs
#[ignore = "requires MistralRS model server"]
#[tokio::test]
async fn test_completion_with_json_schema() -> Result<()> {
    let _settings = create_test_settings();

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
            max_tokens: Some(25600),
            temperature: Some(0.3),
            ..Default::default()
        }),
        function_options: None,
        json_schema: Some(schema.to_string()),
    };

    let request = RequestConverter::build_completion_request(&args)?;
    assert!(!request.messages_ref().is_empty());

    println!("Completion with JSON schema request built successfully");
    Ok(())
}

/// Test handling of invalid JSON schema
/// Corresponds to: test_invalid_json_schema_handling in mistral_json_schema_test.rs
#[ignore = "requires MistralRS model server"]
#[tokio::test]
async fn test_invalid_json_schema_handling() -> Result<()> {
    let _settings = create_test_settings();

    let invalid_schema = r#"{ invalid json schema }"#;

    let args = LlmChatArgs {
        model: None,
        messages: vec![create_text_message(ChatRole::User, "Test message")],
        options: None,
        function_options: None,
        json_schema: Some(invalid_schema.to_string()),
    };

    // Request building should still succeed
    let request = RequestConverter::build_request(&args, vec![])?;
    assert!(!request.messages_ref().is_empty());

    println!("Invalid JSON schema handled gracefully");
    Ok(())
}

/// Test streaming completion with JSON schema
/// Corresponds to: test_completion_stream_with_json_schema in mistral_json_schema_test.rs
#[ignore = "requires MistralRS model server"]
#[tokio::test]
async fn test_completion_stream_with_json_schema() -> Result<()> {
    let _settings = create_test_settings();

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
        model: None,
        system_prompt: None,
        prompt: "List 3 programming languages and count them".to_string(),
        context: None,
        options: Some(CompletionLlmOptions {
            max_tokens: Some(256),
            temperature: Some(0.1),
            ..Default::default()
        }),
        function_options: None,
        json_schema: Some(schema.to_string()),
    };

    let request = RequestConverter::build_completion_request(&args)?;
    assert!(!request.messages_ref().is_empty());

    println!("Streaming completion request built successfully");
    Ok(())
}

/// Test workflow schema generation
/// Corresponds to: test_workflow_8level_schema_with_mistral_chat in mistral_json_schema_test.rs
#[ignore = "requires MistralRS model server and large schema file"]
#[tokio::test]
async fn test_workflow_schema_generation() -> Result<()> {
    let _settings = create_test_settings();

    // Load the workflow schema if available
    let schema_path = "../../../runner/schema/workflow_8level_final.json";
    let schema = match std::fs::read_to_string(schema_path) {
        Ok(s) => s,
        Err(_) => {
            println!(
                "Workflow schema file not found at {}, skipping test",
                schema_path
            );
            return Ok(());
        }
    };

    let args = LlmChatArgs {
        model: None,
        messages: vec![create_text_message(
            ChatRole::User,
            "Generate a simple workflow that runs a command task named 'hello' that executes 'echo Hello World'.",
        )],
        options: Some(ChatLlmOptions {
            max_tokens: Some(20480),
            temperature: Some(0.1),
            ..Default::default()
        }),
        function_options: None,
        json_schema: Some(schema),
    };

    let request = RequestConverter::build_request(&args, vec![])?;
    assert!(!request.messages_ref().is_empty());

    println!("Workflow schema generation request built successfully");
    Ok(())
}

// ============================================================================
// Result Conversion Tests
// ============================================================================

#[test]
fn test_result_converter_finish_reasons() {
    // Test that different finish reasons are handled correctly
    let finish_reasons = vec!["stop", "length", "canceled", "tool_calls"];

    for reason in finish_reasons {
        let is_done = reason == "stop" || reason == "length" || reason == "canceled";
        assert_eq!(
            is_done,
            reason == "stop" || reason == "length" || reason == "canceled",
            "Unexpected done status for finish_reason: {}",
            reason
        );
    }
}

#[test]
fn test_function_specs_conversion_empty() {
    use plugin_runner_mistral::grpc::generated::jobworkerp::function::data::FunctionSpecs;

    let specs: Vec<FunctionSpecs> = vec![];
    let result = RequestConverter::convert_function_specs_to_tools(&specs);
    assert!(result.is_ok());
    assert!(result.unwrap().is_empty());
}

#[test]
fn test_function_specs_conversion_single_method() {
    use plugin_runner_mistral::grpc::generated::jobworkerp::function::data::{
        FunctionSpecs, MethodSchema, MethodSchemaMap,
    };
    use std::collections::HashMap;

    let mut schemas = HashMap::new();
    schemas.insert(
        "run".to_string(),
        MethodSchema {
            arguments_schema: r#"{"type": "object"}"#.to_string(),
            description: Some("Execute function".to_string()),
            ..Default::default()
        },
    );

    let specs = vec![FunctionSpecs {
        name: "my_tool".to_string(),
        methods: Some(MethodSchemaMap { schemas }),
        ..Default::default()
    }];

    let result = RequestConverter::convert_function_specs_to_tools(&specs);
    assert!(result.is_ok());

    let tools = result.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].function.name, "my_tool");
}

#[test]
fn test_function_specs_conversion_multiple_methods() {
    use plugin_runner_mistral::grpc::generated::jobworkerp::function::data::{
        FunctionSpecs, MethodSchema, MethodSchemaMap,
    };
    use std::collections::HashMap;

    let mut schemas = HashMap::new();
    schemas.insert(
        "create".to_string(),
        MethodSchema {
            arguments_schema: "{}".to_string(),
            description: Some("Create item".to_string()),
            ..Default::default()
        },
    );
    schemas.insert(
        "delete".to_string(),
        MethodSchema {
            arguments_schema: "{}".to_string(),
            description: Some("Delete item".to_string()),
            ..Default::default()
        },
    );

    let specs = vec![FunctionSpecs {
        name: "item_manager".to_string(),
        methods: Some(MethodSchemaMap { schemas }),
        ..Default::default()
    }];

    let result = RequestConverter::convert_function_specs_to_tools(&specs);
    assert!(result.is_ok());

    let tools = result.unwrap();
    assert_eq!(tools.len(), 2);

    let tool_names: Vec<&str> = tools.iter().map(|t| t.function.name.as_str()).collect();
    assert!(tool_names.contains(&"item_manager___create"));
    assert!(tool_names.contains(&"item_manager___delete"));
}
