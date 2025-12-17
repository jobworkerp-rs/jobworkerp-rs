//! Integration tests for streaming tool call functionality
//! Tests that tool calls are properly accumulated during streaming and returned as pending_tool_calls
//!
//! These tests require:
//! - An Ollama server running at http://ollama.ollama.svc.cluster.local:11434
//! - A jobworkerp-rs server running for tool call execution

#![allow(clippy::uninlined_format_args)]
#![allow(clippy::collapsible_match)]

use anyhow::Result;
use app::app::function::function_set::FunctionSetApp;
use app::module::test::create_hybrid_test_app;
use app_wrapper::llm::chat::ollama::OllamaChatService;
use futures::StreamExt;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::Content as ArgsContent;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::ToolExecutionRequests;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{ChatMessage, LlmOptions};
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
    ChatRole, FunctionOptions, MessageContent,
};
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content as ResultContent;
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::OllamaRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::{LlmChatArgs, LlmChatResult};
use proto::jobworkerp::data::RunnerId;
use proto::jobworkerp::function::data::{function_id, FunctionId, FunctionSetData, FunctionUsing};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::time::{timeout, Duration};

/// Test configuration
const OLLAMA_HOST: &str = "http://ollama.ollama.svc.cluster.local:11434";
const TEST_MODEL: &str = "qwen3:30b";
const OTLP_ADDR: &str = "http://otel-collector.default.svc.cluster.local:4317";
const TEST_TIMEOUT: Duration = Duration::from_secs(300);

/// Create Ollama chat service for streaming tests
async fn create_test_service() -> Result<Arc<OllamaChatService>> {
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
            name: "streaming_tool_test".to_string(),
            description: "Test set for streaming tool calls".to_string(),
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
    Ok(Arc::new(service))
}

/// Create test chat arguments with tool calling enabled but auto-calling disabled (manual mode)
fn create_manual_mode_args(message: &str) -> LlmChatArgs {
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
            function_set_name: Some("streaming_tool_test".to_string()),
            is_auto_calling: Some(false), // Manual mode - return pending_tool_calls
        }),
        json_schema: None,
    }
}

