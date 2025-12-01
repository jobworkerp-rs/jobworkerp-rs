//! Unit tests for ProtobufHelper trait methods
//!
//! Tests for method_proto_map-based schema parsing

use crate::jobworkerp::data::{MethodProtoMap, MethodSchema, RunnerData, StreamingOutputType};
use crate::ProtobufHelper;
use std::collections::HashMap;

/// Test helper struct that implements ProtobufHelper
struct TestProtobufHelper;
impl ProtobufHelper for TestProtobufHelper {}

/// Create a test RunnerData with valid method_proto_map
fn create_test_runner_data_with_methods(methods: HashMap<String, MethodSchema>) -> RunnerData {
    RunnerData {
        name: "test_runner".to_string(),
        description: "Test runner".to_string(),
        runner_type: 0, // COMMAND
        runner_settings_proto: String::new(),
        definition: String::new(),
        method_proto_map: Some(MethodProtoMap { schemas: methods }),
    }
}

/// Create a valid CommandArgs proto schema
fn valid_command_args_proto() -> String {
    r#"
syntax = "proto3";
package test;
message CommandArgs {
    string command = 1;
}
"#
    .to_string()
}

/// Create a valid CommandResult proto schema
fn valid_command_result_proto() -> String {
    r#"
syntax = "proto3";
package test;
message CommandResult {
    int32 exit_code = 1;
    bytes stdout = 2;
}
"#
    .to_string()
}

/// Create an invalid proto schema (missing message definition)
fn invalid_proto_schema() -> String {
    r#"
syntax = "proto3";
package test;
// No message definition
"#
    .to_string()
}

// ==================== parse_job_args_schema_descriptor Tests ====================

#[test]
fn test_parse_job_args_valid_method() {
    // Valid method with args_proto
    let mut methods = HashMap::new();
    methods.insert(
        "run".to_string(),
        MethodSchema {
            args_proto: valid_command_args_proto(),
            result_proto: valid_command_result_proto(),
            description: Some("Test method".to_string()),
            output_type: StreamingOutputType::Both as i32,
        },
    );

    let runner_data = create_test_runner_data_with_methods(methods);
    let result = TestProtobufHelper::parse_job_args_schema_descriptor(&runner_data, "run");

    assert!(
        result.is_ok(),
        "Should succeed for valid method: {:?}",
        result
    );
    let descriptor = result.unwrap();
    assert!(
        descriptor.is_some(),
        "Should return Some(MessageDescriptor)"
    );
    assert_eq!(
        descriptor.unwrap().name(),
        "CommandArgs",
        "Should return CommandArgs message"
    );
}

#[test]
fn test_parse_job_args_method_not_found() {
    // Method doesn't exist
    let mut methods = HashMap::new();
    methods.insert(
        "run".to_string(),
        MethodSchema {
            args_proto: valid_command_args_proto(),
            result_proto: valid_command_result_proto(),
            description: None,
            output_type: StreamingOutputType::Both as i32,
        },
    );

    let runner_data = create_test_runner_data_with_methods(methods);
    let result = TestProtobufHelper::parse_job_args_schema_descriptor(&runner_data, "nonexistent");

    assert!(
        result.is_err(),
        "Should error when method not found: {:?}",
        result
    );
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("Method 'nonexistent' not found"),
        "Error should mention method name: {}",
        error_msg
    );
    assert!(
        error_msg.contains("available methods"),
        "Error should list available methods: {}",
        error_msg
    );
}

#[test]
fn test_parse_job_args_empty_proto() {
    // Empty args_proto (unstructured input)
    let mut methods = HashMap::new();
    methods.insert(
        "run".to_string(),
        MethodSchema {
            args_proto: String::new(), // Empty proto
            result_proto: valid_command_result_proto(),
            description: None,
            output_type: StreamingOutputType::Both as i32,
        },
    );

    let runner_data = create_test_runner_data_with_methods(methods);
    let result = TestProtobufHelper::parse_job_args_schema_descriptor(&runner_data, "run");

    assert!(
        result.is_ok(),
        "Should succeed for empty proto: {:?}",
        result
    );
    assert_eq!(
        result.unwrap(),
        None,
        "Should return Ok(None) for empty proto"
    );
}

