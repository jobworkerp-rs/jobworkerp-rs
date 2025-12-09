use crate::workflow::execute::checkpoint::CheckPointContext;
use crate::workflow::execute::task::ExecutionId;
use crate::workflow::execute::workflow::WorkflowExecutor;
use anyhow::Result;
use app::module::AppModule;
use async_trait::async_trait;
use command_utils::trace::Tracing;
use futures::stream::BoxStream;
use futures::{pin_mut, StreamExt};
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_base::APP_NAME;
use jobworkerp_runner::jobworkerp::runner::workflow_result::WorkflowStatus;
use jobworkerp_runner::jobworkerp::runner::{InlineWorkflowArgs, WorkflowResult};
use jobworkerp_runner::runner::cancellation_helper::{
    CancelMonitoringHelper, UseCancelMonitoringHelper,
};
use jobworkerp_runner::runner::workflow::InlineWorkflowRunnerSpec;
use jobworkerp_runner::runner::{CollectStreamFuture, RunnerSpec, RunnerTrait};
use opentelemetry::trace::TraceContextExt;
use prost::Message;
use proto::jobworkerp::data::{ResultOutputItem, RunnerType};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct InlineWorkflowRunner {
    app_wrapper_module: Arc<crate::modules::AppWrapperModule>,
    app_module: Arc<AppModule>,
    workflow_executor: Option<Arc<WorkflowExecutor>>,
    cancel_helper: Option<CancelMonitoringHelper>,
}
impl Tracing for InlineWorkflowRunner {}
impl InlineWorkflowRunner {
    /// Constructor without cancellation monitoring (for backward compatibility)
    pub fn new(
        app_wrapper_module: Arc<crate::modules::AppWrapperModule>,
        app_module: Arc<AppModule>,
    ) -> Result<Self> {
        Ok(InlineWorkflowRunner {
            app_wrapper_module,
            app_module,
            workflow_executor: None,
            cancel_helper: None,
        })
    }

    /// Constructor with cancellation monitoring (DI integration version)
    pub fn new_with_cancel_monitoring(
        app_wrapper_module: Arc<crate::modules::AppWrapperModule>,
        app_module: Arc<AppModule>,
        cancel_helper: CancelMonitoringHelper,
    ) -> Result<Self> {
        Ok(InlineWorkflowRunner {
            app_wrapper_module,
            app_module,
            workflow_executor: None,
            cancel_helper: Some(cancel_helper),
        })
    }

    /// Set a cancellation helper for this runner instance
    /// This allows external control over cancellation behavior (for test)
    #[cfg(any(test, feature = "test-utils"))]
    #[allow(dead_code)]
    pub(crate) fn set_cancel_monitoring_helper(&mut self, helper: CancelMonitoringHelper) {
        self.cancel_helper = Some(helper);
    }
}

impl InlineWorkflowRunnerSpec for InlineWorkflowRunner {}

impl RunnerSpec for InlineWorkflowRunner {
    fn name(&self) -> String {
        InlineWorkflowRunnerSpec::name(self)
    }

    fn runner_settings_proto(&self) -> String {
        InlineWorkflowRunnerSpec::runner_settings_proto(self)
    }

    fn method_proto_map(
        &self,
    ) -> std::collections::HashMap<String, proto::jobworkerp::data::MethodSchema> {
        InlineWorkflowRunnerSpec::method_proto_map(self)
    }

