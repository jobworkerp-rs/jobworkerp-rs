use anyhow::{anyhow, Context, Result};
use app::module::AppModule;
use async_trait::async_trait;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_runner::jobworkerp::runner::create_workflow_args::{WorkerOptions, WorkflowSource};
use jobworkerp_runner::jobworkerp::runner::create_workflow_result::WorkerId as CreateWorkflowWorkerId;
use jobworkerp_runner::jobworkerp::runner::{CreateWorkflowArgs, CreateWorkflowResult};
use jobworkerp_runner::runner::cancellation_helper::{
    CancelMonitoringHelper, UseCancelMonitoringHelper,
};
use jobworkerp_runner::runner::create_workflow::CreateWorkflowRunnerSpec;
use jobworkerp_runner::runner::{RunnerSpec, RunnerTrait};
use jobworkerp_runner::validation::workflow::{StandardWorkflowValidator, WorkflowValidator};
use prost::Message;
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;
use tracing;

pub struct CreateWorkflowRunnerImpl {
    pub app: Arc<AppModule>,
    _validator: Box<dyn WorkflowValidator + Send + Sync>, // TODO use validator for workflow json
    cancel_helper: Option<CancelMonitoringHelper>,
}

impl CreateWorkflowRunnerImpl {
    pub fn new(app_module: Arc<AppModule>) -> Result<Self> {
        Ok(Self {
            app: app_module,
            _validator: Box::new(StandardWorkflowValidator),
            cancel_helper: None, // No cancellation monitoring by default
        })
    }

    pub fn new_with_cancel_monitoring(
        app_module: Arc<AppModule>,
        cancel_helper: CancelMonitoringHelper,
    ) -> Self {
        Self {
            app: app_module,
            _validator: Box::new(StandardWorkflowValidator),
            cancel_helper: Some(cancel_helper),
        }
    }

    pub fn new_with_validator(
        app_module: Arc<AppModule>,
        _validator: Box<dyn WorkflowValidator + Send + Sync>,
    ) -> Result<Self> {
        Ok(Self {
            app: app_module,
            _validator,
            cancel_helper: None,
        })
    }

    async fn validate_and_parse_args(
        &self,
        args: &CreateWorkflowArgs,
    ) -> Result<(serde_json::Value, String)> {
        // 1. Validate worker name
        if args.name.trim().is_empty() {
            return Err(anyhow!("Worker name cannot be empty"));
        }

        // 2. Validate workflow_source
        let workflow_json = match &args.workflow_source {
            Some(workflow_source) => match workflow_source {
                WorkflowSource::WorkflowData(data) => serde_json::from_str(data)
                    .map_err(|e| anyhow!("Invalid workflow JSON: {}", e))?,
                WorkflowSource::WorkflowUrl(url) => self.load_workflow_from_url(url).await?,
            },
            None => {
                return Err(anyhow!("Workflow source is required"));
            }
        };

        // TODO Validate workflow structure (using common Validator)
        // self.validator.validate_workflow(&workflow_json).await?;
        // tracing::debug!(
        //     "Validated workflow JSON: {}",
        //     serde_json::to_string_pretty(&workflow_json)?
        // );

        Ok((workflow_json, args.name.clone()))
    }

    async fn load_workflow_from_url(&self, url: &str) -> Result<serde_json::Value> {
        tracing::info!("Loading workflow from URL: {}", url);

        // Use WorkflowLoader from AppModule (handles URL validation, HTTP fetching, and JSON/YAML parsing)
        let workflow_schema = self
            .app
            .workflow_loader
            .load_workflow(Some(url), None, true)
            .await
            .context("Failed to load workflow from URL")?;

        // Convert WorkflowSchema to serde_json::Value for compatibility with existing code
        let workflow_json = serde_json::to_value(&workflow_schema)
            .context("Failed to serialize WorkflowSchema to JSON")?;

        tracing::info!("Successfully loaded and parsed workflow from URL: {}", url);
        Ok(workflow_json)
    }

    async fn create_worker_with_name(
        &self,
        workflow_def: serde_json::Value,
        worker_name: String,
        worker_options: Option<WorkerOptions>,
    ) -> Result<CreateWorkflowWorkerId> {
        tracing::debug!("Creating worker with name: {}", worker_name);

        // Build WorkerData (correct method according to design document)
        let worker_data =
            self.build_worker_data(worker_name.clone(), workflow_def, worker_options)?;

        // Create worker using WorkerApp API
        let worker = self
            .app
            .worker_app
            .create(&worker_data)
            .await
            .context("Failed to create worker")?;

        tracing::info!("CREATE_WORKFLOW Worker created: {:?}", worker);

        // Convert WorkerId to CreateWorkflowWorkerId and return
        let create_workflow_worker_id = CreateWorkflowWorkerId {
            value: worker.value,
        };
        Ok(create_workflow_worker_id)
    }

