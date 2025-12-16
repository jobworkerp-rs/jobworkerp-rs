//! Integration tests for LLM Unified Runner (multi-method support)
//! This test requires an Ollama server running at http://ollama.ollama.svc.cluster.local:11434

#![allow(clippy::uninlined_format_args)]

use anyhow::Result;
use app::module::test::create_hybrid_test_app;
use app_wrapper::llm::unified::LLMUnifiedRunnerImpl;
use futures::StreamExt;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
    message_content, ChatMessage, ChatRole, LlmOptions as ChatLlmOptions, MessageContent,
};
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result;
use jobworkerp_runner::jobworkerp::runner::llm::llm_completion_args::LlmOptions as CompletionLlmOptions;
use jobworkerp_runner::jobworkerp::runner::llm::llm_completion_result;
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::OllamaRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::{
    LlmChatArgs, LlmChatResult, LlmCompletionArgs, LlmCompletionResult, LlmRunnerSettings,
};
use jobworkerp_runner::runner::RunnerTrait;
use prost::Message;
use proto::jobworkerp::data::result_output_item;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::time::{timeout, Duration};

/// Test configuration
const OLLAMA_HOST: &str = "http://ollama.ollama.svc.cluster.local:11434";
const TEST_MODEL: &str = "qwen3:30b";
// const TEST_MODEL: &str = "gpt-oss:20b";
const TEST_TIMEOUT: Duration = Duration::from_secs(300);

/// Create test LLM unified runner
async fn create_test_unified_runner() -> Result<LLMUnifiedRunnerImpl> {
    let app_module = Arc::new(create_hybrid_test_app().await?);
    Ok(LLMUnifiedRunnerImpl::new(app_module))
}

/// Create runner settings for Ollama
fn create_ollama_settings() -> Vec<u8> {
    let settings = LlmRunnerSettings {
        settings: Some(
            jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::Settings::Ollama(
                OllamaRunnerSettings {
                    model: TEST_MODEL.to_string(),
                    base_url: Some(OLLAMA_HOST.to_string()),
                    system_prompt: Some(
                        "You are a helpful assistant. Respond concisely.".to_string(),
                    ),
                    pull_model: Some(false),
                },
            ),
        ),
    };
    settings.encode_to_vec()
}

#[ignore = "need to run with ollama server"]
#[tokio::test]
async fn test_unified_runner_completion_method() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    let mut runner = create_test_unified_runner().await?;
    runner.load(create_ollama_settings()).await?;

    let args = LlmCompletionArgs {
        prompt: "What is 2+2? Answer with just the number:".to_string(),
        options: Some(CompletionLlmOptions {
            max_tokens: Some(64),
            temperature: Some(0.1),
            ..Default::default()
        }),
        ..Default::default()
    };

    let args_bytes = args.encode_to_vec();
    let (result, _metadata) = timeout(
        TEST_TIMEOUT,
        runner.run(&args_bytes, HashMap::new(), Some("completion")),
    )
    .await?;

    let output = result?;
    let response = LlmCompletionResult::decode(&output[..])?;

    assert!(response.content.is_some());
    if let Some(content) = response.content {
        if let Some(llm_completion_result::message_content::Content::Text(text)) = content.content {
            tracing::info!("Completion response: {}", text);
            assert!(!text.is_empty());
        }
    }

    tracing::info!("Unified runner completion method test passed!");
    Ok(())
}

#[ignore = "need to run with ollama server"]
#[tokio::test]
async fn test_unified_runner_chat_method() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    let mut runner = create_test_unified_runner().await?;
    runner.load(create_ollama_settings()).await?;

    let args = LlmChatArgs {
        messages: vec![ChatMessage {
            role: ChatRole::User.into(),
            content: Some(MessageContent {
                content: Some(message_content::Content::Text(
                    "What is the capital of Japan? Answer in one word.".to_string(),
                )),
            }),
        }],
        options: Some(ChatLlmOptions {
            max_tokens: Some(64),
            temperature: Some(0.1),
            ..Default::default()
        }),
        ..Default::default()
    };

    let args_bytes = args.encode_to_vec();
    let (result, _metadata) = timeout(
        TEST_TIMEOUT,
        runner.run(&args_bytes, HashMap::new(), Some("chat")),
    )
    .await?;

    let output = result?;
    let response = LlmChatResult::decode(&output[..])?;

    assert!(response.content.is_some());
    if let Some(content) = response.content {
        if let Some(llm_chat_result::message_content::Content::Text(text)) = content.content {
            tracing::info!("Chat response: {}", text);
            assert!(!text.is_empty());
        }
    }

    tracing::info!("Unified runner chat method test passed!");
    Ok(())
}

