//! JSON Schema to Protobuf schema converter for MCP tools
//!
//! This module converts MCP tool JSON schemas to Protobuf schema strings.
//! The generated Protobuf schemas are used for type-safe job argument serialization.
//!
//! # Supported Type Conversions
//!
//! | JSON Schema | Protobuf | Notes |
//! |------------|----------|-------|
//! | string | string | |
//! | integer | int64 | Safe range for all integers |
//! | number | double | Floating point |
//! | boolean | bool | |
//! | array | repeated T | items required |
//! | object | string | Phase 1: serialized as JSON string |
//! | null | ERROR | Not supported |
//! | anyOf/oneOf/allOf | ERROR | Not supported |

use anyhow::{anyhow, Result};
use serde_json::Value;
use std::collections::HashSet;

/// Maximum nesting depth for object types (defensive programming)
const MAX_NEST_DEPTH: usize = 10;

/// Reserved sub_method names that cannot be used
const RESERVED_NAMES: &[&str] = &["all", "default", "none", "system"];

/// Convert a string to PascalCase
fn to_pascal_case(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = true;

    for c in s.chars() {
        if c == '_' || c == '-' || c == '.' {
            capitalize_next = true;
        } else if capitalize_next {
            result.push(c.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            result.push(c);
        }
    }

    result
}

/// Sanitize a name for use in Protobuf (replace invalid characters)
fn sanitize_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Validate a sub_method name according to security rules
///
/// # Rules
/// - Length: 1-64 characters
/// - Characters: [a-zA-Z0-9_\-\.] allowed (hyphens and dots are common in MCP tool names)
/// - Reserved words: "all", "default", "none", "system" are forbidden
/// - Must start with alphanumeric character
///
/// # Note
/// MCP tool names often contain hyphens (e.g., "fetch-html") or dots.
/// These are allowed in sub_method names for MCP compatibility.
/// When generating Protobuf schemas, use `sanitize_name()` to convert them.
pub fn validate_sub_method_name(name: &str) -> Result<()> {
    // Length check
    if name.is_empty() {
        return Err(anyhow!("sub_method cannot be empty"));
    }
    if name.len() > 64 {
        return Err(anyhow!("sub_method '{}' too long (max 64 chars)", name));
    }

    // Must start with alphanumeric character
    if let Some(first_char) = name.chars().next() {
        if !first_char.is_ascii_alphanumeric() {
            return Err(anyhow!(
                "sub_method '{}' must start with alphanumeric character",
                name
            ));
        }
    }

    // Character check (alphanumeric, underscore, hyphen, and dot allowed)
    // Hyphens and dots are common in MCP tool names (e.g., "fetch-html", "get.time")
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(anyhow!(
            "sub_method '{}' contains invalid characters (allowed: [a-zA-Z0-9_\\-\\.])",
            name
        ));
    }

    // Reserved word check (check sanitized version too)
    let sanitized = sanitize_name(name);
    if RESERVED_NAMES.contains(&name.to_lowercase().as_str())
        || RESERVED_NAMES.contains(&sanitized.to_lowercase().as_str())
    {
        return Err(anyhow!("sub_method '{}' is reserved", name));
    }

    Ok(())
}

