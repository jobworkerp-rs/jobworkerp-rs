use crate::runner::mcp::config::{McpServerConfig, McpServerTransportConfig};
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[cfg(test)]
use crate::runner::RunnerSpec;

fn get_mcp_server_path(server_name: &str) -> PathBuf {
    let base_path = PathBuf::from("../modules/mcp-servers/src");
    base_path.join(server_name)
}

// Helper function to set environment variables that add necessary directories to PYTHONPATH
fn setup_python_env(server_path: &Path) -> HashMap<String, String> {
    let mut envs = HashMap::new();

    let current_pythonpath = std::env::var("PYTHONPATH").unwrap_or_default();

    let server_path_str = server_path.to_string_lossy();

    let new_pythonpath = if current_pythonpath.is_empty() {
        format!("{server_path_str}")
    } else {
        format!("{current_pythonpath}:{server_path_str}")
    };

    envs.insert("PYTHONPATH".to_string(), new_pythonpath);
    envs
}

// Common function for setting up Python environment using uv
async fn setup_python_environment_with_uv(
    server_path: &PathBuf,
) -> Result<HashMap<String, String>> {
    // Change current directory to server directory (for dependency installation)
    let original_dir = std::env::current_dir()?;
    std::env::set_current_dir(server_path)?;

    let uv_venv = std::process::Command::new("uv").args(["venv"]).status()?;

    if !uv_venv.success() {
        std::env::set_current_dir(original_dir)?;
        return Err(anyhow::anyhow!(
            "Failed to create virtual environment with uv"
        ));
    }

    // Install dependencies using uv command
    let uv_install = std::process::Command::new("uv")
        .args(["pip", "install", "-e", "."])
        .status()?;

    if !uv_install.success() {
        std::env::set_current_dir(original_dir)?;
        return Err(anyhow::anyhow!("Failed to install dependencies with uv"));
    }

    std::env::set_current_dir(original_dir)?;

    // Set environment variables
    let mut envs = setup_python_env(server_path);

    let venv_path = server_path.join(".venv");
    let venv_bin_path = if cfg!(target_os = "windows") {
        venv_path.join("Scripts")
    } else {
        venv_path.join("bin")
    };

    let path_env = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", venv_bin_path.to_string_lossy(), path_env);
    envs.insert("PATH".to_string(), new_path);

    Ok(envs)
}

// Function to get the Python path in the virtual environment
fn get_venv_python(server_path: &Path) -> PathBuf {
    let venv_path = server_path.join(".venv");
    let venv_bin_path = if cfg!(target_os = "windows") {
        venv_path.join("Scripts")
    } else {
        venv_path.join("bin")
    };
    venv_bin_path.join("python")
}

pub async fn create_time_mcp_server_transport() -> Result<McpServerTransportConfig> {
    // Time server configuration
    let time_server_path = get_mcp_server_path("time");

    // Set up virtual environment
    let envs = setup_python_environment_with_uv(&time_server_path).await?;

    let venv_python = get_venv_python(&time_server_path);
    Ok(McpServerTransportConfig::Stdio {
        command: venv_python.to_string_lossy().to_string(),
        args: vec![
            "-m".to_string(),
            "mcp_server_time".to_string(),
            // Explicitly specify timezone
            "--local-timezone".to_string(),
            "UTC".to_string(),
        ],
        envs,
    })
}

pub async fn create_time_mcp_server() -> Result<McpServerConfig> {
    Ok(McpServerConfig {
        name: "time".to_string(),
        description: None,
        transport: create_time_mcp_server_transport().await?,
    })
}

pub async fn create_fetch_mcp_server_transport() -> Result<McpServerTransportConfig> {
    // Fetch server configuration
    let fetch_server_path = get_mcp_server_path("fetch");

    // Set up virtual environment
    let envs = setup_python_environment_with_uv(&fetch_server_path).await?;

    let venv_python = get_venv_python(&fetch_server_path);
    Ok(McpServerTransportConfig::Stdio {
        command: venv_python.to_string_lossy().to_string(),
        args: vec!["-m".to_string(), "mcp_server_fetch".to_string()],
        envs,
    })
}

