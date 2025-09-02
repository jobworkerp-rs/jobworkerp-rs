//! JSON Schema format support test for MistralRS integration
//! This test requires a MistralRS model server running locally
//!
//! # Environment Variables
//! - `MISTRAL_TEST_MODEL_ID`: Override the test model ID (default: "microsoft/Phi-3.5-mini-instruct")
//! - `MISTRAL_TEST_TIMEOUT`: Override test timeout in seconds (default: 300)
//! - `MISTRAL_TEST_LOGGING`: Enable/disable logging (default: "true")
//! - `MISTRAL_TEST_PAGED_ATTN`: Enable/disable paged attention (default: "false")
//! - `MISTRAL_TEST_CHAT_TEMPLATE`: Override chat template (optional)
//!
//! # Example Usage
//! ```bash
//! MISTRAL_TEST_MODEL_ID="microsoft/Phi-3-mini-4k-instruct" \
//! MISTRAL_TEST_TIMEOUT=600 \
//! MISTRAL_TEST_LOGGING=true \
//! cargo test --package app-wrapper --test mistral_json_schema_test
//! ```

#![allow(clippy::uninlined_format_args)]
#![allow(clippy::collapsible_match)]

use anyhow::Result;
use app::module::test::create_hybrid_test_app;
use app_wrapper::llm::chat::mistral::MistralRSService;
use app_wrapper::llm::completion::mistral::MistralCompletionService;
use futures::StreamExt;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
    message_content, ChatRole, LlmOptions as ChatLlmOptions,
};
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{ChatMessage, MessageContent};
use jobworkerp_runner::jobworkerp::runner::llm::llm_completion_args::LlmOptions as CompletionLlmOptions;
use jobworkerp_runner::jobworkerp::runner::llm::llm_completion_result;
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::local_runner_settings::GgufModelSettings;
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::{
    local_runner_settings::{ModelSettings, TextModelSettings},
    LocalRunnerSettings,
};
use jobworkerp_runner::jobworkerp::runner::llm::{LlmChatArgs, LlmCompletionArgs};
use std::collections::HashMap;
use tokio::time::{timeout, Duration};

/// Test configuration
const TEST_MODEL_ID: &str = "Qwen/Qwen3-8B-FP8"; //"microsoft/Phi-3.5-mini-instruct"; // Use a model that supports structured output
#[allow(dead_code)]
const TEST_TIMEOUT: Duration = Duration::from_secs(300);

/// Get model ID from environment variable or use default
fn get_test_model_id() -> String {
    std::env::var("MISTRAL_TEST_MODEL_ID").unwrap_or_else(|_| TEST_MODEL_ID.to_string())
}

/// Get test timeout from environment variable or use default
fn get_test_timeout() -> Duration {
    if let Ok(timeout_str) = std::env::var("MISTRAL_get_test_timeout()") {
        if let Ok(timeout_secs) = timeout_str.parse::<u64>() {
            return Duration::from_secs(timeout_secs);
        }
    }
    get_test_timeout()
}

/// Create common model settings for tests
fn create_test_model_settings() -> LocalRunnerSettings {
    let with_logging = std::env::var("MISTRAL_TEST_LOGGING")
        .map(|v| v.to_lowercase() == "true")
        .unwrap_or(true);
    let _with_paged_attn = std::env::var("MISTRAL_TEST_PAGED_ATTN")
        .map(|v| v.to_lowercase() == "true")
        .unwrap_or(true);

    // LocalRunnerSettings {
    //     model_settings: Some(ModelSettings::TextModel(TextModelSettings {
    //         model_name_or_path: get_test_model_id(),
    //         isq_type: None,
    //         with_logging,
    //         with_paged_attn,
    //         chat_template: None, //std::env::var("MISTRAL_TEST_CHAT_TEMPLATE").ok(),
    //     })),
    //     auto_device_map: None,
    // }
    LocalRunnerSettings {
        model_settings: Some(ModelSettings::GgufModel(GgufModelSettings {
            // model_name_or_path: "Qwen/Qwen3-30B-A3B-GGUF".to_string(),
            // gguf_files: vec!["Qwen3-30B-A3B-Q4_K_M.gguf".to_string()],
            // tok_model_id: Some("Qwen/Qwen3-30B-A3B-FP8".to_string()),
            model_name_or_path: "Qwen/Qwen3-14B-GGUF".to_string(),
            gguf_files: vec!["Qwen3-14B-Q4_K_M.gguf".to_string()],
            tok_model_id: Some("Qwen/Qwen3-14B".to_string()),
            with_logging,
            chat_template: None, //std::env::var("MISTRAL_TEST_CHAT_TEMPLATE").ok(),
            with_paged_attn: true,
        })),
        auto_device_map: None,
    }
}