/// Convert JSON Schema to Protobuf schema string
///
/// # Arguments
/// * `json_schema` - The JSON Schema object from MCP tool definition
/// * `server_name` - The MCP server name (used for message naming)
/// * `tool_name` - The MCP tool name (used for message naming)
///
/// # Returns
/// A Protobuf schema string defining the message for this tool's arguments
///
/// # Example
/// ```ignore
/// let schema = json!({
///     "type": "object",
///     "properties": {
///         "url": {"type": "string"},
///         "timeout_ms": {"type": "integer"}
///     },
///     "required": ["url"]
/// });
///
/// let proto = json_schema_to_protobuf(&schema, "fetch", "fetch_html")?;
/// // Returns: syntax = "proto3";\n\nmessage FetchFetchHtmlArgs {\n  string url = 1;\n  optional int64 timeout_ms = 2;\n}
/// ```
pub fn json_schema_to_protobuf(
    json_schema: &Value,
    server_name: &str,
    tool_name: &str,
) -> Result<String> {
    // Validate tool name
    validate_sub_method_name(tool_name)?;

    // Generate message name: {ServerName}{ToolName}Args
    let sanitized_server_name = sanitize_name(server_name);
    let sanitized_tool_name = sanitize_name(tool_name);

    let message_name = format!(
        "{}{}Args",
        to_pascal_case(&sanitized_server_name),
        to_pascal_case(&sanitized_tool_name)
    );

    // Extract fields from JSON Schema
    let fields = extract_fields_from_schema(json_schema, &message_name, 0)?;

    // Generate Protobuf schema string
    let proto_schema = format!(
        "syntax = \"proto3\";\n\nmessage {} {{\n{}\n}}",
        message_name,
        fields.join("\n")
    );

    Ok(proto_schema)
}

/// Extract Protobuf field definitions from JSON Schema properties
fn extract_fields_from_schema(
    schema: &Value,
    _message_name: &str,
    depth: usize,
) -> Result<Vec<String>> {
    if depth > MAX_NEST_DEPTH {
        return Err(anyhow!(
            "Nested object depth exceeds maximum ({})",
            MAX_NEST_DEPTH
        ));
    }

    // Handle empty or non-object schemas
    let _schema_type = schema.get("type").and_then(|t| t.as_str());

    // If no properties, return empty fields (valid empty message)
    let properties = match schema.get("properties") {
        Some(props) => props
            .as_object()
            .ok_or_else(|| anyhow!("'properties' must be an object"))?,
        None => {
            // No properties - valid for empty schema or non-object types
            return Ok(vec![]);
        }
    };

    // Get required fields
    let required: HashSet<&str> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    let mut fields = Vec::new();
    for (i, (field_name, field_schema)) in properties.iter().enumerate() {
        let field_type = json_type_to_proto_type(field_schema)?;
        let optional_prefix = if required.contains(field_name.as_str()) {
            ""
        } else {
            "optional "
        };

        // Sanitize field name for Protobuf
        let sanitized_field_name = sanitize_name(field_name);

        fields.push(format!(
            "  {}{} {} = {};",
            optional_prefix,
            field_type,
            sanitized_field_name,
            i + 1
        ));
    }

    Ok(fields)
}

/// Convert JSON Schema type to Protobuf type
fn json_type_to_proto_type(schema: &Value) -> Result<String> {
    // Check for anyOf/oneOf/allOf (not supported)
    if schema.get("anyOf").is_some()
        || schema.get("oneOf").is_some()
        || schema.get("allOf").is_some()
    {
        return Err(anyhow!(
            "Complex schema types (anyOf, oneOf, allOf) are not supported"
        ));
    }

    let json_type = schema
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("string"); // Default to string if type is missing

    Ok(match json_type {
        "string" => "string".to_string(),
        "integer" => "int64".to_string(), // Use int64 for safety
        "number" => "double".to_string(),
        "boolean" => "bool".to_string(),
        "array" => {
            // items is required for array type
            let items = schema
                .get("items")
                .ok_or_else(|| anyhow!("Array type must have 'items' property"))?;
            let item_type = json_type_to_proto_type(items)?;
            format!("repeated {}", item_type)
        }
        "object" => {
            // Phase 1: Serialize nested objects as JSON string
            // Phase 2+: Generate nested message definitions
            "string".to_string()
        }
        "null" => {
            return Err(anyhow!("Null type is not supported in Protobuf conversion"));
        }
        _ => {
            return Err(anyhow!(
                "Unsupported JSON Schema type: '{}'. Supported types: string, integer, number, boolean, array, object",
                json_type
            ));
        }
    })
}

/// Information about a tool including its schemas
#[derive(Debug, Clone)]
pub struct ToolSchemaInfo {
    /// Tool name (sub_method name)
    pub name: String,
    /// Tool description
    pub description: Option<String>,
    /// Original JSON Schema from MCP
    pub json_schema: Value,
    /// Generated Protobuf schema string
    pub proto_schema: String,
}

