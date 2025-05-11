use crate::workflow::{definition::WorkflowLoader, execute::workflow::WorkflowExecutor};
use anyhow::Result;
use app::module::AppModule;
use async_trait::async_trait;
use futures::executor::block_on;
use futures::stream::BoxStream;
use futures::StreamExt;
use infra_utils::infra::net::reqwest::ReqwestClient;
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::jobworkerp::runner::workflow_result::WorkflowStatus;
use jobworkerp_runner::jobworkerp::runner::{Empty, InlineWorkflowArgs, WorkflowResult};
use jobworkerp_runner::runner::workflow::InlineWorkflowRunnerSpec;
use jobworkerp_runner::runner::{RunnerSpec, RunnerTrait};
use prost::Message;
use proto::jobworkerp::data::StreamingOutputType;
use proto::jobworkerp::data::{ResultOutputItem, RunnerType};
use schemars::JsonSchema;
use std::{sync::Arc, time::Duration};

#[derive(Debug, Clone)]
pub struct InlineWorkflowRunner {
    app_module: Arc<AppModule>,
    http_client: ReqwestClient,
    workflow_executor: Option<Arc<WorkflowExecutor>>,
    canceled: bool,
}
impl InlineWorkflowRunner {
    // for workflow file reqwest
    const DEFAULT_REQUEST_TIMEOUT_SEC: u32 = 120; // 2 minutes
    const DEFAULT_USER_AGENT: &str = "simple-workflow/1.0";

    pub fn new(app_module: Arc<AppModule>) -> Result<Self> {
        let http_client = ReqwestClient::new(
            Some(Self::DEFAULT_USER_AGENT),
            Some(Duration::from_secs(
                Self::DEFAULT_REQUEST_TIMEOUT_SEC as u64,
            )),
            Some(Duration::from_secs(
                Self::DEFAULT_REQUEST_TIMEOUT_SEC as u64,
            )),
            Some(2),
        )?;

        Ok(InlineWorkflowRunner {
            app_module,
            http_client,
            workflow_executor: None,
            canceled: false,
        })
    }
}
impl InlineWorkflowRunnerSpec for InlineWorkflowRunner {}

#[derive(Debug, JsonSchema, serde::Deserialize, serde::Serialize)]
struct WorkflowRunnerInputSchema {
    args: InlineWorkflowArgs,
}

impl RunnerSpec for InlineWorkflowRunner {
    fn name(&self) -> String {
        InlineWorkflowRunnerSpec::name(self)
    }

    fn runner_settings_proto(&self) -> String {
        InlineWorkflowRunnerSpec::runner_settings_proto(self)
    }

    fn job_args_proto(&self) -> String {
        InlineWorkflowRunnerSpec::job_args_proto(self)
    }

    fn result_output_proto(&self) -> Option<String> {
        InlineWorkflowRunnerSpec::result_output_proto(self)
    }

    fn output_type(&self) -> StreamingOutputType {
        InlineWorkflowRunnerSpec::output_type(self)
    }
    fn settings_schema(&self) -> String {
        // plain string with title
        let schema = schemars::schema_for!(Empty);
        match serde_json::to_string(&schema) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("error in settings_json_schema: {:?}", e);
                "".to_string()
            }
        }
    }
    fn arguments_schema(&self) -> String {
        let schema = schemars::schema_for!(WorkflowRunnerInputSchema);
        match serde_json::to_string(&schema) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("error in input_json_schema: {:?}", e);
                "".to_string()
            }
        }
    }
    fn output_schema(&self) -> Option<String> {
        // plain string with title
        let schema = schemars::schema_for!(WorkflowResult);
        match serde_json::to_string(&schema) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::error!("error in output_json_schema: {:?}", e);
                None
            }
        }
    }
}