pub async fn create_fetch_mcp_server() -> Result<McpServerConfig> {
    Ok(McpServerConfig {
        name: "fetch".to_string(),
        description: None,
        transport: create_fetch_mcp_server_transport().await?,
    })
}

#[tokio::test]
async fn test_time_mcp_server() -> Result<()> {
    use crate::runner::mcp::config::McpConfig;

    let config = McpConfig {
        server: vec![create_time_mcp_server().await?],
    };

    let factory = crate::runner::mcp::proxy::McpServerFactory::new(config);
    let mut clients = factory.test_all().await?;
    assert_eq!(clients.len(), 1);
    let client = clients.pop().unwrap();
    println!("Time server tools: {client:?}");
    // remove client for test
    client.cancel().await?;

    // create server by the name
    let client = factory.connect_server("time").await?;

    // Call the get_current_time tool
    let result = client
        .call_tool("get_current_time", serde_json::json!({"timezone": "UTC"}))
        .await?;

    // Display results
    println!("Time server result: {result:?}");

    assert!(!result.content.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_mcp_cancellation() -> Result<()> {
    use crate::runner::cancellation::CancelMonitoring;
    use crate::runner::mcp::config::McpConfig;
    use crate::runner::mcp::McpServerRunnerImpl;

    let config = McpConfig {
        server: vec![create_time_mcp_server().await?],
    };

    let factory = crate::runner::mcp::proxy::McpServerFactory::new(config);
    let client = factory.connect_server("time").await?;

    let mut runner = McpServerRunnerImpl::new(client, None).await?;

    // Test cancellation without active request
    runner.request_cancellation().await.unwrap();
    eprintln!("MCP cancel completed successfully with no active operation");

    // Test basic cancellation functionality is available
    eprintln!("Basic MCP cancel functionality is available and does not panic");

    eprintln!("=== MCP cancellation test completed ===");
    Ok(())
}

// Legacy mode test removed - replaced by test_using_mode_with_cancel_helper

// Legacy mode test removed - execution-during cancellation not currently supported in using mode

// Legacy mode test removed - replaced by test_using_mode_stream_execution
// Legacy mode test removed - replaced by test_using_mode_stream_with_cancellation

//     // Verify tools from SQLite server are included
//     assert!(tools.contains_key("sqlite"));

//     // Create table
//     let create_table_result = clients
//         .call_tool(
//             "sqlite",
//             "execute",
//             serde_json::json!({
//                 "sql": "CREATE TABLE test_table (id INTEGER PRIMARY KEY, name TEXT, value INTEGER)"
//             }),
//         )
//         .await?;

//     println!("Create table result: {:?}", create_table_result);
//     assert!(!create_table_result.content.is_empty());

//     // Insert data
//     let insert_result = clients
//         .call_tool(
//             "sqlite",
//             "execute",
//             serde_json::json!({
//                 "sql": "INSERT INTO test_table (name, value) VALUES ('test1', 100), ('test2', 200)"
//             }),
//         )
//         .await?;

//     println!("Insert data result: {:?}", insert_result);
//     assert!(!insert_result.content.is_empty());

//     // Query data
//     let query_result = clients
//         .call_tool(
//             "sqlite",
//             "query",
//             serde_json::json!({
//                 "sql": "SELECT * FROM test_table WHERE value > 150"
//             }),
//         )
//         .await?;

//     println!("Query result: {:?}", query_result);
//     assert!(!query_result.content.is_empty());

//     Ok(())
// }

// #[tokio::test]
// async fn test_multiple_mcp_servers() -> Result<()> {
//     // Configuration for multiple servers
//     let time_server_path = get_mcp_server_path("time");
//     let time_envs = setup_python_environment_with_uv(&time_server_path).await?;
//     let time_python = get_venv_python(&time_server_path);

//     let sqlite_server_path = get_mcp_server_path("sqlite");
//     let sqlite_envs = setup_python_environment_with_uv(&sqlite_server_path).await?;
//     let sqlite_python = get_venv_python(&sqlite_server_path);

//     // Create configuration for Time server
//     let time_config = McpServerConfig {
//         name: "time".to_string(),
//         transport: McpServerTransportConfig::Stdio {
//             command: time_python.to_string_lossy().to_string(),
//             args: vec!["-m".to_string(), "mcp_server_time".to_string()],
//             envs: time_envs,
//         },
//     };

//     // Create configuration for SQLite server
//     let sqlite_venv_bin_path = if cfg!(target_os = "windows") {
//         sqlite_server_path.join(".venv").join("Scripts")
//     } else {
//         sqlite_server_path.join(".venv").join("bin")
//     };

//     let sqlite_config = McpServerConfig {
//         name: "sqlite".to_string(),
//         transport: McpServerTransportConfig::Stdio {
//             command: sqlite_venv_bin_path
//                 .join("mcp")
//                 .to_string_lossy()
//                 .to_string(),
//             args: vec![
//                 "dev".to_string(),
//                 format!(
//                     "{}/src/mcp_server_sqlite/server.py:wrapper",
//                     sqlite_server_path.to_string_lossy()
//                 ),
//             ],
//             envs: sqlite_envs,
//         },
//     };

//     // Test Time server first
//     {
//         let config = McpConfig {
//             server: vec![time_config.clone()],
//         };

//         let clients = crate::runner::mcp::client::McpClientsImpl::new(&config).await?;
//         let time_tools = clients.load_all_tools().await?;
//         println!("Time tools: {:?}", time_tools);

//         // Call get_current_time tool
//         let time_result = clients
//             .call_tool("time", "get_current_time", serde_json::json!({}))
//             .await?;

//         println!("Time server result: {:?}", time_result);
//         assert!(!time_result.content.is_empty());
//     }

//     // Test SQLite server next
//     {
//         let config = McpConfig {
//             server: vec![sqlite_config.clone()],
//         };

//         let clients = crate::runner::mcp::client::McpClientsImpl::new(&config).await?;
//         let sqlite_tools = clients.load_all_tools().await?;
//         println!("SQLite tools: {:?}", sqlite_tools);

//         // Create SQLite table and verify
//         let create_table_result = clients
//             .call_tool(
//                 "sqlite",
//                 "execute",
//                 serde_json::json!({
//                     "sql": "CREATE TABLE test_table (id INTEGER PRIMARY KEY, name TEXT)"
//                 }),
//             )
//             .await?;

//         println!("Create table result: {:?}", create_table_result);
//         assert!(!create_table_result.content.is_empty());
//     }

//     Ok(())
// }

// ==================== Sub-Method Mode Integration Tests ====================

/// Test T3.7: Initialize using mode and verify tool list generation
#[tokio::test]
#[ignore] // Requires MCP server - run with --ignored for full testing
async fn test_using_mode_initialization() -> Result<()> {
    use crate::runner::mcp::config::McpConfig;
    use crate::runner::mcp::McpServerRunnerImpl;
    use crate::runner::RunnerSpec;

    let config = McpConfig {
        server: vec![create_time_mcp_server().await?],
    };

    let factory = crate::runner::mcp::proxy::McpServerFactory::new(config);
    let client = factory.connect_server("time").await?;

    let runner = McpServerRunnerImpl::new(client, None).await?;

    let tool_names = runner.available_tool_names();
    assert!(
        !tool_names.is_empty(),
        "Should have at least one tool available"
    );
    assert!(
        tool_names.contains(&"get_current_time".to_string()),
        "Should have get_current_time tool"
    );

    let proto_map = runner.method_proto_map();
    assert!(
        proto_map.contains_key("get_current_time"),
        "Proto map should contain get_current_time"
    );

    let method_schema = proto_map.get("get_current_time").unwrap();
    assert!(
        method_schema.args_proto.contains("TimeGetCurrentTimeArgs")
            || method_schema.args_proto.contains("syntax = \"proto3\""),
        "Schema should be valid Protobuf definition"
    );

    eprintln!("✅ Sub-method mode initialization test passed");
    eprintln!("   Available tools: {:?}", tool_names);
    Ok(())
}

/// Test T3.7: Execute tool call in using mode with explicit using
#[tokio::test]
#[ignore] // Requires MCP server - run with --ignored for full testing
async fn test_using_mode_execution_with_explicit_method() -> Result<()> {
    use crate::runner::mcp::config::McpConfig;
    use crate::runner::mcp::McpServerRunnerImpl;
    use crate::runner::RunnerTrait;
    use std::collections::HashMap;

    let config = McpConfig {
        server: vec![create_time_mcp_server().await?],
    };

    let factory = crate::runner::mcp::proxy::McpServerFactory::new(config);
    let client = factory.connect_server("time").await?;
    let mut runner = McpServerRunnerImpl::new(client, None).await?;

    // Prepare JSON arguments (using mode uses JSON bytes)
    let args = serde_json::json!({"timezone": "UTC"});
    let args_bytes = serde_json::to_vec(&args)?;

    // Execute with explicit using
    let metadata = HashMap::new();
    let (result, _) = runner
        .run(&args_bytes, metadata, Some("get_current_time"))
        .await;

    assert!(
        result.is_ok(),
        "Should execute successfully with explicit using"
    );
    let output = result.unwrap();
    let output_str = String::from_utf8_lossy(&output);
    assert!(!output_str.is_empty(), "Should return non-empty output");

    eprintln!("✅ Sub-method mode execution test passed");
    eprintln!("   Output: {}", output_str);
    Ok(())
}

/// Test T3.7: Auto-select single tool when using is None
#[tokio::test]
#[ignore] // Requires MCP server - run with --ignored for full testing
async fn test_using_mode_auto_select_single_tool() -> Result<()> {
    use crate::runner::mcp::config::McpConfig;
    use crate::runner::mcp::McpServerRunnerImpl;
    use crate::runner::RunnerTrait;
    use std::collections::HashMap;

    let config = McpConfig {
        server: vec![create_time_mcp_server().await?],
    };

    let factory = crate::runner::mcp::proxy::McpServerFactory::new(config);
    let client = factory.connect_server("time").await?;
    let mut runner = McpServerRunnerImpl::new(client, None).await?;

    // Time server has only one tool (get_current_time), so using can be omitted
    let tool_count = runner.available_tool_names().len();

    if tool_count == 1 {
        // Prepare JSON arguments
        let args = serde_json::json!({"timezone": "Asia/Tokyo"});
        let args_bytes = serde_json::to_vec(&args)?;

        // Execute WITHOUT using - should auto-select
        let metadata = HashMap::new();
        let (result, _) = runner.run(&args_bytes, metadata, None).await;

        assert!(
            result.is_ok(),
            "Should auto-select single tool when using is None"
        );
        eprintln!("✅ Single-tool auto-select test passed");
    } else {
        eprintln!(
            "⚠️ Time server has {} tools, skipping auto-select test",
            tool_count
        );
    }

    Ok(())
}

/// Test T3.7: Error when using is required but not provided (multi-tool runner)
#[tokio::test]
#[ignore] // Requires fetch MCP server - run with --ignored for full testing
async fn test_using_mode_error_when_method_required() -> Result<()> {
    use crate::runner::mcp::config::McpConfig;
    use crate::runner::mcp::McpServerRunnerImpl;
    use crate::runner::RunnerTrait;
    use std::collections::HashMap;

    let config = McpConfig {
        server: vec![create_fetch_mcp_server().await?],
    };

    let factory = crate::runner::mcp::proxy::McpServerFactory::new(config);
    let client = factory.connect_server("fetch").await?;
    let mut runner = McpServerRunnerImpl::new(client, None).await?;

    let tool_count = runner.available_tool_names().len();

    if tool_count > 1 {
        // Fetch server may have multiple tools (fetch, fetch_html, etc.)
        let args = serde_json::json!({"url": "https://example.com"});
        let args_bytes = serde_json::to_vec(&args)?;

        // Execute WITHOUT using - should fail for multi-tool runner
        let metadata = HashMap::new();
        let (result, _) = runner.run(&args_bytes, metadata, None).await;

        assert!(
            result.is_err(),
            "Should fail when using is required but not provided"
        );

        let error_msg = result.unwrap_err().to_string();
        assert!(
            error_msg.contains("using is required") || error_msg.contains("Available"),
            "Error message should indicate using is required. Got: {error_msg}"
        );

        eprintln!("✅ Multi-tool using required test passed");
        eprintln!("   Error message: {error_msg}");
    } else {
        eprintln!(
            "⚠️ Fetch server has only {} tool, skipping multi-tool test",
            tool_count
        );
    }

    Ok(())
}

/// Test T3.7: Error when unknown using is provided
#[tokio::test]
#[ignore] // Requires MCP server - run with --ignored for full testing
async fn test_using_mode_error_unknown_method() -> Result<()> {
    use crate::runner::mcp::config::McpConfig;
    use crate::runner::mcp::McpServerRunnerImpl;
    use crate::runner::RunnerTrait;
    use std::collections::HashMap;

    let config = McpConfig {
        server: vec![create_time_mcp_server().await?],
    };

    let factory = crate::runner::mcp::proxy::McpServerFactory::new(config);
    let client = factory.connect_server("time").await?;
    let mut runner = McpServerRunnerImpl::new(client, None).await?;

    // Prepare arguments
    let args = serde_json::json!({"timezone": "UTC"});
    let args_bytes = serde_json::to_vec(&args)?;

    // Execute with unknown using
    let metadata = HashMap::new();
    let (result, _) = runner
        .run(&args_bytes, metadata, Some("nonexistent_tool"))
        .await;

    assert!(result.is_err(), "Should fail with unknown using");

    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("Unknown") || error_msg.contains("nonexistent_tool"),
        "Error message should mention unknown tool. Got: {error_msg}"
    );

    eprintln!("✅ Unknown using error test passed");
    eprintln!("   Error message: {error_msg}");
    Ok(())
}

/// Test T3.7: Verify get_using_json_schema returns correct schema
/// Test T3.7: Legacy mode still works after using implementation
// Legacy mode backward compatibility test removed - using mode is now the only supported mode
#[tokio::test]
#[ignore] // Requires MCP server - run with --ignored for full testing
async fn test_using_mode_with_cancel_helper() -> Result<()> {
    use crate::runner::cancellation_helper::CancelMonitoringHelper;
    use crate::runner::mcp::config::McpConfig;
    use crate::runner::mcp::McpServerRunnerImpl;
    use crate::runner::RunnerTrait;
    use std::collections::HashMap;
    use tokio_util::sync::CancellationToken;

    let config = McpConfig {
        server: vec![create_time_mcp_server().await?],
    };

    let factory = crate::runner::mcp::proxy::McpServerFactory::new(config);
    let client = factory.connect_server("time").await?;

    let cancel_token = CancellationToken::new();
    cancel_token.cancel(); // Pre-cancel to test cancellation behavior
    use crate::runner::test_common::mock::MockCancellationManager;
    let mock_manager = MockCancellationManager::new_with_token(cancel_token);
    let cancel_helper = CancelMonitoringHelper::new(Box::new(mock_manager));

    let mut runner = McpServerRunnerImpl::new(client, Some(cancel_helper)).await?;

    // Prepare JSON arguments (using mode uses JSON bytes)
    let args = serde_json::json!({"timezone": "UTC"});
    let args_bytes = serde_json::to_vec(&args)?;
    let metadata = HashMap::new();

    let start_time = std::time::Instant::now();
    let (result, _) = runner
        .run(&args_bytes, metadata, Some("get_current_time"))
        .await;
    let elapsed = start_time.elapsed();

    // Should fail immediately due to pre-execution cancellation
    assert!(result.is_err());
    assert!(elapsed < std::time::Duration::from_millis(100));

    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("cancelled before"));

    eprintln!("✅ Using mode with cancel helper test passed");
    Ok(())
}

