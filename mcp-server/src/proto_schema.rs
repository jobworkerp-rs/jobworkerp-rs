//! Safe, ProtoJSON-oriented schema projection for MCP tools.
//!
//! This deliberately differs from the legacy runner schema projection: MCP
//! results are encoded with ProtoJSON, where 64-bit integers are strings.

use anyhow::{Result, anyhow};
use prost_reflect::{Cardinality, FieldDescriptor, Kind, MessageDescriptor};
use serde_json::{Map, Value};
use std::collections::HashMap;

/// Maximum inline expansion depth for nested messages unless overridden via
/// `MCP_PROTO_SCHEMA_MAX_DEPTH`.
pub const DEFAULT_PROTO_SCHEMA_MAX_DEPTH: usize = 8;

/// Build a JSON Schema matching the ProtoJSON representation of a message.
#[cfg_attr(not(test), allow(dead_code))]
pub fn message_to_protojson_schema(descriptor: &MessageDescriptor) -> Result<Value> {
    message_to_protojson_schema_with_depth(descriptor, DEFAULT_PROTO_SCHEMA_MAX_DEPTH)
}

/// Build a JSON Schema matching the ProtoJSON representation of a message,
/// expanding nested messages inline up to `max_depth` levels below the root.
///
/// Inline expansion keeps the schema self-contained for MCP clients that do
/// not resolve `$defs`/`$ref` references. Beyond the depth limit the schema
/// falls back to a permissive object so self-referencing protos stay finite.
pub fn message_to_protojson_schema_with_depth(
    descriptor: &MessageDescriptor,
    max_depth: usize,
) -> Result<Value> {
    message_to_protojson_schema_with_node_budget(descriptor, max_depth, DEFAULT_MAX_SCHEMA_NODES)
}

/// Build a schema like [`message_to_protojson_schema_with_depth`], but abort
/// with an error once the generated schema exceeds `max_nodes`. Inline
/// expansion duplicates shared message types per path, so the node budget
/// bounds schema size (and downstream `jsonschema` compile cost) for wide
/// DAG-shaped contracts where the depth limit alone is not enough.
pub fn message_to_protojson_schema_with_node_budget(
    descriptor: &MessageDescriptor,
    max_depth: usize,
    max_nodes: usize,
) -> Result<Value> {
    let mut builder = SchemaBuilder::new(max_depth, max_nodes);
    builder.message_object(descriptor)
}

/// Upper bound on emitted schema nodes (objects plus property entries).
/// Chosen well above realistic tool contracts while still capping the
/// exponential blow-up of inline expansion.
pub const DEFAULT_MAX_SCHEMA_NODES: usize = 10_000;

struct SchemaBuilder {
    max_depth: usize,
    node_budget: usize,
}

impl SchemaBuilder {
    fn new(max_depth: usize, node_budget: usize) -> Self {
        Self {
            max_depth,
            node_budget,
        }
    }

    fn charge_node(&mut self) -> Result<()> {
        if self.node_budget == 0 {
            return Err(anyhow!(
                "schema node budget exceeded; the tool contract is too wide to project inline"
            ));
        }
        self.node_budget -= 1;
        Ok(())
    }

