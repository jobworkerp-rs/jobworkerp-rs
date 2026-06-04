//! Provider-specific JSON Schema fixups applied before sending to genai.
//!
//! Centralised so workflow YAMLs can use the full draft-2020-12 vocabulary
//! regardless of which adapter the request lands on.

use genai::adapter::AdapterKind;
use serde_json::Value;

/// Adjust `schema` in place for the given adapter.
///
/// `None` means we could not classify the model — skip the fixups rather
/// than guess, since over-aggressive stripping is worse than letting the
/// provider reject a request explicitly.
pub fn sanitize_schema_for_adapter(schema: &mut Value, adapter: Option<AdapterKind>) {
    // Gemini's responseJsonSchema parser returns 400 INVALID_ARGUMENT when
    // any maxItems/minItems is present (bisected against gemini-3.1-flash-lite
    // in v1beta).
    if adapter == Some(AdapterKind::Gemini) {
        strip_gemini_unsupported(schema);
    }
}

fn strip_gemini_unsupported(node: &mut Value) {
    match node {
        Value::Object(map) => {
            map.remove("maxItems");
            map.remove("minItems");

            if let Some(Value::Object(props)) = map.get_mut("properties") {
                for v in props.values_mut() {
                    strip_gemini_unsupported(v);
                }
            }
            if let Some(items) = map.get_mut("items") {
                match items {
                    Value::Object(_) => strip_gemini_unsupported(items),
                    Value::Array(arr) => {
                        for v in arr.iter_mut() {
                            strip_gemini_unsupported(v);
                        }
                    }
                    _ => {}
                }
            }
            for key in ["prefixItems", "allOf", "anyOf", "oneOf"] {
                if let Some(Value::Array(arr)) = map.get_mut(key) {
                    for v in arr.iter_mut() {
                        strip_gemini_unsupported(v);
                    }
                }
            }
            if let Some(ap) = map.get_mut("additionalProperties")
                && ap.is_object()
            {
                strip_gemini_unsupported(ap);
            }
            for key in ["$defs", "definitions"] {
                if let Some(Value::Object(defs)) = map.get_mut(key) {
                    for v in defs.values_mut() {
                        strip_gemini_unsupported(v);
                    }
                }
            }
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                strip_gemini_unsupported(v);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema_with_max_items() -> Value {
        json!({
            "type": "array",
            "items": {"type": "string"},
            "maxItems": 5,
            "minItems": 1
        })
    }

    #[test]
    fn non_gemini_adapter_is_noop() {
        for adapter in [
            Some(AdapterKind::OpenAI),
            Some(AdapterKind::Anthropic),
            Some(AdapterKind::Ollama),
            None,
        ] {
            let mut schema = schema_with_max_items();
            let original = schema.clone();
            sanitize_schema_for_adapter(&mut schema, adapter);
            assert_eq!(schema, original, "schema must be untouched for {adapter:?}");
        }
    }

    #[test]
    fn gemini_strips_top_level_max_items() {
        let mut schema = schema_with_max_items();
        sanitize_schema_for_adapter(&mut schema, Some(AdapterKind::Gemini));
        assert!(schema.get("maxItems").is_none());
        assert!(schema.get("minItems").is_none());
        assert_eq!(schema["type"], "array");
        assert_eq!(schema["items"]["type"], "string");
    }

    #[test]
    fn gemini_strips_nested_in_properties() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "tags": {"type": "array", "items": {"type": "string"}, "maxItems": 3},
                "facts": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "links": {"type": "array", "items": {"type": "string"}, "maxItems": 4}
                        }
                    },
                    "maxItems": 30
                }
            }
        });
        sanitize_schema_for_adapter(&mut schema, Some(AdapterKind::Gemini));
        assert!(schema["properties"]["tags"].get("maxItems").is_none());
        assert!(schema["properties"]["facts"].get("maxItems").is_none());
        assert!(
            schema["properties"]["facts"]["items"]["properties"]["links"]
                .get("maxItems")
                .is_none()
        );
    }

    #[test]
    fn gemini_strips_inside_oneof_anyof_allof() {
        let mut schema = json!({
            "oneOf": [
                {"type": "array", "items": {"type": "string"}, "maxItems": 1},
                {"type": "object", "properties": {
                    "x": {"type": "array", "items": {"type": "integer"}, "maxItems": 2}
                }}
            ],
            "anyOf": [{"type": "array", "items": {}, "minItems": 1}],
            "allOf": [{"type": "object", "properties": {
                "y": {"type": "array", "items": {}, "maxItems": 9}
            }}]
        });
        sanitize_schema_for_adapter(&mut schema, Some(AdapterKind::Gemini));
        assert!(schema["oneOf"][0].get("maxItems").is_none());
        assert!(
            schema["oneOf"][1]["properties"]["x"]
                .get("maxItems")
                .is_none()
        );
        assert!(schema["anyOf"][0].get("minItems").is_none());
        assert!(
            schema["allOf"][0]["properties"]["y"]
                .get("maxItems")
                .is_none()
        );
    }

    #[test]
    fn gemini_strips_inside_prefix_items_and_additional_properties() {
        let mut schema = json!({
            "type": "object",
            "additionalProperties": {
                "type": "array", "items": {}, "maxItems": 7
            },
            "properties": {
                "tuple": {
                    "type": "array",
                    "prefixItems": [
                        {"type": "array", "items": {}, "maxItems": 3},
                        {"type": "string"}
                    ]
                }
            }
        });
        sanitize_schema_for_adapter(&mut schema, Some(AdapterKind::Gemini));
        assert!(schema["additionalProperties"].get("maxItems").is_none());
        assert!(
            schema["properties"]["tuple"]["prefixItems"][0]
                .get("maxItems")
                .is_none()
        );
    }

    #[test]
    fn gemini_strips_inside_defs() {
        let mut schema = json!({
            "$ref": "#/$defs/Node",
            "$defs": {
                "Node": {"type": "array", "items": {}, "maxItems": 10}
            },
            "definitions": {
                "Legacy": {"type": "array", "items": {}, "minItems": 2}
            }
        });
        sanitize_schema_for_adapter(&mut schema, Some(AdapterKind::Gemini));
        assert!(schema["$defs"]["Node"].get("maxItems").is_none());
        assert!(schema["definitions"]["Legacy"].get("minItems").is_none());
    }

    #[test]
    fn gemini_preserves_unrelated_keys() {
        // bisect proved these keys are safe — make sure the sanitizer leaves them alone.
        let mut schema = json!({
            "type": "object",
            "description": "keep me",
            "required": ["name"],
            "properties": {
                "name": {"type": "string", "maxLength": 100, "pattern": "^[a-z]+$",
                         "enum": ["foo", "bar"]},
                "score": {"type": "number", "minimum": 0.0, "maximum": 1.0}
            }
        });
        let expected = schema.clone();
        sanitize_schema_for_adapter(&mut schema, Some(AdapterKind::Gemini));
        assert_eq!(schema, expected);
    }

    #[test]
    fn gemini_handles_items_array_form() {
        // JSON Schema permits `items` to be an array of schemas (tuple validation).
        let mut schema = json!({
            "type": "array",
            "items": [
                {"type": "array", "items": {}, "maxItems": 1},
                {"type": "array", "items": {}, "minItems": 2}
            ]
        });
        sanitize_schema_for_adapter(&mut schema, Some(AdapterKind::Gemini));
        assert!(schema["items"][0].get("maxItems").is_none());
        assert!(schema["items"][1].get("minItems").is_none());
    }
}
