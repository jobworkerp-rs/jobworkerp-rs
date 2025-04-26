use crate::runner::mcp::client::{McpServerConfig, McpServerTransportConfig};
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
        format!("{}", server_path_str)
    } else {
        format!("{}:{}", current_pythonpath, server_path_str)
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

#[tokio::test]
async fn test_time_mcp_server() -> Result<()> {
    use crate::runner::mcp::client::McpConfig;

    let config = McpConfig {
        server: vec![create_time_mcp_server().await?],
    };

    // Create McpClients
    let factory = crate::runner::mcp::client::McpServerFactory::new(config);
    let mut clients = factory.test_all().await?;
    assert_eq!(clients.len(), 1);
    let client = clients.pop().unwrap();
    println!("Time server tools: {:?}", client);
    // remove client for test
    client.cancel().await?;

    // create server by the name
    let client = factory.create_server("time").await?;

    // Call the get_current_time tool
    let result = client
        .call_tool("get_current_time", serde_json::json!({"timezone": "UTC"}))
        .await?;

    // Display results
    println!("Time server result: {:?}", result);

    // Validate results
    assert!(!result.content.is_empty());

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
