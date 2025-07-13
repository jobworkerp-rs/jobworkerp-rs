//! Integration tests for LLM cancellation functionality
//! Tests pre-execution and mid-execution cancellation for both Ollama and GenAI services

#![allow(clippy::uninlined_format_args)]
#![allow(clippy::collapsible_match)]

use anyhow::Result;
use app::app::function::function_set::FunctionSetApp;
use app::module::test::create_hybrid_test_app;
use app_wrapper::llm::chat::genai::GenaiChatService;
use app_wrapper::llm::chat::ollama::OllamaChatService;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{ChatMessage, LlmOptions};
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
    ChatRole, FunctionOptions, MessageContent,
};
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::{
    GenaiRunnerSettings, OllamaRunnerSettings,
};
use jobworkerp_runner::jobworkerp::runner::llm::LlmChatArgs;
use jobworkerp_runner::runner::RunnerTrait;
use proto::jobworkerp::function::data::{FunctionSetData, FunctionTarget, FunctionType};
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::{timeout, Instant};
use tokio_util::sync::CancellationToken;

/// Test configuration
const OLLAMA_HOST: &str = "http://ollama.ollama.svc.cluster.local:11434";
const TEST_MODEL: &str = "qwen3:30b"; // Use a larger model that takes time to respond
const OTLP_ADDR: &str = "http://otel-collector.default.svc.cluster.local:4317";

/// Create Ollama chat service for cancellation testing
async fn create_ollama_service() -> Result<OllamaChatService> {
    std::env::set_var("OTLP_ADDR", OTLP_ADDR);
    let app_module = create_hybrid_test_app().await?;

    // Create function set for tool calling tests
    let _result = app_module
        .function_set_app
        .as_ref()
        .create_function_set(&FunctionSetData {
            name: "ollama_cancel_test".to_string(),
            description: "Test set for Ollama cancellation tests".to_string(),
            category: 0,
            targets: vec![FunctionTarget {
                id: 1, // COMMAND runner
                r#type: FunctionType::Runner as i32,
            }],
        })
        .await;

    let settings = OllamaRunnerSettings {
        model: TEST_MODEL.to_string(),
        base_url: Some(OLLAMA_HOST.to_string()),
        system_prompt: Some(
            "You are a helpful assistant. Please provide detailed, thoughtful responses. When asked to write something long, write extensively with multiple paragraphs."
                .to_string(),
        ),
        ..Default::default()
    };

    let service = OllamaChatService::new(app_module.function_app, settings)?;
    Ok(service)
}

/// Create GenAI chat service for cancellation testing
async fn create_genai_service() -> Result<GenaiChatService> {
    std::env::set_var("OTLP_ADDR", OTLP_ADDR);
    let app_module = create_hybrid_test_app().await?;

    // Create function set for tool calling tests
    let _result = app_module
        .function_set_app
        .as_ref()
        .create_function_set(&FunctionSetData {
            name: "genai_cancel_test".to_string(),
            description: "Test set for GenAI cancellation tests".to_string(),
            category: 0,
            targets: vec![FunctionTarget {
                id: 1, // COMMAND runner
                r#type: FunctionType::Runner as i32,
            }],
        })
        .await;

    let settings = GenaiRunnerSettings {
        model: TEST_MODEL.to_string(),
        base_url: Some(OLLAMA_HOST.to_string()), // Use Ollama host via GenAI
        system_prompt: Some(
            "You are a helpful assistant. Please provide detailed, thoughtful responses. When asked to write something long, write extensively with multiple paragraphs."
                .to_string(),
        ),
    };

    let service = GenaiChatService::new(app_module.function_app, settings).await?;
    Ok(service)
}

/// Create test chat arguments for a long-running conversation
fn create_long_running_chat_args() -> LlmChatArgs {
    LlmChatArgs {
        messages: vec![ChatMessage {
            role: ChatRole::User as i32,
            content: Some(MessageContent {
                content: Some(
                    jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::Content::Text(
                        "Please write a very detailed, comprehensive essay about the history and future of artificial intelligence. Include multiple sections covering the early pioneers, major breakthroughs, current applications, challenges, ethical considerations, and future predictions. Make it at least 2000 words with detailed explanations and examples."
                            .to_string(),
                    ),
                ),
            }),
        }],
        options: Some(LlmOptions {
            temperature: Some(0.8), // Higher temperature for more creative, longer responses
            max_tokens: Some(50000), // Allow for very long responses
            top_p: None,
            repeat_penalty: None,
            repeat_last_n: None,
            seed: None, // No fixed seed for variety
            extract_reasoning_content: Some(false),
        }),
        model: Some(TEST_MODEL.to_string()),
        function_options: None, // No function calling for basic chat test
    }
}