#[test]
fn test_parse_job_args_invalid_proto() {
    // Invalid proto definition (no message)
    let mut methods = HashMap::new();
    methods.insert(
        "run".to_string(),
        MethodSchema {
            args_proto: invalid_proto_schema(),
            result_proto: valid_command_result_proto(),
            description: None,
            output_type: StreamingOutputType::Both as i32,
        },
    );

    let runner_data = create_test_runner_data_with_methods(methods);
    let result = TestProtobufHelper::parse_job_args_schema_descriptor(&runner_data, "run");

    assert!(
        result.is_err(),
        "Should error for invalid proto: {:?}",
        result
    );
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("No message found in args_proto"),
        "Error should mention missing message: {}",
        error_msg
    );
}

#[test]
fn test_parse_job_args_malformed_proto() {
    // Malformed proto (ProtobufDescriptor::new fails)
    let mut methods = HashMap::new();
    methods.insert(
        "run".to_string(),
        MethodSchema {
            args_proto: "invalid proto syntax {{{".to_string(),
            result_proto: valid_command_result_proto(),
            description: None,
            output_type: StreamingOutputType::Both as i32,
        },
    );

    let runner_data = create_test_runner_data_with_methods(methods);
    let result = TestProtobufHelper::parse_job_args_schema_descriptor(&runner_data, "run");

    assert!(
        result.is_err(),
        "Should error for malformed proto: {:?}",
        result
    );
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("Failed to parse args_proto"),
        "Error should mention parse failure: {}",
        error_msg
    );
}

#[test]
fn test_parse_job_args_missing_method_proto_map() {
    // No method_proto_map
    let runner_data = RunnerData {
        name: "test_runner".to_string(),
        description: "Test runner".to_string(),
        runner_type: 0,
        runner_settings_proto: String::new(),
        definition: String::new(),
        method_proto_map: None, // Missing
    };

    let result = TestProtobufHelper::parse_job_args_schema_descriptor(&runner_data, "run");

    assert!(
        result.is_err(),
        "Should error when method_proto_map is None: {:?}",
        result
    );
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("method_proto_map is required"),
        "Error should mention missing method_proto_map: {}",
        error_msg
    );
}

// ==================== parse_job_result_schema_descriptor Tests ====================

#[test]
fn test_parse_job_result_valid_method() {
    // Valid method with result_proto
    let mut methods = HashMap::new();
    methods.insert(
        "run".to_string(),
        MethodSchema {
            args_proto: valid_command_args_proto(),
            result_proto: valid_command_result_proto(),
            description: Some("Test method".to_string()),
            output_type: StreamingOutputType::Both as i32,
        },
    );

    let runner_data = create_test_runner_data_with_methods(methods);
    let result = TestProtobufHelper::parse_job_result_schema_descriptor(&runner_data, "run");

    assert!(
        result.is_ok(),
        "Should succeed for valid method: {:?}",
        result
    );
    let descriptor = result.unwrap();
    assert!(
        descriptor.is_some(),
        "Should return Some(MessageDescriptor)"
    );
    assert_eq!(
        descriptor.unwrap().name(),
        "CommandResult",
        "Should return CommandResult message"
    );
}

#[test]
fn test_parse_job_result_method_not_found() {
    // Method doesn't exist
    let mut methods = HashMap::new();
    methods.insert(
        "run".to_string(),
        MethodSchema {
            args_proto: valid_command_args_proto(),
            result_proto: valid_command_result_proto(),
            description: None,
            output_type: StreamingOutputType::Both as i32,
        },
    );

    let runner_data = create_test_runner_data_with_methods(methods);
    let result =
        TestProtobufHelper::parse_job_result_schema_descriptor(&runner_data, "nonexistent");

    assert!(
        result.is_err(),
        "Should error when method not found: {:?}",
        result
    );
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("Method 'nonexistent' not found"),
        "Error should mention method name: {}",
        error_msg
    );
}

#[test]
fn test_parse_job_result_empty_proto() {
    // Empty result_proto (unstructured output like COMMAND runner)
    let mut methods = HashMap::new();
    methods.insert(
        "run".to_string(),
        MethodSchema {
            args_proto: valid_command_args_proto(),
            result_proto: String::new(), // Empty proto
            description: None,
            output_type: StreamingOutputType::Both as i32,
        },
    );

    let runner_data = create_test_runner_data_with_methods(methods);
    let result = TestProtobufHelper::parse_job_result_schema_descriptor(&runner_data, "run");

    assert!(
        result.is_ok(),
        "Should succeed for empty proto: {:?}",
        result
    );
    assert_eq!(
        result.unwrap(),
        None,
        "Should return Ok(None) for empty proto"
    );
}

