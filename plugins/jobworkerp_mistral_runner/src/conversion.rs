use jobworkerp_base::error::JobWorkerError;
use mistralrs::{Function, Tool, ToolType};
use proto::jobworkerp::data::RunnerType;
use proto::jobworkerp::function::data::{function_specs, FunctionSpecs};
use serde_json::Value;
use std::collections::HashMap;
use tracing;

pub const CREATION_TOOL_DESCRIPTION: &str =
    "Create Tools from workflow definitions provided as JSON. The workflow definition must:

- Conform to the specified JSON schema
- Include an input schema section that defines the parameters created workflow Tool will accept
- When this workflow is executed as a Tool, it will receive parameters matching this input schema
- Specify execution steps that utilize any available runner(function) in the system (except this creation Tool)";

pub struct ToolConverter;

impl ToolConverter {
    pub fn combine_names(server_name: &str, tool_name: &str) -> String {
        format!("{}__{}", server_name, tool_name)
    }

    pub fn convert_functions_to_tools(functions: Vec<FunctionSpecs>) -> anyhow::Result<Vec<Tool>> {
        let mut tools = Vec::new();

        for tool in functions {
            if tool.worker_id.is_none() && tool.runner_type == RunnerType::ReusableWorkflow as i32 {
                if let Some(converted) = Self::convert_reusable_workflow(&tool) {
                    tools.push(converted);
                }
            } else if tool.runner_type == RunnerType::McpServer as i32 {
                tools.extend(Self::convert_mcp_server(&tool));
            } else {
                if let Some(converted) = Self::convert_normal_function(&tool) {
                    tools.push(converted);
                }
            }
        }

        Ok(tools)
    }

    fn convert_reusable_workflow(tool: &FunctionSpecs) -> Option<Tool> {
        let schema = tool
            .schema
            .as_ref()
            .and_then(|s| match s {
                function_specs::Schema::SingleSchema(function) => {
                    // use settings as input schema for creation workflow as tool
                    function.settings.as_ref().and_then(|f| {
                        serde_json::from_str::<Value>(f.as_str())
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
            .unwrap_or(serde_json::json!({}));

        Some(Tool {
            tp: ToolType::Function,
            function: Function {
                name: tool.name.clone(),
                description: Some(CREATION_TOOL_DESCRIPTION.to_string()),
                parameters: Some(
                    schema
                        .as_object()
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .collect(),
                ),
            },
        })
    }

    fn convert_mcp_server(tool: &FunctionSpecs) -> Vec<Tool> {
        let server_name = tool.name.as_str();
        match &tool.schema {
            Some(function_specs::Schema::McpTools(
                proto::jobworkerp::function::data::McpToolList { list },
            )) => list
                .iter()
                .map(|mcp_tool| {
                    let schema = serde_json::from_str::<Value>(&mcp_tool.input_schema)
                        .unwrap_or(serde_json::json!({}));
                    Tool {
                        tp: ToolType::Function,
                        function: Function {
                            name: Self::combine_names(server_name, mcp_tool.name.as_str()),
                            description: mcp_tool.description.clone(),
                            parameters: Some(
                                schema
                                    .as_object()
                                    .cloned()
                                    .unwrap_or_default()
                                    .into_iter()
                                    .collect(),
                            ),
                        },
                    }
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

    fn convert_normal_function(tool: &FunctionSpecs) -> Option<Tool> {
        // Note: SchemaCombiner logic is simplified here as we don't have command-utils dependency
        // We just merge settings and arguments schemas manually if needed, or just use arguments
        // For now, let's try to use a simplified approach merging properties

        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();

        if let Some(function_specs::Schema::SingleSchema(function)) = &tool.schema {
            // Handle settings
            if let Some(settings_str) = &function.settings {
                if let Ok(Value::Object(settings_schema)) = serde_json::from_str(settings_str) {
                    // Add settings object as a property "settings"
                    properties.insert("settings".to_string(), Value::Object(settings_schema));
                    required.push("settings".to_string());
                }
            }

            // Handle arguments
            if let Ok(Value::Object(args_schema)) = serde_json::from_str(&function.arguments) {
                // Add arguments object as a property "arguments"
                properties.insert("arguments".to_string(), Value::Object(args_schema));
                required.push("arguments".to_string());
            }
        }

        let schema = serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": required
        });

        Some(Tool {
            tp: ToolType::Function,
            function: Function {
                name: tool.name.clone(),
                description: Some(tool.description.clone()),
                parameters: Some(
                    schema
                        .as_object()
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .collect(),
                ),
            },
        })
    }
}