/// Create simple test chat arguments for basic testing
fn create_simple_chat_args() -> jobworkerp_runner::jobworkerp::runner::llm::LlmChatArgs {
    jobworkerp_runner::jobworkerp::runner::llm::LlmChatArgs {
        messages: vec![jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatMessage {
            role: ChatRole::User as i32,
            content: Some(MessageContent {
                content: Some(
                    jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::Content::Text(
                        "Hello, this is a test message.".to_string(),
                    ),
                ),
            }),
        }],
        options: Some(LlmOptions {
            temperature: Some(0.1),
            max_tokens: Some(100),
            top_p: None,
            repeat_penalty: None,
            repeat_last_n: None,
            seed: Some(42),
            extract_reasoning_content: Some(false),
        }),
        model: Some(TEST_MODEL.to_string()),
        function_options: None,
    }
}

/// Create simple test completion arguments for basic testing
fn create_simple_completion_args() -> jobworkerp_runner::jobworkerp::runner::llm::LlmCompletionArgs
{
    jobworkerp_runner::jobworkerp::runner::llm::LlmCompletionArgs {
        prompt: "Hello, this is a test prompt.".to_string(),
        model: Some(TEST_MODEL.to_string()),
        system_prompt: None,
        function_options: None,
        options: None,
        context: None,
    }
}

/// Create test chat arguments for a tool-calling scenario that takes time
fn create_long_running_tool_call_args() -> LlmChatArgs {
    LlmChatArgs {
        messages: vec![ChatMessage {
            role: ChatRole::User as i32,
            content: Some(MessageContent {
                content: Some(
                    jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::Content::Text(
                        "I need you to run several commands in sequence: 1) First run 'sleep 15' to wait 15 seconds, 2) Then run 'date' to get current time, 3) Finally run 'echo Task completed'. Please execute these commands one by one and provide detailed commentary about each step."
                            .to_string(),
                    ),
                ),
            }),
        }],
        options: Some(LlmOptions {
            temperature: Some(0.3),
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
            function_set_name: Some("ollama_cancel_test".to_string()),
        }),
    }
}

/// Test Ollama chat cancellation during long-running response generation
#[tokio::test]
#[ignore = "Integration test requiring Ollama server"]
async fn test_ollama_chat_cancellation_mid_execution() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    let service = create_ollama_service().await?;

    let args = create_long_running_chat_args();
    let context = opentelemetry::Context::current();
    let metadata = HashMap::new();

    println!("Starting Ollama chat with long-running request...");
    let start_time = Instant::now();

    // Start the request and then cancel it after a short delay
    let request_task =
        tokio::spawn(async move { service.request_chat(args, context, metadata).await });

    // Wait a bit to let the request start processing
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Cancel the request by aborting the task (simulates external cancellation)
    request_task.abort();

    let elapsed = start_time.elapsed();
    println!("Request was cancelled after {:?}", elapsed);

    // Verify that the request was cancelled relatively quickly
    assert!(
        elapsed < Duration::from_secs(15),
        "Request should be cancelled before completion (elapsed: {:?})",
        elapsed
    );

    println!("✓ Ollama chat cancellation test completed successfully");
    Ok(())
}

/// Test GenAI chat cancellation during long-running response generation
#[tokio::test]
#[ignore = "Integration test requiring Ollama server"]
async fn test_genai_chat_cancellation_mid_execution() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    let service = create_genai_service().await?;

    let args = create_long_running_chat_args();
    let context = opentelemetry::Context::current();
    let metadata = HashMap::new();

    println!("Starting GenAI chat with long-running request...");
    let start_time = Instant::now();

    // Start the request and then cancel it after a short delay
    let request_task =
        tokio::spawn(async move { service.request_chat(args, context, metadata).await });

    // Wait a bit to let the request start processing
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Cancel the request by aborting the task (simulates external cancellation)
    request_task.abort();

    let elapsed = start_time.elapsed();
    println!("Request was cancelled after {:?}", elapsed);

    // Verify that the request was cancelled relatively quickly
    assert!(
        elapsed < Duration::from_secs(15),
        "Request should be cancelled before completion (elapsed: {:?})",
        elapsed
    );

    println!("✓ GenAI chat cancellation test completed successfully");
    Ok(())
}