    fn message_object(&mut self, message: &MessageDescriptor) -> Result<Value> {
        if let Some(schema) = well_known_schema(message) {
            return Ok(schema);
        }
        if message.full_name() == "google.protobuf.Any" {
            return Err(anyhow!(
                "google.protobuf.Any requires a closed set of resolvable concrete types"
            ));
        }
        self.charge_node()?;
        let mut properties = Map::new();
        let mut required = Vec::new();
        for field in message.fields() {
            self.charge_node()?;
            properties.insert(field.json_name().to_string(), self.field_schema(&field)?);
            if field.cardinality() == Cardinality::Required {
                required.push(Value::String(field.json_name().to_string()));
            }
        }
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": properties,
            "additionalProperties": false
        });
        if !required.is_empty() {
            schema["required"] = Value::Array(required);
        }
        let aliases = message
            .fields()
            .map(|field| {
                (
                    field.name().to_string(),
                    vec![field.json_name().to_string()],
                )
            })
            .collect();
        let oneof_constraints = oneof_exclusion_constraints(message, &aliases);
        if !oneof_constraints.is_empty() {
            schema["allOf"] = Value::Array(oneof_constraints);
        }
        Ok(schema)
    }

    fn message_reference(&mut self, message: &MessageDescriptor) -> Result<Value> {
        if let Some(schema) = well_known_schema(message) {
            return Ok(schema);
        }
        if message.full_name() == "google.protobuf.Any" {
            return Err(anyhow!(
                "google.protobuf.Any requires a closed set of resolvable concrete types"
            ));
        }
        // Depth 0 is the root message, so nested messages at depth >= max_depth
        // are truncated to a permissive object to keep recursion finite. The
        // fallback intentionally drops `additionalProperties: false`: an overly
        // strict bound here would reject legitimate payloads whose shape the
        // schema can no longer describe.
        if self.max_depth == 0 {
            return Ok(serde_json::json!({"type":"object"}));
        }
        self.max_depth -= 1;
        let schema = self.message_object(message);
        self.max_depth += 1;
        schema
    }

    fn field_schema(&mut self, field: &FieldDescriptor) -> Result<Value> {
        if field.is_list() {
            return Ok(
                serde_json::json!({"type":"array", "items": self.kind_schema(&field.kind())?}),
            );
        }
        if field.is_map() {
            if let Kind::Message(entry) = field.kind()
                && let Some(value) = entry.fields().find(|field| field.number() == 2)
            {
                return Ok(serde_json::json!({
                    "type":"object",
                    "additionalProperties": self.kind_schema(&value.kind())?
                }));
            }
            return Ok(serde_json::json!({"type":"object", "additionalProperties":true}));
        }
        self.kind_schema(&field.kind())
    }

    fn kind_schema(&mut self, kind: &Kind) -> Result<Value> {
        match kind {
            Kind::Double | Kind::Float => Ok(serde_json::json!({
                "oneOf":[
                    {"type":"number"},
                    {"enum":["NaN", "Infinity", "-Infinity"]}
                ]
            })),
            Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 => Ok(
                serde_json::json!({"type":"integer", "minimum":-2147483648i64, "maximum":2147483647i64}),
            ),
            Kind::Uint32 | Kind::Fixed32 => {
                Ok(serde_json::json!({"type":"integer", "minimum":0, "maximum":4294967295u64}))
            }
            // ProtoJSON specifies decimal strings for every 64-bit integer kind.
            Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => {
                Ok(serde_json::json!({"type":"string", "pattern":"^-?(0|[1-9][0-9]*)$"}))
            }
            Kind::Uint64 | Kind::Fixed64 => {
                Ok(serde_json::json!({"type":"string", "pattern":"^(0|[1-9][0-9]*)$"}))
            }
            Kind::Bool => Ok(serde_json::json!({"type":"boolean"})),
            Kind::String => Ok(serde_json::json!({"type":"string"})),
            Kind::Bytes => Ok(serde_json::json!({"type":"string", "contentEncoding":"base64"})),
            Kind::Message(message) => self.message_reference(message),
            Kind::Enum(enumeration) if enumeration.full_name() == "google.protobuf.NullValue" => {
                Ok(serde_json::json!({"type":"null"}))
            }
            // ProtoJSON emits declared enum names as strings, but preserves an
            // unknown numeric enum value as a JSON integer.
            Kind::Enum(enumeration) => Ok(serde_json::json!({
                "oneOf":[
                    {
                        "type":"string",
                        "enum": enumeration
                            .values()
                            .map(|value| value.name().to_string())
                            .collect::<Vec<_>>()
                    },
                    {"type":"integer"}
                ]
            })),
        }
    }
}

/// Return JSON Schema constraints that permit at most one field from each
/// protobuf oneof. `aliases` maps a proto field name to every public name
/// accepted for that field.
pub(crate) fn oneof_exclusion_constraints(
    message: &MessageDescriptor,
    aliases: &HashMap<String, Vec<String>>,
) -> Vec<Value> {
    let mut constraints = Vec::new();
    for oneof in message.oneofs() {
        let names = oneof
            .fields()
            .flat_map(|field| {
                aliases
                    .get(field.name())
                    .cloned()
                    .unwrap_or_else(|| vec![field.json_name().to_string()])
            })
            .collect::<Vec<_>>();
        for (index, first) in names.iter().enumerate() {
            for second in names.iter().skip(index + 1) {
                constraints.push(serde_json::json!({
                    "not": {"required": [first, second]}
                }));
            }
        }
    }
    constraints
}

