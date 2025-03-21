use crate::simple_workflow::{definition::WorkflowLoader, execute::workflow::WorkflowExecutor};
use anyhow::Result;
use app::module::AppModule;
use async_trait::async_trait;
use futures::stream::BoxStream;
use infra_utils::infra::net::reqwest::ReqwestClient;
use jobworkerp_runner::jobworkerp::runner::WorkflowResult;
use jobworkerp_runner::jobworkerp::runner::{workflow_result::WorkflowStatus, WorkflowArg};
use jobworkerp_runner::runner::simple_workflow::SimpleWorkflowRunnerSpec;
use jobworkerp_runner::runner::{RunnerSpec, RunnerTrait};
use prost::Message;
use proto::jobworkerp::data::{ResultOutputItem, RunnerType};
use std::{sync::Arc, time::Duration};

#[derive(Debug, Clone)]
pub struct SimpleWorkflowRunner {
    app_module: Arc<AppModule>,
    workflow_executor: Option<WorkflowExecutor>,
    canceled: bool,
}
impl SimpleWorkflowRunner {
    // for workflow file reqwest
    const DEFAULT_REQUEST_TIMEOUT_SEC: u32 = 120; // 2 minutes
    const DEFAULT_USER_AGENT: &str = "simple-workflow/1.0";

    pub fn new(app_module: Arc<AppModule>) -> Result<Self> {
        Ok(SimpleWorkflowRunner {
            app_module,
            workflow_executor: None,
            canceled: false,
        })
    }
}
impl SimpleWorkflowRunnerSpec for SimpleWorkflowRunner {}
impl RunnerSpec for SimpleWorkflowRunner {
    fn name(&self) -> String {
        SimpleWorkflowRunnerSpec::name(self)
    }

    fn runner_settings_proto(&self) -> String {
        SimpleWorkflowRunnerSpec::runner_settings_proto(self)
    }

    fn job_args_proto(&self) -> String {
        SimpleWorkflowRunnerSpec::job_args_proto(self)
    }

    fn result_output_proto(&self) -> Option<String> {
        SimpleWorkflowRunnerSpec::result_output_proto(self)
    }

    fn output_as_stream(&self) -> Option<bool> {
        SimpleWorkflowRunnerSpec::output_as_stream(self)
    }
}

#[async_trait]
impl RunnerTrait for SimpleWorkflowRunner {
    async fn load(&mut self, _settings: Vec<u8>) -> Result<()> {
        Ok(())
    }
    async fn run(&mut self, args: &[u8]) -> Result<Vec<Vec<u8>>> {
        let arg = WorkflowArg::decode(args)?;
        if self.canceled {
            return Err(anyhow::anyhow!(
                "canceled by user: {}, {:?}",
                RunnerType::SimpleWorkflow.as_str_name(),
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
            Some(2),
        )?;
        let workflow = WorkflowLoader::new(http_client.clone())
            .inspect_err(|e| tracing::error!("Failed to create WorkflowLoader: {:#?}", e))?
            .load_workflow(arg.workflow_url.as_deref(), arg.workflow_yaml.as_deref())
            .await
            .inspect_err(|e| tracing::error!("Failed to load workflow: {:#?}", e))?;
        let mut executor = WorkflowExecutor::new(
            self.app_module.clone(),
            http_client,
            workflow,
            Arc::new(input_json),
            context_json,
        );
        if self.canceled {
            return Err(anyhow::anyhow!(
                "canceled by user: {}, {:?}",
                RunnerType::SimpleWorkflow.as_str_name(),
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