    fn build_worker_data(
        &self,
        worker_name: String,
        workflow_def: serde_json::Value,
        worker_options: Option<WorkerOptions>,
    ) -> Result<proto::jobworkerp::data::WorkerData> {
        use prost::Message;
        use proto::jobworkerp::data::{ResponseType, RunnerId, RunnerType};

        // Extract description from workflow definition
        let document = workflow_def.get("document");
        let workflow_description = document
            .and_then(|d| d.get("summary"))
            .and_then(|d| d.as_str())
            .unwrap_or("")
            .to_string();

        // Set default options
        let opts = worker_options.unwrap_or_default();

        // Build RunnerId for REUSABLE_WORKFLOW (using 65532 according to design document)
        let runner_id = RunnerId {
            value: RunnerType::ReusableWorkflow as i64, // Use i64 type
        };

        // Serialize runner_settings in proto format (REUSABLE_WORKFLOW settings format)
        let runner_settings = {
            use jobworkerp_runner::jobworkerp::runner::ReusableWorkflowRunnerSettings;

            let proto_settings = ReusableWorkflowRunnerSettings {
                json_data: workflow_def.to_string(),
            };

            let mut buf = Vec::new();
            proto_settings
                .encode(&mut buf)
                .context("Failed to encode ReusableWorkflowRunnerSettings")?;
            buf
        };

        // Set retry_policy (default settings same as reference implementation)
        let retry_policy = opts
            .retry_policy
            .map(|rp| proto::jobworkerp::data::RetryPolicy {
                r#type: rp.r#type,
                interval: rp.interval,
                max_interval: rp.max_interval,
                max_retry: rp.max_retry,
                basis: rp.basis,
            });

        Ok(proto::jobworkerp::data::WorkerData {
            name: worker_name,
            description: workflow_description.to_string(),
            runner_id: Some(runner_id),
            runner_settings,
            channel: opts.channel,
            response_type: opts.response_type.unwrap_or(ResponseType::Direct as i32),
            broadcast_results: opts.broadcast_results,
            store_success: opts.store_success,
            store_failure: opts.store_failure,
            use_static: opts.use_static,
            queue_type: opts.queue_type, // Use QueueType directly (same sequential number)
            retry_policy,
            periodic_interval: 0,
        })
    }
}

impl CreateWorkflowRunnerSpec for CreateWorkflowRunnerImpl {}

impl RunnerSpec for CreateWorkflowRunnerImpl {
    fn name(&self) -> String {
        CreateWorkflowRunnerSpec::name(self)
    }

    fn runner_settings_proto(&self) -> String {
        CreateWorkflowRunnerSpec::runner_settings_proto(self)
    }

    fn job_args_proto(&self) -> String {
        CreateWorkflowRunnerSpec::job_args_proto(self)
    }

    fn result_output_proto(&self) -> Option<String> {
        CreateWorkflowRunnerSpec::result_output_proto(self)
    }

    fn output_type(&self) -> proto::jobworkerp::data::StreamingOutputType {
        CreateWorkflowRunnerSpec::output_type(self)
    }

    fn settings_schema(&self) -> String {
        CreateWorkflowRunnerSpec::settings_schema(self)
    }

    fn arguments_schema(&self) -> String {
        CreateWorkflowRunnerSpec::arguments_schema(self)
    }

    fn output_schema(&self) -> Option<String> {
        CreateWorkflowRunnerSpec::output_schema(self)
    }
}

#[async_trait]
impl RunnerTrait for CreateWorkflowRunnerImpl {
    async fn load(&mut self, _settings: Vec<u8>) -> Result<()> {
        // No need to load settings (CREATE_WORKFLOW has no settings)
        Ok(())
    }