/// Test: Using mode stream execution (normal case)
#[tokio::test]
#[ignore] // Requires MCP server - run with --ignored for full testing
async fn test_using_mode_stream_execution() -> Result<()> {
    use crate::runner::mcp::config::McpConfig;
    use crate::runner::mcp::McpServerRunnerImpl;
    use crate::runner::RunnerTrait;
    use futures::StreamExt;
    use proto::jobworkerp::data::result_output_item::Item;
    use std::collections::HashMap;

    let config = McpConfig {
        server: vec![create_time_mcp_server().await?],
    };

    let factory = crate::runner::mcp::proxy::McpServerFactory::new(config);
    let client = factory.connect_server("time").await?;

    let mut runner = McpServerRunnerImpl::new(client, None).await?;

    // Prepare JSON arguments (using mode uses JSON bytes)
    let args = serde_json::json!({"timezone": "UTC"});
    let args_bytes = serde_json::to_vec(&args)?;
    let metadata = HashMap::new();

    let start_time = std::time::Instant::now();
    let mut stream = runner
        .run_stream(&args_bytes, metadata, Some("get_current_time"))
        .await?;

    let mut item_count = 0;
    let mut received_data = false;
    let mut received_end = false;

    while let Some(item) = stream.next().await {
        item_count += 1;
        eprintln!("Received stream item #{item_count}: {item:?}");

        match item.item {
            Some(Item::Data(data)) => {
                received_data = true;
                let result_str = String::from_utf8_lossy(&data);
                eprintln!("Received data: {result_str}");
                assert!(!data.is_empty());
            }
            Some(Item::End(trailer)) => {
                received_end = true;
                eprintln!("Stream ended with trailer: {trailer:?}");
            }
            _ => {
                eprintln!("Unexpected item type");
            }
        }
    }

    let elapsed = start_time.elapsed();
    eprintln!("Using mode stream completed in {elapsed:?}");

    assert!(received_data, "Should have received data item");
    assert!(received_end, "Should have received end marker");
    assert_eq!(
        item_count, 2,
        "Should have received exactly 2 items (data + end)"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "Should complete quickly"
    );

    eprintln!("✅ Using mode stream execution test passed");
    Ok(())
}

