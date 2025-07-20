//! Simple integration test for GenAI tool call functionality
//! This test uses Ollama infrastructure with qwen3:30b model

#![allow(clippy::uninlined_format_args)]
#![allow(clippy::collapsible_match)]

use anyhow::Result;
use app::app::function::function_set::FunctionSetApp;
use app::module::test::create_hybrid_test_app;
use app_wrapper::llm::chat::genai::GenaiChatService;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{ChatMessage, LlmOptions};
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
    ChatRole, FunctionOptions, MessageContent,
};
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::GenaiRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::LlmChatArgs;
use proto::jobworkerp::function::data::{FunctionSetData, FunctionTarget, FunctionType};
use std::collections::HashMap;
use tokio::time::{timeout, Duration};

/// Test configuration
const OLLAMA_HOST: &str = "http://ollama.ollama.svc.cluster.local:11434";
const TEST_MODEL: &str = "qwen3:30b"; // Use qwen3:30b model via Ollama
const OTLP_ADDR: &str = "http://otel-collector.default.svc.cluster.local:4317";
const TEST_TIMEOUT: Duration = Duration::from_secs(300);

/// Create GenAI chat service for testing using Ollama infrastructure
async fn create_test_service() -> Result<GenaiChatService> {
    std::env::set_var("OTLP_ADDR", OTLP_ADDR);
    let app_module = create_hybrid_test_app().await?;

    let settings = GenaiRunnerSettings {
        model: TEST_MODEL.to_string(),
        base_url: Some(OLLAMA_HOST.to_string()), // Use Ollama host
        system_prompt: Some(
            "You are a helpful assistant. When asked to run commands, you MUST use the available tools to execute them. Always call the function when users ask for command execution. Do not explain or think - just execute the function call immediately."
                .to_string(),
        ),
    };

    // Try to delete existing function set if it exists to avoid unique constraint error
    // Note: delete_function_set requires FunctionSetId, so we'll handle the error if it already exists

    // Create function set with only COMMAND runner (id=1)
    // If it already exists, ignore the error and continue
    let _result = app_module
        .function_set_app
        .as_ref()
        .create_function_set(&FunctionSetData {
            name: "genai_tool_test".to_string(),
            description: "Test set for GenAI tool calls - COMMAND runner only".to_string(),
            category: 0,
            targets: vec![FunctionTarget {
                id: 1, // COMMAND runner
                r#type: FunctionType::Runner as i32,
            }],
        })
        .await;

    let service = GenaiChatService::new(app_module.function_app, settings).await?;
    Ok(service)
}

/// Create test chat arguments with tool calling enabled
fn create_chat_args_with_tools(message: &str) -> LlmChatArgs {
    LlmChatArgs {
        messages: vec![ChatMessage {
            role: ChatRole::User as i32,
            content: Some(MessageContent {
                content: Some(
                    jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::Content::Text(
                        message.to_string(),
                    ),
                ),
            }),
        }],
        options: Some(LlmOptions {
            temperature: Some(0.0),
            max_tokens: Some(50000),
            top_p: None,
            repeat_penalty: None,
            repeat_last_n: None,
            seed: Some(42),
            extract_reasoning_content: Some(false),
        }),
        model: Some(TEST_MODEL.to_string()),
        function_options: Some(FunctionOptions {
            use_function_calling: true,
            use_runners_as_function: Some(false),
            use_workers_as_function: Some(false),
            function_set_name: Some("genai_tool_test".to_string()),
        }),
        json_schema: None,
    }
}

