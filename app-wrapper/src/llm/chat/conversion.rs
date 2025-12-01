use app::app::function::helper::McpNameConverter;
use command_utils::util::json_schema::SchemaCombiner;
use proto::jobworkerp::function::data::FunctionSpecs;
use rmcp::model::{ListToolsResult, Tool};
use rmcp::ErrorData as McpError;
use serde_json;
use tracing;

pub struct ToolConverter;
impl McpNameConverter for ToolConverter {}

impl ToolConverter {
    /// Convert FunctionSpecs to MCP Tools by combining settings and arguments schemas.
    /// All runners use unified settings + arguments structure for consistency across runner types.
    pub fn convert_normal_function(tool: &FunctionSpecs) -> Vec<Tool> {
        tool.methods
            .as_ref()
            .map(|methods| {
                methods
                    .schemas
                    .iter()
                    .filter_map(|(method_name, method_schema)| {
                        let mut schema_combiner = SchemaCombiner::new();

                        if !tool.settings_schema.is_empty() {
                            schema_combiner
                                .add_schema_from_string(
                                    "settings",
                                    &tool.settings_schema,
                                    Some("Tool init settings".to_string()),
                                )
                                .ok();
                        }

                        schema_combiner
                            .add_schema_from_string(
                                "arguments",
                                &method_schema.arguments_schema,
                                Some("Tool arguments".to_string()),
                            )
                            .inspect_err(|e| {
                                tracing::error!("Failed to parse arguments schema: {}", e)
                            })
                            .ok();

                        match schema_combiner.generate_combined_schema() {
                            Ok(schema) => {
                                // Default method uses runner name only for simpler tool naming.
                                // Non-default methods combine runner and method names to avoid conflicts.
                                let tool_name = if method_name == proto::DEFAULT_METHOD_NAME {
                                    tool.name.clone()
                                } else {
                                    Self::combine_names(&tool.name, method_name)
                                };

                                Some(Tool::new(
                                    tool_name,
                                    method_schema
                                        .description
                                        .clone()
                                        .unwrap_or_else(|| tool.description.clone()),
                                    schema,
                                ))
                            }
                            Err(e) => {
                                tracing::error!(
                                    "Failed to generate schema for method '{}': {}",
                                    method_name,
                                    e
                                );
                                None
                            }
                        }
                    })
                    .collect()
            })
            .unwrap_or_else(|| {
                tracing::error!("error: no methods found for runner: {:?}", &tool);
                vec![]
            })
    }

    pub fn convert_functions_to_mcp_tools(
        functions: Vec<FunctionSpecs>,
    ) -> Result<ListToolsResult, McpError> {
        let tool_list = functions
            .into_iter()
            .flat_map(|tool| Self::convert_normal_function(&tool))
            .collect::<Vec<_>>();

        Ok(ListToolsResult {
            tools: tool_list,
            next_cursor: None,
        })
    }

    /// Convert MCP Tool to Ollama ToolInfo
    fn mcp_tool_to_ollama(tool: &Tool) -> Option<ollama_rs::generation::tools::ToolInfo> {
        use ollama_rs::generation::tools::{ToolFunctionInfo, ToolInfo, ToolType};
        let params_schema: Option<schemars::Schema> =
            serde_json::from_value(serde_json::Value::Object((*tool.input_schema).clone())).ok();
        params_schema.map(|parameters| ToolInfo {
            tool_type: ToolType::Function,
            function: ToolFunctionInfo {
                name: tool.name.to_string(),
                description: tool
                    .description
                    .as_ref()
                    .map(|d| d.to_string())
                    .unwrap_or_default(),
                parameters,
            },
        })
    }

    /// Convert MCP Tool to GenAI Tool
    fn mcp_tool_to_genai(tool: &Tool) -> genai::chat::Tool {
        use genai::chat::Tool as GenAITool;
        GenAITool {
            name: tool.name.to_string(),
            description: tool.description.as_ref().map(|d| d.to_string()),
            schema: Some(serde_json::Value::Object((*tool.input_schema).clone())),
            config: None,
        }
    }

    /// Convert a list of FunctionSpecs to Vec<ToolInfo> for Ollama FunctionCalling
    pub fn convert_functions_to_ollama_tools(
        functions: Vec<FunctionSpecs>,
    ) -> Vec<ollama_rs::generation::tools::ToolInfo> {
        functions
            .into_iter()
            .flat_map(|tool| {
                Self::convert_normal_function(&tool)
                    .into_iter()
                    .filter_map(|mcp_tool| Self::mcp_tool_to_ollama(&mcp_tool))
            })
            .collect()
    }

