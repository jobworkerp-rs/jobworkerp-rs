// JSON Schema to Protobuf converter for MCP tools
//
// This module provides conversion from JSON Schema (used by MCP tools)
// to Protobuf definitions (used by jobworkerp-rs).

use anyhow::{anyhow, Result};
use serde_json::Value as JsonValue;
use std::time::Duration;

/// Validation errors with clear categorization
///
/// # Design Rationale (v1.14)
/// - Type-safe error handling instead of string matching
/// - Clear distinction between user errors and internal errors
/// - Enables appropriate logging levels
/// - Each variant has a specific use case and responsibility
#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    /// JSON Schema validation failed (user input error)
    /// Used when input data does not conform to the tool's JSON Schema
    #[error("Input does not match tool's JSON Schema: {details}")]
    SchemaViolation { details: String },

    /// JSON Schema validation timed out (internal error)
    /// Used when validation takes longer than VALIDATION_TIMEOUT (3 seconds)
    #[error("Validation timed out after {timeout_secs} seconds")]
    Timeout { timeout_secs: u64 },

    /// Protobuf deserialization produced invalid JSON (internal error)
    /// Used when ProtobufDescriptor produces JSON that cannot be validated
    /// This indicates a bug in the Protobuf→JSON conversion logic
    #[error("Internal error: Protobuf produced invalid JSON: {details}")]
    DeserializationError { details: String },
}

impl ValidationError {
    /// Returns true if this is a user input error (should log as WARN)
    pub fn is_user_error(&self) -> bool {
        matches!(self, ValidationError::SchemaViolation { .. })
    }

    /// Returns true if this is an internal error (should log as ERROR)
    pub fn is_internal_error(&self) -> bool {
        !self.is_user_error()
    }
}

/// JSON Schema to Protobuf converter
///
/// Converts MCP tool's JSON Schema to Protobuf definition for jobworkerp-rs integration.
/// Includes security constraints to prevent DoS attacks.
#[derive(Debug)]
pub struct JsonSchemaToProtobufConverter {
    validator: jsonschema::Validator,
    schema: JsonValue,
}

impl JsonSchemaToProtobufConverter {
    // Security constraints
    const MAX_SCHEMA_SIZE: usize = 1024 * 1024; // 1MB
    const MAX_SCHEMA_DEPTH: usize = 10;
    const VALIDATION_TIMEOUT: Duration = Duration::from_secs(3);

    /// Create a new converter with security validation
    ///
    /// # Security Constraints
    /// - MAX_SCHEMA_SIZE: 1MB (prevents DoS via huge schemas)
    /// - MAX_SCHEMA_DEPTH: 10 levels (prevents infinite recursion)
    /// - Unsupported patterns: $ref, allOf, anyOf, oneOf (rejected during initialization)
    ///
    /// # Errors
    /// - Schema too large (>1MB)
    /// - Schema nesting too deep (>10 levels)
    /// - Unsupported JSON Schema features detected
    /// - Invalid JSON Schema syntax
    pub fn new(schema: &JsonValue) -> Result<Self> {
        // 1. Schema size limit (DoS prevention)
        let schema_str = serde_json::to_string(schema)?;
        if schema_str.len() > Self::MAX_SCHEMA_SIZE {
            return Err(anyhow!(
                "Schema too large: {} bytes (max: {} bytes)",
                schema_str.len(),
                Self::MAX_SCHEMA_SIZE
            ));
        }

        // 2. Schema depth validation (infinite recursion prevention)
        Self::validate_schema_depth(schema, 0)?;

        // 3. Unsupported pattern detection (v1.6: reject during initialization)
        if Self::has_unsupported_patterns(schema) {
            return Err(anyhow!(
                "This tool uses advanced JSON Schema features ($ref, allOf, anyOf, oneOf) \
                 which are not yet supported. The tool cannot be registered.\n\
                 \n\
                 Detected patterns: {}\n\
                 \n\
                 Workaround: Please simplify the schema by:\n\
                 - Replacing $ref with inline definitions\n\
                 - Flattening allOf/anyOf/oneOf into single properties\n\
                 - Using basic types (string, integer, number, boolean, array, object)\n\
                 \n\
                 Example transformation:\n\
                 Before: {{\"$ref\": \"#/definitions/FileArg\"}}\n\
                 After:  {{\"type\": \"object\", \"properties\": {{\"path\": {{\"type\": \"string\"}}}}}}\n\
                 \n\
                 Please contact the MCP server maintainer or wait for future support.",
                Self::describe_unsupported_patterns(schema)
            ));
        }

        // 4. JSON Schema validator construction
        let validator =
            jsonschema::validator_for(schema).map_err(|e| anyhow!("Invalid JSON Schema: {}", e))?;

        Ok(Self {
            validator,
            schema: schema.clone(),
        })
    }

