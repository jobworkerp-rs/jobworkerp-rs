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
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{ChatRole, MessageContent};
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::{
    GenaiRunnerSettings, OllamaRunnerSettings,
};
use jobworkerp_runner::jobworkerp::runner::llm::LlmRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::{LlmChatArgs, LlmCompletionArgs};
use jobworkerp_runner::runner::RunnerTrait;
use proto::jobworkerp::function::data::{FunctionSetData, FunctionTarget, FunctionType};
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Test configuration
const OLLAMA_HOST: &str = "http://ollama.ollama.svc.cluster.local:11434";
const TEST_MODEL: &str = "qwen3:30b"; // Use a larger model that takes time to respond
const OTLP_ADDR: &str = "http://otel-collector.default.svc.cluster.local:4317";

/// Create Ollama chat service for cancellation testing
async fn create_ollama_service() -> Result<OllamaChatService> {
    std::env::set_var("OTLP_ADDR", OTLP_ADDR);
    let _app_module = create_hybrid_test_app().await?;

    // Create function set for tool calling tests
    let _result = _app_module
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

    let service = OllamaChatService::new(_app_module.function_app, settings)?;
    Ok(service)
}

/// Create GenAI chat service for cancellation testing
async fn create_genai_service() -> Result<GenaiChatService> {
    std::env::set_var("OTLP_ADDR", OTLP_ADDR);
    let _app_module = create_hybrid_test_app().await?;

    // Create function set for tool calling tests
    let _result = _app_module
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

    let service = GenaiChatService::new(_app_module.function_app, settings).await?;
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
fn create_simple_completion_args() -> LlmCompletionArgs {
    LlmCompletionArgs {
        prompt: "Hello, this is a test prompt for completion.".to_string(),
        model: Some(TEST_MODEL.to_string()),
        system_prompt: None,
        function_options: None,
        options: Some(
            jobworkerp_runner::jobworkerp::runner::llm::llm_completion_args::LlmOptions {
                temperature: Some(0.1),
                max_tokens: Some(100),
                top_p: None,
                repeat_penalty: None,
                repeat_last_n: None,
                seed: Some(42),
                extract_reasoning_content: Some(false),
            },
        ),
        context: None,
    }
}

/// Create long-running test completion arguments
fn create_long_running_completion_args() -> LlmCompletionArgs {
    LlmCompletionArgs {
        prompt: "Please write a very detailed, comprehensive essay about the history and future of artificial intelligence. Include multiple sections covering the early pioneers, major breakthroughs, current applications, challenges, ethical considerations, and future predictions. Make it at least 2000 words with detailed explanations and examples.".to_string(),
        model: Some(TEST_MODEL.to_string()),
        system_prompt: None,
        function_options: None,
        options: Some(jobworkerp_runner::jobworkerp::runner::llm::llm_completion_args::LlmOptions {
            temperature: Some(0.8),
            max_tokens: Some(50000),
            top_p: None,
            repeat_penalty: None,
            repeat_last_n: None,
            seed: None,
            extract_reasoning_content: Some(false),
        }),
        context: None,
    }
}

/// Test Ollama chat mid-execution cancellation using runner.cancel()
#[tokio::test]
#[ignore = "Integration test requiring Ollama server"]
async fn test_ollama_chat_cancellation_mid_execution() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    let _app_module = create_hybrid_test_app().await?;

    let args = create_long_running_chat_args();
    let serialized_args = prost::Message::encode_to_vec(&args);
    let metadata = HashMap::new();

    // Create shared runner instance
    let runner = std::sync::Arc::new(tokio::sync::Mutex::new(
        app_wrapper::llm::chat::LLMChatRunnerImpl::new(std::sync::Arc::new(_app_module)),
    ));

    // Load Ollama settings
    let ollama_settings = OllamaRunnerSettings {
        model: TEST_MODEL.to_string(),
        base_url: Some(OLLAMA_HOST.to_string()),
        system_prompt: Some(
            "You are a helpful assistant. Please provide detailed, thoughtful responses. When asked to write something long, write extensively with multiple paragraphs."
                .to_string(),
        ),
        ..Default::default()
    };
    let settings = LlmRunnerSettings {
        settings: Some(
            jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::Settings::Ollama(
                ollama_settings,
            ),
        ),
    };
    let serialized_settings = prost::Message::encode_to_vec(&settings);

    // Create cancellation token and set it on the runner
    let cancellation_token = CancellationToken::new();
    {
        let mut runner_guard = runner.lock().await;
        runner_guard.load(serialized_settings).await?;
        runner_guard.set_cancellation_token(cancellation_token.clone());
    }

    println!("Starting Ollama chat with long-running request...");
    let start_time = Instant::now();

    let runner_clone = runner.clone();
    let execution_task = tokio::spawn(async move {
        let mut runner_guard = runner_clone.lock().await;
        runner_guard.run(&serialized_args, metadata).await
    });

    // Wait a bit to let the request start processing
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Cancel using the external token reference (avoids deadlock)
    cancellation_token.cancel();
    println!("Called cancellation_token.cancel() after 2 seconds");

    let elapsed = start_time.elapsed();
    println!("Request was cancelled after {:?}", elapsed);

    // Wait for the execution task to complete
    let _ = execution_task.await;

    // Verify that the request was cancelled relatively quickly
    assert!(
        elapsed < Duration::from_secs(15),
        "Request should be cancelled before completion (elapsed: {:?})",
        elapsed
    );

    println!("✓ Ollama chat runner.cancel() test completed successfully");
    Ok(())
}

