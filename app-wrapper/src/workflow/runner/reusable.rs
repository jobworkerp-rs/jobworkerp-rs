use crate::workflow::definition::workflow::WorkflowSchema;
use crate::workflow::execute::workflow::WorkflowExecutor;
use anyhow::Result;
use app::module::AppModule;
use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::{pin_mut, StreamExt};
use infra_utils::infra::net::reqwest::ReqwestClient;
use infra_utils::infra::trace::Tracing;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_base::APP_NAME;
use jobworkerp_runner::jobworkerp::runner::{
    workflow_result::WorkflowStatus, ReusableWorkflowArgs,
};
use jobworkerp_runner::jobworkerp::runner::{ReusableWorkflowRunnerSettings, WorkflowResult};
use jobworkerp_runner::runner::workflow::ReusableWorkflowRunnerSpec;
use jobworkerp_runner::runner::{RunnerSpec, RunnerTrait};
use opentelemetry::trace::TraceContextExt;
use prost::Message;
use proto::jobworkerp::data::StreamingOutputType;
use proto::jobworkerp::data::{ResultOutputItem, RunnerType};
use std::collections::HashMap;
use std::{sync::Arc, time::Duration};

#[derive(Debug, Clone)]
pub struct ReusableWorkflowRunner {
    app_module: Arc<AppModule>,
    http_client: ReqwestClient,
    workflow_executor: Option<Arc<WorkflowExecutor>>,
    workflow: Option<Arc<WorkflowSchema>>,
    canceled: bool,
}
impl ReusableWorkflowRunner {
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

        Ok(ReusableWorkflowRunner {
            app_module,
            http_client,
            workflow_executor: None,
            workflow: None,
            canceled: false,
        })
    }
}
impl ReusableWorkflowRunnerSpec for ReusableWorkflowRunner {}

impl RunnerSpec for ReusableWorkflowRunner {
    fn name(&self) -> String {
        ReusableWorkflowRunnerSpec::name(self)
    }

    fn runner_settings_proto(&self) -> String {
        ReusableWorkflowRunnerSpec::runner_settings_proto(self)
    }

    fn job_args_proto(&self) -> String {
        ReusableWorkflowRunnerSpec::job_args_proto(self)
    }

    fn result_output_proto(&self) -> Option<String> {
        ReusableWorkflowRunnerSpec::result_output_proto(self)
    }

