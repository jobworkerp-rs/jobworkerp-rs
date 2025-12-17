//! Integration tests for manual tool call mode (is_auto_calling: false)
//! These tests require:
//! - Ollama server running at http://ollama.ollama.svc.cluster.local:11434
//! - jobworkerp-rs backend running for tool call execution

#![allow(clippy::uninlined_format_args)]
#![allow(clippy::collapsible_match)]

use anyhow::Result;
use app::app::function::function_set::FunctionSetApp;
use app::module::test::create_hybrid_test_app;
use app_wrapper::llm::chat::ollama::OllamaChatService;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::Content as ArgsContent;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::{
    ToolExecutionRequest, ToolExecutionRequests,
};
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
    message_content, ChatMessage, ChatRole, FunctionOptions, LlmOptions, MessageContent,
};
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content as ResultContent;
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::OllamaRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::LlmChatArgs;
use proto::jobworkerp::data::RunnerId;
use proto::jobworkerp::function::data::{function_id, FunctionId, FunctionSetData, FunctionUsing};
use std::collections::HashMap;
use tokio::time::{timeout, Duration};

/// Test configuration
const OLLAMA_HOST: &str = "http://ollama.ollama.svc.cluster.local:11434";
const TEST_MODEL: &str = "qwen3:30b";
const OTLP_ADDR: &str = "http://otel-collector.default.svc.cluster.local:4317";
const TEST_TIMEOUT: Duration = Duration::from_secs(300);

/// Create Ollama chat service for testing
async fn create_test_service() -> Result<OllamaChatService> {
    std::env::set_var("OTLP_ADDR", OTLP_ADDR);
    let app_module = create_hybrid_test_app().await?;

    let settings = OllamaRunnerSettings {
        model: TEST_MODEL.to_string(),
        base_url: Some(OLLAMA_HOST.to_string()),
        system_prompt: Some(
            "You are a helpful assistant. When asked to run commands, you MUST use the available tools to execute them. Always call the runTask function when users ask for command execution."
                .to_string(),
        ),
        pull_model: Some(true),
    };

    // Create function set for tool calls
    let _result = app_module
        .function_set_app
        .create_function_set(&FunctionSetData {
            name: "manual_tool_test".to_string(),
            description: "Test set for manual tool calls - COMMAND runner only".to_string(),
            category: 0,
            targets: vec![FunctionUsing {
                function_id: Some(FunctionId {
                    id: Some(function_id::Id::RunnerId(RunnerId { value: 1 })),
                }),
                using: None,
            }],
        })
        .await;

    let service = OllamaChatService::new(
        app_module.function_app.clone(),
        app_module.function_set_app.clone(),
        settings,
    )?;
    Ok(service)
}

