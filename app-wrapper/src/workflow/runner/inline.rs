use crate::workflow::{definition::WorkflowLoader, execute::workflow::WorkflowExecutor};
use anyhow::Result;
use app::module::AppModule;
use async_trait::async_trait;
use futures::stream::BoxStream;
use infra_utils::infra::net::reqwest::ReqwestClient;
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
    workflow_executor: Option<WorkflowExecutor>,
    canceled: bool,
}
impl InlineWorkflowRunner {
    // for workflow file reqwest
    const DEFAULT_REQUEST_TIMEOUT_SEC: u32 = 120; // 2 minutes
    const DEFAULT_USER_AGENT: &str = "simple-workflow/1.0";

    pub fn new(app_module: Arc<AppModule>) -> Result<Self> {
        Ok(InlineWorkflowRunner {
            app_module,
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
        let mut executor = WorkflowExecutor::new(
            self.app_module.clone(),
            http_client,
            Arc::new(workflow),
            Arc::new(input_json),
            context_json,
        );
        if self.canceled {
            return Err(anyhow::anyhow!(
                "canceled by user: {}, {:?}",
                RunnerType::InlineWorkflow.as_str_name(),
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
