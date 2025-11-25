// Integration tests for McpToolRunnerImpl
//
// Tests the Runner trait implementation for individual MCP tools

use super::McpToolRunnerImpl;
use crate::runner::mcp::config::{McpConfig, McpServerConfig, McpServerTransportConfig};
use crate::runner::mcp::proxy::McpServerFactory;
use crate::runner::{RunnerSpec, RunnerTrait};
use anyhow::Result;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

// Helper: Get MCP server path
fn get_mcp_server_path(server_name: &str) -> PathBuf {
    PathBuf::from("../modules/mcp-servers/src").join(server_name)
}

// Helper: Setup Python environment
fn setup_python_env(server_path: &std::path::Path) -> HashMap<String, String> {
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

// Helper: Setup Python environment with uv
async fn setup_python_environment_with_uv(
    server_path: &PathBuf,
) -> Result<HashMap<String, String>> {
    let original_dir = std::env::current_dir()?;
    std::env::set_current_dir(server_path)?;

    let uv_venv = std::process::Command::new("uv").args(["venv"]).status()?;
    if !uv_venv.success() {
        std::env::set_current_dir(original_dir)?;
        return Err(anyhow::anyhow!(
            "Failed to create virtual environment with uv"
        ));
    }

    let uv_install = std::process::Command::new("uv")
        .args(["pip", "install", "-e", "."])
        .status()?;
    if !uv_install.success() {
        std::env::set_current_dir(original_dir)?;
        return Err(anyhow::anyhow!("Failed to install dependencies with uv"));
    }

    std::env::set_current_dir(original_dir)?;

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

// Helper: Get venv Python path
fn get_venv_python(server_path: &std::path::Path) -> PathBuf {
    let venv_path = server_path.join(".venv");
    let venv_bin_path = if cfg!(target_os = "windows") {
        venv_path.join("Scripts")
    } else {
        venv_path.join("bin")
    };
    venv_bin_path.join("python")
}

// Helper: Create time MCP server config
async fn create_time_mcp_server() -> Result<McpServerConfig> {
    let time_server_path = get_mcp_server_path("time");
    let envs = setup_python_environment_with_uv(&time_server_path).await?;
    let venv_python = get_venv_python(&time_server_path);

    Ok(McpServerConfig {
        name: "time".to_string(),
        description: None,
        transport: McpServerTransportConfig::Stdio {
            command: venv_python.to_string_lossy().to_string(),
            args: vec![
                "-m".to_string(),
                "mcp_server_time".to_string(),
                "--local-timezone".to_string(),
                "UTC".to_string(),
            ],
            envs,
        },
    })
}

#[tokio::test]
async fn test_mcp_tool_runner_basic_execution() -> Result<()> {
    // Setup MCP server
    let config = McpConfig {
        server: vec![create_time_mcp_server().await?],
    };

    let factory = McpServerFactory::new(config);
    let proxy = factory.connect_server("time").await?;

    // Get tool schema
    let tools = proxy.load_tools().await?;
    let get_current_time_tool = tools
        .iter()
        .find(|t| t.name == "get_current_time")
        .expect("get_current_time tool not found");

    // Create McpToolRunnerImpl
    let tool_schema = serde_json::Value::Object((*get_current_time_tool.input_schema).clone());
    let mut runner = McpToolRunnerImpl::new(
        Arc::new(proxy),
        "time".to_string(),
        "get_current_time".to_string(),
        tool_schema,
    )?;

    // Test RunnerSpec methods
    assert_eq!(runner.name(), "time___get_current_time");
    assert_eq!(runner.runner_settings_proto(), "");

    // Verify job_args_proto generates valid protobuf
    let proto_def = runner.job_args_proto();
    assert!(proto_def.contains("syntax = \"proto3\";"));
    assert!(proto_def.contains("message TimeGetCurrentTimeArgs"));

    // Test load() method (McpToolRunnerImpl doesn't use settings)
    runner.load(vec![]).await?;

    // Test run() method with valid input
    // McpToolRunnerImpl expects Protobuf binary based on generated schema
    use command_utils::protobuf::ProtobufDescriptor;

    let args_json = serde_json::json!({
        "timezone": "UTC"
    });

    // Get the proto definition and create descriptor
    let proto_def = runner.job_args_proto();
    let descriptor = ProtobufDescriptor::new(&proto_def)?;

    // Convert JSON to protobuf binary using the dynamic descriptor
    let message_name = "jobworkerp.runner.mcp.time.TimeGetCurrentTimeArgs";
    let dynamic_msg = ProtobufDescriptor::get_message_from_json(
        descriptor.get_message_by_name(message_name).unwrap(),
        &args_json.to_string(),
    )?;
    let args_bytes = ProtobufDescriptor::serialize_message(&dynamic_msg);

    let (result, metadata) = runner.run(&args_bytes, HashMap::new()).await;

    // Verify result
    let result_bytes = result.expect("run() should succeed");
    assert!(!result_bytes.is_empty(), "Result should not be empty");
    println!("Result metadata: {:?}", metadata);

    // Decode result as McpServerResult
    use crate::jobworkerp::runner::McpServerResult;
    let mcp_result: McpServerResult = ProstMessageCodec::deserialize_message(&result_bytes)?;
    assert!(!mcp_result.content.is_empty(), "Content should not be empty");
    assert!(!mcp_result.is_error, "Should not be an error");
    println!("result: {:?}", mcp_result.content);

    Ok(())
}

#[tokio::test]
async fn test_mcp_tool_runner_validation_error() -> Result<()> {
    // Setup MCP server
    let config = McpConfig {
        server: vec![create_time_mcp_server().await?],
    };

    let factory = McpServerFactory::new(config);
    let proxy = factory.connect_server("time").await?;

    // Get tool schema
    let tools = proxy.load_tools().await?;
    let get_current_time_tool = tools
        .iter()
        .find(|t| t.name == "get_current_time")
        .expect("get_current_time tool not found");

    // Create McpToolRunnerImpl
    let tool_schema = serde_json::Value::Object((*get_current_time_tool.input_schema).clone());
    let mut runner = McpToolRunnerImpl::new(
        Arc::new(proxy),
        "time".to_string(),
        "get_current_time".to_string(),
        tool_schema,
    )?;

    runner.load(vec![]).await?;

    // Test with invalid input (wrong type for timezone field)
    use command_utils::protobuf::ProtobufDescriptor;

    let invalid_args_json = serde_json::json!({"timezone": 123}); // Should be string

    // Get proto definition and create descriptor
    let proto_def = runner.job_args_proto();
    let descriptor = ProtobufDescriptor::new(&proto_def)?;

    // Try to convert invalid JSON to protobuf - this should fail at JSON->Protobuf conversion
    let message_name = "jobworkerp.runner.mcp.time.TimeGetCurrentTimeArgs";
    let convert_result = ProtobufDescriptor::get_message_from_json(
        descriptor.get_message_by_name(message_name).unwrap(),
        &invalid_args_json.to_string(),
    );

    // The validation error should occur during JSON->Protobuf conversion
    let (result, _metadata) = if let Ok(dynamic_msg) = convert_result {
        let args_bytes = ProtobufDescriptor::serialize_message(&dynamic_msg);
        runner.run(&args_bytes, HashMap::new()).await
    } else {
        // Expected: JSON schema validation fails before Protobuf conversion
        assert!(convert_result.is_err(), "Should fail to convert invalid JSON to Protobuf");
        return Ok(());
    };

    // Should return validation error
    assert!(result.is_err(), "Should fail with validation error");
    let error_msg = result.unwrap_err().to_string();
    println!("Validation error: {}", error_msg);

    Ok(())
}

#[tokio::test]
async fn test_mcp_tool_runner_resource_content_support() -> Result<()> {
    // This test would require an MCP server that returns Resource content types
    // For now, we verify that the runner can be created with appropriate schema

    let config = McpConfig {
        server: vec![create_time_mcp_server().await?],
    };

    let factory = McpServerFactory::new(config);
    let proxy = factory.connect_server("time").await?;

    // Get any tool
    let tools = proxy.load_tools().await?;
    let tool = tools.first().expect("At least one tool should exist");

    // Create runner
    let tool_schema = serde_json::Value::Object((*tool.input_schema).clone());
    let runner = McpToolRunnerImpl::new(
        Arc::new(proxy),
        "time".to_string(),
        tool.name.to_string(),
        tool_schema,
    )?;

    // Verify runner was created successfully
    assert!(runner.name().contains("___"));

    Ok(())
}

#[tokio::test]
async fn test_mcp_tool_runner_streaming_not_supported() -> Result<()> {
    let config = McpConfig {
        server: vec![create_time_mcp_server().await?],
    };

    let factory = McpServerFactory::new(config);
    let proxy = factory.connect_server("time").await?;

    let tools = proxy.load_tools().await?;
    let tool = tools.first().expect("At least one tool should exist");

    let tool_schema = serde_json::Value::Object((*tool.input_schema).clone());
    let mut runner = McpToolRunnerImpl::new(
        Arc::new(proxy),
        "time".to_string(),
        tool.name.to_string(),
        tool_schema,
    )?;

    // Test that streaming is not supported
    use command_utils::protobuf::ProtobufDescriptor;

    let args_json = serde_json::json!({"timezone": "UTC"});

    // Convert JSON to protobuf binary
    let proto_def = runner.job_args_proto();
    let descriptor = ProtobufDescriptor::new(&proto_def)?;
    let message_name = format!(
        "jobworkerp.runner.mcp.time.{}Args",
        command_utils::text::TextUtil::to_pascal_case(&format!("time_{}", tool.name))
    );
    let dynamic_msg = ProtobufDescriptor::get_message_from_json(
        descriptor.get_message_by_name(&message_name).unwrap(),
        &args_json.to_string(),
    )?;
    let args_bytes = ProtobufDescriptor::serialize_message(&dynamic_msg);

    let result = runner.run_stream(&args_bytes, HashMap::new()).await;

    // run_stream should return an error for McpToolRunnerImpl
    match result {
        Err(e) => {
            let error_msg = e.to_string();
            assert!(
                error_msg.contains("not yet supported") || error_msg.contains("not supported"),
                "Error message should indicate streaming is not supported: {}",
                error_msg
            );
        }
        Ok(_) => panic!("Streaming should not be supported for McpToolRunnerImpl"),
    }

    Ok(())
}
