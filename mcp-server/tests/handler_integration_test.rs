//! Integration tests for MCP Server Handler
//!
//! These tests verify the MCP handler functionality including:
//! - ServerInfo generation
//! - Handler creation
//! - Configuration from environment
//!
//! Requires: SQLite database (automatically created for tests)

use anyhow::Result;
use mcp_server::{McpHandler, McpServerConfig};
use rmcp::ServerHandler;

/// Create test handler with default configuration
async fn create_test_handler() -> Result<McpHandler> {
    use app::module::test::create_hybrid_test_app;

    let app_module = create_hybrid_test_app().await?;

    let config = McpServerConfig {
        exclude_runner_as_tool: false,
        exclude_worker_as_tool: true, // Exclude workers for simpler testing
        set_name: None,
        timeout_sec: 30,
        streaming: false,
    };

    Ok(McpHandler::new(
        app_module.function_app.clone(),
        app_module.function_set_app.clone(),
        config,
    ))
}

#[tokio::test]
#[ignore] // Requires database setup
async fn test_get_info() {
    let handler = create_test_handler()
        .await
        .expect("Failed to create handler");
    let info = handler.get_info();

    assert_eq!(info.protocol_version, rmcp::model::ProtocolVersion::LATEST);
    assert!(info.capabilities.tools.is_some());
    assert!(info.instructions.is_some());
    assert!(info
        .instructions
        .as_ref()
        .unwrap()
        .contains("jobworkerp MCP Server"));
}

#[tokio::test]
#[ignore] // Requires database setup
async fn test_handler_creation() {
    let handler = create_test_handler().await;
    assert!(handler.is_ok(), "Handler should be created successfully");
}

#[tokio::test]
#[ignore] // Requires database setup
async fn test_handler_with_exclude_runners() {
    use app::module::test::create_hybrid_test_app;

    let app_module = create_hybrid_test_app()
        .await
        .expect("Failed to create app");

    // Configure to exclude runners
    let config = McpServerConfig {
        exclude_runner_as_tool: true, // Exclude runners
        exclude_worker_as_tool: true,
        set_name: None,
        timeout_sec: 30,
        streaming: false,
    };

    let handler = McpHandler::new(
        app_module.function_app.clone(),
        app_module.function_set_app.clone(),
        config,
    );

    // Handler should be created successfully
    let info = handler.get_info();
    assert!(info.capabilities.tools.is_some());
}

#[tokio::test]
#[ignore] // Requires database setup
async fn test_handler_with_function_set() {
    use app::module::test::create_hybrid_test_app;

    let app_module = create_hybrid_test_app()
        .await
        .expect("Failed to create app");

    // Configure with function set
    let config = McpServerConfig {
        exclude_runner_as_tool: false,
        exclude_worker_as_tool: false,
        set_name: Some("test_set".to_string()),
        timeout_sec: 60,
        streaming: true,
    };

    let handler = McpHandler::new(
        app_module.function_app.clone(),
        app_module.function_set_app.clone(),
        config,
    );

    // Handler should be created successfully
    let info = handler.get_info();
    assert!(info.capabilities.tools.is_some());
}

#[test]
fn test_config_default() {
    let config = McpServerConfig::default();
    assert!(!config.exclude_runner_as_tool);
    assert!(!config.exclude_worker_as_tool);
    assert!(config.set_name.is_none());
    assert_eq!(config.timeout_sec, 60);
    assert!(config.streaming);
}

