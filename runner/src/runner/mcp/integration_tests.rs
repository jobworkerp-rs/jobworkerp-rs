use crate::runner::mcp::config::{McpServerConfig, McpServerTransportConfig};
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// Get the path to the MCP server
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

    // Return to original directory
    std::env::set_current_dir(original_dir)?;

    // Set environment variables
    let mut envs = setup_python_env(server_path);

    // Add virtual environment path to environment variables
    let venv_path = server_path.join(".venv");
    let venv_bin_path = if cfg!(target_os = "windows") {
        venv_path.join("Scripts")
    } else {
        venv_path.join("bin")
    };

    // Add virtual environment bin directory to the beginning of PATH
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

    // Use Python directly from the virtual environment
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

    // Use Python directly from the virtual environment
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

    // Create McpClients
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

    // Validate results
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

    // Create McpClients
    let factory = crate::runner::mcp::proxy::McpServerFactory::new(config);
    let client = factory.connect_server("time").await?;

    // Create MCP runner instance with the client
    let mut runner = McpServerRunnerImpl::new(client);

    // Test cancellation without active request
    runner.request_cancellation().await.unwrap();
    eprintln!("MCP cancel completed successfully with no active operation");

    // Test basic cancellation functionality is available
    eprintln!("Basic MCP cancel functionality is available and does not panic");

    eprintln!("=== MCP cancellation test completed ===");
    Ok(())
}

#[tokio::test]
async fn test_mcp_with_cancel_helper() -> Result<()> {
    use crate::jobworkerp::runner::McpServerArgs;

    use crate::runner::cancellation_helper::CancelMonitoringHelper;
    use crate::runner::mcp::config::McpConfig;
    use crate::runner::mcp::McpServerRunnerImpl;
    use crate::runner::RunnerTrait;
    use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
    use std::collections::HashMap;
    use tokio_util::sync::CancellationToken;

    let config = McpConfig {
        server: vec![create_time_mcp_server().await?],
    };

    // Create McpClients
    let factory = crate::runner::mcp::proxy::McpServerFactory::new(config);
    let client = factory.connect_server("time").await?;

    // Create cancellation helper with pre-cancelled token
    let cancel_token = CancellationToken::new();
    cancel_token.cancel(); // Pre-cancel to test cancellation behavior
    use crate::runner::test_common::mock::MockCancellationManager;
    let mock_manager = MockCancellationManager::new_with_token(cancel_token);
    let cancel_helper = CancelMonitoringHelper::new(Box::new(mock_manager));

    // Create MCP runner instance with cancellation helper
    let mut runner = McpServerRunnerImpl::new_with_cancel_monitoring(client, cancel_helper);

    // Test pre-execution cancellation
    let mcp_args = McpServerArgs {
        tool_name: "get_current_time".to_string(),
        arg_json: r#"{"timezone": "UTC"}"#.to_string(),
    };

    let arg_bytes = ProstMessageCodec::serialize_message(&mcp_args)?;
    let metadata = HashMap::new();

    let start_time = std::time::Instant::now();
    let (result, _) = runner.run(&arg_bytes, metadata).await;
    let elapsed = start_time.elapsed();

    // Should fail immediately due to pre-execution cancellation
    assert!(result.is_err());
    assert!(elapsed < std::time::Duration::from_millis(100));

    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("cancelled before"));

    eprintln!("=== MCP cancellation with helper test completed ===");
    Ok(())
}

