use anyhow::Result;
use infra::infra::runner::rows::RunnerWithSchema;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::jobworkerp::runner::{
    ReusableWorkflowRunnerSettings, WorkflowRunnerSettings,
};
use proto::DEFAULT_METHOD_NAME;
use proto::jobworkerp::data::{RunnerData, RunnerType, StreamingOutputType, WorkerData, WorkerId};
use proto::jobworkerp::function::data::FunctionSpecs;

pub trait FunctionSpecConverter {
    fn get_method_output_type(runner_data: Option<&RunnerData>, method_name: &str) -> i32 {
        runner_data
            .and_then(|d| d.method_proto_map.as_ref())
            .and_then(|map| map.schemas.get(method_name))
            .map(|schema| schema.output_type)
            .unwrap_or(StreamingOutputType::NonStreaming as i32)
    }

    fn convert_runner_to_function_specs(runner: RunnerWithSchema) -> FunctionSpecs {
        let runner_data = runner.data.as_ref();

        let method_schemas = runner_data
            .and_then(|d| d.method_proto_map.as_ref())
            .map(|proto_map| {
                proto_map
                    .schemas
                    .iter()
                    .map(|(name, proto_schema)| {
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
            settings_schema: runner.settings_schema,
            methods: Some(proto::jobworkerp::function::data::MethodSchemaMap {
                schemas: method_schemas,
            }),
        }
    }

    fn convert_worker_to_function_specs(
        id: WorkerId,
        data: WorkerData,
        runner: RunnerWithSchema,
    ) -> Result<FunctionSpecs> {
        let runner_data = runner.data.as_ref();

        let workflow_schema = runner_data.and_then(|d| {
            Self::extract_workflow_schema(d.runner_type, data.runner_settings.as_slice())
        });

        let method_schemas = if let Some(wf_schema) = workflow_schema {
            // Workflow worker: extract summary and input schema from workflow definition
            let (name, summary) = wf_schema
                .get("document")
                .and_then(|d| {
                    d.as_object().map(|d| {
                        (
                            Some(DEFAULT_METHOD_NAME.to_string()),
                            d.get("summary")
                                .and_then(|s| s.as_str().map(|s| s.to_string())),
                        )
                    })
                })
                .unwrap_or_default();
            let input_schema = wf_schema.get("input").cloned();
            let input_schema =
                input_schema.map(|s| Self::parse_as_json_with_key_or_noop("schema", s));
            let input_schema =
                input_schema.map(|s| Self::parse_as_json_with_key_or_noop("document", s));

            let default_method = runner
                .method_json_schema_map
                .as_ref()
                .and_then(|map| map.schemas.get(proto::DEFAULT_METHOD_NAME));

            let result_schema = default_method.and_then(|schema| schema.result_schema.clone());
            let output_type = Self::get_method_output_type(runner_data, proto::DEFAULT_METHOD_NAME);

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
            runner_data
                .and_then(|d| d.method_proto_map.as_ref())
                .map(|proto_map| {
                    proto_map
                        .schemas
                        .iter()
                        .map(|(name, proto_schema)| {
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
            settings_schema: String::new(),
            methods: Some(proto::jobworkerp::function::data::MethodSchemaMap {
                schemas: method_schemas,
            }),
        })
    }

    /// Convert Runner + specific using to FunctionSpecs
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

        let json_schema = runner
            .method_json_schema_map
            .as_ref()
            .and_then(|map| map.schemas.get(using));

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
            settings_schema: String::new(),
            methods: Some(proto::jobworkerp::function::data::MethodSchemaMap {
                schemas: method_schemas,
            }),
        })
    }

    /// Convert Worker + specific using to FunctionSpecs
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

        // For workflow runners with "run" method, extract summary and input_schema
        // from the workflow definition stored in runner_settings
        let workflow_override = if using == DEFAULT_METHOD_NAME {
            Self::extract_workflow_schema(runner_data.runner_type, &worker_data.runner_settings)
        } else {
            None
        };

        let (description, arguments_schema) = if let Some(wf_schema) = workflow_override {
            let summary = wf_schema
                .get("document")
                .and_then(|d| d.get("summary"))
                .and_then(|s| s.as_str())
                .map(|s| s.to_string());
            let input_schema = wf_schema.get("input").cloned();
            let input_schema =
                input_schema.map(|s| Self::parse_as_json_with_key_or_noop("schema", s));
            let input_schema =
                input_schema.map(|s| Self::parse_as_json_with_key_or_noop("document", s));
            let args_str = input_schema
                .map(|s| s.to_string())
                .unwrap_or_else(|| "{}".to_string());
            (summary, args_str)
        } else {
            let json_schema = runner
                .method_json_schema_map
                .as_ref()
                .and_then(|map| map.schemas.get(using));
            (
                proto_schema.description.clone(),
                json_schema
                    .map(|s| s.args_schema.clone())
                    .unwrap_or_else(|| "{}".to_string()),
            )
        };

        let json_schema = runner
            .method_json_schema_map
            .as_ref()
            .and_then(|map| map.schemas.get(using));
        let result_schema = json_schema.and_then(|s| s.result_schema.clone());

        let mut method_schemas = std::collections::HashMap::new();
        method_schemas.insert(
            using.to_string(),
            proto::jobworkerp::function::data::MethodSchema {
                description: description.clone(),
                arguments_schema,
                result_schema,
                output_type: Self::get_method_output_type(Some(runner_data), using),
                annotations: None,
            },
        );

        Ok(FunctionSpecs {
            runner_type: runner_data.runner_type,
            runner_id: runner.id,
            worker_id: Some(worker_id),
            name: format!("{}___{}", worker_data.name, using),
            description: description
                .unwrap_or_else(|| format!("{} - {}", worker_data.description, using)),
            settings_schema: String::new(),
            methods: Some(proto::jobworkerp::function::data::MethodSchemaMap {
                schemas: method_schemas,
            }),
        })
    }

    // Two branches deserialize different protobuf types (ReusableWorkflowRunnerSettings vs WorkflowRunnerSettings)
    #[allow(clippy::if_same_then_else)]
    /// Extract workflow schema from runner_settings bytes.
    /// Supports both unified Workflow and deprecated ReusableWorkflow runners.
    fn extract_workflow_schema(
        runner_type: i32,
        runner_settings: &[u8],
    ) -> Option<serde_json::Map<String, serde_json::Value>> {
        if runner_type == RunnerType::ReusableWorkflow as i32 {
            ProstMessageCodec::deserialize_message::<ReusableWorkflowRunnerSettings>(
                runner_settings,
            )
            .ok()
            .and_then(|s| s.schema())
        } else if runner_type == RunnerType::Workflow as i32 {
            ProstMessageCodec::deserialize_message::<WorkflowRunnerSettings>(runner_settings)
                .ok()
                .and_then(|s| s.schema())
        } else {
            None
        }
    }

    #[allow(clippy::if_same_then_else)]
    fn parse_as_json_with_key_or_noop(key: &str, value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(mut value_map) => {
                if let Some(candidate_value) = value_map.remove(key) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
    use proto::jobworkerp::data::{
        MethodJsonSchema, MethodJsonSchemaMap, MethodProtoMap, MethodSchema, RunnerId,
    };

    struct TestConverter;
    impl FunctionSpecConverter for TestConverter {}

    fn make_workflow_json(summary: &str) -> String {
        serde_json::json!({
            "document": {
                "name": "test-workflow",
                "summary": summary
            },
            "input": {
                "schema": {
                    "document": {
                        "type": "object",
                        "properties": {
                            "owner": { "type": "string" },
                            "repo": { "type": "string" }
                        }
                    }
                }
            }
        })
        .to_string()
    }

    fn make_runner_with_schema(runner_type: RunnerType) -> RunnerWithSchema {
        let mut proto_schemas = std::collections::HashMap::new();
        proto_schemas.insert(
            DEFAULT_METHOD_NAME.to_string(),
            MethodSchema {
                args_proto: String::new(),
                result_proto: String::new(),
                description: Some("Execute workflow with pre-configured definition".to_string()),
                output_type: StreamingOutputType::NonStreaming as i32,
                ..Default::default()
            },
        );
        let mut json_schemas = std::collections::HashMap::new();
        json_schemas.insert(
            DEFAULT_METHOD_NAME.to_string(),
            MethodJsonSchema {
                args_schema: "{}".to_string(),
                result_schema: Some(r#"{"type":"object"}"#.to_string()),
                client_stream_data_schema: None,
            },
        );
        RunnerWithSchema {
            id: Some(RunnerId { value: 1 }),
            data: Some(RunnerData {
                name: runner_type.as_str_name().to_string(),
                description: "Generic runner description".to_string(),
                runner_type: runner_type as i32,
                runner_settings_proto: String::new(),
                definition: String::new(),
                method_proto_map: Some(MethodProtoMap {
                    schemas: proto_schemas,
                }),
            }),
            settings_schema: String::new(),
            method_json_schema_map: Some(MethodJsonSchemaMap {
                schemas: json_schemas,
            }),
        }
    }

    #[test]
    fn test_unified_workflow_worker_uses_summary_as_description() {
        let summary = "Review a Gitea PR using AI code review agent";
        let workflow_json = make_workflow_json(summary);

        let settings = jobworkerp_runner::jobworkerp::runner::WorkflowRunnerSettings {
            workflow_source: Some(
                jobworkerp_runner::jobworkerp::runner::workflow_runner_settings::WorkflowSource::WorkflowData(
                    workflow_json,
                ),
            ),
        };
        let runner_settings = ProstMessageCodec::serialize_message(&settings).unwrap();

        let worker_data = WorkerData {
            name: "gitea-code-review-workflow".to_string(),
            description: "Worker description".to_string(),
            runner_id: Some(RunnerId { value: 1 }),
            runner_settings,
            ..Default::default()
        };

        let runner = make_runner_with_schema(RunnerType::Workflow);
        let worker_id = WorkerId { value: 100 };

        let specs = TestConverter::convert_worker_to_function_specs(worker_id, worker_data, runner)
            .unwrap();

        assert_eq!(specs.name, "gitea-code-review-workflow");

        let methods = specs.methods.unwrap();
        let method = methods.schemas.get(DEFAULT_METHOD_NAME).unwrap();
        assert_eq!(
            method.description.as_deref(),
            Some(summary),
            "Workflow summary should be used as method description, not the generic runner description"
        );
    }

    #[test]
    fn test_reusable_workflow_worker_uses_summary_as_description() {
        let summary = "Process data with reusable workflow";
        let workflow_json = make_workflow_json(summary);

        let settings = ReusableWorkflowRunnerSettings {
            json_data: workflow_json,
        };
        let runner_settings = ProstMessageCodec::serialize_message(&settings).unwrap();

        let worker_data = WorkerData {
            name: "data-processor-workflow".to_string(),
            description: "Worker description".to_string(),
            runner_id: Some(RunnerId { value: 1 }),
            runner_settings,
            ..Default::default()
        };

        let runner = make_runner_with_schema(RunnerType::ReusableWorkflow);
        let worker_id = WorkerId { value: 200 };

        let specs = TestConverter::convert_worker_to_function_specs(worker_id, worker_data, runner)
            .unwrap();

        let methods = specs.methods.unwrap();
        let method = methods.schemas.get(DEFAULT_METHOD_NAME).unwrap();
        assert_eq!(
            method.description.as_deref(),
            Some(summary),
            "Workflow summary should be used as method description"
        );
    }

    #[test]
    fn test_non_workflow_worker_uses_runner_method_description() {
        let worker_data = WorkerData {
            name: "my-command-worker".to_string(),
            description: "Worker description".to_string(),
            runner_id: Some(RunnerId { value: 1 }),
            runner_settings: vec![],
            ..Default::default()
        };

        let runner = make_runner_with_schema(RunnerType::Command);
        let worker_id = WorkerId { value: 300 };

        let specs = TestConverter::convert_worker_to_function_specs(worker_id, worker_data, runner)
            .unwrap();

        let methods = specs.methods.unwrap();
        let method = methods.schemas.get(DEFAULT_METHOD_NAME).unwrap();
        assert_eq!(
            method.description.as_deref(),
            Some("Execute workflow with pre-configured definition"),
            "Non-workflow worker should use runner's method description"
        );
    }

    #[test]
    fn test_workflow_worker_extracts_input_schema() {
        let workflow_json = make_workflow_json("Test workflow");

        let settings = jobworkerp_runner::jobworkerp::runner::WorkflowRunnerSettings {
            workflow_source: Some(
                jobworkerp_runner::jobworkerp::runner::workflow_runner_settings::WorkflowSource::WorkflowData(
                    workflow_json,
                ),
            ),
        };
        let runner_settings = ProstMessageCodec::serialize_message(&settings).unwrap();

        let worker_data = WorkerData {
            name: "test-wf".to_string(),
            description: "desc".to_string(),
            runner_id: Some(RunnerId { value: 1 }),
            runner_settings,
            ..Default::default()
        };

        let runner = make_runner_with_schema(RunnerType::Workflow);
        let worker_id = WorkerId { value: 400 };

        let specs = TestConverter::convert_worker_to_function_specs(worker_id, worker_data, runner)
            .unwrap();

        let methods = specs.methods.unwrap();
        let method = methods.schemas.get(DEFAULT_METHOD_NAME).unwrap();
        let args_schema: serde_json::Value =
            serde_json::from_str(&method.arguments_schema).unwrap();
        let props = args_schema.get("properties").unwrap();
        assert!(
            props.get("owner").is_some(),
            "Input schema should contain 'owner' from workflow definition"
        );
        assert!(
            props.get("repo").is_some(),
            "Input schema should contain 'repo' from workflow definition"
        );
    }

    #[test]
    fn test_convert_worker_using_workflow_run_uses_summary() {
        let summary = "Review a Gitea PR using AI code review agent";
        let workflow_json = make_workflow_json(summary);

        let settings = jobworkerp_runner::jobworkerp::runner::WorkflowRunnerSettings {
            workflow_source: Some(
                jobworkerp_runner::jobworkerp::runner::workflow_runner_settings::WorkflowSource::WorkflowData(
                    workflow_json,
                ),
            ),
        };
        let runner_settings = ProstMessageCodec::serialize_message(&settings).unwrap();

        let worker_data = WorkerData {
            name: "gitea-code-review-workflow".to_string(),
            description: "Worker description".to_string(),
            runner_id: Some(RunnerId { value: 1 }),
            runner_settings,
            ..Default::default()
        };

        let runner = make_runner_with_schema(RunnerType::Workflow);
        let worker_id = WorkerId { value: 500 };

        let specs = TestConverter::convert_worker_using_to_function_specs(
            worker_id,
            worker_data,
            runner,
            "run",
        )
        .unwrap();

        // Name should include ___run suffix
        assert_eq!(specs.name, "gitea-code-review-workflow___run");

        // Description should be workflow summary, not generic runner description
        assert_eq!(specs.description, summary);

        let methods = specs.methods.unwrap();
        let method = methods.schemas.get("run").unwrap();
        assert_eq!(
            method.description.as_deref(),
            Some(summary),
            "Method description should be workflow summary"
        );

        // arguments_schema should be workflow input schema
        let args_schema: serde_json::Value =
            serde_json::from_str(&method.arguments_schema).unwrap();
        let props = args_schema.get("properties").unwrap();
        assert!(
            props.get("owner").is_some(),
            "Input schema should contain 'owner' from workflow definition"
        );
    }

    #[test]
    fn test_convert_worker_using_non_workflow_uses_runner_description() {
        let worker_data = WorkerData {
            name: "my-mcp-worker".to_string(),
            description: "Worker description".to_string(),
            runner_id: Some(RunnerId { value: 1 }),
            runner_settings: vec![],
            ..Default::default()
        };

        let runner = make_runner_with_schema(RunnerType::Command);
        let worker_id = WorkerId { value: 600 };

        let specs = TestConverter::convert_worker_using_to_function_specs(
            worker_id,
            worker_data,
            runner,
            "run",
        )
        .unwrap();

        let methods = specs.methods.unwrap();
        let method = methods.schemas.get("run").unwrap();
        assert_eq!(
            method.description.as_deref(),
            Some("Execute workflow with pre-configured definition"),
            "Non-workflow worker should use runner's method description"
        );
    }
}