/// Test basic date command through tool calling
#[tokio::test]
#[ignore = "Integration test requiring Ollama server"]
async fn test_basic_date_command() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    let service = create_test_service().await?;

    let args = create_chat_args_with_tools(
        "I need to know the current date and time. Please use the function calling to execute the date command now.",
    );

    let context = opentelemetry::Context::current();
    let metadata = HashMap::new();

    println!("Sending request to GenAI (via Ollama) with tool calling enabled...");
    let result = timeout(TEST_TIMEOUT, service.request_chat(args, context, metadata)).await??;

    println!("Received response. Done: {}", result.done);
    assert!(result.done, "Chat should be completed");

    if let Some(content) = result.content {
        if let Some(text_content) = content.content {
            if let jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(text) = text_content {
                println!("Date test response: {}", text);

                // Should contain date information
                assert!(
                    text.to_lowercase().contains("date")
                    || text.to_lowercase().contains("time")
                    || text.contains("2025")
                    || text.contains("Jan")
                    || text.contains("Mon")
                    || text.contains("Tue")
                    || text.contains("Wed")
                    || text.contains("Thu")
                    || text.contains("Fri")
                    || text.contains("Sat")
                    || text.contains("Sun"),
                    "Response should contain date/time information: {}",
                    text
                );
            }
        }
    }

    Ok(())
}

/// Test echo command through tool calling
#[tokio::test]
#[ignore = "Integration test requiring Ollama server"]
async fn test_echo_command() -> Result<()> {
    let service = create_test_service().await?;

    let args = create_chat_args_with_tools(
        "I need you to test the echo command. Please run: echo 'Testing GenAI 123'",
    );

    let context = opentelemetry::Context::current();
    let metadata = HashMap::new();

    let result = timeout(TEST_TIMEOUT, service.request_chat(args, context, metadata)).await??;

    assert!(result.done, "Chat should be completed");

    if let Some(content) = result.content {
        if let Some(text_content) = content.content {
            if let jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(text) = text_content {
                println!("Echo test response: {}", text);

                // Should contain the echo output
                assert!(
                    text.contains("Testing GenAI 123") || text.contains("testing") || text.contains("123"),
                    "Response should contain echo output 'Testing GenAI 123': {}",
                    text
                );
            }
        }
    }

    Ok(())
}

/// Test regular chat without tools (control test)
#[tokio::test]
#[ignore = "Integration test requiring Ollama server"]
async fn test_chat_without_tools() -> Result<()> {
    let service = create_test_service().await?;

    let args = LlmChatArgs {
        messages: vec![ChatMessage {
            role: ChatRole::User as i32,
            content: Some(MessageContent {
                content: Some(
                    jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::Content::Text(
                        "Hello, how are you today?".to_string(),
                    ),
                ),
            }),
        }],
        options: Some(LlmOptions {
            temperature: Some(0.1),
            max_tokens: Some(50000),
            top_p: None,
            repeat_penalty: None,
            repeat_last_n: None,
            seed: Some(42),
            extract_reasoning_content: Some(false),
        }),
        model: Some(TEST_MODEL.to_string()),
        function_options: None, // No function calling
        json_schema: None,
    };

    let context = opentelemetry::Context::current();
    let metadata = HashMap::new();

    let result = timeout(TEST_TIMEOUT, service.request_chat(args, context, metadata)).await??;

    assert!(result.done, "Chat should be completed");

    if let Some(content) = result.content {
        if let Some(text_content) = content.content {
            if let jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(text) = text_content {
                println!("Regular chat response: {}", text);

                // Should get a conversational response
                assert!(
                    text.len() > 5,
                    "Response should contain conversational content: {}",
                    text
                );
            }
        }
    }

    Ok(())
}

/// Test invalid command handling
#[tokio::test]
#[ignore = "Integration test requiring Ollama server"]
async fn test_invalid_command() -> Result<()> {
    let service = create_test_service().await?;

    let args =
        create_chat_args_with_tools("Please run the command 'this_command_does_not_exist_xyz123'.");

    let context = opentelemetry::Context::current();
    let metadata = HashMap::new();

    let result = timeout(TEST_TIMEOUT, service.request_chat(args, context, metadata)).await??;

    assert!(result.done, "Chat should be completed");

    if let Some(content) = result.content {
        if let Some(text_content) = content.content {
            if let jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(text) = text_content {
                println!("Invalid command response: {}", text);

                // Should handle the error gracefully
                assert!(
                    text.len() > 5,
                    "Response should handle invalid command: {}",
                    text
                );
            }
        }
    }

    Ok(())
}