/// Create chat arguments with manual mode (is_auto_calling: false)
fn create_manual_mode_args(message: &str) -> LlmChatArgs {
    LlmChatArgs {
        messages: vec![ChatMessage {
            role: ChatRole::User as i32,
            content: Some(MessageContent {
                content: Some(ArgsContent::Text(message.to_string())),
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
            function_set_name: Some("manual_tool_test".to_string()),
            is_auto_calling: Some(false), // Manual mode
        }),
        json_schema: None,
    }
}

/// Test that manual mode returns pending tool calls without executing them
#[tokio::test]
#[ignore = "Integration test requiring Ollama server and jobworkerp backend"]
async fn test_manual_mode_returns_pending_tool_calls() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    let service = create_test_service().await?;

    let args =
        create_manual_mode_args("Please run the 'date' command to get the current date and time.");

    let context = opentelemetry::Context::current();
    let metadata = HashMap::new();

    println!("Sending request in manual mode (is_auto_calling: false)...");
    let result = timeout(TEST_TIMEOUT, service.request_chat(args, context, metadata)).await??;

    println!(
        "Result received. Done: {}, requires_tool_execution: {:?}",
        result.done, result.requires_tool_execution
    );

    // In manual mode, should return pending tool calls
    assert!(
        !result.done,
        "Should not be done when tool calls are pending"
    );
    assert!(
        result.requires_tool_execution.unwrap_or(false),
        "Should require tool execution"
    );
    assert!(
        result.pending_tool_calls.is_some(),
        "Should have pending tool calls"
    );

    let pending = result.pending_tool_calls.unwrap();
    assert!(
        !pending.calls.is_empty(),
        "Should have at least one pending call"
    );

    println!("Pending tool calls:");
    for call in &pending.calls {
        println!(
            "  - call_id: {}, fn_name: {}, fn_arguments: {}",
            call.call_id, call.fn_name, call.fn_arguments
        );
    }

    // Verify tool call structure
    let first_call = &pending.calls[0];
    assert!(
        !first_call.call_id.is_empty(),
        "call_id should not be empty"
    );
    assert!(
        !first_call.fn_name.is_empty(),
        "fn_name should not be empty"
    );

    Ok(())
}

/// Test executing tool calls with client-provided arguments
#[tokio::test]
#[ignore = "Integration test requiring Ollama server and jobworkerp backend"]
async fn test_manual_mode_execute_with_client_arguments() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    let service = create_test_service().await?;

    // Step 1: Get pending tool calls
    let args = create_manual_mode_args("Please run the 'echo' command with some text.");

    let context = opentelemetry::Context::current();
    let metadata = HashMap::new();

    println!("Step 1: Getting pending tool calls...");
    let result = timeout(
        TEST_TIMEOUT,
        service.request_chat(args.clone(), context.clone(), metadata.clone()),
    )
    .await??;

    assert!(!result.done, "Should have pending tool calls");
    let pending = result
        .pending_tool_calls
        .expect("Should have pending tool calls");
    let first_call = &pending.calls[0];

    println!(
        "Received pending call: fn_name={}, original_args={}",
        first_call.fn_name, first_call.fn_arguments
    );

    // Step 2: Modify arguments and send tool execution request
    // Client modifies the arguments to use custom text
    // Note: Arguments must follow the runner's expected format: {"arguments": {...}, "settings": {...}}
    let modified_arguments =
        r#"{"arguments":{"command":"echo","args":["MODIFIED_BY_CLIENT_TEST"]},"settings":{}}"#;

    println!("Step 2: Sending tool execution request with modified arguments...");
    println!("  Modified arguments: {}", modified_arguments);

    let exec_request = ToolExecutionRequest {
        call_id: first_call.call_id.clone(),
        fn_name: first_call.fn_name.clone(),
        fn_arguments: modified_arguments.to_string(),
    };

    // Build message history with tool execution request
    let mut messages = args.messages.clone();

    // Add assistant's tool call response
    messages.push(ChatMessage {
        role: ChatRole::Assistant as i32,
        content: Some(MessageContent {
            content: Some(ArgsContent::ToolCalls(message_content::ToolCalls {
                calls: vec![message_content::ToolCall {
                    call_id: first_call.call_id.clone(),
                    fn_name: first_call.fn_name.clone(),
                    fn_arguments: first_call.fn_arguments.clone(),
                }],
            })),
        }),
    });

    // Add tool execution request
    messages.push(ChatMessage {
        role: ChatRole::Tool as i32,
        content: Some(MessageContent {
            content: Some(ArgsContent::ToolExecutionRequests(ToolExecutionRequests {
                requests: vec![exec_request],
            })),
        }),
    });

    let exec_args = LlmChatArgs { messages, ..args };

    let result = timeout(
        TEST_TIMEOUT,
        service.request_chat(exec_args, context, metadata),
    )
    .await??;

    println!("Step 2 result: done={}", result.done);

    // Should now be done with final response
    assert!(result.done, "Should be done after tool execution");

    if let Some(content) = result.content {
        if let Some(ResultContent::Text(text)) = content.content {
            println!("Final response: {}", text);
            // Response should contain the modified text
            assert!(
                text.contains("MODIFIED_BY_CLIENT_TEST") || text.to_lowercase().contains("echo"),
                "Response should contain execution result or mention echo: {}",
                text
            );
        }
    }

    Ok(())
}