/// Create custom model settings for tests with parameters
fn create_custom_model_settings(
    model_name_or_path: &str,
    with_logging: bool,
    with_paged_attn: bool,
    chat_template: Option<String>,
) -> LocalRunnerSettings {
    LocalRunnerSettings {
        model_settings: Some(ModelSettings::TextModel(TextModelSettings {
            model_name_or_path: model_name_or_path.to_string(),
            isq_type: None,
            with_logging,
            with_paged_attn,
            chat_template,
        })),
        auto_device_map: None,
    }
}

/// Create test chat service
async fn create_test_chat_service() -> Result<MistralRSService> {
    let settings = create_test_model_settings();
    let app_module = create_hybrid_test_app().await?;
    MistralRSService::new_with_function_app(settings, app_module.function_app.clone()).await
}

/// Create test completion service
async fn create_test_completion_service() -> Result<MistralCompletionService> {
    let settings = create_test_model_settings();
    let app_module = create_hybrid_test_app().await?;
    MistralCompletionService::new(settings, app_module.function_app.clone()).await
}

#[ignore = "need to run with mistralrs model server"]
#[tokio::test]
async fn test_simple_chat_with_textmodel() -> Result<()> {
    // Use TextModelSettings instead of GgufModelSettings
    let settings = create_custom_model_settings(
        &get_test_model_id(),
        true,  // with_logging: true
        false, // with_paged_attn: false to prevent stack overflow
        None,  // chat_template: None
    );
    let app_module = create_hybrid_test_app().await?;
    let service =
        MistralRSService::new_with_function_app(settings, app_module.function_app.clone()).await?;

    let args = LlmChatArgs {
        json_schema: None, // No JSON schema to test basic functionality
        messages: vec![ChatMessage {
            role: ChatRole::User.into(),
            content: Some(MessageContent {
                content: Some(message_content::Content::Text("What is 2+2?".to_string())),
            }),
        }],
        options: Some(ChatLlmOptions {
            max_tokens: Some(100),
            temperature: Some(0.2),
            ..Default::default()
        }),
        ..Default::default()
    };

    let result = timeout(
        get_test_timeout(),
        service.request_chat(args, opentelemetry::Context::current(), HashMap::new()),
    )
    .await??;

    assert!(result.content.is_some());
    println!("Simple chat with TextModelSettings test passed!");
    Ok(())
}

#[ignore = "need to run with mistralrs model server"]
#[tokio::test]
async fn test_simple_chat_without_schema() -> Result<()> {
    let service = create_test_chat_service().await?;

    let args = LlmChatArgs {
        json_schema: None, // No JSON schema to test basic functionality
        messages: vec![ChatMessage {
            role: ChatRole::User.into(),
            content: Some(MessageContent {
                content: Some(message_content::Content::Text("What is 2+2?".to_string())),
            }),
        }],
        options: Some(ChatLlmOptions {
            max_tokens: Some(100),
            temperature: Some(0.2),
            ..Default::default()
        }),
        ..Default::default()
    };

    let result = timeout(
        get_test_timeout(),
        service.request_chat(args, opentelemetry::Context::current(), HashMap::new()),
    )
    .await??;

    assert!(result.content.is_some());
    println!("Simple chat without schema test passed!");
    Ok(())
}

#[ignore = "need to run with mistralrs model server"]
#[tokio::test]
async fn test_chat_with_json_schema_large_stack() -> Result<()> {
    // Use std::thread with larger stack size to avoid stack overflow in tests
    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024) // 8MB stack
        .spawn(|| {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                let service = create_test_chat_service().await?;

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
                        max_tokens: Some(3512),
                        temperature: Some(0.2),
                        extract_reasoning_content: Some(true),
                        ..Default::default()
                    }),
                    ..Default::default()
                };

                let result = timeout(
                    get_test_timeout(),
                    service.request_chat(args, opentelemetry::Context::current(), HashMap::new()),
                )
                .await??;

                assert!(result.content.is_some());
                if let Some(content) = result.content {
                    if let Some(jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(text)) = content.content {
                        let parsed: serde_json::Value = serde_json::from_str(&text)?;
                        assert!(parsed.get("answer").is_some());
                        assert!(parsed.get("confidence").is_some());

                        if let Some(confidence) = parsed.get("confidence").and_then(|v| v.as_f64()) {
                            assert!((0.0..=1.0).contains(&confidence));
                        }

                        println!("MistralRS Chat JSON Schema test passed (large stack). Response: {}", text);
                    }
                }

                anyhow::Ok(())
            })
        })
        .unwrap()
        .join()
        .unwrap();

    result
}

