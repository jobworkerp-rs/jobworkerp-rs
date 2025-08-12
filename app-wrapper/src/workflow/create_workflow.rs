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
use net_utils::net::reqwest::ReqwestClient;
use prost::Message;
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;
use tracing;

pub struct CreateWorkflowRunnerImpl {
    pub app: Arc<AppModule>,
    _validator: Box<dyn WorkflowValidator + Send + Sync>, // TODO use validator for workflow json
    http_client: ReqwestClient,
    cancel_helper: Option<CancelMonitoringHelper>,
}

impl CreateWorkflowRunnerImpl {
    pub fn new(app_module: Arc<AppModule>) -> Result<Self> {
        let http_client = ReqwestClient::new(
            Some("jobworkerp-rs/create-workflow-runner"),
            Some(Duration::from_secs(30)),
            Some(Duration::from_secs(10)),
            Some(3), // 3回リトライ
        )?;

        Ok(Self {
            app: app_module,
            _validator: Box::new(StandardWorkflowValidator),
            http_client,
            cancel_helper: None, // No cancellation monitoring by default
        })
    }

    pub fn new_with_validator(
        app_module: Arc<AppModule>,
        _validator: Box<dyn WorkflowValidator + Send + Sync>,
    ) -> Result<Self> {
        let http_client = ReqwestClient::new(
            Some("jobworkerp-rs/create-workflow-runner"),
            Some(Duration::from_secs(30)),
            Some(Duration::from_secs(10)),
            Some(3), // 3回リトライ
        )?;

        Ok(Self {
            app: app_module,
            _validator,
            http_client,
            cancel_helper: None,
        })
    }

    pub fn new_with_http_client(
        app_module: Arc<AppModule>,
        _validator: Box<dyn WorkflowValidator + Send + Sync>,
        http_client: ReqwestClient,
    ) -> Self {
        Self {
            app: app_module,
            _validator,
            http_client,
            cancel_helper: None,
        }
    }

    async fn validate_and_parse_args(
        &self,
        args: &CreateWorkflowArgs,
    ) -> Result<(serde_json::Value, String)> {
        // 1. worker名の検証
        if args.name.trim().is_empty() {
            return Err(anyhow!("Worker name cannot be empty"));
        }

        // 2. workflow_sourceの検証
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

        // TODO workflow構造検証（共通Validatorを使用）
        // self.validator.validate_workflow(&workflow_json).await?;
        // tracing::debug!(
        //     "Validated workflow JSON: {}",
        //     serde_json::to_string_pretty(&workflow_json)?
        // );

        Ok((workflow_json, args.name.clone()))
    }

    async fn load_workflow_from_url(&self, url: &str) -> Result<serde_json::Value> {
        use url::Url;

        // URLの妥当性検証
        let parsed_url = Url::parse(url).map_err(|e| anyhow!("Invalid URL format: {}", e))?;

        tracing::info!("Loading workflow from URL: {}", url);

        // HTTP(S)スキームのみサポート
        if !matches!(parsed_url.scheme(), "http" | "https") {
            return Err(anyhow!(
                "Unsupported URL scheme: {}. Only http/https are supported",
                parsed_url.scheme()
            ));
        }

        // ReqwestClientを使用してワークフロー定義を取得
        let response = self
            .http_client
            .client()
            .get(parsed_url)
            .send()
            .await
            .map_err(|e| anyhow!("Failed to fetch workflow from URL: {}", e))?;

        // レスポンス状態の確認
        if !response.status().is_success() {
            return Err(anyhow!(
                "HTTP error while fetching workflow: {} {}",
                response.status().as_u16(),
                response.status().canonical_reason().unwrap_or("Unknown")
            ));
        }

        // レスポンス本文の取得
        let body = response
            .text()
            .await
            .map_err(|e| anyhow!("Failed to read response body: {}", e))?;

        tracing::debug!("Fetched workflow content: {} chars", body.len());

        // JSON/YAMLパース（YAMLも対応）
        let workflow_json = serde_json::from_str(&body).or_else(|json_err| {
            serde_yaml::from_str(&body).map_err(|yaml_err| {
                anyhow!(
                    "Failed to parse workflow content as JSON or YAML. JSON error: {}, YAML error: {}",
                    json_err, yaml_err
                )
            })
        })?;

        tracing::info!("Successfully loaded and parsed workflow from URL: {}", url);
        Ok(workflow_json)
    }