#[ignore = "need to run with ollama server"]
#[tokio::test]
async fn test_unified_runner_completion_stream() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    let mut runner = create_test_unified_runner().await?;
    runner.load(create_ollama_settings()).await?;

    let args = LlmCompletionArgs {
        prompt: "Count from 1 to 5, one number per line:".to_string(),
        options: Some(CompletionLlmOptions {
            max_tokens: Some(128),
            temperature: Some(0.1),
            ..Default::default()
        }),
        ..Default::default()
    };

    let args_bytes = args.encode_to_vec();
    let stream = timeout(
        TEST_TIMEOUT,
        runner.run_stream(&args_bytes, HashMap::new(), Some("completion")),
    )
    .await??;

    let mut chunks = Vec::new();
    let mut combined_text = String::new();
    let mut stream = stream;

    while let Some(item) = stream.next().await {
        match item.item {
            Some(result_output_item::Item::Data(data)) => {
                // Decode each chunk as LlmCompletionResult
                if let Ok(result) = LlmCompletionResult::decode(&data[..]) {
                    if let Some(content) = result.content {
                        if let Some(llm_completion_result::message_content::Content::Text(text)) =
                            content.content
                        {
                            tracing::debug!("Stream chunk: {}", text);
                            combined_text.push_str(&text);
                        }
                    }
                }
                chunks.push(data);
            }
            Some(result_output_item::Item::End(_)) => break,
            _ => {}
        }
    }

    assert!(!chunks.is_empty(), "No streaming chunks received");
    assert!(!combined_text.is_empty(), "Combined text is empty");
    tracing::info!(
        "Received {} streaming chunks for completion, combined text: {}",
        chunks.len(),
        combined_text
    );

    tracing::info!("Unified runner completion stream test passed!");
    Ok(())
}

#[ignore = "need to run with ollama server"]
#[tokio::test]
async fn test_unified_runner_chat_stream() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    let mut runner = create_test_unified_runner().await?;
    runner.load(create_ollama_settings()).await?;

    let args = LlmChatArgs {
        messages: vec![ChatMessage {
            role: ChatRole::User.into(),
            content: Some(MessageContent {
                content: Some(message_content::Content::Text(
                    "List 3 colors, one per line.".to_string(),
                )),
            }),
        }],
        options: Some(ChatLlmOptions {
            max_tokens: Some(128),
            temperature: Some(0.1),
            ..Default::default()
        }),
        ..Default::default()
    };

    let args_bytes = args.encode_to_vec();
    let stream = timeout(
        TEST_TIMEOUT,
        runner.run_stream(&args_bytes, HashMap::new(), Some("chat")),
    )
    .await??;

    let mut chunks = Vec::new();
    let mut combined_text = String::new();
    let mut stream = stream;

    while let Some(item) = stream.next().await {
        match item.item {
            Some(result_output_item::Item::Data(data)) => {
                // Decode each chunk as LlmChatResult
                if let Ok(result) = LlmChatResult::decode(&data[..]) {
                    if let Some(content) = result.content {
                        if let Some(llm_chat_result::message_content::Content::Text(text)) =
                            content.content
                        {
                            tracing::debug!("Stream chunk: {}", text);
                            combined_text.push_str(&text);
                        }
                    }
                }
                chunks.push(data);
            }
            Some(result_output_item::Item::End(_)) => break,
            _ => {}
        }
    }

    assert!(!chunks.is_empty(), "No streaming chunks received");
    assert!(!combined_text.is_empty(), "Combined text is empty");
    tracing::info!(
        "Received {} streaming chunks for chat, combined text: {}",
        chunks.len(),
        combined_text
    );

    tracing::info!("Unified runner chat stream test passed!");
    Ok(())
}

