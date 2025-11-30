use anyhow::Result;
use infra::infra::runner::rows::RunnerWithSchema;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::jobworkerp::runner::ReusableWorkflowRunnerSettings;
use proto::jobworkerp::data::{RunnerData, RunnerType, StreamingOutputType, WorkerData, WorkerId};
use proto::jobworkerp::function::data::FunctionSpecs;

pub trait FunctionSpecConverter {
    // Helper function to get output_type for a specific method
    // Phase 6.7: Get from method_proto_map (method-specific)
    fn get_method_output_type(runner_data: Option<&RunnerData>, method_name: &str) -> i32 {
        runner_data
            .and_then(|d| d.method_proto_map.as_ref())
            .and_then(|map| map.schemas.get(method_name))
            .map(|schema| schema.output_type)
            .unwrap_or(StreamingOutputType::NonStreaming as i32)
    }

    // Helper function to convert Runner to FunctionSpecs (Phase 6.7)
    // Phase 6.7: Unified schema conversion for all runners (MCP/Plugin/Normal)
    fn convert_runner_to_function_specs(runner: RunnerWithSchema) -> FunctionSpecs {
        let runner_data = runner.data.as_ref();

        // Phase 6.7: Combine method_proto_map (for description) and method_json_schema_map (for schemas)
        // - description: from method_proto_map (not cached in method_json_schema_map)
        // - arguments_schema/result_schema: from method_json_schema_map (cached conversion)
        let method_schemas = runner_data
            .and_then(|d| d.method_proto_map.as_ref())
            .map(|proto_map| {
                proto_map
                    .schemas
                    .iter()
                    .map(|(name, proto_schema)| {
                        // Get cached JSON schema conversion
                        let json_schema = runner
                            .method_json_schema_map
                            .as_ref()
                            .and_then(|m| m.schemas.get(name));

                        (
                            name.clone(),
                            proto::jobworkerp::function::data::MethodSchema {
                                description: proto_schema.description.clone(),
                                arguments_schema: json_schema
                                    .map(|s| s.args_schema.clone())
                                    .unwrap_or_else(|| "{}".to_string()),
                                result_schema: json_schema.and_then(|s| s.result_schema.clone()),
                                output_type: Self::get_method_output_type(runner_data, name),
                                annotations: None, // MCP annotations can be added here if needed
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();

        FunctionSpecs {
            runner_type: runner_data
                .map(|d| d.runner_type)
                .unwrap_or(RunnerType::Plugin as i32),
            runner_id: runner.id,
            worker_id: None,
            name: runner_data.map_or(String::new(), |d| d.name.clone()),
            description: runner_data.map_or(String::new(), |d| d.description.clone()),
            settings_schema: runner.settings_schema, // REQUIRED (empty string if no settings)
            methods: Some(proto::jobworkerp::function::data::MethodSchemaMap {
                schemas: method_schemas,
            }),
        }
    }

    // Helper function to convert Worker to FunctionSpecs (Phase 6.7)
    fn convert_worker_to_function_specs(
        id: WorkerId,
        data: WorkerData,
        runner: RunnerWithSchema,
    ) -> Result<FunctionSpecs> {
        let runner_data = runner.data.as_ref();

        // Phase 6.7: Unified schema conversion (all workers use MethodSchemaMap)
        let method_schemas = if runner_data
            .is_some_and(|d| d.runner_type == RunnerType::ReusableWorkflow as i32)
        {
            // ReusableWorkflow: Override arguments schema with saved workflow's input schema
            let settings = ProstMessageCodec::deserialize_message::<ReusableWorkflowRunnerSettings>(
                data.runner_settings.as_slice(),
            )?;
            let (name, summary) = settings
                .schema()
                .and_then(|m| {
                    m.get("document").and_then(|d| {
                        d.as_object().map(|d| {
                            (
                                d.get("name")
                                    .and_then(|s| s.as_str().map(|s| s.to_string())),
                                d.get("summary")
                                    .and_then(|s| s.as_str().map(|s| s.to_string())),
                            )
                        })
                    })
                })
                .unwrap_or_default();
            let input_schema = settings.schema().and_then(|m| m.get("input").cloned());
            let input_schema =
                input_schema.map(|s| Self::parse_as_json_with_key_or_noop("schema", s));
            let input_schema =
                input_schema.map(|s| Self::parse_as_json_with_key_or_noop("document", s));

            // Get result schema and output_type from runner's default method
            let default_method = runner
                .method_json_schema_map
                .as_ref()
                .and_then(|map| map.schemas.get(proto::DEFAULT_METHOD_NAME));

            let result_schema = default_method.and_then(|schema| schema.result_schema.clone());
            let output_type = Self::get_method_output_type(runner_data, proto::DEFAULT_METHOD_NAME);

            // Create single method with overridden input schema
            let mut schemas = std::collections::HashMap::new();
            schemas.insert(
                name.unwrap_or(proto::DEFAULT_METHOD_NAME.to_string()),
                proto::jobworkerp::function::data::MethodSchema {
                    description: summary,
                    arguments_schema: input_schema
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "{}".to_string()),
                    result_schema,
                    output_type,
                    annotations: None,
                },
            );
            schemas
        } else {
            // All other workers: Combine method_proto_map and method_json_schema_map
            runner_data
                .and_then(|d| d.method_proto_map.as_ref())
                .map(|proto_map| {
                    proto_map
                        .schemas
                        .iter()
                        .map(|(name, proto_schema)| {
                            // Get cached JSON schema conversion
                            let json_schema = runner
                                .method_json_schema_map
                                .as_ref()
                                .and_then(|m| m.schemas.get(name));

                            (
                                name.clone(),
                                proto::jobworkerp::function::data::MethodSchema {
                                    description: proto_schema.description.clone(),
                                    arguments_schema: json_schema
                                        .map(|s| s.args_schema.clone())
                                        .unwrap_or_else(|| "{}".to_string()),
                                    result_schema: json_schema
                                        .and_then(|s| s.result_schema.clone()),
                                    output_type: Self::get_method_output_type(runner_data, name),
                                    annotations: None,
                                },
                            )
                        })
                        .collect()
                })
                .unwrap_or_default()
        };

        Ok(FunctionSpecs {
            runner_type: runner_data
                .map(|d| d.runner_type)
                .unwrap_or(RunnerType::Plugin as i32),
            runner_id: runner.id,
            worker_id: Some(id),
            name: data.name,
            description: data.description,
            settings_schema: String::new(), // Workers don't have settings (empty string)
            methods: Some(proto::jobworkerp::function::data::MethodSchemaMap {
                schemas: method_schemas,
            }),
        })
    }

    /// Convert Runner + specific using to FunctionSpecs (Phase 6.7)
    ///
    /// Returns a FunctionSpecs with a single tool (the specified using).
    /// Works for MCP servers, Plugins, and all other runners with method_json_schema_map.
    /// Returns error if the runner doesn't support usings or the using doesn't exist.
    fn convert_runner_using_to_function_specs(
        runner: RunnerWithSchema,
        using: &str,
    ) -> Result<FunctionSpecs> {
        let runner_data = runner
            .data
            .as_ref()
            .ok_or_else(|| JobWorkerError::NotFound("Runner data not found".to_string()))?;

        // Get proto schema for description
        let proto_schema = runner_data
            .method_proto_map
            .as_ref()
            .and_then(|map| map.schemas.get(using))
            .ok_or_else(|| {
                JobWorkerError::NotFound(format!(
                    "Method '{}' not found in runner '{}'",
                    using, runner_data.name
                ))
            })?;

        // Get cached JSON schema for args/result
        let json_schema = runner
            .method_json_schema_map
            .as_ref()
            .and_then(|map| map.schemas.get(using));

        // Phase 6.7: Use methods map with single method
        let mut method_schemas = std::collections::HashMap::new();
        method_schemas.insert(
            using.to_string(),
            proto::jobworkerp::function::data::MethodSchema {
                description: proto_schema.description.clone(),
                arguments_schema: json_schema
                    .map(|s| s.args_schema.clone())
                    .unwrap_or_else(|| "{}".to_string()),
                result_schema: json_schema.and_then(|s| s.result_schema.clone()),
                output_type: Self::get_method_output_type(Some(runner_data), using),
                annotations: None,
            },
        );

        Ok(FunctionSpecs {
            runner_type: runner_data.runner_type,
            runner_id: runner.id,
            worker_id: None,
            name: format!("{}___{}", runner_data.name, using),
            description: proto_schema
                .description
                .clone()
                .unwrap_or_else(|| format!("{} - {}", runner_data.description, using)),
            settings_schema: String::new(), // Using-specific functions don't need settings
            methods: Some(proto::jobworkerp::function::data::MethodSchemaMap {
                schemas: method_schemas,
            }),
        })
    }

    /// Convert Worker + specific using to FunctionSpecs (Phase 6.7)
    ///
    /// Returns a FunctionSpecs with a single tool (the specified using).
    /// Works for Workers backed by MCP servers, Plugins, and all other runners with method_json_schema_map.
    /// Returns error if the runner doesn't support usings or the using doesn't exist.
    fn convert_worker_using_to_function_specs(
        worker_id: WorkerId,
        worker_data: WorkerData,
        runner: RunnerWithSchema,
        using: &str,
    ) -> Result<FunctionSpecs> {
        let runner_data = runner
            .data
            .as_ref()
            .ok_or_else(|| JobWorkerError::NotFound("Runner data not found".to_string()))?;

        // Get proto schema for description
        let proto_schema = runner_data
            .method_proto_map
            .as_ref()
            .and_then(|map| map.schemas.get(using))
            .ok_or_else(|| {
                JobWorkerError::NotFound(format!(
                    "Method '{}' not found in Worker '{}' (runner '{}')",
                    using, worker_data.name, runner_data.name
                ))
            })?;

        // Get cached JSON schema for args/result
        let json_schema = runner
            .method_json_schema_map
            .as_ref()
            .and_then(|map| map.schemas.get(using));

        // Phase 6.7: Use methods map with single method
        let mut method_schemas = std::collections::HashMap::new();
        method_schemas.insert(
            using.to_string(),
            proto::jobworkerp::function::data::MethodSchema {
                description: proto_schema.description.clone(),
                arguments_schema: json_schema
                    .map(|s| s.args_schema.clone())
                    .unwrap_or_else(|| "{}".to_string()),
                result_schema: json_schema.and_then(|s| s.result_schema.clone()),
                output_type: Self::get_method_output_type(Some(runner_data), using),
                annotations: None,
            },
        );

        Ok(FunctionSpecs {
            runner_type: runner_data.runner_type,
            runner_id: runner.id,
            worker_id: Some(worker_id),
            name: format!("{}___{}", worker_data.name, using),
            description: proto_schema
                .description
                .clone()
                .unwrap_or_else(|| format!("{} - {}", worker_data.description, using)),
            settings_schema: String::new(), // Workers don't have settings (already configured)
            methods: Some(proto::jobworkerp::function::data::MethodSchemaMap {
                schemas: method_schemas,
            }),
        })
    }

    // Parse JSON value with key extraction
    #[allow(clippy::if_same_then_else)]
    fn parse_as_json_with_key_or_noop(key: &str, value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(mut value_map) => {
                if let Some(candidate_value) = value_map.remove(key) {
                    // Try to remove key or noop
                    // Check if not empty object
                    if candidate_value.is_object()
                        && candidate_value.as_object().is_some_and(|o| !o.is_empty())
                    {
                        candidate_value
                    } else if candidate_value.is_string()
                        && candidate_value.as_str().is_some_and(|s| !s.is_empty())
                    {
                        candidate_value
                    } else {
                        tracing::warn!(
                            "data key:{} is not a valid json or string: {:#?}",
                            key,
                            &candidate_value
                        );
                        // Original value
                        value_map.insert(key.to_string(), candidate_value.clone());
                        serde_json::Value::Object(value_map)
                    }
                } else {
                    serde_json::Value::Object(value_map)
                }
            }
            _ => {
                tracing::warn!("value is not json object: {:#?}", &value);
                value
            }
        }
    }
}