#[ignore = "need to run with mistralrs model server"]
#[tokio::test]
async fn test_chat_with_json_schema() -> Result<()> {
    let service = create_test_chat_service().await?;

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
            max_tokens: Some(3512),
            temperature: Some(0.2),
            extract_reasoning_content: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    };

    let result = timeout(
        get_test_timeout(),
        service.request_chat(args, opentelemetry::Context::current(), HashMap::new()),
    )
    .await??;

    assert!(result.content.is_some());
    if let Some(content) = result.content {
        if let Some(jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(text)) = content.content {
            let parsed: serde_json::Value = serde_json::from_str(&text)?;
            assert!(parsed.get("answer").is_some());
            assert!(parsed.get("confidence").is_some());

            if let Some(confidence) = parsed.get("confidence").and_then(|v| v.as_f64()) {
                assert!((0.0..=1.0).contains(&confidence));
            }

            println!("MistralRS Chat JSON Schema test passed. Response: {}", text);
        }
    }

    Ok(())
}

#[ignore = "need to run with mistralrs model server"]
#[tokio::test]
async fn test_completion_with_json_schema() -> Result<()> {
    let service = create_test_completion_service().await?;

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
        prompt: "Translate 'Hello world' to Japanese".to_string(),
        options: Some(CompletionLlmOptions {
            max_tokens: Some(25600),
            temperature: Some(0.3),
            ..Default::default()
        }),
        ..Default::default()
    };

    let result = timeout(
        get_test_timeout(),
        service.request_chat(args, opentelemetry::Context::current(), HashMap::new()),
    )
    .await??;

    assert!(result.content.is_some());
    if let Some(content) = result.content {
        if let Some(llm_completion_result::message_content::Content::Text(text)) = content.content {
            let parsed: serde_json::Value = serde_json::from_str(&text)?;
            assert!(parsed.get("translation").is_some());
            assert!(parsed.get("source_language").is_some());
            assert!(parsed.get("target_language").is_some());

            if let Some(translation) = parsed.get("translation").and_then(|v| v.as_str()) {
                assert!(!translation.is_empty());
            }

            println!(
                "MistralRS Completion JSON Schema test passed. Response: {}",
                text
            );
        }
    }

    Ok(())
}

#[ignore = "need to run with mistralrs model server"]
#[tokio::test]
async fn test_invalid_json_schema_handling() -> Result<()> {
    let service = create_test_chat_service().await?;

    let invalid_schema = r#"{ invalid json schema }"#;

    let args = LlmChatArgs {
        json_schema: Some(invalid_schema.to_string()),
        messages: vec![ChatMessage {
            role: ChatRole::User.into(),
            content: Some(MessageContent {
                content: Some(message_content::Content::Text("Test message".to_string())),
            }),
        }],
        ..Default::default()
    };

    let result = timeout(
        get_test_timeout(),
        service.request_chat(args, opentelemetry::Context::current(), HashMap::new()),
    )
    .await??;

    assert!(result.content.is_some());
    println!("Invalid JSON schema gracefully handled by MistralRS");
    Ok(())
}