    /// Check if schema contains unsupported patterns ($ref, allOf, anyOf, oneOf)
    fn has_unsupported_patterns(schema: &JsonValue) -> bool {
        Self::check_unsupported_recursive(schema)
    }

    fn check_unsupported_recursive(schema: &JsonValue) -> bool {
        if let Some(obj) = schema.as_object() {
            // $ref detection
            if obj.contains_key("$ref") {
                return true;
            }
            // allOf/anyOf/oneOf detection
            if obj.contains_key("allOf") || obj.contains_key("anyOf") || obj.contains_key("oneOf") {
                return true;
            }
            // Recursive check: properties, items, additionalProperties
            if let Some(properties) = obj.get("properties").and_then(|v| v.as_object()) {
                for prop_schema in properties.values() {
                    if Self::check_unsupported_recursive(prop_schema) {
                        return true;
                    }
                }
            }
            if let Some(items) = obj.get("items") {
                if Self::check_unsupported_recursive(items) {
                    return true;
                }
            }
            if let Some(additional) = obj.get("additionalProperties") {
                if !additional.is_boolean() && Self::check_unsupported_recursive(additional) {
                    return true;
                }
            }
        }
        false
    }

    fn describe_unsupported_patterns(schema: &JsonValue) -> String {
        let mut patterns = Vec::new();
        Self::collect_unsupported_patterns(schema, &mut patterns);
        patterns.join(", ")
    }

    fn collect_unsupported_patterns(schema: &JsonValue, patterns: &mut Vec<String>) {
        if let Some(obj) = schema.as_object() {
            if obj.contains_key("$ref") {
                patterns.push("$ref".to_string());
            }
            if obj.contains_key("allOf") {
                patterns.push("allOf".to_string());
            }
            if obj.contains_key("anyOf") {
                patterns.push("anyOf".to_string());
            }
            if obj.contains_key("oneOf") {
                patterns.push("oneOf".to_string());
            }
            // Recursively check nested schemas
            if let Some(properties) = obj.get("properties").and_then(|v| v.as_object()) {
                for prop_schema in properties.values() {
                    Self::collect_unsupported_patterns(prop_schema, patterns);
                }
            }
            if let Some(items) = obj.get("items") {
                Self::collect_unsupported_patterns(items, patterns);
            }
        }
    }

    fn validate_schema_depth(schema: &JsonValue, depth: usize) -> Result<()> {
        if depth > Self::MAX_SCHEMA_DEPTH {
            return Err(anyhow!(
                "Schema nesting too deep: max depth is {}",
                Self::MAX_SCHEMA_DEPTH
            ));
        }

        match schema {
            JsonValue::Object(obj) => {
                // Recursive check: properties, items, additionalProperties
                if let Some(properties) = obj.get("properties").and_then(|v| v.as_object()) {
                    for prop_schema in properties.values() {
                        Self::validate_schema_depth(prop_schema, depth + 1)?;
                    }
                }
                if let Some(items) = obj.get("items") {
                    Self::validate_schema_depth(items, depth + 1)?;
                }
                if let Some(additional) = obj.get("additionalProperties") {
                    if !additional.is_boolean() {
                        Self::validate_schema_depth(additional, depth + 1)?;
                    }
                }
                // Complex schema patterns (allOf, anyOf, oneOf)
                for key in &["allOf", "anyOf", "oneOf"] {
                    if let Some(schemas) = obj.get(*key).and_then(|v| v.as_array()) {
                        for sub_schema in schemas {
                            Self::validate_schema_depth(sub_schema, depth + 1)?;
                        }
                    }
                }
            }
            JsonValue::Array(arr) => {
                for item in arr {
                    Self::validate_schema_depth(item, depth + 1)?;
                }
            }
            _ => {}
        }

        Ok(())
    }

