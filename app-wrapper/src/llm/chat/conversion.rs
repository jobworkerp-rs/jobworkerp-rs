use app::app::function::helper::McpNameConverter;
use command_utils::util::json_schema::SchemaCombiner;
use proto::jobworkerp::data::RunnerType;
use proto::jobworkerp::function::data::{function_specs, FunctionSpecs};
use rmcp::model::{ListToolsResult, Tool};
use rmcp::ErrorData as McpError;
use serde_json;
use tracing;
pub const CREATION_TOOL_DESCRIPTION: &str =
    "Create Tools from workflow definitions provided as JSON. The workflow definition must:

- Conform to the specified JSON schema
- Include an input schema section that defines the parameters created workflow Tool will accept
- When this workflow is executed as a Tool, it will receive parameters matching this input schema
- Specify execution steps that utilize any available runner(function) in the system (except this creation Tool)";

pub struct ToolConverter;
impl McpNameConverter for ToolConverter {}

impl ToolConverter {
    pub fn convert_reusable_workflow(tool: &FunctionSpecs) -> Option<Tool> {
        Some(Tool::new(
            tool.name.clone(),
            CREATION_TOOL_DESCRIPTION,
            tool.schema
                .as_ref()
                .and_then(|s| match s {
                    function_specs::Schema::SingleSchema(function) => {
                        // use settings as input schema for creation workflow as tool
                        function.settings.as_ref().and_then(|f| {
                            serde_json::from_str(f.as_str())
                                .or_else(|e1| {
                                    tracing::warn!("Failed to parse settings as json: {}", e1);
                                    serde_yaml::from_str(f.as_str()).inspect_err(|e2| {
                                        tracing::warn!("Failed to parse settings as yaml: {}", e2);
                                    })
                                })
                                .ok()
                        })
                    }
                    function_specs::Schema::McpTools(mcp) => {
                        let mes = format!("error: expect workflow but got mcp: {mcp:?}");
                        tracing::error!(mes);
                        None
                    }
                })
                .unwrap_or(serde_json::json!({}))
                .as_object()
                .cloned()
                .unwrap_or_default(),
        ))
    }

    pub fn convert_mcp_server(tool: &FunctionSpecs) -> Vec<Tool> {
        let server_name = tool.name.as_str();
        match &tool.schema {
            Some(function_specs::Schema::McpTools(
                proto::jobworkerp::function::data::McpToolList { list },
            )) => list
                .iter()
                .map(|tool| {
                    Tool::new(
                        Self::combine_names(server_name, tool.name.as_str()),
                        tool.description.clone().unwrap_or_default(),
                        serde_json::from_str(tool.input_schema.as_str())
                            .unwrap_or(serde_json::json!({}))
                            .as_object()
                            .cloned()
                            .unwrap_or_default(),
                    )
                })
                .collect(),
            Some(function_specs::Schema::SingleSchema(function)) => {
                tracing::error!("error: expect workflow but got function: {:?}", function);
                vec![]
            }
            None => {
                tracing::error!("error: expect workflow but got none: {:?}", &tool);
                vec![]
            }
        }
    }

    pub fn convert_normal_function(tool: &FunctionSpecs) -> Option<Tool> {
        let mut schema_combiner = SchemaCombiner::new();
        tool.schema
            .as_ref()
            .and_then(|s| match s {
                function_specs::Schema::SingleSchema(function) => function.settings.clone(),
                function_specs::Schema::McpTools(_) => {
                    tracing::error!("got mcp tool in not mcp tool runner type: {:#?}", &tool);
                    None
                }
            })
            .and_then(|s| {
                schema_combiner
                    .add_schema_from_string(
                        "settings",
                        s.as_str(),
                        Some("Tool init settings".to_string()),
                    )
                    .inspect_err(|e| tracing::error!("Failed to parse schema: {}", e))
                    .ok()
            });
        tool.schema
            .as_ref()
            .map(|s| match s {
                function_specs::Schema::SingleSchema(function) => function.arguments.clone(),
                function_specs::Schema::McpTools(_) => {
                    tracing::error!("got mcp tool in not mcp tool runner type: {:#?}", &tool);
                    "".to_string()
                }
            })
            .and_then(|args| {
                schema_combiner
                    .add_schema_from_string(
                        "arguments",
                        args.as_str(),
                        Some("Tool arguments".to_string()),
                    )
                    .inspect_err(|e| tracing::error!("Failed to parse schema: {}", e))
                    .ok()
            });
        match schema_combiner.generate_combined_schema() {
            Ok(schema) => Some(Tool::new(
                tool.name.clone(),
                tool.description.clone(),
                schema,
            )),
            Err(e) => {
                tracing::error!("Failed to generate schema: {}", e);
                None
            }
        }
    }