#[async_trait]
impl RunnerTrait for InlineWorkflowRunner {
    async fn load(&mut self, _settings: Vec<u8>) -> Result<()> {
        Ok(())
    }
    async fn run(&mut self, args: &[u8]) -> Result<Vec<Vec<u8>>> {
        let arg = InlineWorkflowArgs::decode(args)?;
        tracing::debug!("workflow args: {:#?}", arg);
        if self.canceled {
            return Err(anyhow::anyhow!(
                "canceled by user: {}, {:?}",
                RunnerType::InlineWorkflow.as_str_name(),
                arg
            ));
        }
        let input_json = serde_json::from_str(&arg.input)
            .unwrap_or_else(|_| serde_json::Value::String(arg.input.clone()));
        tracing::debug!(
            "workflow input_json: {}",
            serde_json::to_string_pretty(&input_json).unwrap_or_default()
        );
        let context_json = Arc::new(
            arg.workflow_context
                .as_deref()
                .map(|c| {
                    serde_json::from_str(c).unwrap_or(serde_json::Value::Object(Default::default()))
                    // ignore error
                })
                .unwrap_or_else(|| serde_json::Value::Object(Default::default())),
        );
        tracing::debug!(
            "workflow context_json: {}",
            serde_json::to_string_pretty(&context_json).unwrap_or_default()
        );
        let http_client = ReqwestClient::new(
            Some(Self::DEFAULT_USER_AGENT),
            Some(Duration::from_secs(
                Self::DEFAULT_REQUEST_TIMEOUT_SEC as u64,
            )),
            Some(Duration::from_secs(
                Self::DEFAULT_REQUEST_TIMEOUT_SEC as u64,
            )),
            Some(2),
        )?;
        let source = arg.workflow_source.as_ref().ok_or({
            tracing::error!("workflow_source is required in workflow args");
            anyhow::anyhow!("workflow_source is required in workflow args")
        })?;
        tracing::debug!("workflow source: {:?}", source);
        let workflow = WorkflowLoader::new(http_client.clone())
            .inspect_err(|e| tracing::error!("Failed to create WorkflowLoader: {:#?}", e))?
            .load_workflow_source(source)
            .await
            .inspect_err(|e| tracing::error!("Failed to load workflow: {:#?}", e))?;
        tracing::debug!("workflow: {:#?}", workflow);
        let executor = WorkflowExecutor::new(
            self.app_module.clone(),
            http_client,
            Arc::new(workflow),
            Arc::new(input_json),
            context_json.clone(),
        );

        // Get the stream of workflow context updates
        let mut workflow_stream = executor.execute_workflow();

        // Store the final workflow context
        let mut final_context = None;

        // Process the stream of workflow context results
        while let Some(result) = workflow_stream.next().await {
            match result {
                Ok(context) => {
                    final_context = Some(context);
                    if self.canceled {
                        return Err(JobWorkerError::RuntimeError(format!(
                            "canceled by user: {}, {:?}",
                            RunnerType::InlineWorkflow.as_str_name(),
                            arg
                        ))
                        .into());
                    }
                }
                Err(e) => {
                    return Err(e.context("Failed to execute workflow"));
                }
            }
        }

        // Return the final workflow context or an error if none was received
        let res =
            final_context.ok_or_else(|| anyhow::anyhow!("No workflow context was returned"))?;

        tracing::info!("Workflow result: {}", res.read().await.output_string());

        let res = res.read().await;
        let r = WorkflowResult {
            id: res.id.to_string().clone(),
            output: serde_json::to_string(&res.output)?,
            status: WorkflowStatus::from_str_name(res.status.to_string().as_str())
                .unwrap_or(WorkflowStatus::Faulted) as i32,
            error_message: if res.status == WorkflowStatus::Completed.into() {
                None
            } else {
                res.output.as_ref().map(|o| o.to_string())
            },
        };
        drop(res);
        Ok(vec![r.encode_to_vec()])
    }