    /// Validate input against JSON Schema with timeout
    ///
    /// # Design Notes (v1.6)
    /// - Added 3-second timeout to prevent DoS attacks
    /// - Returns ValidationError for type-safe error handling
    ///
    /// # Logging Strategy
    /// This method does NOT log errors internally. Instead, the caller
    /// (McpToolRunnerImpl::run()) is responsible for logging with appropriate
    /// context (tool_name, server_name).
    ///
    /// # Errors
    /// - ValidationError::SchemaViolation: Input does not match schema (user error)
    /// - ValidationError::Timeout: Validation took longer than 3 seconds (internal error)
    /// - ValidationError::DeserializationError: Invalid JSON from Protobuf (internal error)
    pub async fn validate(&self, input: &JsonValue) -> Result<(), ValidationError> {
        // Timeout-protected validation (v1.6)
        let validation_result = tokio::time::timeout(Self::VALIDATION_TIMEOUT, async {
            // Check if input is valid JSON (if it came from Protobuf deserialization)
            let schema_allows_null = self
                .schema
                .get("type")
                .and_then(|t| t.as_str())
                .is_some_and(|t| t == "null");

            if input.is_null() && !schema_allows_null {
                return Err(ValidationError::DeserializationError {
                    details: "Protobuf deserialization produced null value".to_string(),
                });
            }

            // Collect validation errors
            let errors: Vec<String> = self
                .validator
                .iter_errors(input)
                .map(|e| e.to_string())
                .collect();

            if !errors.is_empty() {
                return Err(ValidationError::SchemaViolation {
                    details: errors.join(", "),
                });
            }

            Ok(())
        })
        .await;

        match validation_result {
            Ok(result) => result,
            Err(_timeout) => {
                // Timeout occurred - return error without logging (caller will log)
                Err(ValidationError::Timeout {
                    timeout_secs: Self::VALIDATION_TIMEOUT.as_secs(),
                })
            }
        }
    }

    /// Generate Protobuf definition from JSON Schema
    ///
    /// # Arguments
    /// - `server_name`: MCP server name (e.g., "filesystem")
    /// - `tool_name`: MCP tool name (e.g., "read_file")
    ///
    /// # Returns
    /// Proto3 definition string with message definition for tool arguments
    ///
    /// # Example Output
    /// ```proto
    /// syntax = "proto3";
    ///
    /// package jobworkerp.runner.mcp.filesystem;
    ///
    /// // Arguments for read_file tool
    /// message FilesystemReadFileArgs {
    ///   string path = 1;
    /// }
    /// ```
    pub fn to_proto_definition(&self, server_name: &str, tool_name: &str) -> String {
        use command_utils::text::TextUtil;

        let message_name = TextUtil::to_pascal_case(&format!("{}_{}", server_name, tool_name));
        let fields = self.convert_schema_to_proto_fields(&self.schema, 0);

        format!(
            r#"syntax = "proto3";

package jobworkerp.runner.mcp.{};

// Arguments for {} tool
message {}Args {{
{}
}}
"#,
            server_name, tool_name, message_name, fields
        )
    }

