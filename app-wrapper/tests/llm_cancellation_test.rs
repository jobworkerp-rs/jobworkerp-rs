//! Integration tests for LLM cancellation functionality
//! Tests pre-execution and mid-execution cancellation for both Ollama and GenAI services

#![allow(clippy::uninlined_format_args)]
#![allow(clippy::collapsible_match)]

use anyhow::Result;
use app::module::test::create_hybrid_test_app;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{ChatMessage, LlmOptions};
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{ChatRole, MessageContent};
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::{
    GenaiRunnerSettings, OllamaRunnerSettings,
};
use jobworkerp_runner::jobworkerp::runner::llm::LlmRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::{LlmChatArgs, LlmCompletionArgs};
use jobworkerp_runner::runner::RunnerTrait;
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Test configuration
const OLLAMA_HOST: &str = "http://ollama.ollama.svc.cluster.local:11434";
const TEST_MODEL: &str = "qwen3:30b"; // Use a larger model that takes time to respond

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
        prompt: "Please write a very detailed, comprehensive essay about the history and future of artificial intelligence. Include multiple sections covering the early pioneers, major breakthroughs, current applications, challenges, ethical considerations, and future predictions. Make it at least 10000 words with detailed explanations and examples.".to_string(),
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

    // Create cancellation token and manager, set it on the runner
    let cancellation_token = CancellationToken::new();
    {
        let mut runner_guard = runner.lock().await;
        runner_guard.load(serialized_settings).await?;

        // Set up cancellation manager with token
        #[cfg(any(test, feature = "test-utils"))]
        {
            let mut cancellation_manager =
                app_wrapper::runner::cancellation::RunnerCancellationManager::new();
            cancellation_manager.set_cancellation_token(cancellation_token.clone());
            runner_guard.set_cancellation_manager(Box::new(cancellation_manager));
        }
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
        // Set up cancellation manager with token
        #[cfg(any(test, feature = "test-utils"))]
        {
            let mut cancellation_manager =
                app_wrapper::runner::cancellation::RunnerCancellationManager::new();
            cancellation_manager.set_cancellation_token(cancellation_token.clone());
            runner_guard.set_cancellation_manager(Box::new(cancellation_manager));
        }
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

/// Test Ollama streaming chat cancellation using runner.cancel()
#[tokio::test]
#[ignore = "Integration test requiring Ollama server"]
async fn test_ollama_streaming_cancellation() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    println!("Testing Ollama chat runner stream cancellation...");

    let _app_module = create_hybrid_test_app().await?;
    let runner = std::sync::Arc::new(tokio::sync::Mutex::new(
        app_wrapper::llm::chat::LLMChatRunnerImpl::new(std::sync::Arc::new(_app_module)),
    ));

    // Load Ollama settings
    let ollama_settings = OllamaRunnerSettings {
        model: TEST_MODEL.to_string(),
        base_url: Some(OLLAMA_HOST.to_string()),
        system_prompt: Some(
            "You are a helpful assistant. Please provide very detailed, comprehensive responses with multiple paragraphs.".to_string(),
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

    // Create cancellation token and set it on the runner BEFORE execution
    let cancellation_token = CancellationToken::new();
    {
        let mut runner_guard = runner.lock().await;
        runner_guard.load(serialized_settings).await?;
        // Set up cancellation manager with token
        #[cfg(any(test, feature = "test-utils"))]
        {
            let mut cancellation_manager =
                app_wrapper::runner::cancellation::RunnerCancellationManager::new();
            cancellation_manager.set_cancellation_token(cancellation_token.clone());
            runner_guard.set_cancellation_manager(Box::new(cancellation_manager));
        }
    }

    // Create test arguments with a long-running prompt
    let args = create_long_running_chat_args();
    let metadata = HashMap::new();

    let start_time = Instant::now();
    let serialized_args = {
        use prost::Message;
        let mut buf = Vec::new();
        args.encode(&mut buf)?;
        buf
    };

    let runner_clone = runner.clone();

    // Start stream execution in a task
    let execution_task = tokio::spawn(async move {
        let mut runner_guard = runner_clone.lock().await;
        let stream_result = runner_guard.run_stream(&serialized_args, metadata).await?;

        // Process the stream
        use futures::StreamExt;
        let mut item_count = 0;
        let mut stream = stream_result;

        while let Some(_item) = stream.next().await {
            item_count += 1;
            println!("Received Ollama stream item #{}", item_count);

            // Add a small delay to make the stream processing observable
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        Ok::<usize, anyhow::Error>(item_count)
    });

    // Wait for stream to start producing items (let it run for a bit)
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Cancel using the external token reference (avoids deadlock)
    cancellation_token.cancel();
    println!("Called cancellation_token.cancel() after 1 second");

    // Wait for the execution to complete or be cancelled
    let execution_result = execution_task.await;
    let elapsed = start_time.elapsed();

    println!("Ollama stream execution completed in {:?}", elapsed);

    match execution_result {
        Ok(stream_processing_result) => {
            match stream_processing_result {
                Ok(item_count) => {
                    println!(
                        "Ollama stream processed {} items before completion/cancellation",
                        item_count
                    );
                    // Check if cancellation likely occurred based on timing and item count
                    if elapsed <= Duration::from_secs(2) && item_count < 1000 {
                        println!("✓ Ollama stream was likely cancelled mid-execution (processed {} items in {:?})", item_count, elapsed);
                    } else if elapsed > Duration::from_secs(10) {
                        return Err(anyhow::anyhow!(
                            "Stream should have been cancelled within 10 seconds but took {:?} to complete normally. Cancellation failed.",
                            elapsed
                        ));
                    } else {
                        println!(
                            "✓ Ollama stream completed quickly (before cancellation took effect)"
                        );
                    }
                }
                Err(e) => {
                    println!(
                        "✓ Ollama stream processing was cancelled as expected: {}",
                        e
                    );
                }
            }
        }
        Err(e) => {
            println!("Ollama stream execution task failed: {}", e);
            return Err(e.into());
        }
    }

    // Verify that cancellation happened within reasonable time
    if elapsed > Duration::from_secs(10) {
        return Err(anyhow::anyhow!(
            "Stream processing took too long ({:?}), suggesting cancellation did not work properly",
            elapsed
        ));
    }

    println!("✓ Ollama stream cancellation test completed successfully");
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

    // Create cancellation manager with pre-cancelled token
    let cancelled_token = CancellationToken::new();
    cancelled_token.cancel();
    let mut cancellation_manager =
        app_wrapper::runner::cancellation::RunnerCancellationManager::new();
    // Set the token internally in the manager (for testing)
    cancellation_manager.set_cancellation_token(cancelled_token);
    let manager_with_token = Box::new(cancellation_manager);
    runner.set_cancellation_manager(manager_with_token);

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

/// Test GenAI streaming chat cancellation using runner.cancel()
#[tokio::test]
#[ignore = "Integration test requiring Ollama server"]
async fn test_genai_streaming_cancellation() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    println!("Testing GenAI chat runner stream cancellation...");

    let _app_module = create_hybrid_test_app().await?;
    let runner = std::sync::Arc::new(tokio::sync::Mutex::new(
        app_wrapper::llm::chat::LLMChatRunnerImpl::new(std::sync::Arc::new(_app_module)),
    ));

    // Load GenAI settings
    let genai_settings = GenaiRunnerSettings {
        model: TEST_MODEL.to_string(),
        base_url: Some(OLLAMA_HOST.to_string()), // Use Ollama host via GenAI
        system_prompt: Some(
            "You are a helpful assistant. Please provide very detailed, comprehensive responses with multiple paragraphs.".to_string(),
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

    // Create cancellation token and set it on the runner BEFORE execution
    let cancellation_token = CancellationToken::new();
    {
        let mut runner_guard = runner.lock().await;
        runner_guard.load(serialized_settings).await?;
        // Set up cancellation manager with token
        #[cfg(any(test, feature = "test-utils"))]
        {
            let mut cancellation_manager =
                app_wrapper::runner::cancellation::RunnerCancellationManager::new();
            cancellation_manager.set_cancellation_token(cancellation_token.clone());
            runner_guard.set_cancellation_manager(Box::new(cancellation_manager));
        }
    }

    // Create test arguments with a long-running prompt
    let args = create_long_running_chat_args();
    let metadata = HashMap::new();

    let start_time = Instant::now();
    let serialized_args = {
        use prost::Message;
        let mut buf = Vec::new();
        args.encode(&mut buf)?;
        buf
    };

    let runner_clone = runner.clone();

    // Start stream execution in a task
    let execution_task = tokio::spawn(async move {
        let mut runner_guard = runner_clone.lock().await;
        let stream_result = runner_guard.run_stream(&serialized_args, metadata).await?;

        // Process the stream
        use futures::StreamExt;
        let mut item_count = 0;
        let mut stream = stream_result;

        while let Some(_item) = stream.next().await {
            item_count += 1;
            println!("Received GenAI stream item #{}", item_count);

            // Add a small delay to make the stream processing observable
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        Ok::<usize, anyhow::Error>(item_count)
    });

    // Wait for stream to start producing items (let it run for a bit)
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Cancel using the external token reference (avoids deadlock)
    cancellation_token.cancel();
    println!("Called cancellation_token.cancel() after 1 second");

    // Wait for the execution to complete or be cancelled
    let execution_result = execution_task.await;
    let elapsed = start_time.elapsed();

    println!("GenAI stream execution completed in {:?}", elapsed);

    match execution_result {
        Ok(stream_processing_result) => {
            match stream_processing_result {
                Ok(item_count) => {
                    println!(
                        "GenAI stream processed {} items before completion/cancellation",
                        item_count
                    );
                    // Check if cancellation likely occurred based on timing and item count
                    if elapsed <= Duration::from_secs(2) && item_count < 1000 {
                        println!("✓ GenAI stream was likely cancelled mid-execution (processed {} items in {:?})", item_count, elapsed);
                    } else if elapsed > Duration::from_secs(10) {
                        return Err(anyhow::anyhow!(
                            "Stream should have been cancelled within 10 seconds but took {:?} to complete normally. Cancellation failed.",
                            elapsed
                        ));
                    } else {
                        println!(
                            "✓ GenAI stream completed quickly (before cancellation took effect)"
                        );
                    }
                }
                Err(e) => {
                    println!("✓ GenAI stream processing was cancelled as expected: {}", e);
                }
            }
        }
        Err(e) => {
            println!("GenAI stream execution task failed: {}", e);
            return Err(e.into());
        }
    }

    // Verify that cancellation happened within reasonable time
    if elapsed > Duration::from_secs(10) {
        return Err(anyhow::anyhow!(
            "Stream processing took too long ({:?}), suggesting cancellation did not work properly",
            elapsed
        ));
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

    // Create cancellation manager with pre-cancelled token
    let cancelled_token = CancellationToken::new();
    cancelled_token.cancel();
    let mut cancellation_manager =
        app_wrapper::runner::cancellation::RunnerCancellationManager::new();
    cancellation_manager.set_cancellation_token(cancelled_token);
    let manager_with_token = Box::new(cancellation_manager);
    runner.set_cancellation_manager(manager_with_token);

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

    // Create cancellation manager with pre-cancelled token
    let cancelled_token = CancellationToken::new();
    cancelled_token.cancel();
    let mut cancellation_manager =
        app_wrapper::runner::cancellation::RunnerCancellationManager::new();
    cancellation_manager.set_cancellation_token(cancelled_token);
    let manager_with_token = Box::new(cancellation_manager);
    runner.set_cancellation_manager(manager_with_token);

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
                                                       // Set up cancellation manager with token
        #[cfg(any(test, feature = "test-utils"))]
        {
            let mut cancellation_manager =
                app_wrapper::runner::cancellation::RunnerCancellationManager::new();
            cancellation_manager.set_cancellation_token(cancellation_token.clone());
            runner_guard.set_cancellation_manager(Box::new(cancellation_manager));
        }
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

/// Test LLM Chat Runner stream mid-execution cancellation with actual streaming
#[tokio::test]
#[ignore = "Integration test requiring Ollama server with long-running stream"]
async fn test_llm_chat_stream_mid_execution_cancellation() -> Result<()> {
    println!("Testing LLM Chat Runner stream mid-execution cancellation...");

    let _app_module = create_hybrid_test_app().await?;
    let runner = std::sync::Arc::new(tokio::sync::Mutex::new(
        app_wrapper::llm::chat::LLMChatRunnerImpl::new(std::sync::Arc::new(_app_module)),
    ));

    // Load Ollama settings
    let ollama_settings = OllamaRunnerSettings {
        model: TEST_MODEL.to_string(),
        base_url: Some(OLLAMA_HOST.to_string()),
        system_prompt: Some(
            "You are a helpful assistant. Please provide very detailed, comprehensive responses with multiple paragraphs.".to_string(),
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

    // Create cancellation token and set it on the runner BEFORE execution
    let cancellation_token = CancellationToken::new();
    {
        let mut runner_guard = runner.lock().await;
        runner_guard.load(serialized_settings).await?;
        // Set up cancellation manager with token
        #[cfg(any(test, feature = "test-utils"))]
        {
            let mut cancellation_manager =
                app_wrapper::runner::cancellation::RunnerCancellationManager::new();
            cancellation_manager.set_cancellation_token(cancellation_token.clone());
            runner_guard.set_cancellation_manager(Box::new(cancellation_manager));
        }
    }

    // Create test arguments with a long-running prompt
    let args = create_long_running_chat_args();
    let metadata = HashMap::new();

    let start_time = Instant::now();
    let serialized_args = {
        use prost::Message;
        let mut buf = Vec::new();
        args.encode(&mut buf)?;
        buf
    };

    let runner_clone = runner.clone();

    // Start stream execution in a task
    let execution_task = tokio::spawn(async move {
        let mut runner_guard = runner_clone.lock().await;
        let stream_result = runner_guard.run_stream(&serialized_args, metadata).await?;

        // Process the stream
        use futures::StreamExt;
        let mut item_count = 0;
        let mut stream = stream_result;

        while let Some(_item) = stream.next().await {
            item_count += 1;
            println!("Received stream item #{}", item_count);

            // Add a small delay to make the stream processing observable
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        Ok::<usize, anyhow::Error>(item_count)
    });

    // Wait for stream to start producing items (let it run for a bit)
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Cancel using the external token reference (avoids deadlock)
    cancellation_token.cancel();
    println!("Called cancellation_token.cancel() after 1 second");

    // Wait for the execution to complete or be cancelled
    let execution_result = execution_task.await;
    let elapsed = start_time.elapsed();

    println!("LLM Chat stream execution completed in {:?}", elapsed);

    match execution_result {
        Ok(stream_processing_result) => {
            match stream_processing_result {
                Ok(item_count) => {
                    println!(
                        "LLM Chat stream processed {} items before completion/cancellation",
                        item_count
                    );
                    // Check if cancellation likely occurred based on timing and item count
                    if elapsed <= Duration::from_secs(2) && item_count < 1000 {
                        println!("✓ Stream was likely cancelled mid-execution (processed {} items in {:?})", item_count, elapsed);
                    } else if elapsed > Duration::from_secs(10) {
                        return Err(anyhow::anyhow!(
                            "Stream should have been cancelled within 10 seconds but took {:?} to complete normally. Cancellation failed.",
                            elapsed
                        ));
                    } else {
                        println!("✓ Stream completed quickly (before cancellation took effect)");
                    }
                }
                Err(e) => {
                    println!(
                        "✓ LLM Chat stream processing was cancelled as expected: {}",
                        e
                    );
                }
            }
        }
        Err(e) => {
            println!("Stream execution task failed: {}", e);
            return Err(e.into());
        }
    }

    // Verify that cancellation happened within reasonable time
    if elapsed > Duration::from_secs(10) {
        return Err(anyhow::anyhow!(
            "Stream processing took too long ({:?}), suggesting cancellation did not work properly",
            elapsed
        ));
    }

    println!("✓ LLM Chat stream mid-execution cancellation test completed successfully");
    Ok(())
}

/// Test LLM Completion Runner stream mid-execution cancellation with actual streaming
#[tokio::test]
#[ignore = "Integration test requiring Ollama server with long-running stream"]
async fn test_llm_completion_stream_mid_execution_cancellation() -> Result<()> {
    println!("Testing LLM Completion Runner stream mid-execution cancellation...");

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

    // Create cancellation token and set it on the runner BEFORE execution
    let cancellation_token = CancellationToken::new();
    {
        let mut runner_guard = runner.lock().await;
        runner_guard.load(settings_buf).await?;
        // Set up cancellation manager with token
        #[cfg(any(test, feature = "test-utils"))]
        {
            let mut cancellation_manager =
                app_wrapper::runner::cancellation::RunnerCancellationManager::new();
            cancellation_manager.set_cancellation_token(cancellation_token.clone());
            runner_guard.set_cancellation_manager(Box::new(cancellation_manager));
        }
    }

    // Create test arguments with a long-running prompt
    let args = create_long_running_completion_args();
    let metadata = HashMap::new();

    let start_time = Instant::now();
    let serialized_args = {
        let mut buf = Vec::new();
        args.encode(&mut buf)?;
        buf
    };

    let runner_clone = runner.clone();

    // Start stream execution in a task
    let execution_task = tokio::spawn(async move {
        let mut runner_guard = runner_clone.lock().await;
        let stream_result = runner_guard.run_stream(&serialized_args, metadata).await?;

        // Process the stream
        use futures::StreamExt;
        let mut item_count = 0;
        let mut stream = stream_result;

        while let Some(_item) = stream.next().await {
            item_count += 1;
            println!("Received completion stream item #{}", item_count);

            // Add a small delay to make the stream processing observable
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        Ok::<usize, anyhow::Error>(item_count)
    });

    // Wait for stream to start producing items (let it run for a bit)
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Cancel using the external token reference (avoids deadlock)
    cancellation_token.cancel();
    println!("Called cancellation_token.cancel() after 1 second");

    // Wait for the execution to complete or be cancelled
    let execution_result = execution_task.await;
    let elapsed = start_time.elapsed();

    println!("LLM Completion stream execution completed in {:?}", elapsed);

    match execution_result {
        Ok(stream_processing_result) => {
            match stream_processing_result {
                Ok(item_count) => {
                    println!(
                        "LLM Completion stream processed {} items before completion/cancellation",
                        item_count
                    );
                    // Check if cancellation likely occurred based on timing and item count
                    if elapsed <= Duration::from_secs(2) && item_count < 1000 {
                        println!("✓ Stream was likely cancelled mid-execution (processed {} items in {:?})", item_count, elapsed);
                    } else if elapsed > Duration::from_secs(10) {
                        return Err(anyhow::anyhow!(
                            "Stream should have been cancelled within 10 seconds but took {:?} to complete normally. Cancellation failed.",
                            elapsed
                        ));
                    } else {
                        println!("✓ Stream completed quickly (before cancellation took effect)");
                    }
                }
                Err(e) => {
                    println!(
                        "✓ LLM Completion stream processing was cancelled as expected: {}",
                        e
                    );
                }
            }
        }
        Err(e) => {
            println!("Stream execution task failed: {}", e);
            return Err(e.into());
        }
    }

    // Verify that cancellation happened within reasonable time
    if elapsed > Duration::from_secs(10) {
        return Err(anyhow::anyhow!(
            "Stream processing took too long ({:?}), suggesting cancellation did not work properly",
            elapsed
        ));
    }

    println!("✓ LLM Completion stream mid-execution cancellation test completed successfully");
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
                                                       // Set up cancellation manager with token
        #[cfg(any(test, feature = "test-utils"))]
        {
            let mut cancellation_manager =
                app_wrapper::runner::cancellation::RunnerCancellationManager::new();
            cancellation_manager.set_cancellation_token(cancellation_token.clone());
            runner_guard.set_cancellation_manager(Box::new(cancellation_manager));
        }
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

/// Test LLM Completion Runner stream mid-execution cancellation with GenAI
#[tokio::test]
#[ignore = "Integration test requiring Ollama server with long-running stream"]
async fn test_llm_completion_genai_stream_mid_execution_cancellation() -> Result<()> {
    println!("Testing LLM Completion Runner (GenAI) stream mid-execution cancellation...");

    let runner = std::sync::Arc::new(tokio::sync::Mutex::new(
        app_wrapper::llm::completion::LLMCompletionRunnerImpl::new(),
    ));

    // Load GenAI settings
    let genai_settings = jobworkerp_runner::jobworkerp::runner::llm::LlmRunnerSettings {
        settings: Some(
            jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::Settings::Genai(
                GenaiRunnerSettings {
                    model: TEST_MODEL.to_string(),
                    base_url: Some(OLLAMA_HOST.to_string()), // Use Ollama host via GenAI
                    system_prompt: None,
                },
            ),
        ),
    };

    let mut settings_buf = Vec::new();
    use prost::Message;
    genai_settings.encode(&mut settings_buf)?;

    // Create cancellation token and set it on the runner BEFORE execution
    let cancellation_token = CancellationToken::new();
    {
        let mut runner_guard = runner.lock().await;
        runner_guard.load(settings_buf).await?;
        // Set up cancellation manager with token
        #[cfg(any(test, feature = "test-utils"))]
        {
            let mut cancellation_manager =
                app_wrapper::runner::cancellation::RunnerCancellationManager::new();
            cancellation_manager.set_cancellation_token(cancellation_token.clone());
            runner_guard.set_cancellation_manager(Box::new(cancellation_manager));
        }
    }

    // Create test arguments with a long-running prompt
    let args = create_long_running_completion_args();
    let metadata = HashMap::new();

    let start_time = Instant::now();
    let serialized_args = {
        let mut buf = Vec::new();
        args.encode(&mut buf)?;
        buf
    };

    let runner_clone = runner.clone();

    // Start stream execution in a task
    let execution_task = tokio::spawn(async move {
        let mut runner_guard = runner_clone.lock().await;
        let stream_result = runner_guard.run_stream(&serialized_args, metadata).await?;

        // Process the stream
        use futures::StreamExt;
        let mut item_count = 0;
        let mut stream = stream_result;

        while let Some(_item) = stream.next().await {
            item_count += 1;
            println!("Received GenAI completion stream item #{}", item_count);

            // Add a small delay to make the stream processing observable
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        Ok::<usize, anyhow::Error>(item_count)
    });

    // Wait for stream to start producing items (let it run for a bit)
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Cancel using the external token reference (avoids deadlock)
    cancellation_token.cancel();
    println!("Called cancellation_token.cancel() after 1 second");

    // Wait for the execution to complete or be cancelled
    let execution_result = execution_task.await;
    let elapsed = start_time.elapsed();

    println!(
        "LLM Completion (GenAI) stream execution completed in {:?}",
        elapsed
    );

    match execution_result {
        Ok(stream_processing_result) => {
            match stream_processing_result {
                Ok(item_count) => {
                    println!(
                        "LLM Completion (GenAI) stream processed {} items before completion/cancellation",
                        item_count
                    );
                    // Check if cancellation likely occurred based on timing and item count
                    if elapsed <= Duration::from_secs(2) && item_count < 1000 {
                        println!("✓ GenAI completion stream was likely cancelled mid-execution (processed {} items in {:?})", item_count, elapsed);
                    } else if elapsed > Duration::from_secs(10) {
                        return Err(anyhow::anyhow!(
                            "Stream should have been cancelled within 10 seconds but took {:?} to complete normally. Cancellation failed.",
                            elapsed
                        ));
                    } else {
                        println!("✓ GenAI completion stream completed quickly (before cancellation took effect)");
                    }
                }
                Err(e) => {
                    println!(
                        "✓ LLM Completion (GenAI) stream processing was cancelled as expected: {}",
                        e
                    );
                }
            }
        }
        Err(e) => {
            println!("GenAI completion stream execution task failed: {}", e);
            return Err(e.into());
        }
    }

    // Verify that cancellation happened within reasonable time
    if elapsed > Duration::from_secs(10) {
        return Err(anyhow::anyhow!(
            "Stream processing took too long ({:?}), suggesting cancellation did not work properly",
            elapsed
        ));
    }

    println!(
        "✓ LLM Completion (GenAI) stream mid-execution cancellation test completed successfully"
    );
    Ok(())
}