    /// Convert a list of FunctionSpecs to Vec<genai::chat::Tool> for genai FunctionCalling
    pub fn convert_functions_to_genai_tools(
        functions: Vec<FunctionSpecs>,
    ) -> Vec<genai::chat::Tool> {
        functions
            .into_iter()
            .flat_map(|tool| {
                Self::convert_normal_function(&tool)
                    .into_iter()
                    .map(|mcp_tool| Self::mcp_tool_to_genai(&mcp_tool))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::jobworkerp::function::data::FunctionSpecs;
    use serde_json::{json, Value};

    fn make_single_schema_spec() -> FunctionSpecs {
        let mut method_schemas = std::collections::HashMap::new();
        method_schemas.insert(
            proto::DEFAULT_METHOD_NAME.to_string(),
            proto::jobworkerp::function::data::MethodSchema {
                description: Some("desc_single".to_string()),
                arguments_schema:
                    json!({"type": "object", "properties": {"arg_a": {"type": "boolean"}}})
                        .to_string(),
                result_schema: Some(
                    json!({"type": "object", "properties": {"result": {"type": "string"}}})
                        .to_string(),
                ),
                output_type: proto::jobworkerp::data::StreamingOutputType::NonStreaming as i32,
                annotations: None,
            },
        );

        FunctionSpecs {
            name: "test_single".to_string(),
            description: "desc_single".to_string(),
            settings_schema:
                json!({"type": "object", "properties": {"setting_a": {"type": "string"}}})
                    .to_string(),
            methods: Some(proto::jobworkerp::function::data::MethodSchemaMap {
                schemas: method_schemas,
            }),
            runner_type: proto::jobworkerp::data::RunnerType::Command as i32,
            worker_id: None,
            ..Default::default()
        }
    }

    fn make_reusable_workflow_spec() -> FunctionSpecs {
        let mut method_schemas = std::collections::HashMap::new();
        method_schemas.insert(
            proto::DEFAULT_METHOD_NAME.to_string(),
            proto::jobworkerp::function::data::MethodSchema {
                description: Some("desc_workflow".to_string()),
                arguments_schema:
                    json!({"type": "object", "properties": {"arg_b": {"type": "number"}}})
                        .to_string(),
                result_schema: Some(
                    json!({"type": "object", "properties": {"result": {"type": "string"}}})
                        .to_string(),
                ),
                output_type: proto::jobworkerp::data::StreamingOutputType::NonStreaming as i32,
                annotations: None,
            },
        );

        FunctionSpecs {
            name: "test_workflow".to_string(),
            description: "desc_workflow".to_string(),
            settings_schema:
                json!({"type": "object", "properties": {"setting_b": {"type": "integer"}}})
                    .to_string(),
            methods: Some(proto::jobworkerp::function::data::MethodSchemaMap {
                schemas: method_schemas,
            }),
            runner_type: proto::jobworkerp::data::RunnerType::ReusableWorkflow as i32,
            worker_id: None,
            ..Default::default()
        }
    }

    fn make_mcp_tools_spec() -> FunctionSpecs {
        let mut method_schemas = std::collections::HashMap::new();
        method_schemas.insert(
            "inner".to_string(),
            proto::jobworkerp::function::data::MethodSchema {
                description: Some("desc_inner".to_string()),
                arguments_schema:
                    json!({"type": "object", "properties": {"c": {"type": "boolean"}}}).to_string(),
                result_schema: None,
                output_type: proto::jobworkerp::data::StreamingOutputType::NonStreaming as i32,
                annotations: None,
            },
        );

        FunctionSpecs {
            name: "test_mcp".to_string(),
            description: "desc_mcp".to_string(),
            settings_schema: String::new(), // MCP Server typically doesn't have settings
            methods: Some(proto::jobworkerp::function::data::MethodSchemaMap {
                schemas: method_schemas,
            }),
            runner_type: proto::jobworkerp::data::RunnerType::McpServer as i32,
            worker_id: None,
            ..Default::default()
        }
    }

    fn make_multi_method_plugin_spec() -> FunctionSpecs {
        let mut method_schemas = std::collections::HashMap::new();

        method_schemas.insert(
            "method_a".to_string(),
            proto::jobworkerp::function::data::MethodSchema {
                description: Some("First method".to_string()),
                arguments_schema:
                    json!({"type": "object", "properties": {"param_a": {"type": "string"}}})
                        .to_string(),
                result_schema: Some(
                    json!({"type": "object", "properties": {"output_a": {"type": "string"}}})
                        .to_string(),
                ),
                output_type: proto::jobworkerp::data::StreamingOutputType::NonStreaming as i32,
                annotations: None,
            },
        );

        method_schemas.insert(
            "method_b".to_string(),
            proto::jobworkerp::function::data::MethodSchema {
                description: Some("Second method".to_string()),
                arguments_schema:
                    json!({"type": "object", "properties": {"param_b": {"type": "integer"}}})
                        .to_string(),
                result_schema: Some(
                    json!({"type": "object", "properties": {"output_b": {"type": "integer"}}})
                        .to_string(),
                ),
                output_type: proto::jobworkerp::data::StreamingOutputType::NonStreaming as i32,
                annotations: None,
            },
        );

        FunctionSpecs {
            name: "test_plugin".to_string(),
            description: "Multi-method plugin".to_string(),
            settings_schema:
                json!({"type": "object", "properties": {"api_key": {"type": "string"}}}).to_string(),
            methods: Some(proto::jobworkerp::function::data::MethodSchemaMap {
                schemas: method_schemas,
            }),
            runner_type: proto::jobworkerp::data::RunnerType::Plugin as i32,
            worker_id: None,
            ..Default::default()
        }
    }

    /// Helper function to verify schema has required fields
    fn assert_schema_required_fields(
        schema: &serde_json::Map<String, Value>,
        required_fields: &[&str],
    ) {
        let required = schema.get("required").and_then(|r| r.as_array());
        assert!(required.is_some(), "Schema should have 'required' field");

        let required_values: Vec<String> = required
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();

        for field in required_fields {
            assert!(
                required_values.contains(&field.to_string()),
                "Field '{field}' should be in required list: {required_values:?}"
            );
        }
    }

    /// Helper function to verify schema has specific properties with expected structure
    fn assert_schema_property(
        schema: &serde_json::Map<String, Value>,
        property_name: &str,
        expected_property: &Value,
    ) {
        let properties = schema.get("properties").and_then(|p| p.as_object());
        assert!(
            properties.is_some(),
            "Schema should have 'properties' field"
        );

        let properties = properties.unwrap();
        assert!(
            properties.contains_key(property_name),
            "Schema should contain property '{property_name}'"
        );

        let actual_property = &properties[property_name];
        assert_json_subset(expected_property, actual_property);
    }

    /// Verifies that expected is a subset of actual (all keys in expected exist in actual with the same values)
    fn assert_json_subset(expected: &Value, actual: &Value) {
        match (expected, actual) {
            (Value::Object(exp_obj), Value::Object(act_obj)) => {
                for (key, exp_val) in exp_obj {
                    assert!(
                        act_obj.contains_key(key),
                        "Expected key '{key}' not found in actual"
                    );
                    assert_json_subset(exp_val, &act_obj[key]);
                }
            }
            (Value::Array(exp_arr), Value::Array(act_arr)) => {
                assert!(
                    exp_arr.len() <= act_arr.len(),
                    "Expected array length {} but got {}",
                    exp_arr.len(),
                    act_arr.len()
                );
                for (exp_val, act_val) in exp_arr.iter().zip(act_arr.iter()) {
                    assert_json_subset(exp_val, act_val);
                }
            }
            (exp, act) => {
                assert_eq!(exp, act, "Expected value {exp:?} but got {act:?}");
            }
        }
    }

    #[test]
    fn test_convert_functions_to_mcp_tools() {
        let specs = vec![
            make_single_schema_spec(),
            make_reusable_workflow_spec(),
            make_mcp_tools_spec(),
            make_multi_method_plugin_spec(),
        ];
        let result = ToolConverter::convert_functions_to_mcp_tools(specs).unwrap();

        // SingleSchema
        let tool = result
            .tools
            .iter()
            .find(|t| t.name == "test_single")
            .unwrap();
        assert_eq!(tool.name, "test_single");
        assert_eq!(tool.description.as_ref().unwrap(), "desc_single");

        assert_eq!(
            tool.input_schema.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "Schema should have type 'object'"
        );

        assert_schema_required_fields(&tool.input_schema, &["arguments", "settings"]);

        let arguments_expected = json!({
            "type": "object",
            "properties": {
                "arg_a": {
                    "type": "boolean"
                }
            },
            "description": "Tool arguments"
        });
        assert_schema_property(&tool.input_schema, "arguments", &arguments_expected);

        let settings_expected = json!({
            "type": "object",
            "properties": {
                "setting_a": {
                    "type": "string"
                }
            },
            "description": "Tool init settings"
        });
        assert_schema_property(&tool.input_schema, "settings", &settings_expected);

        // ReusableWorkflow
        let tool = result
            .tools
            .iter()
            .find(|t| t.name == "test_workflow")
            .unwrap();
        assert_eq!(tool.name, "test_workflow");
        assert_eq!(tool.description.as_ref().unwrap(), "desc_workflow");

        // ReusableWorkflow now uses settings/arguments structure like other runners
        assert_schema_required_fields(&tool.input_schema, &["arguments"]);

        let expected_settings = json!({
            "type": "object",
            "properties": {
                "setting_b": {
                    "type": "integer"
                }
            },
            "description": "Tool init settings"
        });
        assert_schema_property(&tool.input_schema, "settings", &expected_settings);

        let expected_arguments = json!({
            "type": "object",
            "properties": {
                "arg_b": {
                    "type": "number"
                }
            },
            "description": "Tool arguments"
        });
        assert_schema_property(&tool.input_schema, "arguments", &expected_arguments);

        // McpTools
        let tool = result
            .tools
            .iter()
            .find(|t| t.name == "test_mcp___inner")
            .unwrap();
        assert_eq!(tool.name, "test_mcp___inner");
        assert_eq!(tool.description.as_ref().unwrap(), "desc_inner");

        // McpTools should have arguments with property c (settings/arguments structure)
        assert_schema_required_fields(&tool.input_schema, &["arguments"]);

        let expected_arguments = json!({
            "type": "object",
            "properties": {
                "c": {
                    "type": "boolean"
                }
            },
            "description": "Tool arguments"
        });
        assert_schema_property(&tool.input_schema, "arguments", &expected_arguments);

        // Multi-method Plugin - should generate 2 tools
        let plugin_tools: Vec<_> = result
            .tools
            .iter()
            .filter(|t| t.name.starts_with("test_plugin"))
            .collect();
        assert_eq!(
            plugin_tools.len(),
            2,
            "Multi-method plugin should generate 2 tools"
        );

        let tool_a = result
            .tools
            .iter()
            .find(|t| t.name == "test_plugin___method_a")
            .expect("Should have test_plugin___method_a tool");
        assert_eq!(tool_a.name, "test_plugin___method_a");
        assert_eq!(tool_a.description.as_ref().unwrap(), "First method");

        assert_schema_required_fields(&tool_a.input_schema, &["arguments", "settings"]);

        let expected_settings = json!({
            "type": "object",
            "properties": {
                "api_key": {
                    "type": "string"
                }
            },
            "description": "Tool init settings"
        });
        assert_schema_property(&tool_a.input_schema, "settings", &expected_settings);

        let expected_arguments_a = json!({
            "type": "object",
            "properties": {
                "param_a": {
                    "type": "string"
                }
            },
            "description": "Tool arguments"
        });
        assert_schema_property(&tool_a.input_schema, "arguments", &expected_arguments_a);

        let tool_b = result
            .tools
            .iter()
            .find(|t| t.name == "test_plugin___method_b")
            .expect("Should have test_plugin___method_b tool");
        assert_eq!(tool_b.name, "test_plugin___method_b");
        assert_eq!(tool_b.description.as_ref().unwrap(), "Second method");

        assert_schema_required_fields(&tool_b.input_schema, &["arguments", "settings"]);

        let expected_arguments_b = json!({
            "type": "object",
            "properties": {
                "param_b": {
                    "type": "integer"
                }
            },
            "description": "Tool arguments"
        });
        assert_schema_property(&tool_b.input_schema, "arguments", &expected_arguments_b);
        assert_schema_property(&tool_b.input_schema, "settings", &expected_settings);
    }
    #[test]
    fn test_convert_functions_to_ollama_tools() {
        let specs = vec![
            make_single_schema_spec(),
            make_reusable_workflow_spec(),
            make_mcp_tools_spec(),
            make_multi_method_plugin_spec(),
        ];
        let result = ToolConverter::convert_functions_to_ollama_tools(specs);

        // SingleSchema
        let tool = result
            .iter()
            .find(|t| t.function.name == "test_single")
            .unwrap();
        println!("single Tool: {tool:#?}");
        assert_eq!(tool.function.name, "test_single");
        assert_eq!(tool.function.description, "desc_single");

        let schema_value = tool.function.parameters.clone().to_value();

        let schema_obj = schema_value.as_object().unwrap();
        assert_eq!(
            schema_obj.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "Schema should have type 'object'"
        );

        let props = schema_obj
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            props.contains_key("settings"),
            "Schema should contain settings property"
        );
        assert!(
            props.contains_key("arguments"),
            "Schema should contain arguments property"
        );

        let settings = props.get("settings").and_then(|s| s.as_object()).unwrap();
        let settings_props = settings
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            settings_props.contains_key("setting_a"),
            "Settings should contain setting_a property"
        );

        let setting_a = settings_props
            .get("setting_a")
            .and_then(|s| s.as_object())
            .unwrap();
        assert_eq!(
            setting_a.get("type").and_then(|t| t.as_str()),
            Some("string"),
            "setting_a should have type string"
        );

        let arguments = props.get("arguments").and_then(|s| s.as_object()).unwrap();
        let args_props = arguments
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            args_props.contains_key("arg_a"),
            "Arguments should contain arg_a property"
        );

        let arg_a = args_props.get("arg_a").and_then(|s| s.as_object()).unwrap();
        assert_eq!(
            arg_a.get("type").and_then(|t| t.as_str()),
            Some("boolean"),
            "arg_a should have type boolean"
        );

        // ReusableWorkflow
        let tool = result
            .iter()
            .find(|t| t.function.name == "test_workflow")
            .unwrap();
        println!("workflow Tool: {tool:#?}");
        assert_eq!(tool.function.name, "test_workflow");
        assert_eq!(tool.function.description, "desc_workflow");

        let schema_value = tool.function.parameters.clone().to_value();
        let schema_obj = schema_value.as_object().unwrap();

        assert_eq!(
            schema_obj.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "Schema should have type 'object'"
        );

        // ReusableWorkflow now uses settings/arguments structure
        let props = schema_obj
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();

        assert!(
            props.contains_key("settings"),
            "Schema should contain settings property"
        );
        let settings = props.get("settings").and_then(|s| s.as_object()).unwrap();
        let settings_props = settings
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            settings_props.contains_key("setting_b"),
            "Settings should contain setting_b property"
        );
        let setting_b = settings_props
            .get("setting_b")
            .and_then(|s| s.as_object())
            .unwrap();
        assert_eq!(
            setting_b.get("type").and_then(|t| t.as_str()),
            Some("integer"),
            "setting_b should have type integer"
        );