    pub fn convert_functions_to_mcp_tools(
        functions: Vec<FunctionSpecs>,
    ) -> Result<ListToolsResult, McpError> {
        let tool_list = functions
            .into_iter()
            .flat_map(|tool| {
                if tool.worker_id.is_none()
                    && tool.runner_type == RunnerType::ReusableWorkflow as i32
                {
                    Self::convert_reusable_workflow(&tool)
                        .into_iter()
                        .collect::<Vec<_>>()
                } else if tool.runner_type == RunnerType::McpServer as i32 {
                    Self::convert_mcp_server(&tool)
                } else {
                    Self::convert_normal_function(&tool)
                        .into_iter()
                        .collect::<Vec<_>>()
                }
            })
            .collect::<Vec<_>>();

        Ok(ListToolsResult {
            tools: tool_list,
            next_cursor: None,
        })
    }

    /// Convert a list of FunctionSpecs to Vec<ToolInfo> for Ollama FunctionCalling
    pub fn convert_functions_to_ollama_tools(
        functions: Vec<FunctionSpecs>,
    ) -> Vec<ollama_rs::generation::tools::ToolInfo> {
        use ollama_rs::generation::tools::ToolInfo;
        functions
            .into_iter()
            .flat_map(|tool| {
                match &tool.schema {
                    Some(function_specs::Schema::SingleSchema(function))
                        if tool.runner_type == RunnerType::ReusableWorkflow as i32 =>
                    {
                        // ReusableWorkflow: use settings as JSON Schema
                        let name = tool.name.clone();
                        let description = CREATION_TOOL_DESCRIPTION.to_string();
                        let params_schema: Option<schemars::Schema> = function
                            .settings
                            .as_ref()
                            .and_then(|s| serde_json::from_str(s).ok());
                        params_schema
                            .map(|parameters| ToolInfo {
                                tool_type: ollama_rs::generation::tools::ToolType::Function,
                                function: ollama_rs::generation::tools::ToolFunctionInfo {
                                    name,
                                    description,
                                    parameters,
                                },
                            })
                            .into_iter()
                            .collect::<Vec<_>>()
                    }
                    Some(function_specs::Schema::SingleSchema(function)) => {
                        let name = tool.name.clone();
                        let description = tool.description.clone();

                        // Use SchemaCombiner to combine settings and arguments schemas
                        let mut schema_combiner = SchemaCombiner::new();

                        // Add settings schema if present
                        if let Some(settings) = &function.settings {
                            let _ = schema_combiner
                                .add_schema_from_string(
                                    "settings",
                                    settings.as_str(),
                                    Some("Tool init settings".to_string()),
                                )
                                .inspect_err(|e| {
                                    tracing::error!("Failed to parse settings schema: {}", e)
                                });
                        }

                        // Add arguments schema
                        let _ = schema_combiner
                            .add_schema_from_string(
                                "arguments",
                                function.arguments.as_str(),
                                Some("Tool arguments".to_string()),
                            )
                            .inspect_err(|e| {
                                tracing::error!("Failed to parse arguments schema: {}", e)
                            });

                        // Generate combined schema and convert to schemars::Schema
                        let params_schema: Option<schemars::Schema> = schema_combiner
                            .generate_combined_schema()
                            .ok()
                            .and_then(|combined_map| {
                                let combined_value = serde_json::Value::Object(combined_map);
                                serde_json::from_value(combined_value).ok()
                            });

                        params_schema
                            .map(|parameters| ToolInfo {
                                tool_type: ollama_rs::generation::tools::ToolType::Function,
                                function: ollama_rs::generation::tools::ToolFunctionInfo {
                                    name,
                                    description,
                                    parameters,
                                },
                            })
                            .into_iter()
                            .collect::<Vec<_>>()
                    }
                    Some(function_specs::Schema::McpTools(mcp_tools)) => mcp_tools
                        .list
                        .iter()
                        .filter_map(|mcp_tool| {
                            let name =
                                Self::combine_names(tool.name.as_str(), mcp_tool.name.as_str());
                            let description = mcp_tool.description.clone().unwrap_or_default();
                            let params_schema: Option<schemars::Schema> =
                                serde_json::from_str(&mcp_tool.input_schema).ok();
                            params_schema.map(|parameters| ToolInfo {
                                tool_type: ollama_rs::generation::tools::ToolType::Function,
                                function: ollama_rs::generation::tools::ToolFunctionInfo {
                                    name,
                                    description,
                                    parameters,
                                },
                            })
                        })
                        .collect::<Vec<_>>(),
                    _ => Vec::new(),
                }
            })
            .collect()
    }