fn well_known_schema(message: &MessageDescriptor) -> Option<Value> {
    let schema = match message.full_name() {
        "google.protobuf.Timestamp" => serde_json::json!({"type":"string", "format":"date-time"}),
        "google.protobuf.Duration" => {
            serde_json::json!({"type":"string", "pattern":"^-?[0-9]+(?:\\.[0-9]{1,9})?s$"})
        }
        "google.protobuf.FieldMask" => serde_json::json!({"type":"string"}),
        "google.protobuf.Empty" => serde_json::json!({"type":"object", "maxProperties":0}),
        "google.protobuf.BoolValue" => serde_json::json!({"type":"boolean"}),
        "google.protobuf.StringValue" => serde_json::json!({"type":"string"}),
        "google.protobuf.BytesValue" => {
            serde_json::json!({"type":"string", "contentEncoding":"base64"})
        }
        "google.protobuf.Int32Value" | "google.protobuf.UInt32Value" => {
            serde_json::json!({"type":"integer"})
        }
        "google.protobuf.Int64Value" | "google.protobuf.UInt64Value" => {
            serde_json::json!({"type":"string"})
        }
        "google.protobuf.FloatValue" | "google.protobuf.DoubleValue" => serde_json::json!({
            "oneOf":[{"type":"number"}, {"enum":["NaN", "Infinity", "-Infinity"]}]
        }),
        "google.protobuf.Value" => serde_json::json!({}),
        "google.protobuf.Struct" => serde_json::json!({"type":"object", "additionalProperties":{}}),
        "google.protobuf.ListValue" => serde_json::json!({"type":"array", "items":{}}),
        _ => return None,
    };
    Some(schema)
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_PROTO_SCHEMA_MAX_DEPTH, message_to_protojson_schema,
        message_to_protojson_schema_with_depth, message_to_protojson_schema_with_node_budget,
    };
    use command_utils::protobuf::ProtobufDescriptor;

    fn descriptor(proto: &str, name: &str) -> prost_reflect::MessageDescriptor {
        ProtobufDescriptor::new(&proto.to_string())
            .unwrap()
            .get_message_by_name(name)
            .unwrap()
    }

    #[test]
    fn projects_64_bit_values_as_protojson_strings() {
        let proto = "syntax = \"proto3\"; message Response { int64 count = 1; uint64 total = 2; }";
        let descriptor = descriptor(proto, "Response");
        let schema = message_to_protojson_schema(&descriptor).unwrap();
        assert_eq!(schema["properties"]["count"]["type"], "string");
        assert_eq!(schema["properties"]["total"]["type"], "string");
        let bytes = ProtobufDescriptor::json_value_to_message(
            descriptor.clone(),
            &serde_json::json!({"count":"42", "total":"43"}),
            false,
            false,
        )
        .unwrap();
        let message = ProtobufDescriptor::get_message_from_bytes(descriptor, &bytes).unwrap();
        let output = ProtobufDescriptor::message_to_json_value(&message).unwrap();
        let validator = jsonschema::draft202012::new(&schema).unwrap();
        assert!(validator.is_valid(&output), "{output}");
    }

    #[test]
    fn represents_recursive_messages_inline_within_depth_limit() {
        let schema = message_to_protojson_schema_with_depth(
            &descriptor(
                "syntax = \"proto3\"; message Node { Node child = 1; }",
                "Node",
            ),
            2,
        )
        .unwrap();
        // max_depth counts message expansions below the root, so a limit of 2
        // inlines two levels and the third level falls back to a permissive
        // object schema.
        assert!(schema.get("$defs").is_none());
        let child2 = &schema["properties"]["child"]["properties"]["child"];
        assert_eq!(child2["type"], "object");
        let child3 = &child2["properties"]["child"];
        assert_eq!(child3["type"], "object");
        assert_eq!(child3.get("properties"), None);
    }

    #[test]
    fn expands_nested_oneof_messages_inline_without_definitions() {
        let proto = r#"
syntax = "proto3";
message TransitionIssueArgs {
  string issue_id = 2;
  oneof terminal_report {
    CompletionReport completion = 8;
    CancellationReport cancellation = 9;
  }
}
message CompletionReport {
  oneof result {
    VerifiedCompletion verified = 1;
    UnverifiedCompletion unverified = 2;
  }
}
message VerifiedCompletion { string evidence_id = 1; }
message UnverifiedCompletion { string reason = 1; }
message CancellationReport { string classification = 1; }
"#;
        let schema =
            message_to_protojson_schema(&descriptor(proto, "TransitionIssueArgs")).unwrap();
        assert!(schema.get("$defs").is_none());
        let completion = &schema["properties"]["completion"];
        assert_eq!(completion["type"], "object");
        assert_eq!(completion["properties"]["verified"]["type"], "object");
        // Nested oneof exclusions survive inline expansion.
        let validator = jsonschema::draft202012::new(&schema).unwrap();
        let valid_args = serde_json::json!({
            "issueId": "42",
            "completion": {"verified": {"evidenceId": "e1"}}
        });
        let errors: Vec<_> = validator
            .iter_errors(&valid_args)
            .map(|error| error.to_string())
            .collect();
        assert!(
            errors.is_empty(),
            "schema rejected valid args: {errors:?}; schema={schema}"
        );
        let conflicting = serde_json::json!({
            "issueId": "42",
            "completion": {
                "verified": {"evidence_id": "e1"},
                "unverified": {"reason": "why"}
            }
        });
        assert!(!validator.is_valid(&conflicting));
    }

    #[test]
    fn wide_dag_exceeding_node_budget_returns_error() {
        // A diamond proto: the shared Tail message is expanded once per path,
        // so a wide root quickly multiplies inline nodes.
        let proto = r#"
syntax = "proto3";
message Root { repeated Branch branches = 1; }
message Branch { repeated Leaf leaves = 1; }
message Leaf { repeated Tail tails = 1; }
message Tail { repeated TailNode nodes = 1; }
message TailNode { repeated TailLeaf leaves = 1; }
message TailLeaf { string value = 1; }
"#;
        let descriptor = descriptor(proto, "Root");
        let schema = message_to_protojson_schema_with_depth(&descriptor, 6).unwrap();
        // Sanity: schema generation succeeds for this modest contract.
        assert!(schema.get("$defs").is_none());

        // A tight node budget must abort generation instead of returning a
        // truncated (misleading) schema.
        let error = message_to_protojson_schema_with_node_budget(&descriptor, 6, 8).unwrap_err();
        assert!(error.to_string().contains("node budget exceeded"));
    }

    #[test]
    fn default_depth_expands_deeply_without_overflow() {
        let schema = message_to_protojson_schema(&descriptor(
            "syntax = \"proto3\"; message Node { Node child = 1; }",
            "Node",
        ))
        .unwrap();
        assert!(schema.get("$defs").is_none());
        let mut node = &schema["properties"]["child"];
        for _ in 0..DEFAULT_PROTO_SCHEMA_MAX_DEPTH {
            node = &node["properties"]["child"];
        }
        assert_eq!(node["type"], "object");
        assert_eq!(node.get("properties"), None);
    }

    #[test]
    fn enum_and_null_value_allow_their_protojson_forms() {
        let enum_schema = message_to_protojson_schema(&descriptor(
            "syntax = \"proto3\"; message Response { enum State { STATE_UNSPECIFIED = 0; } State state = 1; }",
            "Response",
        ))
        .unwrap();
        let state = &enum_schema["properties"]["state"];
        assert_eq!(state["oneOf"][0]["type"], "string");
        assert_eq!(state["oneOf"][1]["type"], "integer");

        let null_schema = message_to_protojson_schema(&descriptor(
            "syntax = \"proto3\"; package google.protobuf; enum NullValue { NULL_VALUE = 0; } message Response { NullValue value = 1; }",
            "google.protobuf.Response",
        ))
        .unwrap();
        assert_eq!(null_schema["properties"]["value"]["type"], "null");
    }

    #[test]
    fn struct_keeps_its_dynamic_object_properties() {
        let schema = message_to_protojson_schema(&descriptor(
            "syntax = \"proto3\"; package google.protobuf; message Struct {}",
            "google.protobuf.Struct",
        ))
        .unwrap();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], serde_json::json!({}));
    }

    #[test]
    fn oneof_fields_are_optional_but_mutually_exclusive() {
        let schema = message_to_protojson_schema(&descriptor(
            "syntax = \"proto3\"; message Request { oneof selector { string id = 1; string name = 2; } }",
            "Request",
        ))
        .unwrap();
        let validator = jsonschema::draft202012::new(&schema).unwrap();

        assert!(validator.is_valid(&serde_json::json!({})));
        assert!(validator.is_valid(&serde_json::json!({"id":"123"})));
        assert!(validator.is_valid(&serde_json::json!({"name":"abc"})));
        assert!(!validator.is_valid(&serde_json::json!({"id":"123", "name":"abc"})));
    }
}