        assert!(
            props.contains_key("arguments"),
            "Schema should contain arguments property"
        );
        let arguments = props.get("arguments").and_then(|a| a.as_object()).unwrap();
        let args_props = arguments
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            args_props.contains_key("arg_b"),
            "Arguments should contain arg_b property"
        );
        let arg_b = args_props.get("arg_b").and_then(|a| a.as_object()).unwrap();
        assert_eq!(
            arg_b.get("type").and_then(|t| t.as_str()),
            Some("number"),
            "arg_b should have type number"
        );

        // McpTools
        let tool = result
            .iter()
            .find(|t| t.function.name == "test_mcp___inner")
            .unwrap();
        println!("mcp Tool: {tool:#?}");
        assert_eq!(tool.function.name, "test_mcp___inner");
        assert_eq!(tool.function.description, "desc_inner");

        let schema_value = tool.function.parameters.clone().to_value();
        let schema_obj = schema_value.as_object().unwrap();

        assert_eq!(
            schema_obj.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "Schema should have type 'object'"
        );

        let props = schema_obj
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            props.contains_key("arguments"),
            "Schema should contain arguments property"
        );

        let arguments = props.get("arguments").and_then(|a| a.as_object()).unwrap();
        let args_props = arguments
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            args_props.contains_key("c"),
            "Arguments should contain c property"
        );
        let prop_c = args_props.get("c").and_then(|s| s.as_object()).unwrap();
        assert_eq!(
            prop_c.get("type").and_then(|t| t.as_str()),
            Some("boolean"),
            "c should have type boolean"
        );

        // Multi-method Plugin - should generate 2 tools
        let plugin_tools: Vec<_> = result
            .iter()
            .filter(|t| t.function.name.starts_with("test_plugin"))
            .collect();
        assert_eq!(
            plugin_tools.len(),
            2,
            "Multi-method plugin should generate 2 tools"
        );

        let tool_a = result
            .iter()
            .find(|t| t.function.name == "test_plugin___method_a")
            .expect("Should have test_plugin___method_a tool");
        assert_eq!(tool_a.function.name, "test_plugin___method_a");
        assert_eq!(tool_a.function.description, "First method");

        let schema_value_a = tool_a.function.parameters.clone().to_value();
        let schema_obj_a = schema_value_a.as_object().unwrap();
        let props_a = schema_obj_a
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();

        assert!(
            props_a.contains_key("settings"),
            "Schema should contain settings property"
        );
        assert!(
            props_a.contains_key("arguments"),
            "Schema should contain arguments property"
        );

        let settings_a = props_a.get("settings").and_then(|s| s.as_object()).unwrap();
        let settings_props_a = settings_a
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            settings_props_a.contains_key("api_key"),
            "Settings should contain api_key"
        );

        let arguments_a = props_a
            .get("arguments")
            .and_then(|a| a.as_object())
            .unwrap();
        let args_props_a = arguments_a
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            args_props_a.contains_key("param_a"),
            "Arguments should contain param_a"
        );
        let param_a = args_props_a
            .get("param_a")
            .and_then(|p| p.as_object())
            .unwrap();
        assert_eq!(
            param_a.get("type").and_then(|t| t.as_str()),
            Some("string"),
            "param_a should have type string"
        );

        let tool_b = result
            .iter()
            .find(|t| t.function.name == "test_plugin___method_b")
            .expect("Should have test_plugin___method_b tool");
        assert_eq!(tool_b.function.name, "test_plugin___method_b");
        assert_eq!(tool_b.function.description, "Second method");

        let schema_value_b = tool_b.function.parameters.clone().to_value();
        let schema_obj_b = schema_value_b.as_object().unwrap();
        let props_b = schema_obj_b
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();

        let arguments_b = props_b
            .get("arguments")
            .and_then(|a| a.as_object())
            .unwrap();
        let args_props_b = arguments_b
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            args_props_b.contains_key("param_b"),
            "Arguments should contain param_b"
        );
        let param_b = args_props_b
            .get("param_b")
            .and_then(|p| p.as_object())
            .unwrap();
        assert_eq!(
            param_b.get("type").and_then(|t| t.as_str()),
            Some("integer"),
            "param_b should have type integer"
        );
    }

    #[test]
    fn test_command_runner_schema_generation() {
        let mut method_schemas = std::collections::HashMap::new();
        method_schemas.insert(
            proto::DEFAULT_METHOD_NAME.to_string(),
            proto::jobworkerp::function::data::MethodSchema {
                description: Some("Run shell commands".to_string()),
                arguments_schema: r#"{"type": "object", "properties": {"command": {"type": "string", "description": "The command to execute"}, "args": {"type": "array", "items": {"type": "string"}, "description": "Command arguments"}, "with_memory_monitoring": {"type": "boolean", "description": "Enable memory monitoring"}}, "required": ["command", "args"]}"#.to_string(),
                result_schema: Some(r#"{"type": "object", "properties": {"exit_code": {"type": "integer"}, "stdout": {"type": "string"}, "stderr": {"type": "string"}}}"#.to_string()),
                output_type: proto::jobworkerp::data::StreamingOutputType::NonStreaming as i32,
                annotations: None,
            },
        );

        let command_spec = FunctionSpecs {
            name: "COMMAND".to_string(),
            description: "Run shell commands".to_string(),
            settings_schema: r#"{"type": "object", "properties": {}}"#.to_string(),
            methods: Some(proto::jobworkerp::function::data::MethodSchemaMap {
                schemas: method_schemas,
            }),
            runner_type: proto::jobworkerp::data::RunnerType::Command as i32,
            runner_id: None,
            worker_id: None,
        };

        let ollama_tools = ToolConverter::convert_functions_to_ollama_tools(vec![command_spec]);

        assert_eq!(ollama_tools.len(), 1, "Should generate exactly one tool");

        let tool = &ollama_tools[0];
        assert_eq!(tool.function.name, "COMMAND");
        assert_eq!(tool.function.description, "Run shell commands");

        let schema_value = tool.function.parameters.clone().to_value();
        let schema_obj = schema_value.as_object().unwrap();

        let props = schema_obj
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();

        assert!(
            props.contains_key("settings"),
            "Schema should contain settings"
        );
        assert!(
            props.contains_key("arguments"),
            "Schema should contain arguments"
        );

        let arguments = props.get("arguments").and_then(|a| a.as_object()).unwrap();
        let arg_props = arguments
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();

        assert!(
            arg_props.contains_key("command"),
            "Arguments should contain 'command' field"
        );
        assert!(
            arg_props.contains_key("args"),
            "Arguments should contain 'args' field"
        );
        assert!(
            arg_props.contains_key("with_memory_monitoring"),
            "Arguments should contain 'with_memory_monitoring' field"
        );

        let command_field = arg_props
            .get("command")
            .and_then(|c| c.as_object())
            .unwrap();
        assert_eq!(
            command_field.get("type").and_then(|t| t.as_str()),
            Some("string"),
            "Command field should be a string"
        );

        let args_field = arg_props.get("args").and_then(|a| a.as_object()).unwrap();
        assert_eq!(
            args_field.get("type").and_then(|t| t.as_str()),
            Some("array"),
            "Args field should be an array"
        );
    }

    #[test]
    fn test_convert_functions_to_genai_tools() {
        let specs = vec![
            make_single_schema_spec(),
            make_reusable_workflow_spec(),
            make_mcp_tools_spec(),
            make_multi_method_plugin_spec(),
        ];
        let result = ToolConverter::convert_functions_to_genai_tools(specs);

        // SingleSchema
        let tool = result.iter().find(|t| t.name == "test_single").unwrap();
        assert_eq!(tool.name, "test_single");
        assert_eq!(tool.description.as_ref().unwrap(), "desc_single");

        let schema = tool.schema.as_ref().unwrap();
        let schema_obj = schema.as_object().unwrap();

        assert_eq!(
            schema_obj.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "Schema should have type 'object'"
        );

        let props = schema_obj
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            props.contains_key("settings"),
            "Schema should contain settings property"
        );
        assert!(
            props.contains_key("arguments"),
            "Schema should contain arguments property"
        );

        let settings = props.get("settings").and_then(|s| s.as_object()).unwrap();
        let settings_props = settings
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            settings_props.contains_key("setting_a"),
            "Settings should contain setting_a property"
        );

        let setting_a = settings_props
            .get("setting_a")
            .and_then(|s| s.as_object())
            .unwrap();
        assert_eq!(
            setting_a.get("type").and_then(|t| t.as_str()),
            Some("string"),
            "setting_a should have type string"
        );

        let arguments = props.get("arguments").and_then(|s| s.as_object()).unwrap();
        let args_props = arguments
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            args_props.contains_key("arg_a"),
            "Arguments should contain arg_a property"
        );

        let arg_a = args_props.get("arg_a").and_then(|s| s.as_object()).unwrap();
        assert_eq!(
            arg_a.get("type").and_then(|t| t.as_str()),
            Some("boolean"),
            "arg_a should have type boolean"
        );

        // ReusableWorkflow
        let tool = result.iter().find(|t| t.name == "test_workflow").unwrap();
        assert_eq!(tool.name, "test_workflow");
        assert_eq!(tool.description.as_ref().unwrap(), "desc_workflow");

        let schema = tool.schema.as_ref().unwrap();
        let schema_obj = schema.as_object().unwrap();

        assert_eq!(
            schema_obj.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "Schema should have type 'object'"
        );

        // ReusableWorkflow now uses settings/arguments structure
        let props = schema_obj
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();

        assert!(
            props.contains_key("settings"),
            "Schema should contain settings property"
        );
        let settings = props.get("settings").and_then(|s| s.as_object()).unwrap();
        let settings_props = settings
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            settings_props.contains_key("setting_b"),
            "Settings should contain setting_b property"
        );
        let setting_b = settings_props
            .get("setting_b")
            .and_then(|s| s.as_object())
            .unwrap();
        assert_eq!(
            setting_b.get("type").and_then(|t| t.as_str()),
            Some("integer"),
            "setting_b should have type integer"
        );

        assert!(
            props.contains_key("arguments"),
            "Schema should contain arguments property"
        );
        let arguments = props.get("arguments").and_then(|a| a.as_object()).unwrap();
        let args_props = arguments
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            args_props.contains_key("arg_b"),
            "Arguments should contain arg_b property"
        );
        let arg_b = args_props.get("arg_b").and_then(|a| a.as_object()).unwrap();
        assert_eq!(
            arg_b.get("type").and_then(|t| t.as_str()),
            Some("number"),
            "arg_b should have type number"
        );

        // McpTools
        let tool = result
            .iter()
            .find(|t| t.name == "test_mcp___inner")
            .unwrap();
        assert_eq!(tool.name, "test_mcp___inner");
        assert_eq!(tool.description.as_ref().unwrap(), "desc_inner");

        let schema = tool.schema.as_ref().unwrap();
        let schema_obj = schema.as_object().unwrap();

        assert_eq!(
            schema_obj.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "Schema should have type 'object'"
        );

        let props = schema_obj
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            props.contains_key("arguments"),
            "Schema should contain arguments property"
        );

        let arguments = props.get("arguments").and_then(|a| a.as_object()).unwrap();
        let args_props = arguments
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            args_props.contains_key("c"),
            "Arguments should contain c property"
        );
        let prop_c = args_props.get("c").and_then(|s| s.as_object()).unwrap();
        assert_eq!(
            prop_c.get("type").and_then(|t| t.as_str()),
            Some("boolean"),
            "c should have type boolean"
        );

        // Multi-method Plugin - should generate 2 tools
        let plugin_tools: Vec<_> = result
            .iter()
            .filter(|t| t.name.starts_with("test_plugin"))
            .collect();
        assert_eq!(
            plugin_tools.len(),
            2,
            "Multi-method plugin should generate 2 tools"
        );

        let tool_a = result
            .iter()
            .find(|t| t.name == "test_plugin___method_a")
            .expect("Should have test_plugin___method_a tool");
        assert_eq!(tool_a.name, "test_plugin___method_a");
        assert_eq!(tool_a.description.as_ref().unwrap(), "First method");

        let schema_a = tool_a.schema.as_ref().unwrap();
        let schema_obj_a = schema_a.as_object().unwrap();
        let props_a = schema_obj_a
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();

        assert!(
            props_a.contains_key("settings"),
            "Schema should contain settings property"
        );
        assert!(
            props_a.contains_key("arguments"),
            "Schema should contain arguments property"
        );

        let settings_a = props_a.get("settings").and_then(|s| s.as_object()).unwrap();
        let settings_props_a = settings_a
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            settings_props_a.contains_key("api_key"),
            "Settings should contain api_key"
        );

        let arguments_a = props_a
            .get("arguments")
            .and_then(|a| a.as_object())
            .unwrap();
        let args_props_a = arguments_a
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            args_props_a.contains_key("param_a"),
            "Arguments should contain param_a"
        );
        let param_a = args_props_a
            .get("param_a")
            .and_then(|p| p.as_object())
            .unwrap();
        assert_eq!(
            param_a.get("type").and_then(|t| t.as_str()),
            Some("string"),
            "param_a should have type string"
        );

        let tool_b = result
            .iter()
            .find(|t| t.name == "test_plugin___method_b")
            .expect("Should have test_plugin___method_b tool");
        assert_eq!(tool_b.name, "test_plugin___method_b");
        assert_eq!(tool_b.description.as_ref().unwrap(), "Second method");

        let schema_b = tool_b.schema.as_ref().unwrap();
        let schema_obj_b = schema_b.as_object().unwrap();
        let props_b = schema_obj_b
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();

        let arguments_b = props_b
            .get("arguments")
            .and_then(|a| a.as_object())
            .unwrap();
        let args_props_b = arguments_b
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            args_props_b.contains_key("param_b"),
            "Arguments should contain param_b"
        );
        let param_b = args_props_b
            .get("param_b")
            .and_then(|p| p.as_object())
            .unwrap();
        assert_eq!(
            param_b.get("type").and_then(|t| t.as_str()),
            Some("integer"),
            "param_b should have type integer"
        );
    }
}