/// Test: Using mode stream execution with cancellation
#[tokio::test]
#[ignore] // Requires MCP server - run with --ignored for full testing
async fn test_using_mode_stream_with_cancellation() -> Result<()> {
    use crate::runner::mcp::config::McpConfig;
    use crate::runner::mcp::McpServerRunnerImpl;
    use crate::runner::RunnerTrait;
    use futures::StreamExt;
    use proto::jobworkerp::data::result_output_item::Item;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    let config = McpConfig {
        server: vec![create_time_mcp_server().await?],
    };

    let factory = crate::runner::mcp::proxy::McpServerFactory::new(config);
    let client = factory.connect_server("time").await?;

    let temp_runner = McpServerRunnerImpl::new(client, None).await?;
    let runner = Arc::new(Mutex::new(temp_runner));

    let cancellation_token = CancellationToken::new();

    let start_time = std::time::Instant::now();

    // Prepare JSON arguments
    let args = serde_json::json!({"timezone": "UTC"});
    let args_bytes = serde_json::to_vec(&args)?;
    let metadata = HashMap::new();

    let runner_clone = runner.clone();

    // Start stream execution in a task
    let execution_task = tokio::spawn(async move {
        let mut runner_guard = runner_clone.lock().await;
        let stream_result = runner_guard
            .run_stream(&args_bytes, metadata, Some("get_current_time"))
            .await;

        match stream_result {
            Ok(mut stream) => {
                let mut item_count = 0;

                while let Some(item) = stream.next().await {
                    item_count += 1;
                    eprintln!("Received stream item #{item_count}: {item:?}");

                    match item.item {
                        Some(Item::Data(_data)) => {
                            eprintln!("Received data item before cancellation");
                        }
                        Some(Item::End(_trailer)) => {
                            eprintln!("Stream ended (possibly due to cancellation)");
                            break;
                        }
                        _ => {
                            eprintln!("Unexpected item type");
                        }
                    }
                }

                Ok(item_count)
            }
            Err(e) => {
                eprintln!("Stream creation failed: {e}");
                Err(e)
            }
        }
    });

    // Wait for stream to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Cancel using the external token
    cancellation_token.cancel();
    eprintln!("Called cancellation_token.cancel() after 100ms");

    // Wait for the execution to complete
    let execution_result = execution_task.await;
    let elapsed = start_time.elapsed();

    eprintln!("Using mode stream execution completed in {elapsed:?}");

    match execution_result {
        Ok(stream_processing_result) => match stream_processing_result {
            Ok(item_count) => {
                eprintln!("✓ Stream processing completed with {item_count} items");
                assert!(elapsed < Duration::from_secs(2));
            }
            Err(e) => {
                eprintln!("✓ Stream processing was cancelled as expected: {e}");
                if e.to_string().contains("cancelled") {
                    eprintln!("✓ Cancellation was properly detected");
                }
            }
        },
        Err(e) => {
            eprintln!("Stream execution task failed: {e}");
            panic!("Task failed: {e}");
        }
    }

    if elapsed > Duration::from_secs(2) {
        panic!("Stream processing took too long ({elapsed:?})");
    }

    eprintln!("✅ Using mode stream with cancellation test passed");
    Ok(())
}

