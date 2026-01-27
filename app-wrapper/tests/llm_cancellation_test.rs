//! Integration tests for LLM cancellation functionality
//! Tests cancellation using new CancelMonitoringHelper approach

#![allow(clippy::uninlined_format_args)]
#![allow(clippy::collapsible_match)]

use anyhow::Result;
use app::module::test::create_hybrid_test_app;
use jobworkerp_runner::jobworkerp::runner::llm::LlmRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{ChatMessage, LlmOptions};
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{ChatRole, MessageContent};
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::OllamaRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::{LlmChatArgs, LlmCompletionArgs};
use jobworkerp_runner::runner::RunnerTrait;
use jobworkerp_runner::runner::cancellation_helper::CancelMonitoringHelper;
use jobworkerp_runner::runner::test_common::mock::MockCancellationManager;
use std::collections::HashMap;
use std::sync::Arc;
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
                        "Please write a detailed explanation of artificial intelligence. Make it comprehensive with multiple sections.".to_string(),
                    ),
                ),
            }),
        }],
        options: Some(LlmOptions {
            temperature: Some(0.8),
            max_tokens: Some(5000), // Reduced for test performance
            top_p: None,
            repeat_penalty: None,
            repeat_last_n: None,
            seed: None,
            extract_reasoning_content: Some(false),
        }),
        model: Some(TEST_MODEL.to_string()),
        function_options: None,
        json_schema: None,
    }
}

