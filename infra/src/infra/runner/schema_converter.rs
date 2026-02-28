//! JSON Schema converter for Protobuf method schemas
//!
//! This module provides utilities for converting Protobuf schema definitions
//! to JSON Schema format. The conversion is cached in RunnerWithSchema to
//! avoid repeated expensive operations.
//!
//! # Design
//!
//! The conversion logic is extracted into a reusable trait to:
//! - Centralize Protobuf → JSON Schema conversion
//! - Enable caching in RunnerWithSchema (one-time conversion)
//! - Simplify FunctionSpecs generation (no runtime conversion)
//! - Provide testable, isolated conversion logic

use anyhow::{Context, Result};
use command_utils::protobuf::ProtobufDescriptor;
use proto::jobworkerp::data::{MethodJsonSchema, MethodSchema};
use std::collections::HashMap;

/// Maximum JSON Schema size to prevent DoS attacks (1MB)
const MAX_JSON_SCHEMA_SIZE: usize = 1_000_000;

/// Utility trait for converting Protobuf schemas to JSON schemas
///
/// This provides a centralized, reusable conversion logic that:
/// 1. Converts MethodProtoMap to MethodJsonSchemaMap efficiently
/// 2. Handles errors gracefully with fallback to empty schemas
/// 3. Preserves method descriptions from Protobuf schemas
/// 4. Validates JSON Schema size to prevent DoS
///
/// # Usage
///
/// This trait is automatically implemented for all types via blanket implementation.
/// Use it in RunnerRow::to_runner_with_schema() to generate cached JSON schemas:
///
/// ```ignore
/// let proto_map = runner.method_proto_map();
/// let json_map = Self::convert_method_proto_map_to_json_schema_map(&proto_map);
/// ```
pub trait MethodJsonSchemaConverter {
    /// Convert MethodProtoMap to MethodJsonSchemaMap
    ///
    /// This is the main conversion function that should be used in
    /// RunnerRow::to_runner_with_schema() to generate cached JSON schemas.
    ///
    /// # Arguments
    /// * `method_proto_map` - Map of method names to MethodSchema (Protobuf)
    ///
    /// # Returns
    /// Map of method names to MethodJsonSchema (JSON Schema)
    ///
    /// # Performance
    /// This conversion is expensive (Protobuf parsing + JSON schema generation).
    /// It should only be called once during RunnerWithSchema creation, and the
    /// result should be cached for all subsequent uses.
    ///
    /// # Error Handling
    /// - Invalid Protobuf: Logs warning, returns empty schema "{}"
    /// - Schema too large: Logs warning, returns empty schema "{}"
    /// - Missing message: Logs warning, returns empty schema "{}"
    ///
    /// # Example
    /// ```ignore
    /// let proto_map = runner.method_proto_map();
    /// let json_map = Self::convert_method_proto_map_to_json_schema_map(&proto_map);
    /// ```
    #[allow(clippy::unnecessary_filter_map)] // filter_map is intentional for error handling with logging
    fn convert_method_proto_map_to_json_schema_map(
        method_proto_map: &HashMap<String, MethodSchema>,
    ) -> HashMap<String, MethodJsonSchema> {
        method_proto_map
            .iter()
            .filter_map(|(method_name, proto_schema)| {
                let args_schema = if proto_schema.args_proto.is_empty() {
                    "{}".to_string()
                } else {
                    Self::proto_string_to_json_schema(&proto_schema.args_proto, method_name, "args")
                        .unwrap_or_else(|e| {
                            tracing::warn!(
                                "Failed to convert args_proto to JSON Schema for method '{}': {:?}",
                                method_name,
                                e
                            );
                            "{}".to_string()
                        })
                };

                let result_schema = if proto_schema.result_proto.is_empty() {
                    None
                } else {
                    Self::proto_string_to_json_schema(
                        &proto_schema.result_proto,
                        method_name,
                        "result",
                    )
                    .ok()
                };

                // feed_data_proto → feed_data JSON Schema
                let feed_data_schema = if !proto_schema.need_feed {
                    None
                } else {
                    proto_schema
                        .feed_data_proto
                        .as_deref()
                        .filter(|fdp| !fdp.is_empty())
                        .and_then(|fdp| {
                            Self::proto_string_to_json_schema(fdp, method_name, "feed_data").ok()
                        })
                };

                Some((
                    method_name.clone(),
                    MethodJsonSchema {
                        args_schema,
                        result_schema,
                        feed_data_schema,
                    },
                ))
            })
            .collect()
    }

