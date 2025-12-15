//! Unified Workflow Runner implementation for app-wrapper
//!
//! This module provides a unified Workflow runner that supports 'run' and 'create' methods
//! via the `using` parameter.
//!
//! # Settings
//! Uses `WorkflowRunnerSettings` with optional `workflow_source`:
//! - If workflow_source is specified in settings, 'run' method can use it without args override
//! - If workflow_source is not in settings, 'run' method args must include workflow_source
//! - 'create' method always uses workflow_source from args (ignores settings)

use crate::modules::AppWrapperModule;
use crate::workflow::create_workflow::CreateWorkflowRunnerImpl;
use crate::workflow::definition::workflow::WorkflowSchema;
use crate::workflow::execute::checkpoint::CheckPointContext;
use crate::workflow::execute::task::ExecutionId;
use crate::workflow::execute::workflow::WorkflowExecutor;
use anyhow::{anyhow, Result};
use app::module::AppModule;
use async_trait::async_trait;
use command_utils::trace::Tracing;
use futures::stream::BoxStream;
use futures::{pin_mut, StreamExt};
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_base::APP_NAME;
use jobworkerp_runner::jobworkerp::runner::workflow_result::WorkflowStatus;
use jobworkerp_runner::jobworkerp::runner::workflow_run_args::WorkflowSource as ArgsWorkflowSource;
use jobworkerp_runner::jobworkerp::runner::workflow_runner_settings::WorkflowSource as SettingsWorkflowSource;
use jobworkerp_runner::jobworkerp::runner::{
    WorkflowResult, WorkflowRunArgs, WorkflowRunnerSettings,
};
use jobworkerp_runner::runner::cancellation::CancelMonitoring;
use jobworkerp_runner::runner::cancellation_helper::{
    CancelMonitoringHelper, UseCancelMonitoringHelper,
};
use jobworkerp_runner::runner::workflow_unified::{
    WorkflowUnifiedRunnerSpecImpl, METHOD_CREATE, METHOD_RUN,
};
use jobworkerp_runner::runner::{MethodJsonSchema, RunnerSpec, RunnerTrait};
use opentelemetry::trace::TraceContextExt;
use prost::Message;
use proto::jobworkerp::data::{JobData, JobId, JobResult, ResultOutputItem, RunnerType};
use std::collections::HashMap;
use std::sync::Arc;

/// Unified Workflow Runner implementation that supports 'run' and 'create' methods
pub struct WorkflowUnifiedRunnerImpl {
    app_wrapper_module: Arc<AppWrapperModule>,
    app_module: Arc<AppModule>,
    /// Workflow from settings (optional, used by 'run' method if args don't specify workflow_source)
    settings_workflow: Option<Arc<WorkflowSchema>>,
    /// Create runner for 'create' method
    create_runner: CreateWorkflowRunnerImpl,
    spec: WorkflowUnifiedRunnerSpecImpl,
    cancel_helper: Option<CancelMonitoringHelper>,
}

impl WorkflowUnifiedRunnerImpl {
    pub fn new(
        app_wrapper_module: Arc<AppWrapperModule>,
        app_module: Arc<AppModule>,
    ) -> Result<Self> {
        Ok(Self {
            app_wrapper_module,
            app_module: app_module.clone(),
            settings_workflow: None,
            create_runner: CreateWorkflowRunnerImpl::new(app_module)?,
            spec: WorkflowUnifiedRunnerSpecImpl::new(),
            cancel_helper: None,
        })
    }

    pub fn new_with_cancel_monitoring(
        app_wrapper_module: Arc<AppWrapperModule>,
        app_module: Arc<AppModule>,
        cancel_helper: CancelMonitoringHelper,
    ) -> Result<Self> {
        Ok(Self {
            app_wrapper_module,
            app_module: app_module.clone(),
            settings_workflow: None,
            create_runner: CreateWorkflowRunnerImpl::new_with_cancel_monitoring(
                app_module,
                cancel_helper.clone(),
            ),
            spec: WorkflowUnifiedRunnerSpecImpl::new(),
            cancel_helper: Some(cancel_helper),
        })
    }