#[ignore = "need to run with ollama server"]
#[tokio::test]
async fn test_unified_runner_missing_method_error() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    let mut runner = create_test_unified_runner().await?;
    runner.load(create_ollama_settings()).await?;

    let args = LlmCompletionArgs {
        prompt: "Test".to_string(),
        ..Default::default()
    };

    let args_bytes = args.encode_to_vec();

    // Call without 'using' parameter - should fail for LLM runner
    let (result, _metadata) = runner.run(&args_bytes, HashMap::new(), None).await;

    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Method specification required"),
        "Expected 'Method specification required' error, got: {}",
        err_msg
    );

    tracing::info!("Missing method error test passed!");
    Ok(())
}

#[ignore = "need to run with ollama server"]
#[tokio::test]
async fn test_unified_runner_unknown_method_error() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    let mut runner = create_test_unified_runner().await?;
    runner.load(create_ollama_settings()).await?;

    let args = LlmCompletionArgs {
        prompt: "Test".to_string(),
        ..Default::default()
    };

    let args_bytes = args.encode_to_vec();

    // Call with unknown method
    let (result, _metadata) = runner
        .run(&args_bytes, HashMap::new(), Some("unknown_method"))
        .await;

    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Unknown method"),
        "Expected 'Unknown method' error, got: {}",
        err_msg
    );

    tracing::info!("Unknown method error test passed!");
    Ok(())
}

#[ignore = "need to run with ollama server"]
#[tokio::test]
async fn test_unified_runner_chat_with_json_schema() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    let mut runner = create_test_unified_runner().await?;
    runner.load(create_ollama_settings()).await?;

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
        json_schema: Some(schema.to_string()),
        messages: vec![ChatMessage {
            role: ChatRole::User.into(),
            content: Some(MessageContent {
                content: Some(message_content::Content::Text(
                    "What is 2+2? Respond with your answer and confidence level.".to_string(),
                )),
            }),
        }],
        options: Some(ChatLlmOptions {
            max_tokens: Some(512),
            temperature: Some(0.1),
            ..Default::default()
        }),
        ..Default::default()
    };

    let args_bytes = args.encode_to_vec();
    let (result, _metadata) = timeout(
        TEST_TIMEOUT,
        runner.run(&args_bytes, HashMap::new(), Some("chat")),
    )
    .await?;

    let output = result?;
    let response = LlmChatResult::decode(&output[..])?;

    assert!(response.content.is_some());
    if let Some(content) = response.content {
        if let Some(llm_chat_result::message_content::Content::Text(text)) = content.content {
            let parsed: serde_json::Value = serde_json::from_str(&text)?;
            assert!(parsed.get("answer").is_some());
            assert!(parsed.get("confidence").is_some());

            if let Some(confidence) = parsed.get("confidence").and_then(|v| v.as_f64()) {
                assert!((0.0..=1.0).contains(&confidence));
            }

            tracing::info!("Chat JSON Schema test passed. Response: {}", text);
        }
    }

    tracing::info!("Unified runner chat with JSON schema test passed!");
    Ok(())
}

#[ignore = "need to run with ollama server"]
#[tokio::test]
async fn test_unified_runner_completion_with_json_schema() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    let mut runner = create_test_unified_runner().await?;
    runner.load(create_ollama_settings()).await?;

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
        json_schema: Some(schema.to_string()),
        //system_prompt: Some("Translate 'Hello world' to Japanese: \n".to_string()),
        prompt: "Translate 'Hello world' to Japanese:\n\n".to_string(),
        options: Some(CompletionLlmOptions {
            max_tokens: Some(2560),
            temperature: Some(0.3),
            ..Default::default()
        }),
        ..Default::default()
    };

    let args_bytes = args.encode_to_vec();
    let (result, _metadata) = timeout(
        TEST_TIMEOUT,
        runner.run(&args_bytes, HashMap::new(), Some("completion")),
    )
    .await?;

    let output = result?;
    let response = LlmCompletionResult::decode(&output[..])?;

    assert!(response.content.is_some());
    if let Some(content) = response.content {
        tracing::info!("content: {:#?}", &content);
        if let Some(llm_completion_result::message_content::Content::Text(text)) = content.content {
            let parsed: serde_json::Value = serde_json::from_str(&text)?;
            assert!(parsed.get("translation").is_some());
            assert!(parsed.get("source_language").is_some());
            assert!(parsed.get("target_language").is_some());

            tracing::info!("Completion JSON Schema test passed. Response: {}", text);
        }
    }

    tracing::info!("Unified runner completion with JSON schema test passed!");
    Ok(())
}