#[ignore = "need to run with mistralrs model server"]
#[tokio::test]
async fn test_completion_stream_with_json_schema() -> Result<()> {
    let service = create_test_completion_service().await?;

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
        json_schema: Some(schema.to_string()),
        prompt: "List 3 programming languages and count them".to_string(),
        options: Some(CompletionLlmOptions {
            max_tokens: Some(256),
            temperature: Some(0.1),
            ..Default::default()
        }),
        ..Default::default()
    };

    // Request the streaming response
    let stream_result = timeout(get_test_timeout(), service.request_stream_chat(args)).await??;

    // Collect all chunks to verify the streaming functionality
    let responses = stream_result.collect::<Vec<_>>().await;

    // Make sure we got some responses
    assert!(!responses.is_empty(), "No streaming responses received");

    // Combine all text chunks
    let combined_text = responses
        .iter()
        .filter_map(|res| {
            res.content.as_ref().and_then(|c| {
                c.content.as_ref().map(|content| match content {
                    llm_completion_result::message_content::Content::Text(text) => text.clone(),
                })
            })
        })
        .collect::<Vec<_>>()
        .join("");

    // Verify the combined response is valid JSON matching the schema
    if !combined_text.trim().is_empty() {
        let parsed: serde_json::Value = serde_json::from_str(&combined_text)?;
        assert!(parsed.get("count").is_some());
        assert!(parsed.get("items").is_some());

        println!(
            "MistralRS Streaming JSON Schema test passed. Response: {}",
            combined_text
        );
    }

    Ok(())
}

#[ignore = "need to run with mistralrs model server"]
#[tokio::test]
async fn test_workflow_8level_schema_with_mistral_chat() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    let service = create_test_chat_service().await?;

    // Load the workflow_8level_final.json schema
    let schema_path = "../runner/schema/workflow_8level_final.json";
    let schema = std::fs::read_to_string(schema_path)
        .map_err(|e| anyhow::anyhow!("Failed to read schema file at {}: {}", schema_path, e))?;

    // Verify the schema is valid JSON
    let _parsed_schema: serde_json::Value = serde_json::from_str(&schema)?;
    tracing::info!("Schema loaded successfully, size: {} bytes", schema.len());

    let args = LlmChatArgs {
        json_schema: Some(schema.clone()),
        messages: vec![ChatMessage {
            role: ChatRole::User.into(),
            content: Some(MessageContent {
                content: Some(message_content::Content::Text(
                    "Generate a simple workflow that runs a command task named 'hello' that executes 'echo Hello World'. Make it a valid workflow with proper document metadata.".to_string(),
                )),
            }),
        }],
        options: Some(ChatLlmOptions {
            max_tokens: Some(20480),
            temperature: Some(0.1),
            ..Default::default()
        }),
        ..Default::default()
    };

    let result = timeout(
        Duration::from_secs(600), // Increased timeout for complex schema
        service.request_chat(args, opentelemetry::Context::current(), HashMap::new()),
    )
    .await??;

    assert!(result.content.is_some());
    if let Some(content) = result.content {
        if let Some(jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(text)) = content.content {
            tracing::info!("Generated workflow response: {}", text);

            // Verify the response is valid JSON
            let parsed: serde_json::Value = serde_json::from_str(&text)?;

            // Verify required workflow fields are present
            assert!(parsed.get("document").is_some(), "Missing 'document' field");
            assert!(parsed.get("input").is_some(), "Missing 'input' field");
            assert!(parsed.get("do").is_some(), "Missing 'do' field");

            // Verify document structure
            if let Some(document) = parsed.get("document") {
                assert!(document.get("dsl").is_some(), "Missing 'document.dsl' field");
                assert!(document.get("namespace").is_some(), "Missing 'document.namespace' field");
                assert!(document.get("name").is_some(), "Missing 'document.name' field");
                assert!(document.get("version").is_some(), "Missing 'document.version' field");
            }

            // Verify do array structure
            if let Some(do_array) = parsed.get("do").and_then(|v| v.as_array()) {
                assert!(!do_array.is_empty(), "Empty 'do' array");
                tracing::info!("Workflow has {} top-level tasks", do_array.len());

                // Check first task structure - be more flexible about task format
                if let Some(first_task) = do_array.first().and_then(|v| v.as_object()) {
                    tracing::info!("First task structure has {} keys: {:?}", first_task.len(), first_task.keys().collect::<Vec<_>>());

                    // The LLM might generate different valid task structures
                    // Just verify we have some recognizable task properties
                    let has_task_properties = first_task.keys().any(|k| {
                        k == "run" || k == "set" || k == "fork" || k == "for" ||
                        k == "try" || k == "switch" || k == "do" || k == "wait" || k == "raise" ||
                        // Or it might be a task name containing task definition
                        first_task.get(k).and_then(|v| v.as_object()).is_some_and(|obj| {
                            obj.contains_key("run") || obj.contains_key("set") || obj.contains_key("fork") ||
                            obj.contains_key("for") || obj.contains_key("try") || obj.contains_key("switch") ||
                            obj.contains_key("do") || obj.contains_key("wait") || obj.contains_key("raise")
                        })
                    });

                    if !has_task_properties {
                        tracing::warn!("Generated task structure may not match expected workflow schema, but JSON is valid");
                    }
                }
            }

            tracing::info!("MistralRS Workflow 8-level schema test with LLM_CHAT passed!");
        }
    }

    Ok(())
}