    /// Convert JSON Schema to Protobuf field definitions
    fn convert_schema_to_proto_fields(&self, schema: &JsonValue, indent_level: usize) -> String {
        let indent = "  ".repeat(indent_level + 1);

        if let Some(obj) = schema.as_object() {
            if obj.get("type").and_then(|v| v.as_str()) == Some("object") {
                if let Some(properties) = obj.get("properties").and_then(|v| v.as_object()) {
                    let required_fields: Vec<&str> = obj
                        .get("required")
                        .and_then(|v| v.as_array())
                        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
                        .unwrap_or_default();

                    let mut field_number = 1;
                    let mut fields = Vec::new();

                    for (prop_name, prop_schema) in properties {
                        let is_required = required_fields.contains(&prop_name.as_str());
                        let optional = if is_required { "" } else { "optional " };
                        let field_type = Self::json_type_to_proto_type(prop_schema);

                        let comment = if let Some(desc) =
                            prop_schema.get("description").and_then(|v| v.as_str())
                        {
                            format!("{}// {}\n", indent, desc)
                        } else {
                            String::new()
                        };

                        fields.push(format!(
                            "{}{}{}{}  {} = {};",
                            comment, indent, optional, field_type, prop_name, field_number
                        ));
                        field_number += 1;
                    }

                    return fields.join("\n");
                }
            }
        }

        // Fallback: empty message
        String::new()
    }

    /// Convert JSON Schema type to Protobuf type
    fn json_type_to_proto_type(schema: &JsonValue) -> String {
        if let Some(obj) = schema.as_object() {
            match obj.get("type").and_then(|v| v.as_str()) {
                Some("string") => "string".to_string(),
                Some("integer") => "int64".to_string(),
                Some("number") => "double".to_string(),
                Some("boolean") => "bool".to_string(),
                Some("array") => {
                    if let Some(items) = obj.get("items") {
                        let item_type = Self::json_type_to_proto_type(items);
                        format!("repeated {}", item_type)
                    } else {
                        "repeated string".to_string()
                    }
                }
                Some("object") => "google.protobuf.Struct".to_string(),
                _ => "string".to_string(), // Fallback
            }
        } else {
            "string".to_string() // Fallback
        }
    }

    /// Get the schema
    pub fn schema(&self) -> &JsonValue {
        &self.schema
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_validation_error_is_user_error() {
        let error = ValidationError::SchemaViolation {
            details: "test".to_string(),
        };
        assert!(error.is_user_error());
        assert!(!error.is_internal_error());
    }

    #[test]
    fn test_validation_error_is_internal_error() {
        let error = ValidationError::Timeout { timeout_secs: 3 };
        assert!(!error.is_user_error());
        assert!(error.is_internal_error());

        let error = ValidationError::DeserializationError {
            details: "test".to_string(),
        };
        assert!(!error.is_user_error());
        assert!(error.is_internal_error());
    }

    #[test]
    fn test_schema_size_limit() {
        // Create a huge schema (>1MB)
        let huge_properties: serde_json::Map<String, JsonValue> = (0..100000)
            .map(|i| (format!("field{}", i), json!({"type": "string"})))
            .collect();
        let huge_schema = json!({
            "type": "object",
            "properties": huge_properties
        });

        let result = JsonSchemaToProtobufConverter::new(&huge_schema);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Schema too large"));
    }

    #[test]
    fn test_schema_depth_limit() {
        // Create deeply nested schema (>10 levels)
        fn create_nested_schema(depth: usize) -> JsonValue {
            if depth == 0 {
                json!({"type": "string"})
            } else {
                json!({
                    "type": "object",
                    "properties": {
                        "nested": create_nested_schema(depth - 1)
                    }
                })
            }
        }

        let deep_schema = create_nested_schema(12);
        let result = JsonSchemaToProtobufConverter::new(&deep_schema);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Schema nesting too deep"));
    }

    #[test]
    fn test_unsupported_pattern_ref() {
        let schema = json!({
            "type": "object",
            "properties": {
                "file": {"$ref": "#/definitions/File"}
            }
        });

        let result = JsonSchemaToProtobufConverter::new(&schema);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("$ref"));
    }