    /// Convert a list of FunctionSpecs to Vec<genai::chat::Tool> for genai FunctionCalling
    pub fn convert_functions_to_genai_tools(
        functions: Vec<FunctionSpecs>,
    ) -> Vec<genai::chat::Tool> {
        use genai::chat::Tool;
        use serde_json::Value;
        functions
            .into_iter()
            .flat_map(|tool| match &tool.schema {
                Some(function_specs::Schema::SingleSchema(function))
                    if tool.runner_type == RunnerType::ReusableWorkflow as i32 =>
                {
                    let name = tool.name.clone();
                    let description = Some(CREATION_TOOL_DESCRIPTION.to_string());
                    let schema: Option<Value> = function
                        .settings
                        .as_ref()
                        .and_then(|s| serde_json::from_str::<Value>(s).ok());
                    schema
                        .map(|schema| Tool {
                            name,
                            description,
                            schema: Some(schema),
                            config: None,
                        })
                        .into_iter()
                        .collect::<Vec<_>>()
                }
                Some(function_specs::Schema::SingleSchema(function)) => {
                    let name = tool.name.clone();
                    let description = Some(tool.description.clone());

                    // Use SchemaCombiner to combine settings and arguments schemas
                    let mut schema_combiner = SchemaCombiner::new();

                    // Add settings schema if present
                    if let Some(settings) = &function.settings {
                        let _ = schema_combiner
                            .add_schema_from_string(
                                "settings",
                                settings.as_str(),
                                Some("Tool init settings".to_string()),
                            )
                            .inspect_err(|e| {
                                tracing::error!("Failed to parse settings schema: {}", e)
                            });
                    }

                    // Add arguments schema
                    let _ = schema_combiner
                        .add_schema_from_string(
                            "arguments",
                            function.arguments.as_str(),
                            Some("Tool arguments".to_string()),
                        )
                        .inspect_err(|e| {
                            tracing::error!("Failed to parse arguments schema: {}", e)
                        });

                    // Generate combined schema
                    let schema: Option<Value> = schema_combiner
                        .generate_combined_schema()
                        .ok()
                        .map(serde_json::Value::Object);

                    schema
                        .map(|schema| Tool {
                            name,
                            description,
                            schema: Some(schema),
                            config: None,
                        })
                        .into_iter()
                        .collect::<Vec<_>>()
                }
                Some(function_specs::Schema::McpTools(mcp_tools)) => mcp_tools
                    .list
                    .iter()
                    .filter_map(|mcp_tool| {
                        let name = Self::combine_names(tool.name.as_str(), mcp_tool.name.as_str());
                        let description = Some(mcp_tool.description.clone().unwrap_or_default());
                        let schema: Option<Value> =
                            serde_json::from_str::<Value>(&mcp_tool.input_schema).ok();
                        schema.map(|schema| Tool {
                            name,
                            description,
                            schema: Some(schema),
                            config: None,
                        })
                    })
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::jobworkerp::function::data::{function_specs, FunctionSpecs, McpTool, McpToolList};
    use serde_json::{json, Value};

    fn make_single_schema_spec() -> FunctionSpecs {
        FunctionSpecs {
            name: "test_single".to_string(),
            description: "desc_single".to_string(),
            schema: Some(function_specs::Schema::SingleSchema(
                proto::jobworkerp::function::data::FunctionSchema {
                    settings: Some(
                        json!({"type": "object", "properties": {"setting_a": {"type": "string"}}})
                            .to_string(),
                    ),
                    arguments:
                        json!({"type": "object", "properties": {"arg_a": {"type": "boolean"}}})
                            .to_string(),
                    result_output_schema: Some(
                        json!({"type": "object", "properties": {"result": {"type": "string"}}})
                            .to_string(),
                    ),
                },
            )),
            runner_type: proto::jobworkerp::data::RunnerType::Command as i32,
            worker_id: None,
            ..Default::default()
        }
    }

    // reusable workflow (creation tool)
    fn make_reusable_workflow_spec() -> FunctionSpecs {
        FunctionSpecs {
            name: "test_workflow".to_string(),
            description: "desc_workflow".to_string(),
            schema: Some(function_specs::Schema::SingleSchema(
                proto::jobworkerp::function::data::FunctionSchema {
                    settings: Some(
                        json!({"type": "object", "properties": {"setting_b": {"type": "integer"}}})
                            .to_string(),
                    ),
                    arguments:
                        json!({"type": "object", "properties": {"arg_b": {"type": "number"}}})
                            .to_string(),
                    result_output_schema: Some(
                        json!({"type": "object", "properties": {"result": {"type": "string"}}})
                            .to_string(),
                    ),
                },
            )),
            runner_type: proto::jobworkerp::data::RunnerType::ReusableWorkflow as i32,
            worker_id: None,
            ..Default::default()
        }
    }

    // mcp tools (multiple tools in a Mcp server, combined name)
    fn make_mcp_tools_spec() -> FunctionSpecs {
        FunctionSpecs {
            name: "test_mcp".to_string(),
            description: "desc_mcp".to_string(),
            schema: Some(function_specs::Schema::McpTools(McpToolList {
                list: vec![McpTool {
                    name: "inner".to_string(),
                    description: Some("desc_inner".to_string()),
                    input_schema:
                        json!({"type": "object", "properties": {"c": {"type": "boolean"}}})
                            .to_string(),
                    ..Default::default()
                }],
            })),
            runner_type: proto::jobworkerp::data::RunnerType::McpServer as i32,
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

    /// Helper function to verify schema structure matches expected structure
    fn assert_json_schema_match(actual: &serde_json::Map<String, Value>, expected: &Value) {
        let actual_value = Value::Object(actual.clone());
        assert_json_subset(expected, &actual_value);
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

        // Verify the schema has the correct type
        assert_eq!(
            tool.input_schema.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "Schema should have type 'object'"
        );

        // Verify required fields
        assert_schema_required_fields(&tool.input_schema, &["arguments", "settings"]);

        // Verify arguments property
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

        // Verify settings property
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
        assert_eq!(
            tool.description.as_ref().unwrap(),
            CREATION_TOOL_DESCRIPTION
        );

        // Reusable workflow tools should include settings_b and arg_b properties directly
        let expected_reusable_schema = json!({
            "type": "object",
            "properties": {
                "setting_b": {
                    "type": "integer"
                },
            }
        });

        assert_json_schema_match(&tool.input_schema, &expected_reusable_schema);

        // McpTools
        let tool = result
            .tools
            .iter()
            .find(|t| t.name == "test_mcp___inner")
            .unwrap();
        assert_eq!(tool.name, "test_mcp___inner");
        assert_eq!(tool.description.as_ref().unwrap(), "desc_inner");

        // McpTools should include property c directly
        let expected_mcp_schema = json!({
            "type": "object",
            "properties": {
                "c": {
                    "type": "boolean"
                }
            }
        });

        assert_json_schema_match(&tool.input_schema, &expected_mcp_schema);
    }
    #[test]
    fn test_convert_functions_to_ollama_tools() {
        let specs = vec![
            make_single_schema_spec(),
            make_reusable_workflow_spec(),
            make_mcp_tools_spec(),
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

        // Convert Schema to Value for easier inspection and comparison
        let schema_value = tool.function.parameters.clone().to_value();

        // Verify schema is an object with the right type
        let schema_obj = schema_value.as_object().unwrap();
        assert_eq!(
            schema_obj.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "Schema should have type 'object'"
        );

        // Verify schema has properties object with settings and arguments
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

        // Verify settings has setting_a property
        let settings = props.get("settings").and_then(|s| s.as_object()).unwrap();
        let settings_props = settings
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            settings_props.contains_key("setting_a"),
            "Settings should contain setting_a property"
        );

        // Verify setting_a has correct type
        let setting_a = settings_props
            .get("setting_a")
            .and_then(|s| s.as_object())
            .unwrap();
        assert_eq!(
            setting_a.get("type").and_then(|t| t.as_str()),
            Some("string"),
            "setting_a should have type string"
        );

        // Verify arguments has arg_a property
        let arguments = props.get("arguments").and_then(|s| s.as_object()).unwrap();
        let args_props = arguments
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            args_props.contains_key("arg_a"),
            "Arguments should contain arg_a property"
        );

        // Verify arg_a has correct type
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
        assert_eq!(tool.function.description, CREATION_TOOL_DESCRIPTION);

        // Convert Schema to Value for easier inspection and comparison
        let schema_value = tool.function.parameters.clone().to_value();
        let schema_obj = schema_value.as_object().unwrap();

        // Verify schema is an object
        assert_eq!(
            schema_obj.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "Schema should have type 'object'"
        );

        // Verify schema has properties with setting_b
        let props = schema_obj
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            props.contains_key("setting_b"),
            "Schema should contain setting_b property"
        );

        // Verify setting_b has correct type
        let setting_b = props.get("setting_b").and_then(|s| s.as_object()).unwrap();
        assert_eq!(
            setting_b.get("type").and_then(|t| t.as_str()),
            Some("integer"),
            "setting_b should have type integer"
        );

        // Verify schema has properties with arg_b (if the implementation handles arguments)
        if props.contains_key("arg_b") {
            let arg_b = props.get("arg_b").and_then(|s| s.as_object()).unwrap();
            assert_eq!(
                arg_b.get("type").and_then(|t| t.as_str()),
                Some("number"),
                "arg_b should have type number"
            );
        }

        // McpTools
        let tool = result
            .iter()
            .find(|t| t.function.name == "test_mcp___inner")
            .unwrap();
        println!("mcp Tool: {tool:#?}");
        assert_eq!(tool.function.name, "test_mcp___inner");
        assert_eq!(tool.function.description, "desc_inner");

        // Convert Schema to Value for easier inspection and comparison
        let schema_value = tool.function.parameters.clone().to_value();
        let schema_obj = schema_value.as_object().unwrap();

        // Verify schema is an object
        assert_eq!(
            schema_obj.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "Schema should have type 'object'"
        );

        // Verify schema has properties with c
        let props = schema_obj
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(props.contains_key("c"), "Schema should contain c property");

        // Verify c has correct type
        let prop_c = props.get("c").and_then(|s| s.as_object()).unwrap();
        assert_eq!(
            prop_c.get("type").and_then(|t| t.as_str()),
            Some("boolean"),
            "c should have type boolean"
        );
    }

    #[test]
    fn test_command_runner_schema_generation() {
        // Test to verify COMMAND runner generates proper schema with command and args parameters
        let command_spec = FunctionSpecs {
            name: "COMMAND".to_string(),
            description: "Run shell commands".to_string(),
            schema: Some(function_specs::Schema::SingleSchema(
                proto::jobworkerp::function::data::FunctionSchema {
                    // Empty settings schema (as COMMAND runner returns)
                    settings: Some(r#"{"type": "object", "properties": {}}"#.to_string()),
                    // CommandArgs schema with "command" and "args" fields
                    arguments: r#"{"type": "object", "properties": {"command": {"type": "string", "description": "The command to execute"}, "args": {"type": "array", "items": {"type": "string"}, "description": "Command arguments"}, "with_memory_monitoring": {"type": "boolean", "description": "Enable memory monitoring"}}, "required": ["command", "args"]}"#.to_string(),
                    result_output_schema: Some(r#"{"type": "object", "properties": {"exit_code": {"type": "integer"}, "stdout": {"type": "string"}, "stderr": {"type": "string"}}}"#.to_string()),
                },
            )),
            runner_type: proto::jobworkerp::data::RunnerType::Command as i32,
            runner_id: None,
            worker_id: None,
            output_type: 0,
        };

        let ollama_tools = ToolConverter::convert_functions_to_ollama_tools(vec![command_spec]);

        assert_eq!(ollama_tools.len(), 1, "Should generate exactly one tool");

        let tool = &ollama_tools[0];
        assert_eq!(tool.function.name, "COMMAND");
        assert_eq!(tool.function.description, "Run shell commands");

        // Convert to JSON for easier inspection
        let schema_value = tool.function.parameters.clone().to_value();
        let schema_obj = schema_value.as_object().unwrap();

        // Verify the schema has the expected structure
        let props = schema_obj
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();

        // Check for both settings and arguments
        assert!(
            props.contains_key("settings"),
            "Schema should contain settings"
        );
        assert!(
            props.contains_key("arguments"),
            "Schema should contain arguments"
        );

        // Check arguments for command and args fields
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

        // Verify command field is a string
        let command_field = arg_props
            .get("command")
            .and_then(|c| c.as_object())
            .unwrap();
        assert_eq!(
            command_field.get("type").and_then(|t| t.as_str()),
            Some("string"),
            "Command field should be a string"
        );

        // Verify args field is an array
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
        ];
        let result = ToolConverter::convert_functions_to_genai_tools(specs);

        // SingleSchema
        let tool = result.iter().find(|t| t.name == "test_single").unwrap();
        assert_eq!(tool.name, "test_single");
        assert_eq!(tool.description.as_ref().unwrap(), "desc_single");

        // Verify schema structure
        let schema = tool.schema.as_ref().unwrap();
        let schema_obj = schema.as_object().unwrap();

        // Check that it's an object type
        assert_eq!(
            schema_obj.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "Schema should have type 'object'"
        );

        // Check that it has properties
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

        // Verify settings has setting_a property
        let settings = props.get("settings").and_then(|s| s.as_object()).unwrap();
        let settings_props = settings
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            settings_props.contains_key("setting_a"),
            "Settings should contain setting_a property"
        );

        // Check setting_a type
        let setting_a = settings_props
            .get("setting_a")
            .and_then(|s| s.as_object())
            .unwrap();
        assert_eq!(
            setting_a.get("type").and_then(|t| t.as_str()),
            Some("string"),
            "setting_a should have type string"
        );

        // Verify arguments has arg_a property
        let arguments = props.get("arguments").and_then(|s| s.as_object()).unwrap();
        let args_props = arguments
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            args_props.contains_key("arg_a"),
            "Arguments should contain arg_a property"
        );

        // Check arg_a type
        let arg_a = args_props.get("arg_a").and_then(|s| s.as_object()).unwrap();
        assert_eq!(
            arg_a.get("type").and_then(|t| t.as_str()),
            Some("boolean"),
            "arg_a should have type boolean"
        );

        // ReusableWorkflow
        let tool = result.iter().find(|t| t.name == "test_workflow").unwrap();
        assert_eq!(tool.name, "test_workflow");
        assert_eq!(
            tool.description.as_ref().unwrap(),
            CREATION_TOOL_DESCRIPTION
        );

        let schema = tool.schema.as_ref().unwrap();
        let schema_obj = schema.as_object().unwrap();

        // Verify schema is an object
        assert_eq!(
            schema_obj.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "Schema should have type 'object'"
        );

        // Verify schema has properties with setting_b
        let props = schema_obj
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            props.contains_key("setting_b"),
            "Schema should contain setting_b property"
        );

        // Verify setting_b has correct type
        let setting_b = props.get("setting_b").and_then(|s| s.as_object()).unwrap();
        assert_eq!(
            setting_b.get("type").and_then(|t| t.as_str()),
            Some("integer"),
            "setting_b should have type integer"
        );

        // Verify schema has properties with arg_b (if implementation handles arguments)
        if props.contains_key("arg_b") {
            let arg_b = props.get("arg_b").and_then(|s| s.as_object()).unwrap();
            assert_eq!(
                arg_b.get("type").and_then(|t| t.as_str()),
                Some("number"),
                "arg_b should have type number"
            );
        }

        // McpTools
        let tool = result
            .iter()
            .find(|t| t.name == "test_mcp___inner")
            .unwrap();
        assert_eq!(tool.name, "test_mcp___inner");
        assert_eq!(tool.description.as_ref().unwrap(), "desc_inner");

        let schema = tool.schema.as_ref().unwrap();
        let schema_obj = schema.as_object().unwrap();

        // Verify schema is an object
        assert_eq!(
            schema_obj.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "Schema should have type 'object'"
        );

        // Verify schema has properties with c
        let props = schema_obj
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(props.contains_key("c"), "Schema should contain c property");

        // Verify c has correct type
        let prop_c = props.get("c").and_then(|s| s.as_object()).unwrap();
        assert_eq!(
            prop_c.get("type").and_then(|t| t.as_str()),
            Some("boolean"),
            "c should have type boolean"
        );
    }
}