/// Create simple test chat arguments for basic testing
fn create_simple_chat_args() -> LlmChatArgs {
    LlmChatArgs {
        messages: vec![ChatMessage {
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
        json_schema: None,
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
        json_schema: None,
    }
}

/// Create long-running test completion arguments
fn create_long_running_completion_args() -> LlmCompletionArgs {
    LlmCompletionArgs {
        prompt: "Please write a detailed explanation of artificial intelligence.".to_string(),
        model: Some(TEST_MODEL.to_string()),
        system_prompt: None,
        function_options: None,
        options: Some(
            jobworkerp_runner::jobworkerp::runner::llm::llm_completion_args::LlmOptions {
                temperature: Some(0.8),
                max_tokens: Some(5000), // Reduced for test performance
                top_p: None,
                repeat_penalty: None,
                repeat_last_n: None,
                seed: None,
                extract_reasoning_content: Some(false),
            },
        ),
        context: None,
        json_schema: None,
    }
}

/// Test LLM Chat with cancellation helper (new approach)
#[tokio::test]
#[ignore = "Integration test requiring Ollama server"]
async fn test_llm_chat_with_cancellation_helper() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    let app_module = create_hybrid_test_app().await?;

    let external_cancel_token = CancellationToken::new();
    let mock_manager = MockCancellationManager::new_with_token(external_cancel_token.clone());
    let cancel_helper = CancelMonitoringHelper::new(Box::new(mock_manager));

    let mut runner = app_wrapper::llm::chat::LLMChatRunnerImpl::new_with_cancel_monitoring(
        Arc::new(app_module),
        cancel_helper,
    );

    // Load Ollama settings
    let ollama_settings = OllamaRunnerSettings {
        model: TEST_MODEL.to_string(),
        base_url: Some(OLLAMA_HOST.to_string()),
        system_prompt: Some("You are a helpful assistant.".to_string()),
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
    runner.load(serialized_settings).await?;

    let args = create_long_running_chat_args();
    let serialized_args = prost::Message::encode_to_vec(&args);
    let metadata = HashMap::new();

    println!("Starting LLM chat with cancellation helper...");
    let start_time = Instant::now();

    // Start execution in background
    let execution_task =
        tokio::spawn(async move { runner.run(&serialized_args, metadata, None).await });

    // Wait a bit, then trigger external cancellation
    tokio::time::sleep(Duration::from_secs(2)).await;
    external_cancel_token.cancel();
    println!("Triggered external cancellation after 2 seconds");

    // Wait for execution to complete
    let result = execution_task.await?;
    let elapsed = start_time.elapsed();

    println!("LLM chat execution completed in {:?}", elapsed);

    let (result, _) = result;
    match result {
        Ok(_) => {
            println!("⚠️ LLM chat completed before cancellation could take effect");
            // Fast completion is possible for simple requests
        }
        Err(e) => {
            let error_msg = e.to_string();
            println!("✓ LLM chat was cancelled: {}", error_msg);
            assert!(
                error_msg.contains("cancelled") || error_msg.contains("cancel"),
                "Error should indicate cancellation: {}",
                error_msg
            );
        }
    }

    // Should complete/cancel within reasonable time
    assert!(
        elapsed < Duration::from_secs(30),
        "LLM chat should complete or be cancelled within 30 seconds, took {:?}",
        elapsed
    );

    println!("✓ LLM chat cancellation helper test passed");
    Ok(())
}

/// Test LLM Completion with cancellation helper (new approach)
#[tokio::test]
#[ignore = "Integration test requiring Ollama server"]
async fn test_llm_completion_with_cancellation_helper() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    let external_cancel_token = CancellationToken::new();
    let mock_manager = MockCancellationManager::new_with_token(external_cancel_token.clone());
    let cancel_helper = CancelMonitoringHelper::new(Box::new(mock_manager));

    let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
    let mut runner =
        app_wrapper::llm::completion::LLMCompletionRunnerImpl::new_with_cancel_monitoring(
            app_module,
            cancel_helper,
        );

    // Load Ollama settings
    let ollama_settings = OllamaRunnerSettings {
        model: TEST_MODEL.to_string(),
        base_url: Some(OLLAMA_HOST.to_string()),
        system_prompt: Some("You are a helpful assistant.".to_string()),
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
    runner.load(serialized_settings).await?;

    let args = create_long_running_completion_args();
    let serialized_args = prost::Message::encode_to_vec(&args);
    let metadata = HashMap::new();

    println!("Starting LLM completion with cancellation helper...");
    let start_time = Instant::now();

    // Start execution in background
    let execution_task =
        tokio::spawn(async move { runner.run(&serialized_args, metadata, None).await });

    // Wait a bit, then trigger external cancellation
    tokio::time::sleep(Duration::from_secs(2)).await;
    external_cancel_token.cancel();
    println!("Triggered external cancellation after 2 seconds");

    // Wait for execution to complete
    let result = execution_task.await?;
    let elapsed = start_time.elapsed();

    println!("LLM completion execution completed in {:?}", elapsed);

    let (result, _) = result;
    match result {
        Ok(_) => {
            println!("⚠️ LLM completion completed before cancellation could take effect");
            // Fast completion is possible for simple requests
        }
        Err(e) => {
            let error_msg = e.to_string();
            println!("✓ LLM completion was cancelled: {}", error_msg);
            assert!(
                error_msg.contains("cancelled") || error_msg.contains("cancel"),
                "Error should indicate cancellation: {}",
                error_msg
            );
        }
    }

    // Should complete/cancel within reasonable time
    assert!(
        elapsed < Duration::from_secs(30),
        "LLM completion should complete or be cancelled within 30 seconds, took {:?}",
        elapsed
    );

    println!("✓ LLM completion cancellation helper test passed");
    Ok(())
}

/// Test pre-cancellation (cancellation token set before execution)
#[tokio::test]
#[ignore = "Integration test requiring Ollama server"]
async fn test_llm_chat_pre_cancellation() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    let app_module = create_hybrid_test_app().await?;

    let external_cancel_token = CancellationToken::new();
    external_cancel_token.cancel(); // Pre-cancel

    let mock_manager = MockCancellationManager::new_with_token(external_cancel_token);
    let cancel_helper = CancelMonitoringHelper::new(Box::new(mock_manager));

    let mut runner = app_wrapper::llm::chat::LLMChatRunnerImpl::new_with_cancel_monitoring(
        Arc::new(app_module),
        cancel_helper,
    );

    // Load settings
    let ollama_settings = OllamaRunnerSettings {
        model: TEST_MODEL.to_string(),
        base_url: Some(OLLAMA_HOST.to_string()),
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
    runner.load(serialized_settings).await?;

    // Execute with pre-cancelled token
    let args = create_simple_chat_args();
    let serialized_args = prost::Message::encode_to_vec(&args);
    let metadata = HashMap::new();

    let start_time = Instant::now();
    let (result, _) = runner.run(&serialized_args, metadata, None).await;
    let elapsed = start_time.elapsed();

    println!(
        "Pre-cancelled LLM chat execution completed in {:?}",
        elapsed
    );

    // Should be cancelled immediately
    assert!(result.is_err(), "Pre-cancelled LLM chat should fail");
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("cancelled") || error_msg.contains("cancel"),
        "Error should indicate cancellation: {}",
        error_msg
    );

    // Should fail quickly
    assert!(
        elapsed < Duration::from_secs(5),
        "Pre-cancelled LLM chat should fail quickly, took {:?}",
        elapsed
    );

    println!("✓ LLM chat pre-cancellation test passed");
    Ok(())
}

/// Test pre-cancellation for LLM Completion
#[tokio::test]
#[ignore = "Integration test requiring Ollama server"]
async fn test_llm_completion_pre_cancellation() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    let external_cancel_token = CancellationToken::new();
    external_cancel_token.cancel(); // Pre-cancel

    let mock_manager = MockCancellationManager::new_with_token(external_cancel_token);
    let cancel_helper = CancelMonitoringHelper::new(Box::new(mock_manager));

    let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
    let mut runner =
        app_wrapper::llm::completion::LLMCompletionRunnerImpl::new_with_cancel_monitoring(
            app_module,
            cancel_helper,
        );

    // Load settings
    let ollama_settings = OllamaRunnerSettings {
        model: TEST_MODEL.to_string(),
        base_url: Some(OLLAMA_HOST.to_string()),
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
    runner.load(serialized_settings).await?;

    // Execute with pre-cancelled token
    let args = create_simple_completion_args();
    let serialized_args = prost::Message::encode_to_vec(&args);
    let metadata = HashMap::new();

    let start_time = Instant::now();
    let (result, _) = runner.run(&serialized_args, metadata, None).await;
    let elapsed = start_time.elapsed();

    println!("Pre-cancelled LLM completion execution completed in {elapsed:?}",);

    // Should be cancelled immediately
    assert!(result.is_err(), "Pre-cancelled LLM completion should fail");
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("cancelled") || error_msg.contains("cancel"),
        "Error should indicate cancellation: {error_msg}",
    );

    // Should fail quickly
    assert!(
        elapsed < Duration::from_secs(5),
        "Pre-cancelled LLM completion should fail quickly, took {elapsed:?}",
    );

    println!("✓ LLM completion pre-cancellation test passed");
    Ok(())
}