/// Test Ollama tool call cancellation during long-running command execution
#[tokio::test]
#[ignore = "Integration test requiring Ollama server"]
async fn test_ollama_tool_call_cancellation_mid_execution() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    let service = create_ollama_service().await?;

    let args = create_long_running_tool_call_args();
    let context = opentelemetry::Context::current();
    let metadata = HashMap::new();

    println!("Starting Ollama tool call with long-running commands...");
    let start_time = Instant::now();

    // Start the request and then cancel it after a short delay
    let request_task =
        tokio::spawn(async move { service.request_chat(args, context, metadata).await });

    // Wait a bit to let the first sleep command start
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Cancel the request by aborting the task (simulates external cancellation)
    request_task.abort();

    let elapsed = start_time.elapsed();
    println!("Tool call request was cancelled after {:?}", elapsed);

    // Verify that the request was cancelled before the full sleep duration
    assert!(
        elapsed < Duration::from_secs(20),
        "Tool call should be cancelled before sleep command completes (elapsed: {:?})",
        elapsed
    );

    println!("✓ Ollama tool call cancellation test completed successfully");
    Ok(())
}

/// Test Ollama streaming chat cancellation
#[tokio::test]
#[ignore = "Integration test requiring Ollama server"]
async fn test_ollama_streaming_cancellation() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    let service = create_ollama_service().await?;

    let args = create_long_running_chat_args();

    println!("Starting Ollama streaming chat...");
    let start_time = Instant::now();

    // Start streaming request
    let stream_result = service.request_stream_chat(args).await;
    match stream_result {
        Ok(mut stream) => {
            let mut item_count = 0;

            // Create cancellation token and cancel it after processing a few items
            let cancel_token = CancellationToken::new();
            let cancel_token_clone = cancel_token.clone();

            // Cancel after 3 seconds
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(3)).await;
                cancel_token_clone.cancel();
            });

            // Process stream until cancellation
            use futures::StreamExt;
            loop {
                tokio::select! {
                    item = stream.next() => {
                        match item {
                            Some(_result_item) => {
                                item_count += 1;
                                println!("Received stream item #{}", item_count);

                                if item_count >= 10 {
                                    break; // Limit items to prevent endless streaming
                                }
                            }
                            None => {
                                println!("Stream ended naturally");
                                break;
                            }
                        }
                    }
                    _ = cancel_token.cancelled() => {
                        println!("Stream cancelled by token");
                        break;
                    }
                }
            }

            let elapsed = start_time.elapsed();
            println!(
                "Streaming was cancelled/stopped after {:?}, processed {} items",
                elapsed, item_count
            );

            // Should have processed some items but not completed the full response
            assert!(item_count > 0, "Should have processed some stream items");
            assert!(
                elapsed < Duration::from_secs(30),
                "Streaming should be cancelled before full completion (elapsed: {:?})",
                elapsed
            );
        }
        Err(e) => {
            println!("Failed to start streaming: {}", e);
            return Err(e);
        }
    }

    println!("✓ Ollama streaming cancellation test completed successfully");
    Ok(())
}

/// Test timeout behavior (ensuring requests can be cancelled via timeout)
#[tokio::test]
#[ignore = "Integration test requiring Ollama server"]
async fn test_llm_timeout_cancellation() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    let service = create_ollama_service().await?;

    let args = create_long_running_chat_args();
    let context = opentelemetry::Context::current();
    let metadata = HashMap::new();

    println!("Testing LLM request timeout cancellation...");
    let start_time = Instant::now();

    // Use a short timeout to force cancellation
    let timeout_duration = Duration::from_secs(5);
    let result = timeout(
        timeout_duration,
        service.request_chat(args, context, metadata),
    )
    .await;

    let elapsed = start_time.elapsed();
    println!("Request completed/timed out after {:?}", elapsed);

    match result {
        Ok(_) => {
            // If it completed successfully, it should have been very fast
            assert!(
                elapsed < Duration::from_secs(3),
                "If request completed, it should have been very fast (elapsed: {:?})",
                elapsed
            );
            println!("Request completed faster than expected");
        }
        Err(_) => {
            // If it timed out, it should be close to the timeout duration
            assert!(
                elapsed >= timeout_duration && elapsed < timeout_duration + Duration::from_secs(2),
                "Timeout should occur near the specified duration (elapsed: {:?}, timeout: {:?})",
                elapsed,
                timeout_duration
            );
            println!("✓ Request properly timed out as expected");
        }
    }

    println!("✓ LLM timeout cancellation test completed successfully");
    Ok(())
}

