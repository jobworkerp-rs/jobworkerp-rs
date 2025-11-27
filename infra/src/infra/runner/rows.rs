use jobworkerp_runner::runner::{
    mcp::McpServerRunnerImpl, timeout_config::RunnerTimeoutConfig, RunnerSpec,
};
use proto::jobworkerp::data::{Runner, RunnerData, RunnerId};
use proto::jobworkerp::function::data::McpTool;
use std::any::Any;

// db row definitions
#[derive(sqlx::FromRow, Debug, Clone, PartialEq)]
pub struct RunnerRow {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub definition: String,
    pub r#type: i32,
    pub created_at: i64,
}

impl RunnerRow {
    pub async fn to_runner_with_schema(
        &self,
        runner: Box<dyn RunnerSpec + Send + Sync>,
    ) -> RunnerWithSchema {
        if let Some(mcp_runner) =
            (runner.as_ref() as &dyn Any).downcast_ref::<McpServerRunnerImpl>()
        {
            // Get tools from MCP runner (already loaded during construction)
            let tools = mcp_runner.tools().unwrap_or_else(|e| {
                tracing::error!("MCP runner '{}' tools retrieval failed: {:?}", self.name, e);
                Vec::default()
            });

            RunnerWithSchema {
                id: Some(RunnerId { value: self.id }),
                data: Some(RunnerData {
                    name: self.name.clone(),
                    description: self.description.clone(),
                    runner_type: self.r#type,
                    runner_settings_proto: runner.runner_settings_proto(),
                    job_args_proto: Some(runner.job_args_proto()),
                    result_output_proto: runner.result_output_proto(),
                    output_type: runner.output_type() as i32,
                    definition: self.definition.clone(),
                    using_protos: None,
                }),
                settings_schema: runner.settings_schema(),
                arguments_schema: runner.arguments_schema(),
                output_schema: runner.output_schema(),
                tools,
            }
        } else {
            // Plugin schema methods are synchronous and may block, so we need to run them in spawn_blocking
            // to allow timeout to work properly
            let timeout_config = RunnerTimeoutConfig::global();
            let id = self.id;
            let description = self.description.clone();
            let r#type = self.r#type;
            let definition = self.definition.clone();

            let schema_result = tokio::time::timeout(
                timeout_config.plugin_schema_load,
                tokio::task::spawn_blocking(move || {
                    let runner_settings_proto = runner.runner_settings_proto();
                    let job_args_proto = runner.job_args_proto();
                    let result_output_proto = runner.result_output_proto();
                    let settings_schema = runner.settings_schema();
                    let arguments_schema = runner.arguments_schema();
                    let output_schema = runner.output_schema();
                    let output_type = runner.output_type() as i32;

                    (
                        runner_settings_proto,
                        job_args_proto,
                        result_output_proto,
                        settings_schema,
                        arguments_schema,
                        output_schema,
                        output_type,
                    )
                }),
            )
            .await;

            match schema_result {
                Ok(Ok((
                    runner_settings_proto,
                    job_args_proto,
                    result_output_proto,
                    settings_schema,
                    arguments_schema,
                    output_schema,
                    output_type,
                ))) => RunnerWithSchema {
                    id: Some(RunnerId { value: id }),
                    data: Some(RunnerData {
                        name: self.name.clone(),
                        description,
                        runner_type: r#type,
                        runner_settings_proto,
                        job_args_proto: Some(job_args_proto),
                        result_output_proto,
                        output_type,
                        definition,
                        using_protos: None,
                    }),
                    settings_schema,
                    arguments_schema,
                    output_schema,
                    tools: Vec::default(),
                },
                Ok(Err(e)) => {
                    tracing::error!(
                        "Plugin runner '{}' (id={}, definition='{}') schema loading failed: {:?}",
                        self.name,
                        id,
                        self.definition,
                        e
                    );
                    // Return minimal schema on error
                    RunnerWithSchema {
                        id: Some(RunnerId { value: id }),
                        data: Some(RunnerData {
                            name: self.name.clone(),
                            description,
                            runner_type: r#type,
                            runner_settings_proto: String::new(),
                            job_args_proto: Some(String::new()),
                            result_output_proto: None,
                            output_type: 0,
                            definition,
                            using_protos: None,
                        }),
                        settings_schema: String::new(),
                        arguments_schema: String::new(),
                        output_schema: None,
                        tools: Vec::default(),
                    }
                }
                Err(_) => {
                    tracing::error!(
                        "Plugin runner '{}' (id={}, definition='{}') schema loading timed out after {:?}",
                        self.name,
                        id,
                        self.definition,
                        timeout_config.plugin_schema_load
                    );
                    // Return minimal schema on timeout
                    RunnerWithSchema {
                        id: Some(RunnerId { value: id }),
                        data: Some(RunnerData {
                            name: self.name.clone(),
                            description,
                            runner_type: r#type,
                            runner_settings_proto: String::new(),
                            job_args_proto: Some(String::new()),
                            result_output_proto: None,
                            output_type: 0,
                            definition,
                            using_protos: None,
                        }),
                        settings_schema: String::new(),
                        arguments_schema: String::new(),
                        output_schema: None,
                        tools: Vec::default(),
                    }
                }
            }
        }
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize, PartialEq, ::prost::Message)]
pub struct RunnerWithSchema {
    #[prost(message, tag = "1")]
    pub id: Option<RunnerId>,
    #[prost(message, tag = "2")]
    pub data: Option<RunnerData>,
    #[prost(string, tag = "3")]
    pub settings_schema: String,
    #[prost(string, tag = "4")]
    pub arguments_schema: String,
    #[prost(string, optional, tag = "5")]
    pub output_schema: Option<String>,
    #[prost(message, repeated, tag = "6")]
    pub tools: Vec<McpTool>,
}

impl RunnerWithSchema {
    pub fn to_proto(&self) -> Runner {
        Runner {
            id: self.id,
            data: self.data.clone(),
        }
    }
    pub fn into_proto(self) -> Runner {
        Runner {
            id: self.id,
            data: self.data,
        }
    }
}