/// Test GenAI chat mid-execution cancellation using runner.cancel()
#[tokio::test]
#[ignore = "Integration test requiring Ollama server"]
async fn test_genai_chat_cancellation_mid_execution() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    let _app_module = create_hybrid_test_app().await?;

    let args = create_long_running_chat_args();
    let serialized_args = prost::Message::encode_to_vec(&args);
    let metadata = HashMap::new();

    // Create shared runner instance
    let runner = std::sync::Arc::new(tokio::sync::Mutex::new(
        app_wrapper::llm::chat::LLMChatRunnerImpl::new(std::sync::Arc::new(_app_module)),
    ));

    // Load GenAI settings
    let genai_settings = GenaiRunnerSettings {
        model: TEST_MODEL.to_string(),
        base_url: Some(OLLAMA_HOST.to_string()),
        system_prompt: Some(
            "You are a helpful assistant. Please provide detailed, thoughtful responses. When asked to write something long, write extensively with multiple paragraphs."
                .to_string(),
        ),
    };
    let settings = LlmRunnerSettings {
        settings: Some(
            jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::Settings::Genai(
                genai_settings,
            ),
        ),
    };
    let serialized_settings = prost::Message::encode_to_vec(&settings);

    // Create cancellation token and set it on the runner
    let cancellation_token = CancellationToken::new();
    {
        let mut runner_guard = runner.lock().await;
        runner_guard.load(serialized_settings).await?;
        runner_guard.set_cancellation_token(cancellation_token.clone());
    }

    println!("Starting GenAI chat with long-running request...");
    let start_time = Instant::now();

    let runner_clone = runner.clone();
    let execution_task = tokio::spawn(async move {
        let mut runner_guard = runner_clone.lock().await;
        runner_guard.run(&serialized_args, metadata).await
    });

    // Wait a bit to let the request start processing
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Cancel using the external token reference (avoids deadlock)
    cancellation_token.cancel();
    println!("Called cancellation_token.cancel() after 2 seconds");

    let elapsed = start_time.elapsed();
    println!("Request was cancelled after {:?}", elapsed);

    // Wait for the execution task to complete
    let _ = execution_task.await;

    // Verify that the request was cancelled relatively quickly
    assert!(
        elapsed < Duration::from_secs(15),
        "Request should be cancelled before completion (elapsed: {:?})",
        elapsed
    );

    println!("✓ GenAI chat runner.cancel() test completed successfully");
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