    async fn run(
        &mut self,
        args: &[u8],
        metadata: HashMap<String, String>,
        _using: Option<&str>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        tracing::info!("Starting CREATE_WORKFLOW execution");

        let result: Result<Vec<u8>> = async {
            // 1. Deserialize arguments (actual proto deserialization)
            let create_args = if args.is_empty() {
                return Err(anyhow!("CREATE_WORKFLOW requires arguments"));
            } else {
                CreateWorkflowArgs::decode(&mut Cursor::new(args))
                    .context("Failed to deserialize CreateWorkflowArgs from proto")?
            };

            tracing::info!("CREATE_WORKFLOW request for worker: {}", create_args.name);

            // 2. Validate and parse arguments
            let (workflow_def, worker_name) = self.validate_and_parse_args(&create_args).await?;
            // tracing::debug!("Validated workflow definition for worker: {}", worker_name);

            if self.cancel_helper.is_some()
                && self
                    .cancel_helper
                    .as_ref()
                    .unwrap()
                    .get_cancellation_token()
                    .await
                    .is_cancelled()
            {
                tracing::warn!("CREATE_WORKFLOW execution cancelled before creating worker");
                return Err(anyhow!("CREATE_WORKFLOW execution cancelled"));
            }
            // 3. Create actual worker (using AppModule)
            let worker_id = self
                .create_worker_with_name(
                    workflow_def,
                    worker_name.clone(),
                    create_args.worker_options,
                )
                .await?;

            // 4. Return result
            let result = CreateWorkflowResult {
                worker_id: Some(worker_id),
            };

            tracing::info!(
                "CREATE_WORKFLOW completed successfully: worker_name={}, worker_id={}",
                worker_name,
                worker_id.value
            );

            // 5. Serialize result (actual proto serialization)
            ProstMessageCodec::serialize_message(&result)
                .context("Failed to serialize CreateWorkflowResult to proto")
        }
        .await;

        let output = match result {
            Ok(serialized_result) => Ok(serialized_result),
            Err(e) => {
                tracing::error!("CREATE_WORKFLOW execution failed: {}", e);
                Err(e)
            }
        };

        (output, metadata)
    }

    async fn run_stream(
        &mut self,
        _args: &[u8],
        _metadata: HashMap<String, String>,
        _using: Option<&str>,
    ) -> Result<
        std::pin::Pin<
            Box<
                dyn futures::Stream<Item = proto::jobworkerp::data::ResultOutputItem>
                    + Send
                    + 'static,
            >,
        >,
    > {
        // CREATE_WORKFLOW does not support streaming
        Err(anyhow!("CREATE_WORKFLOW does not support streaming"))
    }
}
#[async_trait]
impl jobworkerp_runner::runner::cancellation::CancelMonitoring for CreateWorkflowRunnerImpl {
    /// Initialize cancellation monitoring for specific job
    async fn setup_cancellation_monitoring(
        &mut self,
        job_id: proto::jobworkerp::data::JobId,
        _job_data: &proto::jobworkerp::data::JobData,
    ) -> anyhow::Result<Option<proto::jobworkerp::data::JobResult>> {
        tracing::debug!(
            "Setting up cancellation monitoring for CreateWorkflowRunner job {}",
            job_id.value
        );

        Ok(None) // Continue with normal execution
    }

    /// Cleanup cancellation monitoring
    async fn cleanup_cancellation_monitoring(&mut self) -> anyhow::Result<()> {
        tracing::trace!("Cleaning up cancellation monitoring for CreateWorkflowRunner");

        // Clear the cancellation helper
        if let Some(helper) = &mut self.cancel_helper {
            helper.cleanup_monitoring_impl().await?;
        }

        Ok(())
    }

    /// Signals cancellation token for CreateWorkflowRunner
    async fn request_cancellation(&mut self) -> anyhow::Result<()> {
        if let Some(helper) = &self.cancel_helper {
            let token = helper.get_cancellation_token().await;
            if !token.is_cancelled() {
                token.cancel();
                tracing::info!("CreateWorkflowRunner: cancellation token signaled");
            }
        } else {
            tracing::warn!("CreateWorkflowRunner: no cancellation helper available");
        }
        Ok(())
    }

    async fn reset_for_pooling(&mut self) -> anyhow::Result<()> {
        // CreateWorkflowRunner typically completes quickly, so always cleanup
        if let Some(helper) = &mut self.cancel_helper {
            helper.reset_for_pooling_impl().await?;
        } else {
            self.cleanup_cancellation_monitoring().await?;
        }

        tracing::debug!("CreateWorkflowRunner reset for pooling");
        Ok(())
    }
}

impl UseCancelMonitoringHelper for CreateWorkflowRunnerImpl {
    fn cancel_monitoring_helper(&self) -> Option<&CancelMonitoringHelper> {
        self.cancel_helper.as_ref()
    }
}