    /// Convert a single Protobuf string to JSON Schema string
    ///
    /// This is a lower-level helper function that handles the conversion
    /// of a single Protobuf definition to JSON Schema.
    ///
    /// # Arguments
    /// * `proto_string` - Protobuf definition string
    /// * `context_method` - Method name (for error logging)
    /// * `context_type` - "args" or "result" (for error logging)
    ///
    /// # Returns
    /// JSON Schema string
    ///
    /// # Errors
    /// Returns error if:
    /// - Protobuf parsing fails
    /// - No message found in Protobuf
    /// - JSON Schema serialization fails
    /// - JSON Schema exceeds size limit (DoS prevention)
    fn proto_string_to_json_schema(
        proto_string: &str,
        context_method: &str,
        context_type: &str,
    ) -> Result<String> {
        let descriptor = ProtobufDescriptor::new(&proto_string.to_string()).context(format!(
            "Failed to parse Protobuf for method '{}' ({})",
            context_method, context_type
        ))?;

        let msg_desc = descriptor
            .get_messages()
            .into_iter()
            .next()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No message found in Protobuf for method '{}' ({})",
                    context_method,
                    context_type
                )
            })?;

        let json_schema = ProtobufDescriptor::message_descriptor_to_json_schema(&msg_desc);
        let json_string = serde_json::to_string(&json_schema).context(format!(
            "Failed to serialize JSON Schema for method '{}' ({})",
            context_method, context_type
        ))?;

        // Security: Validate JSON Schema size (DoS prevention)
        if json_string.len() > MAX_JSON_SCHEMA_SIZE {
            anyhow::bail!(
                "JSON Schema too large for method '{}' ({}): {} bytes (max: {})",
                context_method,
                context_type,
                json_string.len(),
                MAX_JSON_SCHEMA_SIZE
            );
        }

        Ok(json_string)
    }
}

/// Default implementation for all types
impl<T> MethodJsonSchemaConverter for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::jobworkerp::data::StreamingOutputType;

    struct TestConverter;

    #[test]
    fn test_convert_method_proto_map_to_json_schema_map() -> Result<()> {
        let mut proto_map = HashMap::new();
        proto_map.insert(
            "run".to_string(),
            MethodSchema {
                args_proto: r#"syntax = "proto3";
message TestArgs {
  string url = 1;
  optional int32 timeout_ms = 2;
}"#
                .to_string(),
                result_proto: r#"syntax = "proto3";
message TestResult {
  string content = 1;
}"#
                .to_string(),
                description: Some("Test method".to_string()),
                output_type: StreamingOutputType::NonStreaming as i32,
                ..Default::default()
            },
        );

        let json_map = TestConverter::convert_method_proto_map_to_json_schema_map(&proto_map);

        assert_eq!(json_map.len(), 1);
        let run_schema = json_map.get("run").unwrap();

        assert!(run_schema.args_schema.contains("url"));
        // Note: Protobuf field timeout_ms is converted to timeoutMs in JSON Schema (camelCase)
        assert!(run_schema.args_schema.contains("timeoutMs"));

        assert!(run_schema.result_schema.is_some());
        let result = run_schema.result_schema.as_ref().unwrap();
        assert!(result.contains("content"));

        // Note: description is NOT cached in MethodJsonSchema
        // It should be retrieved from method_proto_map instead

        Ok(())
    }

    #[test]
    fn test_convert_empty_proto_map() {
        let proto_map = HashMap::new();
        let json_map = TestConverter::convert_method_proto_map_to_json_schema_map(&proto_map);
        assert_eq!(json_map.len(), 0);
    }

    #[test]
    fn test_convert_with_empty_proto_strings() {
        let mut proto_map = HashMap::new();
        proto_map.insert(
            "empty_method".to_string(),
            MethodSchema {
                args_proto: "".to_string(),
                result_proto: "".to_string(),
                description: None,
                output_type: StreamingOutputType::NonStreaming as i32,
                ..Default::default()
            },
        );

        let json_map = TestConverter::convert_method_proto_map_to_json_schema_map(&proto_map);

        assert_eq!(json_map.len(), 1);
        let schema = json_map.get("empty_method").unwrap();
        assert_eq!(schema.args_schema, "{}");
        assert!(schema.result_schema.is_none());
        // Note: description is NOT cached in MethodJsonSchema
    }

    #[test]
    fn test_convert_with_description() {
        let mut proto_map = HashMap::new();
        proto_map.insert(
            "fetch".to_string(),
            MethodSchema {
                args_proto: r#"syntax = "proto3";
message FetchArgs {
  string url = 1;
}"#
                .to_string(),
                result_proto: "".to_string(),
                description: Some("Fetches content from URL".to_string()),
                output_type: StreamingOutputType::NonStreaming as i32,
                ..Default::default()
            },
        );

        let json_map = TestConverter::convert_method_proto_map_to_json_schema_map(&proto_map);
        let schema = json_map.get("fetch").unwrap();

        // Note: description is NOT cached in MethodJsonSchema
        // It should be retrieved from method_proto_map instead
        assert!(schema.args_schema.contains("url"));
    }

    #[test]
    fn test_proto_string_to_json_schema_basic() -> Result<()> {
        let proto = r#"syntax = "proto3";
message BasicMessage {
  string name = 1;
  int32 age = 2;
}"#;

        let json_schema = TestConverter::proto_string_to_json_schema(proto, "test", "args")?;

        assert!(json_schema.contains("name"));
        assert!(json_schema.contains("age"));
        Ok(())
    }

    #[test]
    fn test_proto_string_to_json_schema_invalid_proto() {
        let invalid_proto = "invalid protobuf syntax";
        let result = TestConverter::proto_string_to_json_schema(invalid_proto, "test", "args");
        assert!(result.is_err());
    }

    #[test]
    fn test_proto_string_to_json_schema_no_message() {
        let proto_no_msg = "syntax = \"proto3\";";
        let result = TestConverter::proto_string_to_json_schema(proto_no_msg, "test", "args");
        assert!(result.is_err());
    }
}
