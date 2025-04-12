use crate::workflow::definition::workflow::WorkflowSchema;
use crate::workflow::execute::workflow::WorkflowExecutor;
use anyhow::Result;
use app::module::AppModule;
use async_trait::async_trait;
use futures::stream::BoxStream;
use infra_utils::infra::net::reqwest::ReqwestClient;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_runner::jobworkerp::runner::{
    workflow_result::WorkflowStatus, ReusableWorkflowArgs,
};
use jobworkerp_runner::jobworkerp::runner::{ReusableWorkflowRunnerSettings, WorkflowResult};
use jobworkerp_runner::runner::workflow::ReusableWorkflowRunnerSpec;
use jobworkerp_runner::runner::{RunnerSpec, RunnerTrait};
use prost::Message;
use proto::jobworkerp::data::StreamingOutputType;
use proto::jobworkerp::data::{ResultOutputItem, RunnerType};
use std::{sync::Arc, time::Duration};

#[derive(Debug, Clone)]
pub struct ReusableWorkflowRunner {
    app_module: Arc<AppModule>,
    workflow_executor: Option<WorkflowExecutor>,
    workflow: Option<Arc<WorkflowSchema>>,
    canceled: bool,
}
impl ReusableWorkflowRunner {
    // for workflow file reqwest
    const DEFAULT_REQUEST_TIMEOUT_SEC: u32 = 120; // 2 minutes
    const DEFAULT_USER_AGENT: &str = "simple-workflow/1.0";

    pub fn new(app_module: Arc<AppModule>) -> Result<Self> {
        Ok(ReusableWorkflowRunner {
            app_module,
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
    async fn run(&mut self, args: &[u8]) -> Result<Vec<Vec<u8>>> {
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
            let mut executor = WorkflowExecutor::new(
                self.app_module.clone(),
                http_client,
                workflow.clone(),
                Arc::new(input_json),
                context_json,
            );
            if self.canceled {
                return Err(anyhow::anyhow!(
                    "canceled by user: {}, {:?}",
                    RunnerType::ReusableWorkflow.as_str_name(),
                    arg
                ));
            }
            let res = executor.execute_workflow().await;
            // tracing::debug!("Workflow result: {:#?}", &res);
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
        } else {
            return Err(anyhow::anyhow!("workflow not loaded"));
        }
    }

    async fn run_stream(&mut self, arg: &[u8]) -> Result<BoxStream<'static, ResultOutputItem>> {
        // default implementation (return empty)
        let _ = arg;
        Err(anyhow::anyhow!("not implemented"))
    }

    async fn cancel(&mut self) {
        self.canceled = true;
        if let Some(executor) = self.workflow_executor.as_ref() {
            executor.cancel().await;
        }
    }
}
