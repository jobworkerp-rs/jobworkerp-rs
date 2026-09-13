//! Safe, ProtoJSON-oriented schema projection for MCP tools.
//!
//! This deliberately differs from the legacy runner schema projection: MCP
//! results are encoded with ProtoJSON, where 64-bit integers are strings.

use anyhow::{Result, anyhow};
use prost_reflect::{Cardinality, FieldDescriptor, Kind, MessageDescriptor};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

/// Build a JSON Schema matching the ProtoJSON representation of a message.
pub fn message_to_protojson_schema(descriptor: &MessageDescriptor) -> Result<Value> {
    let mut builder = SchemaBuilder::default();
    let mut schema = builder.message_object(descriptor)?;
    if !builder.definitions.is_empty() {
        schema["$defs"] = Value::Object(builder.definitions);
    }
    Ok(schema)
}

#[derive(Default)]
struct SchemaBuilder {
    definitions: Map<String, Value>,
    building: HashSet<String>,
}

impl SchemaBuilder {
    fn message_object(&mut self, message: &MessageDescriptor) -> Result<Value> {
        if let Some(schema) = well_known_schema(message) {
            return Ok(schema);
        }
        if message.full_name() == "google.protobuf.Any" {
            return Err(anyhow!(
                "google.protobuf.Any requires a closed set of resolvable concrete types"
            ));
        }
        let mut properties = Map::new();
        let mut required = Vec::new();
        for field in message.fields() {
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
        let name = message.full_name().to_string();
        if !self.definitions.contains_key(&name) && self.building.insert(name.clone()) {
            let definition = self.message_object(message)?;
            self.building.remove(&name);
            self.definitions.insert(name.clone(), definition);
        }
        Ok(serde_json::json!({"$ref": format!("#/$defs/{name}")}))
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
    use super::message_to_protojson_schema;
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
    fn represents_recursive_messages_with_definitions() {
        let schema = message_to_protojson_schema(&descriptor(
            "syntax = \"proto3\"; message Node { Node child = 1; }",
            "Node",
        ))
        .unwrap();
        assert_eq!(schema["properties"]["child"]["$ref"], "#/$defs/Node");
        assert_eq!(
            schema["$defs"]["Node"]["properties"]["child"]["$ref"],
            "#/$defs/Node"
        );
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