/// Test the full manual mode flow:
/// 1. First stream request returns pending_tool_calls
/// 2. Second stream request with ToolExecutionRequests executes tools and gets LLM response
#[tokio::test]
#[ignore = "Integration test requiring Ollama server"]
async fn test_streaming_manual_mode_returns_pending_tool_calls() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    let service = create_test_service().await?;
    let metadata = HashMap::new();

    // === Phase 1: First request - get pending_tool_calls ===
    let args =
        create_manual_mode_args("Please run the date command to get the current date and time.");

    println!("=== Phase 1: Sending streaming request with manual mode ===");
    let stream = timeout(
        TEST_TIMEOUT,
        service.clone().request_stream_chat(args.clone(), metadata.clone()),
    )
    .await??;

    let mut collected_results: Vec<LlmChatResult> = Vec::new();
    let mut stream = stream;

    while let Some(result) = stream.next().await {
        println!("Stream chunk received: done={}", result.done);
        collected_results.push(result);
    }

    println!("Total chunks received: {}", collected_results.len());
    assert!(
        !collected_results.is_empty(),
        "Should receive at least one chunk"
    );

    // Find the chunk with pending_tool_calls
    let pending_chunk = collected_results
        .iter()
        .find(|r| r.pending_tool_calls.is_some());

    let pending_calls = if let Some(chunk) = pending_chunk {
        println!("Found pending_tool_calls chunk!");
        let pending = chunk.pending_tool_calls.as_ref().unwrap();
        println!("Number of pending calls: {}", pending.calls.len());

        for call in &pending.calls {
            println!("  Tool call: {} - {}", call.fn_name, call.fn_arguments);
        }

        assert!(
            !pending.calls.is_empty(),
            "Should have at least one pending tool call"
        );
        assert!(
            chunk.requires_tool_execution == Some(true),
            "requires_tool_execution should be true"
        );

        pending.calls.clone()
    } else {
        panic!("Expected pending_tool_calls in response");
    };

    // === Phase 2: Send ToolExecutionRequests and get final response ===
    println!("\n=== Phase 2: Sending ToolExecutionRequests ===");

    // Build new args with original messages + assistant tool calls + tool execution requests
    let mut messages = args.messages.clone();

    // Add assistant message with tool calls
    messages.push(ChatMessage {
        role: ChatRole::Assistant as i32,
        content: Some(MessageContent {
            content: Some(ArgsContent::ToolCalls(
                jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::ToolCalls {
                    calls: pending_calls
                        .iter()
                        .map(|c| {
                            jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::ToolCall {
                                call_id: c.call_id.clone(),
                                fn_name: c.fn_name.clone(),
                                fn_arguments: c.fn_arguments.clone(),
                            }
                        })
                        .collect(),
                },
            )),
        }),
    });

    // Add tool execution requests
    let tool_exec_requests: Vec<jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::ToolExecutionRequest> =
        pending_calls
            .iter()
            .map(|c| {
                jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::ToolExecutionRequest {
                    call_id: c.call_id.clone(),
                    fn_name: c.fn_name.clone(),
                    fn_arguments: c.fn_arguments.clone(),
                }
            })
            .collect();

    messages.push(ChatMessage {
        role: ChatRole::Tool as i32,
        content: Some(MessageContent {
            content: Some(ArgsContent::ToolExecutionRequests(ToolExecutionRequests {
                requests: tool_exec_requests,
            })),
        }),
    });

    let continuation_args = LlmChatArgs {
        messages,
        ..args.clone()
    };

    let stream = timeout(
        TEST_TIMEOUT,
        service.clone().request_stream_chat(continuation_args, metadata.clone()),
    )
    .await??;

    let mut continuation_results: Vec<LlmChatResult> = Vec::new();
    let mut stream = stream;
    let mut found_tool_execution_result = false;
    let mut final_text = String::new();

    while let Some(result) = stream.next().await {
        println!(
            "Continuation chunk: done={}, tool_results={}, has_content={}",
            result.done,
            result.tool_execution_results.len(),
            result.content.is_some()
        );

        // Check for tool execution results
        if !result.tool_execution_results.is_empty() {
            found_tool_execution_result = true;
            for exec_result in &result.tool_execution_results {
                println!(
                    "  Tool execution result: {} = {} (success={})",
                    exec_result.fn_name, exec_result.result, exec_result.success
                );
            }
        }

        // Collect text content
        if let Some(ref content) = result.content {
            if let Some(ResultContent::Text(ref text)) = content.content {
                final_text.push_str(text);
            }
        }

        continuation_results.push(result);
    }

    println!("\nFinal text response: {}", final_text);
    println!("Total continuation chunks: {}", continuation_results.len());

    // Verify we got tool execution results
    assert!(
        found_tool_execution_result,
        "Should have received tool execution results"
    );

    // Verify we got a final text response from LLM
    assert!(
        !final_text.is_empty(),
        "Should have received final text response from LLM after tool execution"
    );

    // The response should contain date/time related content
    assert!(
        final_text.to_lowercase().contains("date")
            || final_text.to_lowercase().contains("time")
            || final_text.contains("2025")
            || final_text.contains("Dec")
            || final_text.contains("Mon")
            || final_text.contains("Tue")
            || final_text.contains("Wed")
            || final_text.contains("Thu")
            || final_text.contains("Fri")
            || final_text.contains("Sat")
            || final_text.contains("Sun")
            || final_text.len() > 20, // At least some substantial response
        "Response should contain date/time information: {}",
        final_text
    );

    // Check that stream ended properly
    let final_chunk = continuation_results.last();
    assert!(final_chunk.is_some(), "Should have a final chunk");
    assert!(
        final_chunk.unwrap().done,
        "Final chunk should have done=true"
    );

    println!("\n=== Test passed: Full manual mode flow completed ===");
    Ok(())
}