    async fn run_stream(&mut self, args: &[u8]) -> Result<BoxStream<'static, ResultOutputItem>> {
        let arg = InlineWorkflowArgs::decode(args)?;
        tracing::debug!("workflow args: {:#?}", arg);
        if self.canceled {
            return Err(anyhow::anyhow!(
                "canceled by user: {}, {:?}",
                RunnerType::InlineWorkflow.as_str_name(),
                arg
            ));
        }
        let input_json = serde_json::from_str(&arg.input)
            .unwrap_or_else(|_| serde_json::Value::String(arg.input.clone()));
        tracing::debug!(
            "workflow input_json: {}",
            serde_json::to_string_pretty(&input_json).unwrap_or_default()
        );
        let context_json = Arc::new(
            arg.workflow_context
                .as_deref()
                .map(|c| {
                    serde_json::from_str(c).unwrap_or(serde_json::Value::Object(Default::default()))
                    // ignore error
                })
                .unwrap_or_else(|| serde_json::Value::Object(Default::default())),
        );
        tracing::debug!(
            "workflow context_json: {}",
            serde_json::to_string_pretty(&context_json).unwrap_or_default()
        );
        let source = arg.workflow_source.as_ref().ok_or({
            tracing::error!("workflow_source is required in workflow args");
            JobWorkerError::InvalidParameter(
                "workflow_source is required in workflow args".to_string(),
            )
        })?;
        tracing::debug!("workflow source: {:?}", source);
        let http_client = self.http_client.clone();
        let app_module = self.app_module.clone();

        let workflow = WorkflowLoader::new(http_client.clone())
            .inspect_err(|e| tracing::error!("Failed to create WorkflowLoader: {:#?}", e))?
            .load_workflow_source(source)
            .await
            .inspect_err(|e| tracing::error!("Failed to load workflow: {:#?}", e))?;
        tracing::debug!("workflow: {:#?}", workflow);

        let executor = Arc::new(WorkflowExecutor::new(
            app_module,
            http_client,
            Arc::new(workflow),
            Arc::new(input_json),
            context_json.clone(),
        ));
        let workflow_stream = executor.execute_workflow();
        self.workflow_executor = Some(executor.clone());

        let output_stream = workflow_stream
            .map(|result| {
                match result {
                    Ok(context) => {
                        let context = block_on(context.read_owned());
                        // Create a WorkflowResult from the context
                        let workflow_result = WorkflowResult {
                            id: context.id.to_string(),
                            output: serde_json::to_string(&context.output).unwrap_or_default(),
                            status: WorkflowStatus::from_str_name(
                                context.status.to_string().as_str(),
                            )
                            .unwrap_or(WorkflowStatus::Faulted)
                                as i32,
                            error_message: if context.status != WorkflowStatus::Faulted.into() {
                                None
                            } else {
                                context.output.as_ref().map(|o| o.to_string())
                            },
                        };

                        // Encode the workflow result and return it as a data item
                        let buf = workflow_result.encode_to_vec();
                        ResultOutputItem {
                            item: Some(proto::jobworkerp::data::result_output_item::Item::Data(
                                buf,
                            )),
                        }
                    }
                    Err(e) => {
                        // If there's an error, create a WorkflowResult with error information
                        tracing::error!("Error in workflow execution: {:?}", e);
                        let workflow_result = WorkflowResult {
                            id: "error".to_string(),
                            output: "".to_string(),
                            status: WorkflowStatus::Faulted as i32,
                            error_message: Some(format!("Failed to execute workflow: {}", e)),
                        };

                        // Encode the error workflow result and return it as a data item
                        let buf = workflow_result.encode_to_vec();
                        ResultOutputItem {
                            item: Some(proto::jobworkerp::data::result_output_item::Item::Data(
                                buf,
                            )),
                        }
                    }
                }
            })
            .chain(futures::stream::once(async {
                // Add an End item at the end of the stream
                ResultOutputItem {
                    item: Some(proto::jobworkerp::data::result_output_item::Item::End(
                        proto::jobworkerp::data::Empty {},
                    )),
                }
            }))
            .boxed();

        Ok(output_stream)
    }

    async fn cancel(&mut self) {
        self.canceled = true;
    }
}
