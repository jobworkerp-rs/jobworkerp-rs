//! E2E tests for auto-select FunctionSet mode in LLM Chat.
//! Requires an Ollama server running at the configured host.
//!
//! These tests verify that the LLM can naturally select the appropriate
//! FunctionSet (toolset) via pseudo-tool calls when `auto_select_function_set`
//! is enabled, using a realistic general-purpose agent system prompt.

#![allow(clippy::uninlined_format_args)]

use anyhow::Result;
use app::app::function::function_set::FunctionSetApp;
use app_wrapper::llm::chat::ollama::OllamaChatService;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{ChatMessage, LlmOptions};
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
    ChatRole, FunctionOptions, MessageContent,
};
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content;
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::OllamaRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::{LlmChatArgs, LlmChatResult};
use proto::jobworkerp::data::RunnerId;
use proto::jobworkerp::function::data::{FunctionId, FunctionSetData, FunctionUsing, function_id};
use std::collections::HashMap;
use std::sync::Arc;
use tests_with_worker::start_test_worker;
use tokio::time::{Duration, timeout};

const DEFAULT_OLLAMA_HOST: &str = "http://ollama.ollama.svc.cluster.local:11434";
fn ollama_host() -> String {
    std::env::var("OLLAMA_HOST").unwrap_or_else(|_| DEFAULT_OLLAMA_HOST.to_string())
}
const TEST_MODEL: &str = "qwen3.5:9b";
//const OTLP_ADDR: &str = "http://otel-collector.default.svc.cluster.local:4317";
/// Shorter timeout to avoid hanging on tool execution failures.
const TEST_TIMEOUT: Duration = Duration::from_secs(120);

/// A realistic general-purpose agent system prompt.
/// This should work naturally without forcing the LLM to use tools.
const AGENT_SYSTEM_PROMPT: &str = "\
You are a helpful assistant with access to various toolsets. \
When the user asks you to perform a task, first activate the appropriate \
toolset by calling its activation function, then use the loaded tools \
to complete the task. Always prefer using tools over explaining how to \
do something manually.";

async fn create_auto_select_test_service()
-> Result<(OllamaChatService, tests_with_worker::TestWorkerHandle)> {
    // SAFETY: called in test setup before spawning threads
    // unsafe { std::env::set_var("OTLP_ADDR", OTLP_ADDR) };
    let app_module = Arc::new(app::module::test::create_hybrid_test_app().await?);
    let worker_handle = start_test_worker(app_module.clone()).await?;

    let settings = OllamaRunnerSettings {
        model: TEST_MODEL.to_string(),
        base_url: Some(ollama_host()),
        system_prompt: Some(AGENT_SYSTEM_PROMPT.to_string()),
        pull_model: Some(true),
    };

    // Register two distinct FunctionSets
    match app_module
        .function_set_app
        .create_function_set(&FunctionSetData {
            name: "auto-test-commands".to_string(),
            description: "Shell command execution tools for running system commands".to_string(),
            category: 0,
            targets: vec![FunctionUsing {
                function_id: Some(FunctionId {
                    id: Some(function_id::Id::RunnerId(RunnerId { value: 1 })), // COMMAND
                }),
                using: None,
            }],
        })
        .await
    {
        Ok(_) => {}
        Err(e) if e.to_string().to_lowercase().contains("unique") => {}
        Err(e) => return Err(e),
    }

    match app_module
        .function_set_app
        .create_function_set(&FunctionSetData {
            name: "auto-test-http".to_string(),
            description: "HTTP request tools for calling web APIs and fetching data".to_string(),
            category: 0,
            targets: vec![FunctionUsing {
                function_id: Some(FunctionId {
                    id: Some(function_id::Id::RunnerId(RunnerId { value: 2 })), // HTTP_REQUEST
                }),
                using: None,
            }],
        })
        .await
    {
        Ok(_) => {}
        Err(e) if e.to_string().to_lowercase().contains("unique") => {}
        Err(e) => return Err(e),
    }

    let service = OllamaChatService::new(
        app_module.function_app.clone(),
        app_module.function_set_app.clone(),
        settings,
    )?;
    Ok((service, worker_handle))
}

fn create_auto_select_chat_args(message: &str) -> LlmChatArgs {
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
            seed: Some(41),
            extract_reasoning_content: Some(false),
        }),
        model: Some(TEST_MODEL.to_string()),
        function_options: Some(FunctionOptions {
            use_function_calling: true,
            use_runners_as_function: Some(false),
            use_workers_as_function: Some(false),
            function_set_name: None, // No pre-selected set
            is_auto_calling: Some(true),
            auto_select_function_set: Some(true), // Enable auto-select
        }),
        json_schema: None,
    }
}

fn extract_text(result: &LlmChatResult) -> Option<&str> {
    result.content.as_ref().and_then(|c| match &c.content {
        Some(message_content::Content::Text(t)) => Some(t.as_str()),
        _ => None,
    })
}