    /// Parse workflow from JSON or YAML string
    fn parse_workflow(data: &str) -> Result<WorkflowSchema> {
        serde_json::from_str(data).or_else(|_| {
            serde_yaml::from_str(data)
                .map_err(|e| anyhow!("Failed to parse workflow as JSON or YAML: {}", e))
        })
    }

    /// Resolve workflow: args workflow_source takes precedence over settings
    fn resolve_workflow(
        &self,
        args_source: Option<&ArgsWorkflowSource>,
    ) -> Result<Arc<WorkflowSchema>> {
        // Args workflow_source takes precedence
        if let Some(source) = args_source {
            let data = match source {
                ArgsWorkflowSource::WorkflowUrl(url) => {
                    // TODO: Fetch from URL - for now just read as file path
                    std::fs::read_to_string(url)
                        .map_err(|e| anyhow!("Failed to read workflow from {}: {}", url, e))?
                }
                ArgsWorkflowSource::WorkflowData(data) => data.clone(),
            };
            return Ok(Arc::new(Self::parse_workflow(&data)?));
        }

        // Fall back to settings workflow
        self.settings_workflow
            .clone()
            .ok_or_else(|| anyhow!("No workflow_source specified in settings or args"))
    }

    /// Execute workflow run (implementation for 'run' method)
    async fn execute_run(
        &self,
        args: &WorkflowRunArgs,
        metadata: HashMap<String, String>,
    ) -> Result<Vec<u8>> {
        let span = Self::otel_span_from_metadata(&metadata, APP_NAME, "workflow.run");
        let cx = opentelemetry::Context::current_with_span(span);
        let execution_id = ExecutionId::new_opt(args.execution_id.clone());

        // Check for cancellation
        if let Some(helper) = &self.cancel_helper {
            let token = helper.get_cancellation_token().await;
            if token.is_cancelled() {
                return Err(anyhow!(
                    "canceled by user: {}, {:?}",
                    RunnerType::Workflow.as_str_name(),
                    args
                ));
            }
        }

        // Resolve workflow (args takes precedence over settings)
        let workflow = self.resolve_workflow(args.workflow_source.as_ref())?;
        tracing::debug!("Workflow resolved: {:#?}", &workflow);

        let input_json = serde_json::from_str(&args.input)
            .unwrap_or_else(|_| serde_json::Value::String(args.input.clone()));
        let context_json = Arc::new(
            args.workflow_context
                .as_deref()
                .map(serde_json::from_str)
                .unwrap_or_else(|| Ok(serde_json::Value::Object(Default::default())))?,
        );
        let chpoint = if let Some(ch) = args.from_checkpoint.as_ref() {
            Some(CheckPointContext::from_workflow_run(ch)?)
        } else {
            None
        };

        let executor = WorkflowExecutor::init(
            self.app_wrapper_module.clone(),
            self.app_module.clone(),
            workflow,
            Arc::new(input_json),
            execution_id,
            context_json,
            Arc::new(metadata),
            chpoint,
        )
        .await?;

        let workflow_stream = executor.execute_workflow(Arc::new(cx));
        pin_mut!(workflow_stream);

        let mut final_context = None;
        while let Some(result) = workflow_stream.next().await {
            match result {
                Ok(context) => {
                    final_context = Some(context);
                }
                Err(e) => {
                    return Err(JobWorkerError::RuntimeError(format!(
                        "Failed to execute workflow: {e:?}"
                    ))
                    .into());
                }
            }
        }

        let res = final_context.ok_or_else(|| anyhow!("No workflow context was returned"))?;
        tracing::info!("Workflow result: {}", res.output_string());

        let r = WorkflowResult {
            id: res.id.to_string(),
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
        Ok(r.encode_to_vec())
    }

    /// Execute workflow run as stream (implementation for 'run' method streaming)
    async fn execute_run_stream(
        &self,
        args: &WorkflowRunArgs,
        metadata: HashMap<String, String>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        let cx = Self::create_context(&metadata);
        let metadata_arc = Arc::new(metadata.clone());
        let execution_id = ExecutionId::new_opt(args.execution_id.clone());

        // Check for cancellation
        if let Some(helper) = &self.cancel_helper {
            let token = helper.get_cancellation_token().await;
            if token.is_cancelled() {
                return Err(anyhow!(
                    "canceled by user: {}, {:?}",
                    RunnerType::Workflow.as_str_name(),
                    args
                ));
            }
        }

        // Resolve workflow (args takes precedence over settings)
        let workflow = self.resolve_workflow(args.workflow_source.as_ref())?;

        let input_json = serde_json::from_str(&args.input)
            .unwrap_or_else(|_| serde_json::Value::String(args.input.clone()));
        let context_json = Arc::new(
            args.workflow_context
                .as_deref()
                .map(|c| {
                    serde_json::from_str(c).unwrap_or(serde_json::Value::Object(Default::default()))
                })
                .unwrap_or_else(|| serde_json::Value::Object(Default::default())),
        );
        let chpoint = if let Some(ch) = args.from_checkpoint.as_ref() {
            Some(CheckPointContext::from_workflow_run(ch)?)
        } else {
            None
        };

        let executor = Arc::new(
            WorkflowExecutor::init(
                self.app_wrapper_module.clone(),
                self.app_module.clone(),
                workflow,
                Arc::new(input_json),
                execution_id,
                context_json,
                metadata_arc.clone(),
                chpoint,
            )
            .await?,
        );

        let workflow_stream = executor.execute_workflow(Arc::new(cx));
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
                            error_message: if context.status == WorkflowStatus::Completed.into() {
                                None
                            } else {
                                context.output.as_ref().map(|o| o.to_string())
                            },
                        };
                        ResultOutputItem {
                            item: Some(proto::jobworkerp::data::result_output_item::Item::Data(
                                workflow_result.encode_to_vec(),
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
                            error_message: Some(format!("Failed to execute workflow: {e}")),
                        };
                        ResultOutputItem {
                            item: Some(proto::jobworkerp::data::result_output_item::Item::Data(
                                workflow_result.encode_to_vec(),
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
}

impl Tracing for WorkflowUnifiedRunnerImpl {}

impl UseCancelMonitoringHelper for WorkflowUnifiedRunnerImpl {
    fn cancel_monitoring_helper(&self) -> Option<&CancelMonitoringHelper> {
        self.cancel_helper.as_ref()
    }
}

impl RunnerSpec for WorkflowUnifiedRunnerImpl {
    fn name(&self) -> String {
        self.spec.name()
    }

    fn runner_settings_proto(&self) -> String {
        self.spec.runner_settings_proto()
    }

    fn method_proto_map(
        &self,
    ) -> std::collections::HashMap<String, proto::jobworkerp::data::MethodSchema> {
        self.spec.method_proto_map()
    }

    fn method_json_schema_map(&self) -> HashMap<String, MethodJsonSchema> {
        self.spec.method_json_schema_map()
    }

    fn settings_schema(&self) -> String {
        self.spec.settings_schema()
    }

    fn collect_stream(
        &self,
        stream: BoxStream<'static, ResultOutputItem>,
        using: Option<&str>,
    ) -> jobworkerp_runner::runner::CollectStreamFuture {
        self.spec.collect_stream(stream, using)
    }
}

#[async_trait]
impl RunnerTrait for WorkflowUnifiedRunnerImpl {
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        // Parse WorkflowRunnerSettings - workflow_source is optional
        if !settings.is_empty() {
            let parsed_settings =
                ProstMessageCodec::deserialize_message::<WorkflowRunnerSettings>(&settings)?;
            // If workflow_source is specified in settings, parse and store it
            if let Some(source) = parsed_settings.workflow_source {
                let data = match source {
                    SettingsWorkflowSource::WorkflowUrl(url) => {
                        // TODO: Fetch from URL - for now just read as file path
                        std::fs::read_to_string(&url)
                            .map_err(|e| anyhow!("Failed to read workflow from {}: {}", url, e))?
                    }
                    SettingsWorkflowSource::WorkflowData(data) => data,
                };
                let workflow = Self::parse_workflow(&data)?;
                tracing::debug!("Workflow loaded from settings: {:#?}", &workflow);
                self.settings_workflow = Some(Arc::new(workflow));
            }
        }
        // create_runner.load() is a no-op but call it for consistency
        self.create_runner.load(vec![]).await?;
        Ok(())
    }

    async fn run(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
        using: Option<&str>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        match WorkflowUnifiedRunnerSpecImpl::resolve_method(using) {
            Ok(METHOD_RUN) => {
                let args = match ProstMessageCodec::deserialize_message::<WorkflowRunArgs>(arg) {
                    Ok(args) => args,
                    Err(e) => return (Err(e), metadata),
                };
                let result = self.execute_run(&args, metadata.clone()).await;
                (result, metadata)
            }
            Ok(METHOD_CREATE) => self.create_runner.run(arg, metadata, None).await,
            Ok(_) => (
                Err(anyhow!("Internal error: unknown method after validation")),
                metadata,
            ),
            Err(e) => (Err(e), metadata),
        }
    }

    async fn run_stream(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
        using: Option<&str>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        match WorkflowUnifiedRunnerSpecImpl::resolve_method(using) {
            Ok(METHOD_RUN) => {
                let args = ProstMessageCodec::deserialize_message::<WorkflowRunArgs>(arg)?;
                self.execute_run_stream(&args, metadata).await
            }
            Ok(METHOD_CREATE) => self.create_runner.run_stream(arg, metadata, None).await,
            Ok(_) => Err(anyhow!("Internal error: unknown method after validation")),
            Err(e) => Err(e),
        }
    }
}

#[async_trait]
impl CancelMonitoring for WorkflowUnifiedRunnerImpl {
    async fn setup_cancellation_monitoring(
        &mut self,
        job_id: JobId,
        job_data: &JobData,
    ) -> Result<Option<JobResult>> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.setup_monitoring_impl(job_id, job_data).await
        } else {
            Ok(None)
        }
    }

    async fn cleanup_cancellation_monitoring(&mut self) -> Result<()> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.cleanup_monitoring_impl().await
        } else {
            Ok(())
        }
    }

    async fn request_cancellation(&mut self) -> Result<()> {
        if let Some(helper) = &self.cancel_helper {
            let token = helper.get_cancellation_token().await;
            if !token.is_cancelled() {
                token.cancel();
            }
        }
        Ok(())
    }

    async fn reset_for_pooling(&mut self) -> Result<()> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.reset_for_pooling_impl().await
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jobworkerp_runner::runner::RunnerSpec;

    #[test]
    fn test_resolve_method() {
        assert!(WorkflowUnifiedRunnerSpecImpl::resolve_method(Some("run")).is_ok());
        assert!(WorkflowUnifiedRunnerSpecImpl::resolve_method(Some("create")).is_ok());
        // Default to "run" when None
        assert!(WorkflowUnifiedRunnerSpecImpl::resolve_method(None).is_ok());
        assert_eq!(
            WorkflowUnifiedRunnerSpecImpl::resolve_method(None).unwrap(),
            "run"
        );
        assert!(WorkflowUnifiedRunnerSpecImpl::resolve_method(Some("unknown")).is_err());
    }

    #[test]
    fn test_runner_spec_name() {
        let spec = WorkflowUnifiedRunnerSpecImpl::new();
        assert_eq!(spec.name(), "WORKFLOW");
    }

    #[test]
    fn test_method_proto_map_has_both_methods() {
        let spec = WorkflowUnifiedRunnerSpecImpl::new();
        let methods = spec.method_proto_map();

        assert!(methods.contains_key("run"));
        assert!(methods.contains_key("create"));
        assert_eq!(methods.len(), 2);

        // Verify schemas are not empty
        let run = methods.get("run").unwrap();
        assert!(!run.args_proto.is_empty());
        assert!(!run.result_proto.is_empty());

        let create = methods.get("create").unwrap();
        assert!(!create.args_proto.is_empty());
        assert!(!create.result_proto.is_empty());
    }

    #[test]
    fn test_method_json_schema_map_has_both_methods() {
        let spec = WorkflowUnifiedRunnerSpecImpl::new();
        let schemas = spec.method_json_schema_map();

        assert!(schemas.contains_key("run"));
        assert!(schemas.contains_key("create"));
        assert_eq!(schemas.len(), 2);

        // Verify schemas are valid JSON
        for (method_name, schema) in &schemas {
            let parsed: Result<serde_json::Value, _> = serde_json::from_str(&schema.args_schema);
            assert!(
                parsed.is_ok(),
                "Invalid JSON in args_schema for method '{}'",
                method_name
            );
        }
    }
}