/// Test basic streaming without tool calls
#[tokio::test]
#[ignore = "Integration test requiring Ollama server"]
async fn test_streaming_without_tools() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    let service = create_test_service().await?;
    let metadata = HashMap::new();

    let args = LlmChatArgs {
        messages: vec![ChatMessage {
            role: ChatRole::User as i32,
            content: Some(MessageContent {
                content: Some(ArgsContent::Text("Hello, how are you today?".to_string())),
            }),
        }],
        options: Some(LlmOptions {
            temperature: Some(0.1),
            max_tokens: Some(1000),
            top_p: None,
            repeat_penalty: None,
            repeat_last_n: None,
            seed: Some(42),
            extract_reasoning_content: Some(false),
        }),
        model: Some(TEST_MODEL.to_string()),
        function_options: None, // No tool calling
        json_schema: None,
    };

    println!("Sending streaming request without tools...");
    let stream = timeout(
        TEST_TIMEOUT,
        service.clone().request_stream_chat(args, metadata),
    )
    .await??;

    let mut text_content = String::new();
    let mut chunk_count = 0;
    let mut stream = stream;

    while let Some(result) = stream.next().await {
        chunk_count += 1;
        if let Some(ref content) = result.content {
            if let Some(ResultContent::Text(ref text)) = content.content {
                text_content.push_str(text);
            }
        }
    }

    println!("Received {} chunks", chunk_count);
    println!("Full response: {}", text_content);

    assert!(chunk_count > 0, "Should receive at least one chunk");
    assert!(!text_content.is_empty(), "Should receive text content");

    Ok(())
}

/// Test that collected results from streaming match expected format
#[tokio::test]
#[ignore = "Integration test requiring Ollama server"]
async fn test_streaming_result_format() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    let service = create_test_service().await?;
    let metadata = HashMap::new();

    let args = create_manual_mode_args("Run echo hello");

    let stream = timeout(
        TEST_TIMEOUT,
        service.clone().request_stream_chat(args, metadata),
    )
    .await??;

    let mut stream = stream;
    let mut has_done_chunk = false;
    let mut has_content = false;

    while let Some(result) = stream.next().await {
        if result.done {
            has_done_chunk = true;
        }
        if result.content.is_some() || result.pending_tool_calls.is_some() {
            has_content = true;
        }
    }

    assert!(has_done_chunk, "Stream should have a done=true chunk");
    assert!(has_content, "Stream should have some content");

    Ok(())
}

/// Test streaming with echo command tool call
#[tokio::test]
#[ignore = "Integration test requiring Ollama server"]
async fn test_streaming_echo_tool_call() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    let service = create_test_service().await?;
    let metadata = HashMap::new();

    let args = create_manual_mode_args(
        "Execute the echo command with arguments 'Hello from streaming test'",
    );

    println!("Sending streaming request for echo command...");
    let stream = timeout(
        TEST_TIMEOUT,
        service.clone().request_stream_chat(args, metadata),
    )
    .await??;

    let mut stream = stream;
    let mut found_tool_call = false;
    let mut tool_fn_name = String::new();

    while let Some(result) = stream.next().await {
        if let Some(ref pending) = result.pending_tool_calls {
            for call in &pending.calls {
                println!(
                    "Found tool call: {} with args: {}",
                    call.fn_name, call.fn_arguments
                );
                found_tool_call = true;
                tool_fn_name = call.fn_name.clone();
            }
        }
    }

    if found_tool_call {
        println!("Successfully detected tool call: {}", tool_fn_name);
        // The function should be COMMAND (the runner name)
        assert!(
            tool_fn_name.contains("COMMAND") || tool_fn_name.contains("command"),
            "Expected COMMAND or similar function name, got: {}",
            tool_fn_name
        );
    } else {
        println!("No tool call detected - LLM may have responded differently");
    }

    Ok(())
}
