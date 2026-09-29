use super::{ChatError, ChatMessage, ChatRequest, ModelOptions, RegisteredTool, Role, ToolCall};
use serde_json::{Map, Value};
use std::collections::HashSet;

pub(super) fn sanitize_history(history: Vec<ChatMessage>) -> Result<Vec<ChatMessage>, ChatError> {
    history
        .into_iter()
        .filter(|message| message.role != Role::System)
        .map(|message| {
            if message.tool_execution_requests.is_some()
                || contains_execution_request(&message.content)
                || message
                    .tool_calls
                    .iter()
                    .any(|call| contains_execution_request(&call.arguments))
                || message
                    .tool_results
                    .iter()
                    .any(|result| contains_execution_request(&result.content))
            {
                return Err(ChatError::UnsafeHistory);
            }
            Ok(message)
        })
        .collect()
}

fn contains_execution_request(value: &Value) -> bool {
    match value {
        Value::Object(fields) => {
            fields.contains_key("tool_execution_requests")
                || fields.values().any(contains_execution_request)
        }
        Value::Array(values) => values.iter().any(contains_execution_request),
        _ => false,
    }
}

pub(super) fn validate_registered_tool(tool: &RegisteredTool) -> Result<(), ChatError> {
    if tool.name.trim().is_empty() || tool.description.trim().is_empty() {
        return Err(ChatError::InvalidToolDefinition(
            "tool name and description must be non-empty".to_owned(),
        ));
    }
    if tool.worker_id <= 0 || tool.method.trim().is_empty() {
        return Err(ChatError::InvalidToolDefinition(
            "tool target requires a positive Worker ID and method".to_owned(),
        ));
    }
    if tool.schema_revision.trim().is_empty() {
        return Err(ChatError::InvalidToolDefinition(
            "tool target requires a method schema revision".to_owned(),
        ));
    }
    if !tool.input_schema.is_object() {
        return Err(ChatError::InvalidToolDefinition(
            "tool input schema must be a JSON object".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn validate_tool_call_list(calls: &[ToolCall]) -> Result<(), ChatError> {
    let mut ids = HashSet::new();
    for call in calls {
        if call.call_id.is_empty() || call.call_id.len() > 128 {
            return Err(ChatError::InvalidCallId);
        }
        if call.name.trim().is_empty() {
            return Err(ChatError::InvalidModelToolCalls(
                "tool name must be non-empty".to_owned(),
            ));
        }
        if !ids.insert(call.call_id.as_str()) {
            return Err(ChatError::InvalidModelToolCalls(
                "duplicate call_id in one model response".to_owned(),
            ));
        }
    }
    Ok(())
}

pub(super) fn validate_exact_arguments(arguments: &Value, allowed: &[&str]) -> Result<(), String> {
    let object = arguments
        .as_object()
        .ok_or_else(|| "tool arguments must be a JSON object".to_owned())?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("tool arguments contain unsupported fields".to_owned());
    }
    for required in allowed {
        if !object.contains_key(*required) {
            return Err(format!("missing required argument `{required}`"));
        }
    }
    Ok(())
}

/// Validate the useful JSON Schema subset emitted by Worker method schema resolvers. Unknown
/// constraints fail closed rather than allowing arguments whose meaning was not checked.
pub(super) fn validate_tool_arguments(arguments: &Value, schema: &Value) -> Result<(), String> {
    validate_schema_value(arguments, schema, "$", true)
}

fn validate_schema_value(
    value: &Value,
    schema: &Value,
    path: &str,
    top_level: bool,
) -> Result<(), String> {
    let schema_object = schema
        .as_object()
        .ok_or_else(|| format!("input schema at {path} must be an object"))?;
    const ALLOWED_KEYWORDS: &[&str] = &[
        "$schema",
        "title",
        "description",
        "examples",
        "default",
        "type",
        "enum",
        "const",
        "required",
        "properties",
        "additionalProperties",
        "items",
        "minLength",
        "maxLength",
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "minItems",
        "maxItems",
    ];
    if let Some(unsupported) = schema_object
        .keys()
        .find(|key| !ALLOWED_KEYWORDS.contains(&key.as_str()))
    {
        return Err(format!(
            "unsupported schema keyword `{unsupported}` at {path}"
        ));
    }
    let expected_type = schema_object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("input schema at {path} must declare a supported type"))?;
    let type_matches = match expected_type {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value
            .as_number()
            .is_some_and(|number| number.is_i64() || number.is_u64()),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => {
            return Err(format!(
                "unsupported JSON Schema type `{expected_type}` at {path}"
            ));
        }
    };
    if !type_matches {
        return Err(format!(
            "argument at {path} must have type `{expected_type}`"
        ));
    }
    if let Some(choices) = schema_object.get("enum") {
        let choices = choices
            .as_array()
            .ok_or_else(|| format!("enum at {path} must be an array"))?;
        if !choices.contains(value) {
            return Err(format!("argument at {path} is not an allowed value"));
        }
    }
    if schema_object
        .get("const")
        .is_some_and(|expected| expected != value)
    {
        return Err(format!(
            "argument at {path} does not match its required value"
        ));
    }

    match value {
        Value::Object(object) => validate_object(object, schema_object, path, top_level)?,
        Value::Array(items) => validate_array(items, schema_object, path)?,
        Value::String(text) => validate_string(text, schema_object, path)?,
        Value::Number(number) => {
            validate_number(number.as_f64().unwrap_or(f64::NAN), schema_object, path)?
        }
        _ => {}
    }
    Ok(())
}

fn validate_object(
    value: &Map<String, Value>,
    schema: &Map<String, Value>,
    path: &str,
    top_level: bool,
) -> Result<(), String> {
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("object schema at {path} must declare properties"))?;
    if let Some(required) = schema.get("required") {
        let required = required
            .as_array()
            .ok_or_else(|| format!("required at {path} must be an array"))?;
        for name in required {
            let name = name
                .as_str()
                .ok_or_else(|| format!("required names at {path} must be strings"))?;
            if !value.contains_key(name) {
                return Err(format!("missing required argument `{name}`"));
            }
        }
    }
    let additional = schema.get("additionalProperties");
    for (name, child_value) in value {
        let child_path = format!("{path}.{name}");
        if let Some(child_schema) = properties.get(name) {
            validate_schema_value(child_value, child_schema, &child_path, false)?;
        } else if let Some(additional_schema) = additional.and_then(Value::as_object) {
            validate_schema_value(
                child_value,
                &Value::Object(additional_schema.clone()),
                &child_path,
                false,
            )?;
        } else if additional == Some(&Value::Bool(true)) && !top_level {
            continue;
        } else {
            return Err(format!("unsupported argument `{name}`"));
        }
    }
    Ok(())
}

fn validate_array(values: &[Value], schema: &Map<String, Value>, path: &str) -> Result<(), String> {
    if let Some(minimum) = schema.get("minItems").and_then(Value::as_u64)
        && values.len() < minimum as usize
    {
        return Err(format!("array at {path} has too few items"));
    }
    if let Some(maximum) = schema.get("maxItems").and_then(Value::as_u64)
        && values.len() > maximum as usize
    {
        return Err(format!("array at {path} has too many items"));
    }
    let items = schema
        .get("items")
        .ok_or_else(|| format!("array schema at {path} must declare items"))?;
    for (index, value) in values.iter().enumerate() {
        validate_schema_value(value, items, &format!("{path}[{index}]"), false)?;
    }
    Ok(())
}

fn validate_string(value: &str, schema: &Map<String, Value>, path: &str) -> Result<(), String> {
    if schema
        .get("minLength")
        .and_then(Value::as_u64)
        .is_some_and(|minimum| value.chars().count() < minimum as usize)
    {
        return Err(format!("string at {path} is shorter than allowed"));
    }
    if schema
        .get("maxLength")
        .and_then(Value::as_u64)
        .is_some_and(|maximum| value.chars().count() > maximum as usize)
    {
        return Err(format!("string at {path} is longer than allowed"));
    }
    Ok(())
}

fn validate_number(value: f64, schema: &Map<String, Value>, path: &str) -> Result<(), String> {
    type NumberBoundary<'a> = (&'static str, Option<&'a Value>, fn(f64, f64) -> bool);
    let bounds: [NumberBoundary<'_>; 4] = [
        (
            "minimum",
            schema.get("minimum"),
            |value: f64, bound: f64| value >= bound,
        ),
        (
            "maximum",
            schema.get("maximum"),
            |value: f64, bound: f64| value <= bound,
        ),
        (
            "exclusiveMinimum",
            schema.get("exclusiveMinimum"),
            |value: f64, bound: f64| value > bound,
        ),
        (
            "exclusiveMaximum",
            schema.get("exclusiveMaximum"),
            |value: f64, bound: f64| value < bound,
        ),
    ];
    for (name, bound, test) in bounds {
        if let Some(bound) = bound {
            let bound = bound
                .as_f64()
                .ok_or_else(|| format!("{name} at {path} must be a number"))?;
            if !test(value, bound) {
                return Err(format!("number at {path} violates {name}"));
            }
        }
    }
    Ok(())
}

pub(super) fn validate_model_request(request: &ChatRequest) -> Result<(), ChatError> {
    validate_model_parameters(request.llm_worker_id, &request.options)
}

pub(super) fn validate_model_parameters(
    worker_id: i64,
    options: &ModelOptions,
) -> Result<(), ChatError> {
    if worker_id <= 0 {
        return Err(ChatError::InvalidModelSelection);
    }
    if options
        .temperature
        .is_some_and(|temperature| !(0.0..=2.0).contains(&temperature))
        || options
            .top_p
            .is_some_and(|top_p| !(0.0..=1.0).contains(&top_p))
        || options.max_tokens.is_some_and(|max_tokens| max_tokens == 0)
    {
        return Err(ChatError::InvalidModelOptions);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_tool_arguments;
    use serde_json::json;

    #[test]
    fn schema_validation_preserves_type_required_and_boundary_errors() {
        let schema = json!({
            "type": "object",
            "properties": {
                "count": {"type": "integer", "minimum": 1, "maximum": 3},
                "name": {"type": "string", "minLength": 2, "maxLength": 5}
            },
            "required": ["count", "name"],
            "additionalProperties": false
        });

        assert_eq!(
            validate_tool_arguments(&json!({"count": 1, "name": "ab"}), &schema),
            Ok(())
        );
        assert_eq!(
            validate_tool_arguments(&json!({"count": 3, "name": "abcde"}), &schema),
            Ok(())
        );
        assert_eq!(
            validate_tool_arguments(&json!({"count": 1}), &schema),
            Err("missing required argument `name`".to_owned())
        );
        assert_eq!(
            validate_tool_arguments(&json!({"count": 1.5, "name": "ab"}), &schema),
            Err("argument at $.count must have type `integer`".to_owned())
        );
        assert_eq!(
            validate_tool_arguments(&json!({"count": 0, "name": "ab"}), &schema),
            Err("number at $.count violates minimum".to_owned())
        );
        assert_eq!(
            validate_tool_arguments(&json!({"count": 1, "name": "abcdef"}), &schema),
            Err("string at $.name is longer than allowed".to_owned())
        );
    }

    #[test]
    fn unsupported_schema_keywords_fail_before_argument_type_validation() {
        let schema = json!({"type": "integer", "pattern": "^ok$"});

        assert_eq!(
            validate_tool_arguments(&json!("not an integer"), &schema),
            Err("unsupported schema keyword `pattern` at $".to_owned())
        );
    }

    #[test]
    fn nested_additional_properties_schema_validates_values() {
        let schema = json!({
            "type": "object",
            "properties": {
                "labels": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": {"type": "string", "minLength": 2}
                }
            },
            "required": ["labels"],
            "additionalProperties": false
        });

        assert_eq!(
            validate_tool_arguments(&json!({"labels": {"region": "us"}}), &schema),
            Ok(())
        );
        assert_eq!(
            validate_tool_arguments(&json!({"labels": {"region": 7}}), &schema),
            Err("argument at $.labels.region must have type `string`".to_owned())
        );
    }
}