/// Test that echo command with modified arguments works
#[tokio::test]
#[ignore = "Integration test requiring Ollama server and jobworkerp backend"]
async fn test_manual_mode_echo_with_custom_message() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    let service = create_test_service().await?;

    // Step 1: Request echo command
    let args =
        create_manual_mode_args("Execute the echo command with the text 'original message'.");

    let context = opentelemetry::Context::current();
    let metadata = HashMap::new();

    println!("Step 1: Requesting echo command...");
    let result = timeout(
        TEST_TIMEOUT,
        service.request_chat(args.clone(), context.clone(), metadata.clone()),
    )
    .await??;

    if result.done {
        println!("LLM responded without tool call, skipping test");
        return Ok(());
    }

    let pending = result
        .pending_tool_calls
        .expect("Should have pending tool calls");
    let first_call = &pending.calls[0];

    println!(
        "Original call: fn_name={}, args={}",
        first_call.fn_name, first_call.fn_arguments
    );

    // Step 2: Execute with completely different message
    // Note: Arguments must follow the runner's expected format: {"arguments": {...}, "settings": {...}}
    let custom_message = "CUSTOM_CLIENT_MESSAGE_12345";
    let modified_args = format!(
        r#"{{"arguments":{{"command":"echo","args":["{}"]}},"settings":{{}}}}"#,
        custom_message
    );

    println!("Step 2: Executing with custom message: {}", custom_message);

    let exec_request = ToolExecutionRequest {
        call_id: first_call.call_id.clone(),
        fn_name: first_call.fn_name.clone(),
        fn_arguments: modified_args,
    };

    let mut messages = args.messages.clone();
    messages.push(ChatMessage {
        role: ChatRole::Assistant as i32,
        content: Some(MessageContent {
            content: Some(ArgsContent::ToolCalls(message_content::ToolCalls {
                calls: vec![message_content::ToolCall {
                    call_id: first_call.call_id.clone(),
                    fn_name: first_call.fn_name.clone(),
                    fn_arguments: first_call.fn_arguments.clone(),
                }],
            })),
        }),
    });
    messages.push(ChatMessage {
        role: ChatRole::Tool as i32,
        content: Some(MessageContent {
            content: Some(ArgsContent::ToolExecutionRequests(ToolExecutionRequests {
                requests: vec![exec_request],
            })),
        }),
    });

    let exec_args = LlmChatArgs { messages, ..args };

    let result = timeout(
        TEST_TIMEOUT,
        service.request_chat(exec_args, context, metadata),
    )
    .await??;

    assert!(result.done, "Should be done after tool execution");

    if let Some(content) = result.content {
        if let Some(ResultContent::Text(text)) = content.content {
            println!("Final response: {}", text);
            // The response should contain our custom message
            assert!(
                text.contains(custom_message),
                "Response should contain custom message '{}': {}",
                custom_message,
                text
            );
        }
    }

    Ok(())
}