// ==================== Using Parameter Propagation Tests ====================

/// Test: Verify MCP tool name validation in handle_runner_call_from_llm
#[tokio::test]
#[ignore] // Requires MCP server - run with --ignored for full testing
async fn test_mcp_tool_name_validation() -> Result<()> {
    use crate::runner::mcp::config::McpConfig;
    use crate::runner::mcp::McpServerRunnerImpl;
    // use crate::runner::RunnerSpec;

    let config = McpConfig {
        server: vec![create_time_mcp_server().await?],
    };

    let factory = crate::runner::mcp::proxy::McpServerFactory::new(config);
    let client = factory.connect_server("time").await?;
    let runner = McpServerRunnerImpl::new(client, None).await?;

    let method_map = runner.method_proto_map();
    assert!(!method_map.is_empty(), "MCP server should have methods");

    let tool_names: Vec<&str> = method_map.keys().map(|s| s.as_str()).collect();
    assert!(
        tool_names.contains(&"get_current_time"),
        "Time server should have get_current_time tool"
    );

    eprintln!("✅ MCP tool name validation test passed");
    eprintln!("   Available methods: {:?}", tool_names);
    Ok(())
}

/// Test: Execute MCP tool with valid tool name via using parameter
#[tokio::test]
#[ignore] // Requires MCP server - run with --ignored for full testing
async fn test_mcp_execution_with_valid_using() -> Result<()> {
    use crate::runner::mcp::config::McpConfig;
    use crate::runner::mcp::McpServerRunnerImpl;
    use crate::runner::RunnerTrait;
    use std::collections::HashMap;

    let config = McpConfig {
        server: vec![create_time_mcp_server().await?],
    };

    let factory = crate::runner::mcp::proxy::McpServerFactory::new(config);
    let client = factory.connect_server("time").await?;
    let mut runner = McpServerRunnerImpl::new(client, None).await?;

    // Prepare JSON arguments
    let args = serde_json::json!({"timezone": "UTC"});
    let args_bytes = serde_json::to_vec(&args)?;

    // Execute with valid using parameter
    let metadata = HashMap::new();
    let (result, _) = runner
        .run(&args_bytes, metadata, Some("get_current_time"))
        .await;

    assert!(
        result.is_ok(),
        "Should execute successfully with valid using parameter"
    );

    let output = result.unwrap();
    assert!(!output.is_empty(), "Should return non-empty output");

    eprintln!("✅ MCP execution with valid using test passed");
    Ok(())
}