    fn output_type(&self) -> StreamingOutputType {
        ReusableWorkflowRunnerSpec::output_type(self)
    }
    fn settings_schema(&self) -> String {
        include_str!("../../../../runner/schema/workflow.json").to_string()
    }
    fn arguments_schema(&self) -> String {
        let schema = schemars::schema_for!(ReusableWorkflowArgs);
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

impl Tracing for ReusableWorkflowRunner {}

#[async_trait]
impl RunnerTrait for ReusableWorkflowRunner {
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        // load settings
        let settings = ProstMessageCodec::deserialize_message::<ReusableWorkflowRunnerSettings>(
            settings.as_slice(),
        )?;
        let workflow: WorkflowSchema = serde_json::from_str(&settings.json_data)
            .or_else(|_| serde_yaml::from_str(&settings.json_data))?;
        tracing::debug!("Workflow loaded: {:#?}", &workflow);
        self.workflow = Some(Arc::new(workflow));

        Ok(())
    }
    async fn run(
        &mut self,
        args: &[u8],
        metadata: HashMap<String, String>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        let result = async {
            let span = Self::otel_span_from_metadata(&metadata, APP_NAME, "reusable_workflow.run");
            // let _ = span.enter();
            let cx = opentelemetry::Context::current_with_span(span);
            let arg = ProstMessageCodec::deserialize_message::<ReusableWorkflowArgs>(args)?;
            tracing::debug!("Workflow args: {:#?}", &arg);
            if let Some(workflow) = self.workflow.as_ref() {
                tracing::debug!("Workflow: {:#?}", workflow);
                if self.canceled {
                    return Err(anyhow::anyhow!(
                        "canceled by user: {}, {:?}",
                        RunnerType::ReusableWorkflow.as_str_name(),
                        arg
                    ));
                }
                let input_json = serde_json::from_str(&arg.input)
                    .unwrap_or_else(|_| serde_json::Value::String(arg.input.clone()));
                let context_json = Arc::new(
                    arg.workflow_context
                        .as_deref()
                        .map(serde_json::from_str)
                        .unwrap_or_else(|| Ok(serde_json::Value::Object(Default::default())))?,
                );
                let executor = WorkflowExecutor::new(
                    self.app_module.clone(),
                    self.http_client.clone(),
                    workflow.clone(),
                    Arc::new(input_json),
                    context_json,
                    Arc::new(metadata.clone()),
                );

                // Get the stream of workflow context updates
                let workflow_stream = executor.execute_workflow(Arc::new(cx));
                pin_mut!(workflow_stream);

                // Store the final workflow context
                let mut final_context = None;

                // Process the stream of workflow context results
                while let Some(result) = workflow_stream.next().await {
                    match result {
                        Ok(context) => {
                            final_context = Some(context);
                        }
                        Err(e) => {
                            return Err(JobWorkerError::RuntimeError(format!(
                                "Failed to execute workflow: {:?}",
                                e
                            ))
                            .into());
                        }
                    }
                }

                // Return the final workflow context or an error if none was received
                let res = final_context
                    .ok_or_else(|| anyhow::anyhow!("No workflow context was returned"))?;

                tracing::info!("Workflow result: {}", res.read().await.output_string());

                let res = res.read().await;
                let r = WorkflowResult {
                    id: res.id.to_string().clone(),
                    output: serde_json::to_string(&res.output)?,
                    position: res.position.as_json_pointer(),
                    status: WorkflowStatus::from_str_name(res.status.to_string().as_str())
                        .unwrap_or(WorkflowStatus::Faulted) as i32,
                    error_message: if res.status == WorkflowStatus::Completed.into() {
                        None
                    } else {
                        res.output.as_ref().map(|o| o.to_string())
                    },
                };
                drop(res);
                Ok(r.encode_to_vec())
            } else {
                Err(anyhow::anyhow!("workflow not loaded"))
            }
        }
        .await;
        (result, metadata)
    }

    async fn run_stream(
        &mut self,
        args: &[u8],
        metadata: HashMap<String, String>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        let cx = Self::create_context(&metadata);
        let arg = ProstMessageCodec::deserialize_message::<ReusableWorkflowArgs>(args)?;
        tracing::debug!("workflow args: {:#?}", arg);
        let metadata_arc = Arc::new(metadata.clone());

        if self.canceled {
            return Err(anyhow::anyhow!(
                "canceled by user: {}, {:?}",
                RunnerType::ReusableWorkflow.as_str_name(),
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
                })
                .unwrap_or_else(|| serde_json::Value::Object(Default::default())),
        );
        tracing::debug!(
            "workflow context_json: {}",
            serde_json::to_string_pretty(&context_json).unwrap_or_default()
        );

        let workflow = self.workflow.clone().ok_or_else(|| {
            tracing::error!("workflow is not loaded");
            anyhow::anyhow!("workflow is not loaded")
        })?;
        let executor = Arc::new(WorkflowExecutor::new(
            self.app_module.clone(),
            self.http_client.clone(),
            workflow,
            Arc::new(input_json),
            context_json.clone(),
            metadata_arc.clone(),
        ));
        let workflow_stream = executor.execute_workflow(Arc::new(cx));
        self.workflow_executor = Some(executor.clone());

        let output_stream = workflow_stream
            .then(|result| async move {
                match result {
                    Ok(context) => {
                        let context = context.read_owned().await;
                        let workflow_result = WorkflowResult {
                            id: context.id.to_string(),
                            output: serde_json::to_string(&context.output).unwrap_or_default(),
                            position: context.position.as_json_pointer(),
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

                        let buf = workflow_result.encode_to_vec();
                        ResultOutputItem {
                            item: Some(proto::jobworkerp::data::result_output_item::Item::Data(
                                buf,
                            )),
                        }
                    }
                    Err(e) => {
                        tracing::error!("Error in workflow execution: {:?}", e);
                        let workflow_result = WorkflowResult {
                            id: "error".to_string(),
                            output: "".to_string(),
                            position: e.as_ref().instance.clone().unwrap_or_default(),
                            status: WorkflowStatus::Faulted as i32,
                            error_message: Some(format!("Failed to execute workflow: {}", e)),
                        };

                        let buf = workflow_result.encode_to_vec();
                        ResultOutputItem {
                            item: Some(proto::jobworkerp::data::result_output_item::Item::Data(
                                buf,
                            )),
                        }
                    }
                }
            })
            .chain(futures::stream::once(async move {
                ResultOutputItem {
                    item: Some(proto::jobworkerp::data::result_output_item::Item::End(
                        proto::jobworkerp::data::Trailer { metadata },
                    )),
                }
            }))
            .boxed();

        Ok(output_stream)
    }

    async fn cancel(&mut self) {
        self.canceled = true;
        if let Some(executor) = self.workflow_executor.as_ref() {
            executor.cancel().await;
        }
    }
}