    fn settings_schema(&self) -> String {
        InlineWorkflowRunnerSpec::settings_schema(self)
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
        _using: Option<&str>,
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
            if let Some(helper) = &self.cancel_helper {
                let token = helper.get_cancellation_token().await;
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
            let source = arg.workflow_source.as_ref().ok_or({
                tracing::error!("workflow_source is required in workflow args");
                anyhow::anyhow!("workflow_source is required in workflow args")
            })?;
            tracing::debug!("workflow source: {:?}", source);
            let workflow = self
                .app_module
                .workflow_loader
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
                Arc::new(workflow),
                Arc::new(input_json),
                execution_id.clone(),
                context_json.clone(),
                Arc::new(metadata.clone()),
                chpoint,
            )
            .await?;
            let workflow_stream = executor.execute_workflow(Arc::new(cx));
            pin_mut!(workflow_stream);

            // Store the final workflow context
            let mut final_context = None;

            // Process the stream of workflow context results
            while let Some(result) = workflow_stream.next().await {
                match result {
                    Ok(context) => {
                        final_context = Some(context);
                        if let Some(helper) = &self.cancel_helper {
                            let token = helper.get_cancellation_token().await;
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
        _using: Option<&str>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        let cx = Self::create_context(&metadata);

        let arg = InlineWorkflowArgs::decode(args)?;
        tracing::debug!("workflow args: {:#?}", arg);
        if let Some(helper) = &self.cancel_helper {
            let token = helper.get_cancellation_token().await;
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
            tracing::warn!("workflow_source is required in workflow args");
            JobWorkerError::InvalidParameter(
                "workflow_source is required in workflow args".to_string(),
            )
        })?;
        tracing::debug!("workflow source: {:?}", source);
        let app_module = self.app_module.clone();

        let workflow = app_module
            .workflow_loader
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

    /// Collect streaming workflow results into a single WorkflowResult
    ///
    /// Strategy:
    /// - Keeps only the last WorkflowResult (represents final workflow state)
    /// - Intermediate results are discarded
    fn collect_stream(&self, stream: BoxStream<'static, ResultOutputItem>) -> CollectStreamFuture {
        use proto::jobworkerp::data::result_output_item;

        Box::pin(async move {
            let mut final_result: Option<WorkflowResult> = None;
            let mut metadata = HashMap::new();
            let mut stream = stream;

            while let Some(item) = stream.next().await {
                match item.item {
                    Some(result_output_item::Item::Data(data)) => {
                        // Keep the last valid WorkflowResult (represents final state)
                        if let Ok(workflow_result) = WorkflowResult::decode(data.as_slice()) {
                            final_result = Some(workflow_result);
                        }
                    }
                    Some(result_output_item::Item::End(trailer)) => {
                        metadata = trailer.metadata;
                        break;
                    }
                    None => {}
                }
            }

            // Return the final workflow result
            let bytes = final_result.map(|r| r.encode_to_vec()).unwrap_or_default();
            Ok((bytes, metadata))
        })
    }
}

impl UseCancelMonitoringHelper for InlineWorkflowRunner {
    fn cancel_monitoring_helper(&self) -> Option<&CancelMonitoringHelper> {
        self.cancel_helper.as_ref()
    }
}

#[async_trait]
impl jobworkerp_runner::runner::cancellation::CancelMonitoring for InlineWorkflowRunner {
    async fn setup_cancellation_monitoring(
        &mut self,
        job_id: proto::jobworkerp::data::JobId,
        job_data: &proto::jobworkerp::data::JobData,
    ) -> anyhow::Result<Option<proto::jobworkerp::data::JobResult>> {
        // Clear helper availability check to avoid optional complexity
        if let Some(helper) = &mut self.cancel_helper {
            helper.setup_monitoring_impl(job_id, job_data).await
        } else {
            tracing::debug!(
                "No cancel monitoring configured for InlineWorkflow job {}",
                job_id.value
            );
            Ok(None)
        }
    }

    async fn cleanup_cancellation_monitoring(&mut self) -> anyhow::Result<()> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.cleanup_monitoring_impl().await
        } else {
            Ok(())
        }
    }

    /// Signals cancellation token for InlineWorkflowRunner
    async fn request_cancellation(&mut self) -> anyhow::Result<()> {
        if let Some(helper) = &self.cancel_helper {
            let token = helper.get_cancellation_token().await;
            if !token.is_cancelled() {
                token.cancel();
                tracing::info!("InlineWorkflowRunner: cancellation token signaled");
            }
        } else {
            tracing::warn!("InlineWorkflowRunner: no cancellation helper available");
        }
        Ok(())
    }

    async fn reset_for_pooling(&mut self) -> anyhow::Result<()> {
        // InlineWorkflowRunner typically completes quickly, so always cleanup
        if let Some(helper) = &mut self.cancel_helper {
            helper.reset_for_pooling_impl().await?;
        } else {
            self.cleanup_cancellation_monitoring().await?;
        }

        // InlineWorkflowRunner-specific state reset
        tracing::debug!("InlineWorkflowRunner reset for pooling");
        Ok(())
    }
}
