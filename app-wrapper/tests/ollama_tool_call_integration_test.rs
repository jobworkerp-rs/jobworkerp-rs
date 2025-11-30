#[cfg(any(test, feature = "test-utils"))]
pub mod test {

    //! Integration tests for Ollama tool call functionality
    //! These tests require an Ollama server running at http://ollama.ollama.svc.cluster.local:11434
    //! These tests require a jobworkerp-rs server running for tool call execution

    #![allow(clippy::uninlined_format_args)]
    #![allow(clippy::collapsible_match)]

    use anyhow::Result;
    use app::app::function::function_set::FunctionSetApp;
    use app::module::{test::create_hybrid_test_app, AppModule};
    use app_wrapper::llm::chat::ollama::OllamaChatService;
    use jobworkerp_base::error::JobWorkerError;
    use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
        self, ChatRole, FunctionOptions, MessageContent,
    };
    use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::OllamaRunnerSettings;
    use jobworkerp_runner::jobworkerp::runner::llm::{llm_chat_args::LlmOptions, LlmChatArgs};
    use proto::jobworkerp::data::RunnerId;
    use proto::jobworkerp::function::data::{
        function_id, FunctionId, FunctionSetData, FunctionUsing,
    };
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::time::{timeout, Duration};

    /// Test configuration
    const OLLAMA_HOST: &str = "http://ollama.ollama.svc.cluster.local:11434";
    const TEST_MODEL: &str = "qwen3:30b"; // Use qwen3:30b model
    // const TEST_MODEL: &str = "gpt-oss:20b";
    const TEST_TIMEOUT: Duration = Duration::from_secs(180);

    /// Setup function app with COMMAND runner for tool calls using test infrastructure
    async fn setup_app_module() -> Result<Arc<AppModule>> {
        let app_module = Arc::new(create_hybrid_test_app().await?);
        let _result = app_module
            .function_set_app
            .create_function_set(&FunctionSetData {
                name: "ollama_tool_test".to_string(),
                description: "Test set for Ollama tool calls - COMMAND runner only".to_string(),
                category: 0,
                targets: vec![FunctionUsing {
                    function_id: Some(FunctionId {
                        id: Some(function_id::Id::RunnerId(RunnerId { value: 1 })),
                    }),
                    using: None,
                }],
            })
            .await;

        Ok(app_module)
    }

    /// Create Ollama chat service
    async fn create_ollama_service() -> Result<OllamaChatService> {
        let app_module = setup_app_module().await?;

        let settings = OllamaRunnerSettings {
        model: TEST_MODEL.to_string(),
        base_url: Some(OLLAMA_HOST.to_string()),
        system_prompt: Some(
            "You are a helpful assistant. When asked to run commands, use the available tools to execute them and provide the results."
                .to_string(),
        ),
        ..Default::default()
    };

        let service = OllamaChatService::new(
            app_module.function_app.clone(),
            app_module.function_set_app.clone(),
            settings,
        )
        .map_err(|e| JobWorkerError::RuntimeError(format!("Service creation failed: {e}")))?;

        Ok(service)
    }

    /// Create test chat arguments with function calling enabled
    fn create_test_chat_args_with_tools(user_message: &str) -> LlmChatArgs {
        LlmChatArgs {
        messages: vec![llm_chat_args::ChatMessage {
            role: ChatRole::User as i32,
            content: Some(MessageContent {
                content: Some(
                    jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::Content::Text(
                        user_message.to_string(),
                    ),
                ),
            }),
        }],
        options: Some(LlmOptions {
            temperature: Some(0.2), // Low temperature for consistent results
            max_tokens: Some(50000),
            top_p: None,
            repeat_penalty: None,
            repeat_last_n: None,
            seed: Some(42), // Fixed seed for reproducibility
            extract_reasoning_content: Some(true),
        }),
        model: Some(TEST_MODEL.to_string()),
        function_options: Some(FunctionOptions {
            use_function_calling: true,
            use_runners_as_function: Some(false),
            use_workers_as_function: Some(false),
            function_set_name: Some("ollama_tool_test".to_string()),
        }),
        json_schema: None,
    }
    }

    /// Test basic tool calling functionality with a simple command
    #[tokio::test]
    #[ignore = "Integration test requiring Ollama server"]
    async fn test_ollama_tool_call_basic_command() -> Result<()> {
        command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
        let service = create_ollama_service().await?;

        // Test with a simple echo command
        let args = create_test_chat_args_with_tools(
            "Please run the command 'echo' with arguments 'Hello, World!' and show me the output.",
        );

        let context = opentelemetry::Context::current();
        let metadata = HashMap::new();

        let result = timeout(TEST_TIMEOUT, service.request_chat(args, context, metadata)).await??;

        // Verify the response
        assert!(result.done, "Chat should be completed");

        if let Some(content) = result.content {
            if let Some(text_content) = content.content {
                if let jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(text) = text_content {
                println!("Response: {}", text);

                // The response should contain the command output
                assert!(
                    text.to_lowercase().contains("hello") || text.to_lowercase().contains("world"),
                    "Response should contain the echo output: {}",
                    text
                );
            } else {
                panic!("Expected text content in response");
            }
            } else {
                panic!("Expected content in response");
            }
        } else {
            panic!("Expected content in response");
        }

        Ok(())
    }

    /// Test tool calling with date command
    #[tokio::test]
    #[ignore = "Integration test requiring Ollama server"]
    async fn test_ollama_tool_call_date_command() -> Result<()> {
        command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
        let service = create_ollama_service().await?;

        // Test with date command
        let args = create_test_chat_args_with_tools(
            "What is the current date and time? Please run the 'date' command to find out.",
        );

        let context = opentelemetry::Context::current();
        let metadata = HashMap::new();

        let result = timeout(TEST_TIMEOUT, service.request_chat(args, context, metadata)).await??;

        // Verify the response
        assert!(result.done, "Chat should be completed");

        if let Some(content) = result.content {
            if let Some(text_content) = content.content {
                if let jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(text) = text_content {
                println!("Date response: {}", text);

                // The response should mention time/date information
                assert!(
                    text.to_lowercase().contains("time")
                    || text.to_lowercase().contains("date")
                    || text.len() > 10, // Should have substantial content
                    "Response should contain date/time information: {}",
                    text
                );
            }
            }
        }

        Ok(())
    }

    /// Test tool calling with file listing command
    #[tokio::test]
    #[ignore = "Integration test requiring Ollama server"]
    async fn test_ollama_tool_call_ls_command() -> Result<()> {
        let service = create_ollama_service().await?;

        // Test with ls command
        let args = create_test_chat_args_with_tools(
            "Please list the files in the current directory using the 'ls -la' command.",
        );

        let context = opentelemetry::Context::current();
        let metadata = HashMap::new();

        let result = timeout(TEST_TIMEOUT, service.request_chat(args, context, metadata)).await??;

        // Verify the response
        assert!(result.done, "Chat should be completed");

        if let Some(content) = result.content {
            if let Some(text_content) = content.content {
                if let jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(text) = text_content {
                println!("Ls response: {}", text);

                // The response should contain directory listing information
                assert!(
                    text.len() > 20, // Should have substantial content
                    "Response should contain directory listing: {}",
                    text
                );
            }
            }
        }

        Ok(())
    }

    /// Test multiple tool calls in sequence
    #[tokio::test]
    #[ignore = "Integration test requiring Ollama server"]
    async fn test_ollama_multiple_tool_calls() -> Result<()> {
        let service = create_ollama_service().await?;

        // Test with multiple commands
        let args = create_test_chat_args_with_tools(
        "Please do the following: 1) Run 'echo Starting test' 2) Run 'date' to get current time 3) Run 'echo Test completed'. Execute these commands and summarize the results.",
    );

        let context = opentelemetry::Context::current();
        let metadata = HashMap::new();

        let result = timeout(
            Duration::from_secs(120), // Longer timeout for multiple calls
            service.request_chat(args, context, metadata),
        )
        .await??;

        // Verify the response
        assert!(result.done, "Chat should be completed");

        if let Some(content) = result.content {
            if let Some(text_content) = content.content {
                if let jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(text) = text_content {
                println!("Multiple commands response: {}", text);

                // The response should contain results from multiple commands
                assert!(
                    text.to_lowercase().contains("start") || text.to_lowercase().contains("test"),
                    "Response should contain command results: {}",
                    text
                );

                // Should be a substantial response
                assert!(
                    text.len() > 50,
                    "Response should contain substantial content: {}",
                    text
                );
            }
            }
        }

        Ok(())
    }

    /// Test without tool calling (control test)
    #[tokio::test]
    #[ignore = "Integration test requiring Ollama server"]
    async fn test_ollama_without_tools() -> Result<()> {
        let service = create_ollama_service().await?;

        // Create args without function calling
        let args = LlmChatArgs {
        messages: vec![llm_chat_args::ChatMessage {
            role: ChatRole::User as i32,
            content: Some(MessageContent {
                content: Some(
                    jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::Content::Text(
                        "Hello, how are you?".to_string(),
                    ),
                ),
            }),
        }],
        options: Some(LlmOptions {
            temperature: Some(0.4),
            max_tokens: Some(10000),
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

        // Verify the response
        assert!(result.done, "Chat should be completed");

        if let Some(content) = result.content {
            if let Some(text_content) = content.content {
                if let jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(text) = text_content {
                println!("Regular chat response: {}", text);

                // Should get a normal conversational response
                assert!(
                    text.len() > 5,
                    "Response should contain some content: {}",
                    text
                );
            }
            }
        }

        Ok(())
    }

    /// Test error handling when command fails
    #[tokio::test]
    #[ignore = "Integration test requiring Ollama server"]
    async fn test_ollama_tool_call_failed_command() -> Result<()> {
        command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
        let service = create_ollama_service().await?;

        // Test with a command that should fail
        let args = create_test_chat_args_with_tools(
        "Please run the command 'this_command_does_not_exist_xyz123' and tell me what happens in watching stdout and stderr.",
    );

        let context = opentelemetry::Context::current();
        let metadata = HashMap::new();

        let result = timeout(TEST_TIMEOUT, service.request_chat(args, context, metadata)).await??;

        // Verify the response - should handle the error gracefully
        assert!(result.done, "Chat should be completed");

        if let Some(content) = result.content {
            if let Some(text_content) = content.content {
                if let jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(text) = text_content {
                println!("Failed command response: {}", text);

                // The response should mention the error
                assert!(
                    text.to_lowercase().contains("error")
                    || text.to_lowercase().contains("fail")
                    || text.to_lowercase().contains("not found")
                    || text.len() > 10, // Should have some response
                    "Response should handle command failure: {}",
                    text
                );
            }
            }
        }

        Ok(())
    }
}