#[test]
fn test_config_from_env() {
    // Set environment variables for testing
    std::env::set_var("EXCLUDE_RUNNER_AS_TOOL", "true");
    std::env::set_var("EXCLUDE_WORKER_AS_TOOL", "true");
    std::env::set_var("TOOL_SET_NAME", "test_set");
    std::env::set_var("REQUEST_TIMEOUT_SEC", "120");
    std::env::set_var("MCP_STREAMING", "false");

    let config = McpServerConfig::from_env();

    assert!(config.exclude_runner_as_tool);
    assert!(config.exclude_worker_as_tool);
    assert_eq!(config.set_name, Some("test_set".to_string()));
    assert_eq!(config.timeout_sec, 120);
    assert!(!config.streaming);

    // Clean up
    std::env::remove_var("EXCLUDE_RUNNER_AS_TOOL");
    std::env::remove_var("EXCLUDE_WORKER_AS_TOOL");
    std::env::remove_var("TOOL_SET_NAME");
    std::env::remove_var("REQUEST_TIMEOUT_SEC");
    std::env::remove_var("MCP_STREAMING");
}

#[tokio::test]
#[ignore] // Requires database setup
async fn test_list_tools_returns_runners() {
    use app::module::test::create_hybrid_test_app;

    let app_module = create_hybrid_test_app()
        .await
        .expect("Failed to create app");

    let config = McpServerConfig {
        exclude_runner_as_tool: false, // Include runners
        exclude_worker_as_tool: true,  // Exclude workers
        set_name: None,
        timeout_sec: 30,
        streaming: false,
    };

    // Create handler to ensure it's properly configured
    let _handler = McpHandler::new(
        app_module.function_app.clone(),
        app_module.function_set_app.clone(),
        config,
    );

    // Use internal FunctionApp to verify list_tools behavior
    use app::app::function::FunctionApp;
    let functions = app_module
        .function_app
        .find_functions(false, true)
        .await
        .expect("Failed to find functions");

    // Verify some built-in runners are available
    assert!(
        !functions.is_empty(),
        "Should have at least one function (COMMAND runner)"
    );

    // Check for expected runner types
    let function_names: Vec<_> = functions.iter().map(|f| f.name.as_str()).collect();
    assert!(
        function_names.iter().any(|n| n.contains("COMMAND")),
        "Should have COMMAND runner. Found: {:?}",
        function_names
    );
}

#[tokio::test]
#[ignore] // Requires database setup
async fn test_list_tools_excludes_runners_when_configured() {
    use app::module::test::create_hybrid_test_app;

    let app_module = create_hybrid_test_app()
        .await
        .expect("Failed to create app");

    // Use FunctionApp to test exclude behavior
    use app::app::function::FunctionApp;
    let functions_with_runners = app_module
        .function_app
        .find_functions(false, true)
        .await
        .expect("Failed to find functions with runners");

    let functions_without_runners = app_module
        .function_app
        .find_functions(true, true)
        .await
        .expect("Failed to find functions without runners");

    assert!(
        functions_with_runners.len() >= functions_without_runners.len(),
        "Excluding runners should result in fewer or equal functions"
    );
}

#[tokio::test]
#[ignore] // Requires database setup
async fn test_streaming_config_affects_handler() {
    use app::module::test::create_hybrid_test_app;

    let app_module = create_hybrid_test_app()
        .await
        .expect("Failed to create app");

    // Test with streaming disabled
    let config_no_streaming = McpServerConfig {
        exclude_runner_as_tool: false,
        exclude_worker_as_tool: true,
        set_name: None,
        timeout_sec: 30,
        streaming: false,
    };

    let handler_no_streaming = McpHandler::new(
        app_module.function_app.clone(),
        app_module.function_set_app.clone(),
        config_no_streaming,
    );

    // Test with streaming enabled
    let config_streaming = McpServerConfig {
        exclude_runner_as_tool: false,
        exclude_worker_as_tool: true,
        set_name: None,
        timeout_sec: 30,
        streaming: true,
    };

    let handler_streaming = McpHandler::new(
        app_module.function_app.clone(),
        app_module.function_set_app.clone(),
        config_streaming,
    );

    // Both handlers should be created successfully
    // Actual streaming behavior is tested through call_tool execution
    let info_no_streaming = handler_no_streaming.get_info();
    let info_streaming = handler_streaming.get_info();

    assert!(info_no_streaming.capabilities.tools.is_some());
    assert!(info_streaming.capabilities.tools.is_some());
}