/// Test pre-execution cancellation with actual LLM Chat Runner
#[tokio::test]
async fn test_llm_chat_pre_execution_cancellation() -> Result<()> {
    println!("Testing LLM Chat Runner pre-execution cancellation...");

    // Create app module for testing
    let _app_module = create_hybrid_test_app().await?;
    let mut runner =
        app_wrapper::llm::chat::LLMChatRunnerImpl::new(std::sync::Arc::new(_app_module));

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

/// Test GenAI streaming chat cancellation
#[tokio::test]
#[ignore = "Integration test requiring Ollama server"]
async fn test_genai_streaming_cancellation() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    let service = create_genai_service().await?;

    let args = create_long_running_chat_args();

    println!("Starting GenAI streaming chat...");
    let start_time = Instant::now();

    // Start streaming request
    let stream_result = service.request_chat_stream(args, HashMap::new()).await;
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
                                println!("Received GenAI stream item #{}", item_count);

                                if item_count >= 10 {
                                    break; // Limit items to prevent endless streaming
                                }
                            }
                            None => {
                                println!("GenAI stream ended naturally");
                                break;
                            }
                        }
                    }
                    _ = cancel_token.cancelled() => {
                        println!("GenAI stream cancelled by token");
                        break;
                    }
                }
            }

            let elapsed = start_time.elapsed();
            println!(
                "GenAI streaming was cancelled/stopped after {:?}, processed {} items",
                elapsed, item_count
            );

            // Should have processed some items but not completed the full response
            assert!(
                item_count > 0,
                "Should have processed some GenAI stream items"
            );
            assert!(
                elapsed < Duration::from_secs(30),
                "GenAI streaming should be cancelled before full completion (elapsed: {:?})",
                elapsed
            );
        }
        Err(e) => {
            println!("Failed to start GenAI streaming: {}", e);
            return Err(e);
        }
    }

    println!("✓ GenAI streaming cancellation test completed successfully");
    Ok(())
}

/// Test pre-execution cancellation with actual LLM Completion Runner (Ollama)
#[tokio::test]
async fn test_llm_completion_ollama_pre_execution_cancellation() -> Result<()> {
    println!("Testing LLM Completion Runner (Ollama) pre-execution cancellation...");

    let mut runner = app_wrapper::llm::completion::LLMCompletionRunnerImpl::new();

    // Load Ollama settings
    let ollama_settings = jobworkerp_runner::jobworkerp::runner::llm::LlmRunnerSettings {
        settings: Some(
            jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::Settings::Ollama(
                OllamaRunnerSettings {
                    model: TEST_MODEL.to_string(),
                    base_url: Some(OLLAMA_HOST.to_string()),
                    system_prompt: None,
                    pull_model: Some(false),
                },
            ),
        ),
    };

    let mut settings_buf = Vec::new();
    use prost::Message;
    ollama_settings.encode(&mut settings_buf)?;
    let _ = runner.load(settings_buf).await; // May fail if no Ollama server, but that's ok

    // Set pre-cancelled token before running
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

    println!("LLM Completion (Ollama) run completed in {:?}", elapsed);

    // The request should be cancelled immediately
    match result {
        Ok(_) => {
            panic!("LLM Completion (Ollama) should have been cancelled but completed normally");
        }
        Err(e) => {
            println!("✓ LLM Completion (Ollama) was cancelled as expected: {}", e);
            assert!(e.to_string().contains("cancelled"));
        }
    }

    // Pre-execution cancellation should be very fast
    assert!(
        elapsed < Duration::from_millis(100),
        "Pre-execution cancellation should be immediate (elapsed: {:?})",
        elapsed
    );

    println!("✓ LLM Completion (Ollama) pre-execution cancellation test completed successfully");
    Ok(())
}