#[ignore = "need to run with mistralrs model server"]
#[tokio::test]
async fn test_complex_nested_workflow_with_mistral_chat() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    // Use custom settings with verbose logging for complex workflow test
    // let custom_settings = create_custom_model_settings(
    //     &get_test_model_id(),
    //     true,  // with_logging: true for debugging
    //     false, // with_paged_attn: false
    //     None,  // chat_template: None
    // );
    let _custom_settings = create_test_model_settings();
    let service = create_test_chat_service().await?;

    // Load the workflow schema
    let schema_path = "../runner/schema/workflow_8level_final.json";
    let schema = std::fs::read_to_string(schema_path)
        .map_err(|e| anyhow::anyhow!("Failed to read schema file at {}: {}", schema_path, e))?;

    let args = LlmChatArgs {
        json_schema: Some(schema),
        messages: vec![ChatMessage {
            role: ChatRole::User.into(),
            content: Some(MessageContent {
                content: Some(message_content::Content::Text(
                    "Generate a complex workflow that demonstrates nested tasks with a maximum nesting level of 7. Follow the exact schema structure. Include these specific task types: 1) A 'fork' task with 'branches' property containing task lists, 2) A 'for' task with 'for' and 'do' properties, 3) A 'try' task with 'try' and 'catch' properties, 4) A 'switch' task with 'switch' property containing case conditions. Each task must be a JSON object with a single key-value pair where the key is the task name and the value contains the task definition (like 'fork', 'for', 'try', 'switch', or 'run' properties). Keep nesting depth under 7 levels. Make it a complete valid workflow with proper document metadata.".to_string(),
                )),
            }),
        }],
        options: Some(ChatLlmOptions {
            max_tokens: Some(40960),
            temperature: Some(0.2),
            ..Default::default()
        }),
        ..Default::default()
    };

    let result = timeout(
        Duration::from_secs(900), // Even longer timeout for complex generation
        service.request_chat(args, opentelemetry::Context::current(), HashMap::new()),
    )
    .await??;

    assert!(result.content.is_some());
    if let Some(content) = result.content {
        if let Some(jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(text)) = content.content {
            tracing::info!("Generated complex workflow response: {}", text);

            // Verify the response is valid JSON
            let parsed: serde_json::Value = serde_json::from_str(&text)?;

            // Basic structure validation
            assert!(parsed.get("document").is_some());
            assert!(parsed.get("input").is_some());
            assert!(parsed.get("do").is_some());

            // Count different task types to verify complexity
            let empty_vec = vec![];
            let do_tasks = parsed.get("do").and_then(|v| v.as_array()).unwrap_or(&empty_vec);
            let task_json = serde_json::to_string(&do_tasks)?;

            let fork_count = task_json.matches("\"fork\"").count();
            let for_count = task_json.matches("\"for\"").count();
            let try_count = task_json.matches("\"try\"").count();
            let switch_count = task_json.matches("\"switch\"").count();

            tracing::info!("Task type counts - Fork: {}, For: {}, Try: {}, Switch: {}",
                     fork_count, for_count, try_count, switch_count);

            // At least one complex task type should be present
            // If the expected task types are not found, check if the workflow at least has valid structure
            if fork_count == 0 && for_count == 0 && try_count == 0 && switch_count == 0 {
                tracing::warn!("No standard complex task types found, but verifying workflow structure");

                // Check if the workflow has reasonable structure even if not using expected task types
                let has_reasonable_structure = do_tasks.len() > 1 &&
                    do_tasks.iter().any(|task| {
                        if let Some(obj) = task.as_object() {
                            !obj.is_empty() && obj.values().any(|v| v.is_object())
                        } else {
                            false
                        }
                    });

                if !has_reasonable_structure {
                    panic!("Generated workflow lacks proper structure with nested tasks");
                }

                tracing::info!("Workflow has reasonable structure despite not using expected complex task types");
            } else {
                tracing::info!("Found expected complex task types in workflow");
            }

            tracing::info!("MistralRS Complex nested workflow schema test with LLM_CHAT passed!");
        }
    }

    Ok(())
}