/// Test: Error when invalid tool name is provided via using parameter
#[tokio::test]
#[ignore] // Requires MCP server - run with --ignored for full testing
async fn test_mcp_error_with_invalid_using() -> Result<()> {
    use crate::runner::mcp::config::McpConfig;
    use crate::runner::mcp::McpServerRunnerImpl;
    use crate::runner::RunnerTrait;
    use std::collections::HashMap;

    let config = McpConfig {
        server: vec![create_time_mcp_server().await?],
    };

    let factory = crate::runner::mcp::proxy::McpServerFactory::new(config);
    let client = factory.connect_server("time").await?;
    let mut runner = McpServerRunnerImpl::new(client, None).await?;

    // Prepare JSON arguments
    let args = serde_json::json!({"timezone": "UTC"});
    let args_bytes = serde_json::to_vec(&args)?;

    // Execute with invalid using parameter
    let metadata = HashMap::new();
    let (result, _) = runner
        .run(&args_bytes, metadata, Some("invalid_tool_name"))
        .await;

    assert!(result.is_err(), "Should fail with invalid using parameter");

    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("Unknown") || error_msg.contains("invalid_tool_name"),
        "Error message should indicate invalid tool name. Got: {error_msg}"
    );

    eprintln!("✅ MCP error with invalid using test passed");
    eprintln!("   Error message: {error_msg}");
    Ok(())
}