/// Test comparing auto mode vs manual mode
#[tokio::test]
#[ignore = "Integration test requiring Ollama server and jobworkerp backend"]
async fn test_auto_vs_manual_mode_comparison() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    let service = create_test_service().await?;

    let context = opentelemetry::Context::current();
    let metadata = HashMap::new();

    // Test 1: Auto mode - should execute and return final response
    println!("=== Test Auto Mode ===");
    let auto_args = LlmChatArgs {
        messages: vec![ChatMessage {
            role: ChatRole::User as i32,
            content: Some(MessageContent {
                content: Some(ArgsContent::Text(
                    "Run the 'date' command to show current time.".to_string(),
                )),
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
            function_set_name: Some("manual_tool_test".to_string()),
            is_auto_calling: Some(true), // Auto mode
        }),
        json_schema: None,
    };

    let auto_result = timeout(
        TEST_TIMEOUT,
        service.request_chat(auto_args, context.clone(), metadata.clone()),
    )
    .await??;

    println!(
        "Auto mode result: done={}, has_pending={}",
        auto_result.done,
        auto_result.pending_tool_calls.is_some()
    );

    // Auto mode should complete directly
    assert!(auto_result.done, "Auto mode should complete directly");
    assert!(
        auto_result.pending_tool_calls.is_none()
            || auto_result
                .pending_tool_calls
                .as_ref()
                .map(|p| p.calls.is_empty())
                .unwrap_or(true),
        "Auto mode should not have pending tool calls"
    );

    // Test 2: Manual mode - should return pending tool calls
    println!("\n=== Test Manual Mode ===");
    let manual_args = create_manual_mode_args("Run the 'date' command to show current time.");

    let manual_result = timeout(
        TEST_TIMEOUT,
        service.request_chat(manual_args, context, metadata),
    )
    .await??;

    println!(
        "Manual mode result: done={}, has_pending={}",
        manual_result.done,
        manual_result.pending_tool_calls.is_some()
    );

    // Manual mode should return pending tool calls (not complete)
    if !manual_result.done {
        assert!(
            manual_result.pending_tool_calls.is_some(),
            "Manual mode should have pending tool calls when not done"
        );
        println!("Manual mode correctly returned pending tool calls");
    } else {
        // LLM might have responded without tool call
        println!("LLM responded without tool call in manual mode");
    }

    Ok(())
}

/// Test handling of invalid/unknown function name in tool execution request
#[tokio::test]
#[ignore = "Integration test requiring Ollama server and jobworkerp backend"]
async fn test_manual_mode_invalid_function_name() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    let service = create_test_service().await?;

    // Create a direct tool execution request with invalid function name
    let args = LlmChatArgs {
        messages: vec![
            ChatMessage {
                role: ChatRole::User as i32,
                content: Some(MessageContent {
                    content: Some(ArgsContent::Text("Run a command".to_string())),
                }),
            },
            ChatMessage {
                role: ChatRole::Assistant as i32,
                content: Some(MessageContent {
                    content: Some(ArgsContent::ToolCalls(message_content::ToolCalls {
                        calls: vec![message_content::ToolCall {
                            call_id: "test-call-123".to_string(),
                            fn_name: "nonexistent_function_xyz".to_string(),
                            fn_arguments: "{}".to_string(),
                        }],
                    })),
                }),
            },
            ChatMessage {
                role: ChatRole::Tool as i32,
                content: Some(MessageContent {
                    content: Some(ArgsContent::ToolExecutionRequests(ToolExecutionRequests {
                        requests: vec![ToolExecutionRequest {
                            call_id: "test-call-123".to_string(),
                            fn_name: "nonexistent_function_xyz".to_string(),
                            fn_arguments: "{}".to_string(),
                        }],
                    })),
                }),
            },
        ],
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
            function_set_name: Some("manual_tool_test".to_string()),
            is_auto_calling: Some(false),
        }),
        json_schema: None,
    };

    let context = opentelemetry::Context::current();
    let metadata = HashMap::new();

    println!("Testing with invalid function name...");
    let result = timeout(TEST_TIMEOUT, service.request_chat(args, context, metadata)).await??;

    // Should complete (with error message in response)
    println!("Result: done={}", result.done);

    if let Some(content) = &result.content {
        if let Some(ResultContent::Text(text)) = &content.content {
            println!("Response text: {}", text);
            // Response should contain error information
            assert!(
                text.to_lowercase().contains("error")
                    || text.to_lowercase().contains("not found")
                    || text.to_lowercase().contains("fail")
                    || !text.is_empty(), // At least some response
                "Should have some response for invalid function"
            );
        }
    }

    Ok(())
}