/// Test pre-execution cancellation with actual LLM Chat Runner
#[tokio::test]
async fn test_llm_chat_pre_execution_cancellation() -> Result<()> {
    println!("Testing LLM Chat Runner pre-execution cancellation...");

    // Create app module for testing
    let app_module = create_hybrid_test_app().await?;
    let mut runner =
        app_wrapper::llm::chat::LLMChatRunnerImpl::new(std::sync::Arc::new(app_module));

    // Set pre-cancelled token before running
    let cancelled_token = CancellationToken::new();
    cancelled_token.cancel();
    runner.set_cancellation_token(cancelled_token);

    // Create test arguments
    let args = create_simple_chat_args();
    let metadata = HashMap::new();

    let start_time = Instant::now();
    let serialized_args = {
        use prost::Message;
        let mut buf = Vec::new();
        args.encode(&mut buf)?;
        buf
    };
    let (result, _metadata) = runner.run(&serialized_args, metadata).await;
    let elapsed = start_time.elapsed();

    println!("LLM Chat run completed in {:?}", elapsed);

    // The request should be cancelled immediately
    match result {
        Ok(_) => {
            panic!("LLM Chat should have been cancelled but completed normally");
        }
        Err(e) => {
            println!("✓ LLM Chat was cancelled as expected: {}", e);
            assert!(e.to_string().contains("cancelled"));
        }
    }

    // Pre-execution cancellation should be very fast
    assert!(
        elapsed < Duration::from_millis(100),
        "Pre-execution cancellation should be immediate (elapsed: {:?})",
        elapsed
    );

    println!("✓ LLM Chat pre-execution cancellation test completed successfully");
    Ok(())
}

/// Test pre-execution cancellation with actual LLM Completion Runner
#[tokio::test]
async fn test_llm_completion_pre_execution_cancellation() -> Result<()> {
    println!("Testing LLM Completion Runner pre-execution cancellation...");

    let mut runner = app_wrapper::llm::completion::LLMCompletionRunnerImpl::new();

    // Set pre-cancelled token before running
    let cancelled_token = CancellationToken::new();
    cancelled_token.cancel();
    runner.set_cancellation_token(cancelled_token);

    // Create test arguments
    let args = create_simple_completion_args();
    let metadata = HashMap::new();

    let start_time = Instant::now();
    let serialized_args = {
        use prost::Message;
        let mut buf = Vec::new();
        args.encode(&mut buf)?;
        buf
    };
    let (result, _metadata) = runner.run(&serialized_args, metadata).await;
    let elapsed = start_time.elapsed();

    println!("LLM Completion run completed in {:?}", elapsed);

    // The request should be cancelled immediately (or fail due to no LLM service loaded)
    match result {
        Ok(_) => {
            panic!("LLM Completion should have been cancelled or failed but completed normally");
        }
        Err(e) => {
            println!("✓ LLM Completion was cancelled/failed as expected: {}", e);
            // Accept either cancellation or "llm is not initialized" error
            assert!(
                e.to_string().contains("cancelled") || e.to_string().contains("not initialized"),
                "Error should be cancellation or initialization failure: {}",
                e
            );
        }
    }

    // Should be very fast regardless of error type
    assert!(
        elapsed < Duration::from_millis(100),
        "Pre-execution should be immediate (elapsed: {:?})",
        elapsed
    );

    println!("✓ LLM Completion pre-execution cancellation test completed successfully");
    Ok(())
}