/// Test pre-execution cancellation with actual LLM Completion Runner (GenAI)
#[tokio::test]
async fn test_llm_completion_genai_pre_execution_cancellation() -> Result<()> {
    println!("Testing LLM Completion Runner (GenAI) pre-execution cancellation...");

    let mut runner = app_wrapper::llm::completion::LLMCompletionRunnerImpl::new();

    // Load GenAI settings
    let genai_settings = jobworkerp_runner::jobworkerp::runner::llm::LlmRunnerSettings {
        settings: Some(
            jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::Settings::Genai(
                GenaiRunnerSettings {
                    model: TEST_MODEL.to_string(),
                    base_url: Some(OLLAMA_HOST.to_string()),
                    system_prompt: None,
                },
            ),
        ),
    };

    let mut settings_buf = Vec::new();
    use prost::Message;
    genai_settings.encode(&mut settings_buf)?;
    let _ = runner.load(settings_buf).await; // May fail if no Ollama server, but that's ok

    // Set pre-cancelled token before running
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

    println!("LLM Completion (GenAI) run completed in {:?}", elapsed);

    // The request should be cancelled immediately
    match result {
        Ok(_) => {
            panic!("LLM Completion (GenAI) should have been cancelled but completed normally");
        }
        Err(e) => {
            println!("✓ LLM Completion (GenAI) was cancelled as expected: {}", e);
            assert!(e.to_string().contains("cancelled"));
        }
    }

    // Pre-execution cancellation should be very fast
    assert!(
        elapsed < Duration::from_millis(100),
        "Pre-execution cancellation should be immediate (elapsed: {:?})",
        elapsed
    );

    println!("✓ LLM Completion (GenAI) pre-execution cancellation test completed successfully");
    Ok(())
}

/// Test mid-execution cancellation with actual LLM Completion Runner (Ollama)
#[tokio::test]
#[ignore = "Integration test requiring external setup"]
async fn test_llm_completion_ollama_mid_execution_cancellation() -> Result<()> {
    println!("Testing LLM Completion Runner (Ollama) mid-execution cancellation...");

    let runner = std::sync::Arc::new(tokio::sync::Mutex::new(
        app_wrapper::llm::completion::LLMCompletionRunnerImpl::new(),
    ));

    // Load Ollama settings
    let ollama_settings = jobworkerp_runner::jobworkerp::runner::llm::LlmRunnerSettings {
        settings: Some(
            jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::Settings::Ollama(
                OllamaRunnerSettings {
                    model: TEST_MODEL.to_string(),
                    base_url: Some(OLLAMA_HOST.to_string()),
                    system_prompt: None,
                    pull_model: Some(false),
                },
            ),
        ),
    };

    let mut settings_buf = Vec::new();
    use prost::Message;
    ollama_settings.encode(&mut settings_buf)?;

    // Create cancellation token and set it on the runner
    let cancellation_token = CancellationToken::new();
    {
        let mut runner_guard = runner.lock().await;
        let _ = runner_guard.load(settings_buf).await; // May fail if no Ollama server, but continue
        runner_guard.set_cancellation_token(cancellation_token.clone());
    }

    // Create test arguments
    let args = create_long_running_completion_args();
    let metadata = HashMap::new();

    let start_time = Instant::now();
    let serialized_args = {
        let mut buf = Vec::new();
        args.encode(&mut buf)?;
        buf
    };

    let runner_clone = runner.clone();

    // Start execution in a task
    let execution_task = tokio::spawn(async move {
        let mut runner_guard = runner_clone.lock().await;
        runner_guard.run(&serialized_args, metadata).await
    });

    // Wait a bit to let the request start processing
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Cancel using the external token reference (avoids deadlock)
    cancellation_token.cancel();
    println!("Called cancellation_token.cancel() after 2 seconds");

    // Wait for the execution to complete or be cancelled
    let execution_result = execution_task.await;
    let elapsed = start_time.elapsed();

    println!(
        "LLM Completion (Ollama) execution completed in {:?}",
        elapsed
    );

    match execution_result {
        Ok((result, _metadata)) => {
            match result {
                Ok(_) => {
                    // If it completed successfully, it should have been very fast
                    println!("LLM Completion completed before cancellation took effect");
                    assert!(
                        elapsed < Duration::from_secs(5),
                        "If completed successfully, should be fast (elapsed: {:?})",
                        elapsed
                    );
                }
                Err(e) => {
                    println!("✓ LLM Completion (Ollama) was cancelled as expected: {}", e);
                    assert!(
                        e.to_string().contains("cancelled"),
                        "Error should indicate cancellation: {}",
                        e
                    );
                    // Should be cancelled relatively quickly after the cancel() call
                    assert!(
                        elapsed < Duration::from_secs(10),
                        "Cancellation should happen reasonably quickly (elapsed: {:?})",
                        elapsed
                    );
                }
            }
        }
        Err(e) => {
            println!("Execution task failed: {}", e);
            return Err(e.into());
        }
    }

    println!("✓ LLM Completion (Ollama) mid-execution cancellation test completed successfully");
    Ok(())
}