/// Convert all MCP tools to schema info map
///
/// # Arguments
/// * `tools` - List of MCP tools with their JSON schemas
/// * `server_name` - The MCP server name
///
/// # Returns
/// A map of tool name to ToolSchemaInfo
pub fn convert_tools_to_schemas(
    tools: &[(String, Option<String>, Value)], // (name, description, input_schema)
    server_name: &str,
) -> Result<std::collections::HashMap<String, ToolSchemaInfo>> {
    let mut schemas = std::collections::HashMap::new();

    for (tool_name, description, json_schema) in tools {
        let proto_schema = json_schema_to_protobuf(json_schema, server_name, tool_name)?;

        schemas.insert(
            tool_name.clone(),
            ToolSchemaInfo {
                name: tool_name.clone(),
                description: description.clone(),
                json_schema: json_schema.clone(),
                proto_schema,
            },
        );
    }

    Ok(schemas)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_to_pascal_case() {
        assert_eq!(to_pascal_case("hello_world"), "HelloWorld");
        assert_eq!(to_pascal_case("fetch-html"), "FetchHtml");
        assert_eq!(to_pascal_case("get.current.time"), "GetCurrentTime");
        assert_eq!(to_pascal_case("simple"), "Simple");
    }

    #[test]
    fn test_sanitize_name() {
        assert_eq!(sanitize_name("hello-world"), "hello_world");
        assert_eq!(sanitize_name("get.time"), "get_time");
        assert_eq!(sanitize_name("valid_name123"), "valid_name123");
    }

    #[test]
    fn test_validate_sub_method_name() {
        // Valid names (underscore)
        assert!(validate_sub_method_name("fetch_html").is_ok());
        assert!(validate_sub_method_name("get_current_time").is_ok());
        assert!(validate_sub_method_name("tool123").is_ok());

        // Valid names (hyphen - common in MCP tools)
        assert!(validate_sub_method_name("hello-world").is_ok());
        assert!(validate_sub_method_name("fetch-html").is_ok());

        // Valid names (dot - common in MCP tools)
        assert!(validate_sub_method_name("get.time").is_ok());
        assert!(validate_sub_method_name("user.profile.get").is_ok());

        // Invalid: empty
        assert!(validate_sub_method_name("").is_err());

        // Invalid: too long
        let long_name = "a".repeat(65);
        assert!(validate_sub_method_name(&long_name).is_err());

        // Invalid: starts with non-alphanumeric
        assert!(validate_sub_method_name("-hello").is_err());
        assert!(validate_sub_method_name(".hello").is_err());
        assert!(validate_sub_method_name("_hello").is_err());

        // Invalid: special characters (other than hyphen, underscore, dot)
        assert!(validate_sub_method_name("hello@world").is_err());
        assert!(validate_sub_method_name("hello world").is_err());
        assert!(validate_sub_method_name("hello/world").is_err());

        // Invalid: reserved words
        assert!(validate_sub_method_name("all").is_err());
        assert!(validate_sub_method_name("default").is_err());
        assert!(validate_sub_method_name("none").is_err());
        assert!(validate_sub_method_name("system").is_err());
    }

    #[test]
    fn test_json_schema_to_protobuf_simple() {
        let schema = json!({
            "type": "object",
            "properties": {
                "url": {"type": "string"},
                "timeout_ms": {"type": "integer"}
            },
            "required": ["url"]
        });

        let result = json_schema_to_protobuf(&schema, "fetch", "fetch_html").unwrap();

        assert!(result.contains("syntax = \"proto3\""));
        assert!(result.contains("message FetchFetchHtmlArgs"));
        assert!(result.contains("string url = 1;"));
        assert!(result.contains("optional int64 timeout_ms = 2;"));
    }

    #[test]
    fn test_json_schema_to_protobuf_array() {
        let schema = json!({
            "type": "object",
            "properties": {
                "urls": {
                    "type": "array",
                    "items": {"type": "string"}
                }
            },
            "required": ["urls"]
        });

        let result = json_schema_to_protobuf(&schema, "batch", "fetch_urls").unwrap();

        assert!(result.contains("repeated string urls = 1;"));
    }

    #[test]
    fn test_json_schema_to_protobuf_all_types() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "count": {"type": "integer"},
                "value": {"type": "number"},
                "enabled": {"type": "boolean"},
                "tags": {
                    "type": "array",
                    "items": {"type": "string"}
                },
                "metadata": {"type": "object"}
            },
            "required": ["name"]
        });

        let result = json_schema_to_protobuf(&schema, "test", "all_types").unwrap();

        assert!(result.contains("string name = 1;"));
        assert!(result.contains("optional int64 count"));
        assert!(result.contains("optional double value"));
        assert!(result.contains("optional bool enabled"));
        assert!(result.contains("optional repeated string tags"));
        assert!(result.contains("optional string metadata")); // object → string (Phase 1)
    }

    #[test]
    fn test_json_schema_to_protobuf_empty_schema() {
        let schema = json!({
            "type": "object"
        });

        let result = json_schema_to_protobuf(&schema, "empty", "tool").unwrap();

        assert!(result.contains("message EmptyToolArgs"));
        assert!(result.contains("{\n\n}")); // Empty message body
    }

    #[test]
    fn test_json_schema_to_protobuf_with_hyphen_tool_name() {
        // MCP tools often have hyphenated names like "fetch-html"
        let schema = json!({
            "type": "object",
            "properties": {
                "url": {"type": "string"}
            },
            "required": ["url"]
        });

        let result = json_schema_to_protobuf(&schema, "fetch-server", "fetch-html").unwrap();

        // Hyphens should be converted to PascalCase in message name
        assert!(result.contains("syntax = \"proto3\""));
        assert!(result.contains("message FetchServerFetchHtmlArgs"));
        assert!(result.contains("string url = 1;"));
    }

    #[test]
    fn test_json_schema_to_protobuf_with_dot_tool_name() {
        // MCP tools might have dots in names like "user.get"
        let schema = json!({
            "type": "object",
            "properties": {
                "id": {"type": "integer"}
            },
            "required": ["id"]
        });

        let result = json_schema_to_protobuf(&schema, "api.v1", "user.get").unwrap();

        // Dots should be converted to PascalCase in message name
        assert!(result.contains("message ApiV1UserGetArgs"));
    }

    #[test]
    fn test_json_schema_to_protobuf_unsupported_types() {
        // null type
        let schema = json!({
            "type": "object",
            "properties": {
                "value": {"type": "null"}
            }
        });
        assert!(json_schema_to_protobuf(&schema, "test", "null_test").is_err());

        // anyOf
        let schema = json!({
            "type": "object",
            "properties": {
                "value": {
                    "anyOf": [
                        {"type": "string"},
                        {"type": "integer"}
                    ]
                }
            }
        });
        assert!(json_schema_to_protobuf(&schema, "test", "anyof_test").is_err());
    }

    #[test]
    fn test_convert_tools_to_schemas() {
        let tools = vec![
            (
                "fetch_html".to_string(),
                Some("Fetch HTML content".to_string()),
                json!({
                    "type": "object",
                    "properties": {
                        "url": {"type": "string"}
                    },
                    "required": ["url"]
                }),
            ),
            (
                "fetch".to_string(),
                Some("Fetch content".to_string()),
                json!({
                    "type": "object",
                    "properties": {
                        "url": {"type": "string"},
                        "timeout": {"type": "integer"}
                    },
                    "required": ["url"]
                }),
            ),
        ];

        let result = convert_tools_to_schemas(&tools, "fetch_server").unwrap();

        assert_eq!(result.len(), 2);
        assert!(result.contains_key("fetch_html"));
        assert!(result.contains_key("fetch"));

        let fetch_html = result.get("fetch_html").unwrap();
        assert!(fetch_html.proto_schema.contains("FetchServerFetchHtmlArgs"));

        let fetch = result.get("fetch").unwrap();
        assert!(fetch.proto_schema.contains("FetchServerFetchArgs"));
    }
}