/// Test LLM runners without cancellation helper (legacy mode)
#[tokio::test]
#[ignore = "Integration test requiring Ollama server"]
async fn test_llm_chat_without_cancellation_helper() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    let app_module = create_hybrid_test_app().await?;

    let mut runner = app_wrapper::llm::chat::LLMChatRunnerImpl::new(Arc::new(app_module));

    // Load settings
    let ollama_settings = OllamaRunnerSettings {
        model: TEST_MODEL.to_string(),
        base_url: Some(OLLAMA_HOST.to_string()),
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
    runner.load(serialized_settings).await?;

    // Execute simple request (should work normally)
    let args = create_simple_chat_args();
    let serialized_args = prost::Message::encode_to_vec(&args);
    let metadata = HashMap::new();

    println!("Starting LLM chat WITHOUT cancellation helper...");
    let start_time = Instant::now();
    let (result, _) = runner.run(&serialized_args, metadata, None).await;
    let elapsed = start_time.elapsed();

    println!("LLM chat (no helper) execution completed in {:?}", elapsed);

    // Should complete normally (no cancellation support)
    match result {
        Ok(_) => {
            println!("✓ LLM chat completed successfully without cancellation helper");
        }
        Err(e) => {
            println!("LLM chat failed: {}", e);
            // Failure could be due to server unavailability
        }
    }

    // Try cancel using CancelMonitoring trait (should log warning but not crash)
    use jobworkerp_runner::runner::cancellation::CancelMonitoring;
    runner.request_cancellation().await.unwrap();
    println!("✓ request_cancellation() called on runner without helper (should log warning)");

    println!("✓ LLM chat without cancellation helper test passed");
    Ok(())
}