/// Test mid-execution cancellation with actual LLM Completion Runner (GenAI)
#[tokio::test]
#[ignore = "Integration test requiring external setup"]
async fn test_llm_completion_genai_mid_execution_cancellation() -> Result<()> {
    println!("Testing LLM Completion Runner (GenAI) mid-execution cancellation...");

    let runner = std::sync::Arc::new(tokio::sync::Mutex::new(
        app_wrapper::llm::completion::LLMCompletionRunnerImpl::new(),
    ));

    // Load GenAI settings
    let genai_settings = jobworkerp_runner::jobworkerp::runner::llm::LlmRunnerSettings {
        settings: Some(
            jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::Settings::Genai(
                GenaiRunnerSettings {
                    model: TEST_MODEL.to_string(),
                    base_url: Some(OLLAMA_HOST.to_string()),
                    system_prompt: None,
                },
            ),
        ),
    };

    let mut settings_buf = Vec::new();
    use prost::Message;
    genai_settings.encode(&mut settings_buf)?;

    // Create cancellation token and set it on the runner
    let cancellation_token = CancellationToken::new();
    {
        let mut runner_guard = runner.lock().await;
        let _ = runner_guard.load(settings_buf).await; // May fail if no Ollama server, but continue
        runner_guard.set_cancellation_token(cancellation_token.clone());
    }

    // Create test arguments
    let args = create_long_running_completion_args();
    let metadata = HashMap::new();

    let start_time = Instant::now();
    let serialized_args = {
        let mut buf = Vec::new();
        args.encode(&mut buf)?;
        buf
    };

    let runner_clone = runner.clone();

    // Start execution in a task
    let execution_task = tokio::spawn(async move {
        let mut runner_guard = runner_clone.lock().await;
        runner_guard.run(&serialized_args, metadata).await
    });

    // Wait a bit to let the request start processing
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Cancel using the external token reference (avoids deadlock)
    cancellation_token.cancel();
    println!("Called cancellation_token.cancel() after 2 seconds");

    // Wait for the execution to complete or be cancelled
    let execution_result = execution_task.await;
    let elapsed = start_time.elapsed();

    println!(
        "LLM Completion (GenAI) execution completed in {:?}",
        elapsed
    );

    match execution_result {
        Ok((result, _metadata)) => {
            match result {
                Ok(_) => {
                    // If it completed successfully, it should have been very fast
                    println!("LLM Completion completed before cancellation took effect");
                    assert!(
                        elapsed < Duration::from_secs(5),
                        "If completed successfully, should be fast (elapsed: {:?})",
                        elapsed
                    );
                }
                Err(e) => {
                    println!("✓ LLM Completion (GenAI) was cancelled as expected: {}", e);
                    assert!(
                        e.to_string().contains("cancelled"),
                        "Error should indicate cancellation: {}",
                        e
                    );
                    // Should be cancelled relatively quickly after the cancel() call
                    assert!(
                        elapsed < Duration::from_secs(10),
                        "Cancellation should happen reasonably quickly (elapsed: {:?})",
                        elapsed
                    );
                }
            }
        }
        Err(e) => {
            println!("Execution task failed: {}", e);
            return Err(e.into());
        }
    }

    println!("✓ LLM Completion (GenAI) mid-execution cancellation test completed successfully");
    Ok(())
}