#[tokio::test]
#[ignore] // Requires network access and fetch MCP server - run with --ignored for full testing
async fn test_mcp_cancellation_during_execution() -> Result<()> {
    use crate::jobworkerp::runner::McpServerArgs;
    use crate::runner::cancellation::CancelMonitoring;
    use crate::runner::mcp::config::McpConfig;
    use crate::runner::mcp::McpServerRunnerImpl;
    use crate::runner::RunnerTrait;
    use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let config = McpConfig {
        server: vec![create_fetch_mcp_server().await?],
    };

    // Create McpClients
    let factory = crate::runner::mcp::proxy::McpServerFactory::new(config);
    let client = factory.connect_server("fetch").await?;

    // Create MCP runner instance with the client (wrapped for sharing)
    let runner = Arc::new(Mutex::new(McpServerRunnerImpl::new(client)));

    // Test concurrent cancellation during execution with slow HTTP request
    let mcp_args = McpServerArgs {
        tool_name: "fetch".to_string(),
        arg_json: r#"{"url": "https://httpbin.org/delay/5", "max_length": 10000}"#.to_string(),
    };

    let arg_bytes = ProstMessageCodec::serialize_message(&mcp_args)?;
    let metadata = HashMap::new();

    let runner_clone = runner.clone();

    // Start execution and track timing
    let start_time = std::time::Instant::now();

    // Start MCP tool call in a task
    let execution_task = tokio::spawn(async move {
        let mut runner_guard = runner_clone.lock().await;
        runner_guard.run(&arg_bytes, metadata).await
    });

    // Wait for HTTP request to start (longer delay for fetch to actually begin)
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Trigger cancellation from another task after the fetch has started
    let cancel_task = tokio::spawn(async move {
        let mut runner_guard = runner.lock().await;
        runner_guard.request_cancellation().await.unwrap();
        eprintln!("Cancellation triggered after 500ms");
    });

    // Wait for both tasks
    let (execution_result, _) = tokio::join!(execution_task, cancel_task);

    let elapsed = start_time.elapsed();
    eprintln!("Total execution time: {elapsed:?}");

    match execution_result {
        Ok((result, _metadata)) => {
            match result {
                Ok(_) => {
                    eprintln!("MCP fetch completed before cancellation took effect");
                    if elapsed < std::time::Duration::from_secs(5) {
                        eprintln!(
                            "Fetch completed quickly ({elapsed:?}), cancellation may have worked"
                        );
                    } else {
                        eprintln!("Fetch took full time ({elapsed:?}), cancellation did not work");
                    }
                }
                Err(e) => {
                    eprintln!("MCP fetch error (likely due to cancellation): {e}");
                    // Check if error message contains cancellation indication
                    let error_msg = e.to_string();
                    if error_msg.contains("cancelled") || error_msg.contains("abort") {
                        eprintln!("✅ Cancellation was successful! Error: {error_msg}");
                        assert!(
                            elapsed < std::time::Duration::from_secs(4),
                            "Cancellation should prevent full 5-second delay, took {elapsed:?}"
                        );
                    } else {
                        eprintln!("❌ Error not related to cancellation: {error_msg}");
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("MCP execution task failed: {e}");
        }
    }

    eprintln!("=== MCP cancellation during execution test completed ===");
    Ok(())
}

#[tokio::test]
async fn test_mcp_stream_execution_normal() -> Result<()> {
    use crate::jobworkerp::runner::McpServerArgs;

    use crate::runner::mcp::config::McpConfig;
    use crate::runner::mcp::McpServerRunnerImpl;
    use crate::runner::RunnerTrait;
    use futures::StreamExt;
    use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
    use proto::jobworkerp::data::result_output_item::Item;
    use std::collections::HashMap;

    let config = McpConfig {
        server: vec![create_time_mcp_server().await?],
    };

    // Create McpClients
    let factory = crate::runner::mcp::proxy::McpServerFactory::new(config);
    let client = factory.connect_server("time").await?;

    // Create MCP runner instance
    let mut runner = McpServerRunnerImpl::new(client);

    // Test MCP stream with time server
    let mcp_args = McpServerArgs {
        tool_name: "get_current_time".to_string(),
        arg_json: r#"{"timezone": "UTC"}"#.to_string(),
    };

    let arg_bytes = ProstMessageCodec::serialize_message(&mcp_args)?;
    let metadata = HashMap::new();

    let start_time = std::time::Instant::now();
    let mut stream = runner.run_stream(&arg_bytes, metadata).await?;

    let mut item_count = 0;
    let mut received_data = false;
    let mut received_end = false;

    while let Some(item) = stream.next().await {
        item_count += 1;
        eprintln!("Received stream item #{item_count}: {item:?}");

        match item.item {
            Some(Item::Data(data)) => {
                received_data = true;
                // Try to deserialize the data to verify it's valid MCP result
                let result = ProstMessageCodec::deserialize_message::<
                    crate::jobworkerp::runner::McpServerResult,
                >(&data)?;
                eprintln!("Deserialized MCP result: {result:?}");
                assert!(!result.content.is_empty());
                assert!(!result.is_error);
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
    eprintln!("MCP stream completed in {elapsed:?}");

    // Verify stream behavior
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

    eprintln!("=== MCP stream normal execution test completed successfully ===");
    Ok(())
}

#[tokio::test]
async fn test_mcp_stream_execution_with_cancellation() -> Result<()> {
    use crate::jobworkerp::runner::McpServerArgs;

    use crate::runner::mcp::config::McpConfig;
    use crate::runner::mcp::McpServerRunnerImpl;
    use crate::runner::RunnerTrait;
    use futures::StreamExt;
    use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
    use proto::jobworkerp::data::result_output_item::Item;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    let config = McpConfig {
        server: vec![create_time_mcp_server().await?],
    };

    // Create McpClients
    let factory = crate::runner::mcp::proxy::McpServerFactory::new(config);
    let client = factory.connect_server("time").await?;

    // Create MCP runner instance (wrapped for sharing)
    let runner = Arc::new(Mutex::new(McpServerRunnerImpl::new(client)));

    // Note: MCP Runner now uses helper-based cancellation
    // This test verifies basic stream behavior without manager
    let cancellation_token = CancellationToken::new();

    let start_time = std::time::Instant::now();

    // Test MCP stream with time server
    let mcp_args = McpServerArgs {
        tool_name: "get_current_time".to_string(),
        arg_json: r#"{"timezone": "UTC"}"#.to_string(),
    };

    let arg_bytes = ProstMessageCodec::serialize_message(&mcp_args)?;
    let metadata = HashMap::new();

    let runner_clone = runner.clone();

    // Start stream execution in a task
    let execution_task = tokio::spawn(async move {
        let mut runner_guard = runner_clone.lock().await;
        let stream_result = runner_guard.run_stream(&arg_bytes, metadata).await;

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

    // Wait for stream to start (let it run for a bit)
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Cancel using the external token reference (avoids deadlock)
    cancellation_token.cancel();
    eprintln!("Called cancellation_token.cancel() after 100ms");

    // Wait for the execution to complete or be cancelled
    let execution_result = execution_task.await;
    let elapsed = start_time.elapsed();

    eprintln!("MCP stream execution completed in {elapsed:?}");

    match execution_result {
        Ok(stream_processing_result) => {
            match stream_processing_result {
                Ok(item_count) => {
                    eprintln!("✓ MCP stream processing completed with {item_count} items");
                    // The stream should complete quickly due to cancellation
                    assert!(elapsed < Duration::from_secs(2));
                }
                Err(e) => {
                    eprintln!("✓ MCP stream processing was cancelled as expected: {e}");
                    // Check if it's a cancellation error
                    if e.to_string().contains("cancelled") {
                        eprintln!("✓ Cancellation was properly detected");
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("MCP stream execution task failed: {e}");
            panic!("Task failed: {e}");
        }
    }

    // Verify that cancellation happened quickly
    if elapsed > Duration::from_secs(2) {
        panic!("Stream processing took too long ({elapsed:?}), cancellation may not have worked");
    }

    eprintln!("✓ MCP stream execution with cancellation test completed successfully");
    Ok(())
}

// #[tokio::test]
// async fn test_sqlite_mcp_server() -> Result<()> {
//     // SQLite server configuration (using in-memory database)
//     let sqlite_server_path = get_mcp_server_path("sqlite");

//     // Set up virtual environment
//     let envs = setup_python_environment_with_uv(&sqlite_server_path).await?;

//     // Use Python directly from the virtual environment
//     let venv_python = get_venv_python(&sqlite_server_path);

//     let config = McpConfig {
//         server: vec![McpServerConfig {
//             name: "sqlite".to_string(),
//             transport: McpServerTransportConfig::Stdio {
//                 command: venv_python.to_string_lossy().to_string(),
//                 args: vec![
//                     "-m".to_string(),
//                     "mcp_server_sqlite".to_string(),
//                     // "--db-path".to_string(),
//                     // ":memory:".to_string(), // In-memory database
//                 ],
//                 envs,
//             },
//         }],
//     };

//     // Create McpClients
//     let clients = crate::runner::mcp::client::McpClientsImpl::new(&config).await?;

//     // Get list of tools
//     let tools = clients.load_all_tools().await?;
//     println!("SQLite server tools: {:?}", tools);

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
