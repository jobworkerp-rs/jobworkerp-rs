use crate::workflow::execute::checkpoint::CheckPointContext;
use crate::workflow::execute::task::ExecutionId;
use crate::workflow::{definition::WorkflowLoader, execute::workflow::WorkflowExecutor};
use anyhow::Result;
use app::module::AppModule;
use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::{pin_mut, StreamExt};
use infra_utils::infra::net::reqwest::ReqwestClient;
use infra_utils::infra::trace::Tracing;
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_base::APP_NAME;
use jobworkerp_runner::jobworkerp::runner::workflow_result::WorkflowStatus;
use jobworkerp_runner::jobworkerp::runner::{Empty, InlineWorkflowArgs, WorkflowResult};
use jobworkerp_runner::runner::workflow::InlineWorkflowRunnerSpec;
use jobworkerp_runner::runner::{RunnerSpec, RunnerTrait};
use opentelemetry::trace::TraceContextExt;
use prost::Message;
use proto::jobworkerp::data::StreamingOutputType;
use proto::jobworkerp::data::{ResultOutputItem, RunnerType};
use schemars::JsonSchema;
use std::collections::HashMap;
use std::sync::Arc;

use crate::llm::cancellation_helper::CancellationHelper;

#[derive(Debug, Clone)]
pub struct InlineWorkflowRunner {
    app_wrapper_module: Arc<crate::modules::AppWrapperModule>,
    app_module: Arc<AppModule>,
    http_client: ReqwestClient,
    workflow_executor: Option<Arc<WorkflowExecutor>>,
    cancellation_helper: CancellationHelper,
}
impl Tracing for InlineWorkflowRunner {}
impl InlineWorkflowRunner {
    // for workflow file reqwest

    pub fn new(
        app_wrapper_module: Arc<crate::modules::AppWrapperModule>,
        app_module: Arc<AppModule>,
    ) -> Result<Self> {
        let workflow_config = app_wrapper_module.config_module.workflow_config.clone();
        // TODO connection pool and use as global client (move to app module)
        let http_client = ReqwestClient::new(
            Some(workflow_config.http_user_agent.as_str()),
            Some(workflow_config.http_timeout_sec),
            Some(workflow_config.http_timeout_sec),
            Some(2),
        )?;

        Ok(InlineWorkflowRunner {
            app_wrapper_module,
            app_module,
            http_client,
            workflow_executor: None,
            cancellation_helper: CancellationHelper::new(),
        })
    }