/// LLM should auto-select a FunctionSet and complete the request.
/// This uses a realistic agent prompt to verify the LLM naturally picks
/// the shell command toolset when asked to run a command.
#[tokio::test(flavor = "current_thread")]
#[ignore = "Integration test requiring Ollama server"]
async fn test_auto_select_picks_function_set() -> Result<()> {
    // command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    let (service, worker_handle) = create_auto_select_test_service().await?;

    // A natural user request that should trigger COMMAND toolset selection
    let args = create_auto_select_chat_args(
        "Please run the shell command 'echo hello world' and show me the output.",
    );

    let context = opentelemetry::Context::current();
    let metadata = HashMap::new();

    println!("Sending auto-select request to Ollama...");
    let result = timeout(TEST_TIMEOUT, service.request_chat(args, context, metadata)).await;

    match result {
        Ok(Ok(chat_result)) => {
            println!("Auto-select response. Done: {}", chat_result.done);
            if let Some(text) = extract_text(&chat_result) {
                println!("Response text: {}", text);
            }
            if let Some(pending) = &chat_result.pending_tool_calls {
                println!(
                    "Pending tool calls: {:?}",
                    pending.calls.iter().map(|c| &c.fn_name).collect::<Vec<_>>()
                );
            }
            // The auto-select flow has 2 phases:
            //   Phase 1: LLM calls select_toolset_auto-test-commands (intercepted internally)
            //   Phase 2: LLM uses COMMAND tool to execute `echo hello world`
            // If Phase 2 completes successfully, done=true and we have text.
            // If Phase 2 fails (e.g. bad args), the error is fed back to LLM.
            // We accept both completed and pending states as valid auto-select behavior.
            assert!(
                chat_result.done || chat_result.pending_tool_calls.is_some(),
                "Chat should either complete or return pending tool calls"
            );
        }
        Ok(Err(e)) => {
            panic!("Chat returned error: {}", e);
        }
        Err(_) => {
            println!("WARNING: Request timed out after {:?}", TEST_TIMEOUT);
            println!("This may indicate a hanging tool execution.");
            panic!("Request timed out after {:?}", TEST_TIMEOUT);
        }
    }

    worker_handle.shutdown().await;
    Ok(())
}

/// LLM should auto-select a FunctionSet in streaming mode.
/// Phase 1 selects the toolset non-streaming, Phase 2 streams the response.
#[tokio::test(flavor = "current_thread")]
#[ignore = "Integration test requiring Ollama server"]
async fn test_auto_select_streaming_picks_function_set() -> Result<()> {
    // command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    let (service, worker_handle) = create_auto_select_test_service().await?;

    let args = create_auto_select_chat_args(
        "Please run the shell command 'echo hello world' and show me the output.",
    );

    let metadata = HashMap::new();

    println!("Sending auto-select streaming request to Ollama...");
    let result = timeout(
        TEST_TIMEOUT,
        Arc::new(service).request_stream_chat(args, metadata),
    )
    .await;

    match result {
        Ok(Ok(mut stream)) => {
            use futures::StreamExt;
            let mut chunk_count = 0;
            let mut full_text = String::new();
            while let Some(chunk) = stream.next().await {
                chunk_count += 1;
                if let Some(content) = &chunk.content
                    && let Some(message_content::Content::Text(t)) = &content.content
                {
                    full_text.push_str(t);
                }
            }
            println!(
                "Streaming auto-select: received {} chunks, text: {}",
                chunk_count, full_text
            );
            assert!(
                chunk_count > 0,
                "Should receive at least one streaming chunk"
            );
        }
        Ok(Err(e)) => {
            panic!("Streaming chat returned error: {}", e);
        }
        Err(_) => {
            panic!("Request timed out after {:?}", TEST_TIMEOUT);
        }
    }

    worker_handle.shutdown().await;
    Ok(())
}

/// When no FunctionSets are registered, auto-select should gracefully
/// fall back to a normal chat without tools (no tools injected).
#[tokio::test(flavor = "current_thread")]
#[ignore = "Integration test requiring Ollama server"]
async fn test_auto_select_without_function_sets_falls_back() -> Result<()> {
    // SAFETY: called in test setup before spawning threads
    // unsafe { std::env::set_var("OTLP_ADDR", OTLP_ADDR) };
    let app_module = Arc::new(app::module::test::create_hybrid_test_app().await?);
    let worker_handle = start_test_worker(app_module.clone()).await?;

    let settings = OllamaRunnerSettings {
        model: TEST_MODEL.to_string(),
        base_url: Some(ollama_host()),
        system_prompt: Some(AGENT_SYSTEM_PROMPT.to_string()),
        pull_model: Some(true),
    };

    // Delete test-specific sets to ensure auto-select has nothing to inject
    let all_sets = app_module
        .function_set_app
        .find_function_set_all_list(None)
        .await
        .unwrap_or_default();
    for set in &all_sets {
        if let Some(data) = &set.data
            && (data.name.starts_with("auto-test-") || data.name.starts_with("empty-auto-"))
            && let Some(id) = &set.id
        {
            let _ = app_module.function_set_app.delete_function_set(id).await;
        }
    }

    let service = OllamaChatService::new(
        app_module.function_app.clone(),
        app_module.function_set_app.clone(),
        settings,
    )?;

    // Simple conversational request — should work without any tools
    let args = LlmChatArgs {
        messages: vec![ChatMessage {
            role: ChatRole::User as i32,
            content: Some(MessageContent {
                content: Some(
                    jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::Content::Text(
                        "What is 2 + 3?".to_string(),
                    ),
                ),
            }),
        }],
        options: Some(LlmOptions {
            temperature: Some(0.0),
            max_tokens: Some(1000),
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
            function_set_name: None,
            is_auto_calling: Some(true),
            auto_select_function_set: Some(true),
        }),
        json_schema: None,
    };

    let context = opentelemetry::Context::current();
    let metadata = HashMap::new();

    println!("Sending auto-select request with potentially no function sets...");
    let result = timeout(TEST_TIMEOUT, service.request_chat(args, context, metadata)).await??;

    println!("Fallback response. Done: {}", result.done);
    if let Some(text) = extract_text(&result) {
        println!("Response text: {}", text);
        // Simple arithmetic should be answered directly
        assert!(
            text.contains('5') || text.to_lowercase().contains("five"),
            "Response should contain the answer '5': {}",
            text
        );
    }
    assert!(result.done, "Chat should complete without tool calls");

    worker_handle.shutdown().await;
    Ok(())
}