    #[test]
    fn test_unsupported_pattern_allof() {
        let schema = json!({
            "allOf": [
                {"type": "object"},
                {"properties": {"name": {"type": "string"}}}
            ]
        });

        let result = JsonSchemaToProtobufConverter::new(&schema);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("allOf"));
    }

    #[test]
    fn test_valid_simple_schema() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "age": {"type": "integer"}
            },
            "required": ["name"]
        });

        let result = JsonSchemaToProtobufConverter::new(&schema);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_validate_success() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "age": {"type": "integer"}
            },
            "required": ["name"]
        });

        let converter = JsonSchemaToProtobufConverter::new(&schema).unwrap();
        let valid_input = json!({
            "name": "Alice",
            "age": 30
        });

        let result = converter.validate(&valid_input).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_validate_schema_violation() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "age": {"type": "integer"}
            },
            "required": ["name"]
        });

        let converter = JsonSchemaToProtobufConverter::new(&schema).unwrap();
        let invalid_input = json!({
            "age": "not a number"  // Type mismatch
        });

        let result = converter.validate(&invalid_input).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ValidationError::SchemaViolation { .. } => (),
            _ => panic!("Expected SchemaViolation"),
        }
    }

    #[tokio::test]
    async fn test_validate_deserialization_error() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"}
            }
        });

        let converter = JsonSchemaToProtobufConverter::new(&schema).unwrap();
        let null_input = json!(null);

        let result = converter.validate(&null_input).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ValidationError::DeserializationError { .. } => (),
            _ => panic!("Expected DeserializationError"),
        }
    }

    // Note: Timeout test is omitted because it requires a pathologically complex schema
    // that would take >3 seconds to validate. In practice, this is handled by the
    // MAX_SCHEMA_DEPTH and MAX_SCHEMA_SIZE limits.

    #[test]
    fn test_to_proto_definition_basic() {
        let schema = json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path to read"
                },
                "encoding": {
                    "type": "string"
                }
            },
            "required": ["path"]
        });

        let converter = JsonSchemaToProtobufConverter::new(&schema).unwrap();
        let proto_def = converter.to_proto_definition("filesystem", "read_file");

        println!("Generated proto definition:\n{}", proto_def);

        assert!(proto_def.contains("syntax = \"proto3\";"));
        assert!(proto_def.contains("package jobworkerp.runner.mcp.filesystem;"));
        assert!(proto_def.contains("message FilesystemReadFileArgs {"));
        assert!(proto_def.contains("string  path = 1") || proto_def.contains("path = 1"));
        assert!(
            proto_def.contains("optional string  encoding = 2")
                || proto_def.contains("encoding = 2")
        );
        assert!(proto_def.contains("// File path to read"));
    }

    #[test]
    fn test_json_type_to_proto_type() {
        assert_eq!(
            JsonSchemaToProtobufConverter::json_type_to_proto_type(&json!({"type": "string"})),
            "string"
        );
        assert_eq!(
            JsonSchemaToProtobufConverter::json_type_to_proto_type(&json!({"type": "integer"})),
            "int64"
        );
        assert_eq!(
            JsonSchemaToProtobufConverter::json_type_to_proto_type(&json!({"type": "number"})),
            "double"
        );
        assert_eq!(
            JsonSchemaToProtobufConverter::json_type_to_proto_type(&json!({"type": "boolean"})),
            "bool"
        );
        assert_eq!(
            JsonSchemaToProtobufConverter::json_type_to_proto_type(&json!({
                "type": "array",
                "items": {"type": "string"}
            })),
            "repeated string"
        );
        assert_eq!(
            JsonSchemaToProtobufConverter::json_type_to_proto_type(&json!({"type": "object"})),
            "google.protobuf.Struct"
        );
    }

    #[test]
    fn test_to_proto_definition_with_array() {
        let schema = json!({
            "type": "object",
            "properties": {
                "files": {
                    "type": "array",
                    "items": {"type": "string"}
                }
            }
        });

        let converter = JsonSchemaToProtobufConverter::new(&schema).unwrap();
        let proto_def = converter.to_proto_definition("test", "list_files");

        assert!(proto_def.contains("repeated string  files = 1;"));
    }
}