    /// Set a cancellation token for this runner instance
    /// This allows external control over cancellation behavior (for test)
    #[allow(dead_code)]
    pub(crate) fn set_cancellation_token(&mut self, token: tokio_util::sync::CancellationToken) {
        self.cancellation_helper.set_cancellation_token(token);
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
    async fn run(
        &mut self,
        args: &[u8],
        metadata: HashMap<String, String>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        let result = async {
            // let span = Self::tracing_span_from_metadata(&metadata, APP_NAME, "inline_workflow.run");
            // let _ = span.enter();
            // let cx = span.context();
            let span = Self::otel_span_from_metadata(&metadata, APP_NAME, "inline_workflow.run");
            let cx = opentelemetry::Context::current_with_span(span);
            let arg = InlineWorkflowArgs::decode(args)?;
            tracing::debug!("workflow args: {:#?}", arg);
            let execution_id = ExecutionId::new_opt(arg.execution_id.clone());
            if let Some(token) = self.cancellation_helper.get_token() {
                if token.is_cancelled() {
                    return Err(anyhow::anyhow!(
                        "canceled by user: {}, {:?}",
                        RunnerType::InlineWorkflow.as_str_name(),
                        arg
                    ));
                }
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
                        serde_json::from_str(c)
                            .unwrap_or(serde_json::Value::Object(Default::default()))
                        // ignore error
                    })
                    .unwrap_or_else(|| serde_json::Value::Object(Default::default())),
            );
            tracing::debug!(
                "workflow context_json: {}",
                serde_json::to_string_pretty(&context_json).unwrap_or_default()
            );
            let workflow_config = self
                .app_wrapper_module
                .config_module
                .workflow_config
                .clone();
            let http_client = ReqwestClient::new(
                Some(workflow_config.http_user_agent.as_str()),
                Some(workflow_config.http_timeout_sec),
                Some(workflow_config.http_timeout_sec),
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
            let chpoint = if let Some(ch) = arg.from_checkpoint.as_ref() {
                Some(CheckPointContext::from_inline(ch)?)
            } else {
                None
            };
            let executor = WorkflowExecutor::init(
                self.app_wrapper_module.clone(),
                self.app_module.clone(),
                http_client,
                Arc::new(workflow),
                Arc::new(input_json),
                execution_id.clone(),
                context_json.clone(),
                Arc::new(metadata.clone()),
                chpoint,
            )
            .await?;
            // Get the stjream of workflow context updates
            let workflow_stream = executor.execute_workflow(Arc::new(cx));
            pin_mut!(workflow_stream);

            // Store the final workflow context
            let mut final_context = None;

            // Process the stream of workflow context results
            while let Some(result) = workflow_stream.next().await {
                match result {
                    Ok(context) => {
                        final_context = Some(context);
                        if let Some(token) = self.cancellation_helper.get_token() {
                            if token.is_cancelled() {
                                return Err(JobWorkerError::RuntimeError(format!(
                                    "canceled by user: {}, {:?}",
                                    RunnerType::InlineWorkflow.as_str_name(),
                                    arg
                                ))
                                .into());
                            }
                        }
                    }
                    Err(e) => {
                        return Err(JobWorkerError::RuntimeError(format!(
                            "Failed to execute workflow: {e:?}"
                        ))
                        .into());
                    }
                }
            }

            // Return the final workflow context or an error if none was received
            let res =
                final_context.ok_or_else(|| anyhow::anyhow!("No workflow context was returned"))?;

            tracing::info!("Workflow result: {}", res.output_string());

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

        let arg = InlineWorkflowArgs::decode(args)?;
        tracing::debug!("workflow args: {:#?}", arg);
        if let Some(token) = self.cancellation_helper.get_token() {
            if token.is_cancelled() {
                return Err(anyhow::anyhow!(
                    "canceled by user: {}, {:?}",
                    RunnerType::InlineWorkflow.as_str_name(),
                    arg
                ));
            }
        }
        let execution_id = ExecutionId::new_opt(arg.execution_id);
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

        let chpoint = if let Some(ch) = arg.from_checkpoint.as_ref() {
            Some(CheckPointContext::from_inline(ch)?)
        } else {
            None
        };

        let metadata_arc = Arc::new(metadata.clone());
        let executor = Arc::new(
            WorkflowExecutor::init(
                self.app_wrapper_module.clone(),
                app_module,
                http_client,
                Arc::new(workflow),
                Arc::new(input_json),
                execution_id.clone(),
                context_json.clone(),
                metadata_arc.clone(),
                chpoint,
            )
            .await
            .map_err(|e| {
                tracing::error!("Failed to initialize WorkflowExecutor: {:?}", e);
                JobWorkerError::RuntimeError(format!(
                    "Failed to initialize WorkflowExecutor: {e:?}"
                ))
            })?,
        );

        let workflow_stream = executor.execute_workflow(Arc::new(cx));
        self.workflow_executor = Some(executor.clone());

        let output_stream = workflow_stream
            .then(|result| async move {
                match result {
                    Ok(context) => {
                        // Create a WorkflowResult from the context
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
                        tracing::error!("Error in workflow execution: {:?}", &e);
                        let workflow_result = WorkflowResult {
                            id: "error".to_string(),
                            output: "".to_string(),
                            position: e.instance.clone().unwrap_or_default(),
                            status: WorkflowStatus::Faulted as i32,
                            error_message: Some(format!("Failed to execute workflow: {e:?}")),
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
            .chain(futures::stream::once(async move {
                // Add an End item at the end of the stream
                tracing::debug!(
                    "Workflow execution completed, sending end of stream: {metadata:#?}"
                );
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
        self.cancellation_helper.cancel();
    }
}