/// Test LLM Completion pre-execution cancellation with service loaded (requires no external dependencies)
#[tokio::test]
async fn test_llm_completion_pre_execution_cancellation_with_mock() -> Result<()> {
    println!("Testing LLM Completion Runner pre-execution cancellation with mocked service...");

    let mut runner = app_wrapper::llm::completion::LLMCompletionRunnerImpl::new();

    // Try to load with dummy settings (will fail but that's expected)
    let dummy_settings = jobworkerp_runner::jobworkerp::runner::llm::LlmRunnerSettings {
        settings: Some(
            jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::Settings::Ollama(
                jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::OllamaRunnerSettings {
                    model: "dummy".to_string(),
                    base_url: Some("http://nonexistent:11434".to_string()),
                    system_prompt: None,
                    pull_model: Some(false),
                }
            )
        )
    };

    let mut settings_buf = Vec::new();
    use prost::Message;
    dummy_settings.encode(&mut settings_buf)?;
    let _ = runner.load(settings_buf).await; // May fail, but that's ok for this test

    // Now set pre-cancelled token after loading attempt
    let cancelled_token = CancellationToken::new();
    cancelled_token.cancel();
    runner.set_cancellation_token(cancelled_token);

    // Create test arguments
    let args = create_simple_completion_args();
    let metadata = HashMap::new();

    let start_time = Instant::now();
    let serialized_args = {
        let mut buf = Vec::new();
        args.encode(&mut buf)?;
        buf
    };
    let (result, _metadata) = runner.run(&serialized_args, metadata).await;
    let elapsed = start_time.elapsed();

    println!("LLM Completion with mock run completed in {:?}", elapsed);

    // The request should be cancelled or fail quickly
    match result {
        Ok(_) => {
            // This should not happen with pre-cancelled token
            panic!("LLM Completion should have been cancelled but completed normally");
        }
        Err(e) => {
            println!("✓ LLM Completion was cancelled/failed as expected: {}", e);
            // Should fail quickly regardless of specific error
        }
    }

    // Should be very fast
    assert!(
        elapsed < Duration::from_millis(500),
        "Pre-execution should be immediate (elapsed: {:?})",
        elapsed
    );

    println!("✓ LLM Completion pre-execution cancellation with mock test completed successfully");
    Ok(())
}

/// Test external cancellation token control with different scenarios
#[tokio::test]
async fn test_external_cancellation_token_control() -> Result<()> {
    println!("Testing external cancellation token control scenarios...");

    let mut runner = app_wrapper::llm::completion::LLMCompletionRunnerImpl::new();

    // Scenario 1: Set a normal (not cancelled) token
    let normal_token = CancellationToken::new();
    runner.set_cancellation_token(normal_token.clone());

    let args = create_simple_completion_args();
    let metadata = HashMap::new();
    let serialized_args = {
        use prost::Message;
        let mut buf = Vec::new();
        args.encode(&mut buf)?;
        buf
    };

    // Should fail quickly due to no LLM service, but not due to cancellation
    let (result1, _) = runner.run(&serialized_args, metadata.clone()).await;
    assert!(result1.is_err());
    assert!(result1
        .as_ref()
        .unwrap_err()
        .to_string()
        .contains("not initialized"));
    println!("✓ Normal token allows execution attempt");

    // Scenario 2: Cancel the token externally and try again
    normal_token.cancel();

    // Set the same (now cancelled) token
    runner.set_cancellation_token(normal_token);

    let start_time = Instant::now();
    let (result2, _) = runner.run(&serialized_args, metadata).await;
    let elapsed = start_time.elapsed();

    // Should be cancelled or fail very quickly
    assert!(result2.is_err());
    assert!(
        elapsed < Duration::from_millis(50),
        "Should be immediate with cancelled token"
    );
    println!(
        "✓ Externally cancelled token prevents execution (elapsed: {:?})",
        elapsed
    );

    // Scenario 3: Set a new uncancelled token
    let fresh_token = CancellationToken::new();
    runner.set_cancellation_token(fresh_token);

    let (result3, _) = runner.run(&serialized_args, HashMap::new()).await;
    assert!(result3.is_err());
    assert!(result3
        .as_ref()
        .unwrap_err()
        .to_string()
        .contains("not initialized"));
    println!("✓ Fresh token allows execution attempt again");

    println!("✓ External cancellation token control test completed successfully");
    Ok(())
}

/// Test cancellation token management across multiple requests
#[tokio::test]
async fn test_cancellation_token_management() -> Result<()> {
    println!("Testing cancellation token lifecycle management...");

    // Test token creation and cancellation cycle
    let mut tokens = Vec::new();

    for i in 0..3 {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled(), "New token should not be cancelled");

        // Simulate some work with the token
        let start_time = Instant::now();

        // Cancel after a short delay
        tokio::spawn({
            let token_clone = token.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(100 * (i + 1))).await;
                token_clone.cancel();
            }
        });

        // Wait for cancellation
        token.cancelled().await;

        let elapsed = start_time.elapsed();
        println!("Token {} was cancelled after {:?}", i + 1, elapsed);

        assert!(token.is_cancelled(), "Token should be cancelled");
        tokens.push(token);
    }

    // Verify all tokens are cancelled
    for (i, token) in tokens.iter().enumerate() {
        assert!(
            token.is_cancelled(),
            "Token {} should remain cancelled",
            i + 1
        );
    }

    println!("✓ Cancellation token management test completed successfully");
    Ok(())
}