    async fn create_worker_with_app(
        &self,
        workflow_def: serde_json::Value,
        worker_name: String,
        worker_options: Option<WorkerOptions>,
    ) -> Result<CreateWorkflowWorkerId> {
        tracing::debug!("Creating worker with name: {}", worker_name);

        // WorkerDataの構築（設計書に従った正しい方法）
        let worker_data =
            self.build_worker_data(worker_name.clone(), workflow_def, worker_options)?;

        // WorkerApp APIを使用してworkerを作成
        let worker = self
            .app
            .worker_app
            .create(&worker_data)
            .await
            .context("Failed to create worker via worker_app")?;

        tracing::info!("CREATE_WORKFLOW Worker created: {:?}", worker);

        // WorkerIdをCreateWorkflowWorkerIdに変換して返す
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

        // workflow定義からdescriptionを抽出
        let document = workflow_def.get("document");
        let workflow_description = document
            .and_then(|d| d.get("summary"))
            .and_then(|d| d.as_str())
            .unwrap_or("")
            .to_string();

        // デフォルトオプション設定
        let opts = worker_options.unwrap_or_default();

        // REUSABLE_WORKFLOWのRunnerIdを構築（設計書に従い65532を使用）
        let runner_id = RunnerId {
            value: RunnerType::ReusableWorkflow as i64, // i64型を使用
        };

        // runner_settingsをproto形式でシリアライズ（REUSABLE_WORKFLOWのsettings形式）
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

        // retry_policy設定（参照実装と同様にデフォルト設定）
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
            queue_type: opts.queue_type, // QueueTypeを直接使用 (same sequential number)
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
        // 設定の読み込みは不要（CREATE_WORKFLOWは設定なし）
        Ok(())
    }

    async fn run(
        &mut self,
        args: &[u8],
        metadata: HashMap<String, String>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        tracing::info!("Starting CREATE_WORKFLOW execution");

        let result: Result<Vec<u8>> = async {
            // 1. 引数をデシリアライズ（実際のproto deserialization）
            let create_args = if args.is_empty() {
                return Err(anyhow!("CREATE_WORKFLOW requires arguments"));
            } else {
                CreateWorkflowArgs::decode(&mut Cursor::new(args))
                    .context("Failed to deserialize CreateWorkflowArgs from proto")?
            };

            tracing::info!("CREATE_WORKFLOW request for worker: {}", create_args.name);

            // 2. 引数の妥当性検証とパース
            let (workflow_def, worker_name) = self.validate_and_parse_args(&create_args).await?;
            // tracing::debug!("Validated workflow definition for worker: {}", worker_name);

            // 3. 実際のworker作成（AppModuleを使用）
            let worker_id = self
                .create_worker_with_app(
                    workflow_def,
                    worker_name.clone(),
                    create_args.worker_options,
                )
                .await?;

            // 4. 結果を返す
            let result = CreateWorkflowResult {
                worker_id: Some(worker_id),
            };

            tracing::info!(
                "CREATE_WORKFLOW completed successfully: worker_name={}, worker_id={}",
                worker_name,
                worker_id.value
            );

            // 5. 結果をシリアライズ（実際のproto serialization）
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
    ) -> Result<
        std::pin::Pin<
            Box<
                dyn futures::Stream<Item = proto::jobworkerp::data::ResultOutputItem>
                    + Send
                    + 'static,
            >,
        >,
    > {
        // CREATE_WORKFLOWはストリーミング非対応
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
            "Setting up cancellation monitoring for ReusableWorkflowRunner job {}",
            job_id.value
        );

        // For ReusableWorkflowRunner, we use the same pattern as CommandRunner
        // The actual cancellation monitoring will be handled by the CancellationHelper
        // Workflow execution will be cancelled through the executor

        tracing::trace!("Cancellation monitoring started for job {}", job_id.value);
        Ok(None) // Continue with normal execution
    }

    /// Cleanup cancellation monitoring
    async fn cleanup_cancellation_monitoring(&mut self) -> anyhow::Result<()> {
        tracing::trace!("Cleaning up cancellation monitoring for ReusableWorkflowRunner");

        // Clear the cancellation helper
        if let Some(helper) = &mut self.cancel_helper {
            helper.cleanup_monitoring_impl().await?;
        }

        Ok(())
    }

    /// Signals cancellation token for ReusableWorkflowRunner
    async fn request_cancellation(&mut self) -> anyhow::Result<()> {
        if let Some(helper) = &self.cancel_helper {
            let token = helper.get_cancellation_token().await;
            if !token.is_cancelled() {
                token.cancel();
                tracing::info!("ReusableWorkflowRunner: cancellation token signaled");
            }
        } else {
            tracing::warn!("ReusableWorkflowRunner: no cancellation helper available");
        }
        Ok(())
    }

    async fn reset_for_pooling(&mut self) -> anyhow::Result<()> {
        // ReusableWorkflowRunner typically completes quickly, so always cleanup
        if let Some(helper) = &mut self.cancel_helper {
            helper.reset_for_pooling_impl().await?;
        } else {
            self.cleanup_cancellation_monitoring().await?;
        }

        tracing::debug!("ReusableWorkflowRunner reset for pooling");
        Ok(())
    }
}

impl UseCancelMonitoringHelper for CreateWorkflowRunnerImpl {
    fn cancel_monitoring_helper(&self) -> Option<&CancelMonitoringHelper> {
        self.cancel_helper.as_ref()
    }
}