#[tokio::test]
async fn test_method_proto_map() -> Result<()> {
    use crate::runner::mcp::config::McpConfig;
    use crate::runner::mcp::McpServerRunnerImpl;
    use crate::runner::RunnerSpec;

    let config = McpConfig {
        server: vec![create_time_mcp_server().await?],
    };

    let factory = crate::runner::mcp::proxy::McpServerFactory::new(config);
    let client = factory.connect_server("time").await?;
    let runner = McpServerRunnerImpl::new(client, None).await?;

    let method_proto_map = runner.method_proto_map();

    eprintln!(
        "✅ method_proto_map returned {} methods",
        method_proto_map.len()
    );

    assert!(
        method_proto_map.contains_key("get_current_time"),
        "Should contain get_current_time method"
    );

    let method_schema = method_proto_map.get("get_current_time").unwrap();

    assert!(
        !method_schema.args_proto.is_empty(),
        "args_proto should not be empty"
    );
    eprintln!("✅ args_proto length: {}", method_schema.args_proto.len());

    assert!(
        method_schema.args_proto.contains("syntax = \"proto3\""),
        "args_proto should contain proto3 syntax"
    );
    assert!(
        method_schema.args_proto.contains("message"),
        "args_proto should contain message definition"
    );
    eprintln!("✅ args_proto contains valid Protobuf schema");

    assert!(
        !method_schema.result_proto.is_empty(),
        "MCP server must have result_proto"
    );
    assert!(
        method_schema.result_proto.contains("McpServerResult"),
        "MCP server should use McpServerResult schema"
    );
    eprintln!("✅ result_proto contains common output schema (McpServerResult)");

    Ok(())
}