#[test]
fn test_parse_job_result_invalid_proto() {
    // Invalid proto definition (no message)
    let mut methods = HashMap::new();
    methods.insert(
        "run".to_string(),
        MethodSchema {
            args_proto: valid_command_args_proto(),
            result_proto: invalid_proto_schema(),
            description: None,
            output_type: StreamingOutputType::Both as i32,
        },
    );

    let runner_data = create_test_runner_data_with_methods(methods);
    let result = TestProtobufHelper::parse_job_result_schema_descriptor(&runner_data, "run");

    assert!(
        result.is_err(),
        "Should error for invalid proto: {:?}",
        result
    );
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("No message found in result_proto"),
        "Error should mention missing message: {}",
        error_msg
    );
}

#[test]
fn test_parse_job_result_malformed_proto() {
    // Malformed proto (ProtobufDescriptor::new fails)
    let mut methods = HashMap::new();
    methods.insert(
        "run".to_string(),
        MethodSchema {
            args_proto: valid_command_args_proto(),
            result_proto: "invalid proto syntax {{{".to_string(),
            description: None,
            output_type: StreamingOutputType::Both as i32,
        },
    );

    let runner_data = create_test_runner_data_with_methods(methods);
    let result = TestProtobufHelper::parse_job_result_schema_descriptor(&runner_data, "run");

    assert!(
        result.is_err(),
        "Should error for malformed proto: {:?}",
        result
    );
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("Failed to parse result_proto"),
        "Error should mention parse failure: {}",
        error_msg
    );
}

#[test]
fn test_parse_job_result_missing_method_proto_map() {
    // No method_proto_map
    let runner_data = RunnerData {
        name: "test_runner".to_string(),
        description: "Test runner".to_string(),
        runner_type: 0,
        runner_settings_proto: String::new(),
        definition: String::new(),
        method_proto_map: None, // Missing
    };

    let result = TestProtobufHelper::parse_job_result_schema_descriptor(&runner_data, "run");

    assert!(
        result.is_err(),
        "Should error when method_proto_map is None: {:?}",
        result
    );
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("method_proto_map is required"),
        "Error should mention missing method_proto_map: {}",
        error_msg
    );
}

// ==================== Multi-method Runner Tests ====================

#[test]
fn test_parse_multi_method_runner() {
    // MCP Server with multiple tools
    let mut methods = HashMap::new();

    // Method 1: fetch_html
    methods.insert(
        "fetch_html".to_string(),
        MethodSchema {
            args_proto: r#"
syntax = "proto3";
package test;
message FetchHtmlArgs {
    string url = 1;
    int32 timeout_ms = 2;
}
"#
            .to_string(),
            result_proto: r#"
syntax = "proto3";
package test;
message FetchHtmlResult {
    string html = 1;
}
"#
            .to_string(),
            description: Some("Fetch HTML from URL".to_string()),
            output_type: StreamingOutputType::Both as i32,
        },
    );

    // Method 2: get_time
    methods.insert(
        "get_time".to_string(),
        MethodSchema {
            args_proto: r#"
syntax = "proto3";
package test;
message GetTimeArgs {
    string timezone = 1;
}
"#
            .to_string(),
            result_proto: r#"
syntax = "proto3";
package test;
message GetTimeResult {
    int64 timestamp = 1;
}
"#
            .to_string(),
            description: Some("Get current time".to_string()),
            output_type: StreamingOutputType::NonStreaming as i32,
        },
    );

    let runner_data = create_test_runner_data_with_methods(methods);

    // Test method 1
    let result1 = TestProtobufHelper::parse_job_args_schema_descriptor(&runner_data, "fetch_html");
    assert!(result1.is_ok());
    assert_eq!(result1.unwrap().unwrap().name(), "FetchHtmlArgs");

    let result2 =
        TestProtobufHelper::parse_job_result_schema_descriptor(&runner_data, "fetch_html");
    assert!(result2.is_ok());
    assert_eq!(result2.unwrap().unwrap().name(), "FetchHtmlResult");

    // Test method 2
    let result3 = TestProtobufHelper::parse_job_args_schema_descriptor(&runner_data, "get_time");
    assert!(result3.is_ok());
    assert_eq!(result3.unwrap().unwrap().name(), "GetTimeArgs");

    let result4 = TestProtobufHelper::parse_job_result_schema_descriptor(&runner_data, "get_time");
    assert!(result4.is_ok());
    assert_eq!(result4.unwrap().unwrap().name(), "GetTimeResult");

    // Test nonexistent method
    let result5 = TestProtobufHelper::parse_job_args_schema_descriptor(&runner_data, "nonexistent");
    assert!(result5.is_err());
}