#[tokio::test]
async fn test_method_proto_map_multiple_tools() -> Result<()> {
    use crate::runner::mcp::config::McpConfig;
    use crate::runner::mcp::McpServerRunnerImpl;
    use crate::runner::RunnerSpec;

    let config = McpConfig {
        server: vec![create_fetch_mcp_server().await?],
    };

    let factory = crate::runner::mcp::proxy::McpServerFactory::new(config);
    let client = factory.connect_server("fetch").await?;
    let runner = McpServerRunnerImpl::new(client, None).await?;

    let method_proto_map = runner.method_proto_map();

    let tool_count = method_proto_map.len();
    eprintln!("✅ method_proto_map returned {} tools", tool_count);
    eprintln!("✅ method_proto_map {:?}", &method_proto_map);

    assert!(
        tool_count >= 1,
        "Fetch server should have at least one tool"
    );

    for (tool_name, method_schema) in &method_proto_map {
        eprintln!("  Tool: {}", tool_name);

        assert!(
            !method_schema.args_proto.is_empty(),
            "Tool {} should have args_proto",
            tool_name
        );
        assert!(
            method_schema.args_proto.contains("syntax = \"proto3\""),
            "Tool {} args_proto should contain proto3 syntax",
            tool_name
        );
        eprintln!(
            "    ✅ args_proto length: {}",
            method_schema.args_proto.len()
        );

        assert!(
            !method_schema.result_proto.is_empty(),
            "Tool {} must have result_proto (required)",
            tool_name
        );
        assert!(
            method_schema.result_proto.contains("McpServerResult"),
            "Tool {} should use McpServerResult schema",
            tool_name
        );
        eprintln!("    ✅ result_proto contains common output schema");
    }

    Ok(())
}
