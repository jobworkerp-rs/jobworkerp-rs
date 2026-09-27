use crate::llm::skills::{SharedSkillCatalog, SkillCatalog};
use anyhow::{Result, anyhow};
use app::app::function::{EnqueuedFunction, FunctionApp, FunctionAppImpl};
use app::module::AppModule;
use async_stream::stream;
use async_trait::async_trait;
use command_utils::trace::Tracing;
use futures::stream::{BoxStream, StreamExt};
use genai::GenaiChatService;
use jobworkerp_base::APP_WORKER_NAME;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::jobworkerp::runner::llm::{LlmChatArgs, LlmChatResult, LlmRunnerSettings};
use jobworkerp_runner::runner::cancellation_helper::{
    CancelMonitoringHelper, UseCancelMonitoringHelper,
};
use jobworkerp_runner::runner::llm_chat::LLMChatRunnerSpec;
use jobworkerp_runner::runner::{RunnerSpec, RunnerTrait};
use ollama::OllamaChatService;
use opentelemetry::Context;
use opentelemetry::trace::TraceContextExt;
use prost::Message;
use proto::jobworkerp::data::{ResultOutputItem, result_output_item};
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub mod conversion;
pub mod genai;
pub mod ollama;

fn should_forward_ollama_chunk(result: &LlmChatResult) -> bool {
    result
        .content
        .as_ref()
        .is_some_and(|content| content.content.is_some())
        || !result.tool_execution_results.is_empty()
        || result.tool_execution_started.is_some()
}

fn validate_skill_function_options(args: &LlmChatArgs, streaming: bool) -> Result<()> {
    validate_skill_function_options_with_choice(args, streaming, false)
}

fn validate_skill_function_options_for_ollama(args: &LlmChatArgs, streaming: bool) -> Result<()> {
    validate_skill_function_options_with_choice(args, streaming, true)
}

fn validate_skill_function_options_with_choice(
    args: &LlmChatArgs,
    streaming: bool,
    allow_empty_tool_choice: bool,
) -> Result<()> {
    let options = args
        .function_options
        .as_ref()
        .ok_or_else(|| anyhow!("skills require function_options with use_function_calling=true"))?;
    if !options.use_function_calling {
        return Err(anyhow!("skills require use_function_calling=true"));
    }
    if options.auto_select_function_set.unwrap_or(false)
        || options
            .client_tools_json
            .as_deref()
            .is_some_and(|tools| !tools.is_empty())
        || options.use_runners_as_function.unwrap_or(false)
        || options.use_workers_as_function.unwrap_or(false)
        || options.tool_choice.as_deref().is_some_and(|choice| {
            choice != "auto" && !(allow_empty_tool_choice && choice.is_empty())
        })
        || (streaming && options.is_auto_calling.unwrap_or(false))
    {
        return Err(anyhow!("unsupported function_options with skills enabled"));
    }
    Ok(())
}

async fn await_enqueued_tool_result(
    function_app: &FunctionAppImpl,
    enqueued: EnqueuedFunction,
    skills_enabled: bool,
) -> Result<serde_json::Value> {
    if skills_enabled {
        enqueued
            .raw_result
            .as_ref()
            .map(app::app::function::ensure_scoped_job_succeeded)
            .transpose()?;
    }
    if let Some(value) = enqueued.result {
        return Ok(value);
    }
    if let Some(handle) = enqueued.result_handle {
        if skills_enabled {
            function_app
                .await_scoped_function_result(
                    handle,
                    &enqueued.runner_name,
                    enqueued.using.as_deref(),
                )
                .await
        } else {
            function_app
                .await_function_result(handle, &enqueued.runner_name, enqueued.using.as_deref())
                .await
        }
    } else {
        Err(anyhow!("No result or result_handle available"))
    }
}

fn inject_skill_catalog(args: &mut LlmChatArgs, configured_system: Option<&str>, catalog: &str) {
    use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
        ChatMessage, ChatRole, MessageContent, message_content::Content,
    };

    if configured_system.is_some() {
        args.messages.retain(|msg| msg.role() != ChatRole::System);
    } else {
        let (mut systems, others): (Vec<_>, Vec<_>) = args
            .messages
            .drain(..)
            .partition(|message| message.role() == ChatRole::System);
        systems.extend(others);
        args.messages = systems;
    }
    let text = configured_system
        .map(|system| format!("{system}\n\n{catalog}"))
        .unwrap_or_else(|| catalog.to_string());
    let insert_at = if configured_system.is_some() {
        0
    } else {
        args.messages
            .iter()
            .take_while(|message| message.role() == ChatRole::System)
            .count()
    };
    args.messages.insert(
        insert_at,
        ChatMessage {
            role: ChatRole::System.into(),
            content: Some(MessageContent {
                content: Some(Content::Text(text)),
            }),
        },
    );
}

pub struct LLMChatRunnerImpl {
    pub app: Arc<AppModule>,
    pub ollama: Option<OllamaChatService>,
    pub genai: Option<GenaiChatService>,
    cancel_helper: Option<CancelMonitoringHelper>,
    skill_catalog: Option<SharedSkillCatalog>,
    configured_system_prompt: Option<String>,
}

impl LLMChatRunnerImpl {
    /// Constructor without cancellation monitoring (for backward compatibility)
    pub fn new(app_module: Arc<AppModule>) -> Self {
        Self {
            app: app_module,
            ollama: None,
            genai: None,
            cancel_helper: None,
            skill_catalog: None,
            configured_system_prompt: None,
        }
    }

    /// Constructor with cancellation monitoring (DI integration version)
    pub fn new_with_cancel_monitoring(
        app_module: Arc<AppModule>,
        cancel_helper: CancelMonitoringHelper,
    ) -> Self {
        Self {
            app: app_module,
            ollama: None,
            genai: None,
            cancel_helper: Some(cancel_helper),
            skill_catalog: None,
            configured_system_prompt: None,
        }
    }

    /// Unified cancellation token retrieval
    async fn get_cancellation_token(&self) -> CancellationToken {
        if let Some(helper) = &self.cancel_helper {
            helper.get_cancellation_token().await
        } else {
            CancellationToken::new()
        }
    }

    fn prepare_skill_args(&self, args: &mut LlmChatArgs, streaming: bool) -> Result<()> {
        if let Some(catalog) = &self.skill_catalog {
            validate_skill_function_options(args, streaming)?;
            conversion::ToolConverter::validate_skill_execution_requests(args)?;
            if let Some(prompt) = catalog.render_prompt() {
                inject_skill_catalog(args, self.configured_system_prompt.as_deref(), &prompt);
            }
        }
        Ok(())
    }
}

impl Tracing for LLMChatRunnerImpl {}
impl LLMChatRunnerSpec for LLMChatRunnerImpl {}

// DI trait implementation (with optional support)
impl UseCancelMonitoringHelper for LLMChatRunnerImpl {
    fn cancel_monitoring_helper(&self) -> Option<&CancelMonitoringHelper> {
        self.cancel_helper.as_ref()
    }
}
impl RunnerSpec for LLMChatRunnerImpl {
    fn name(&self) -> String {
        LLMChatRunnerSpec::name(self)
    }

    fn runner_settings_proto(&self) -> String {
        LLMChatRunnerSpec::runner_settings_proto(self)
    }

    fn method_proto_map(
        &self,
    ) -> std::collections::HashMap<String, proto::jobworkerp::data::MethodSchema> {
        LLMChatRunnerSpec::method_proto_map(self)
    }

    fn settings_schema(&self) -> String {
        LLMChatRunnerSpec::settings_schema(self)
    }
}

#[async_trait]
impl RunnerTrait for LLMChatRunnerImpl {
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        let settings = LlmRunnerSettings::decode(&mut Cursor::new(settings))
            .map_err(|e| anyhow!("decode error: {}", e))?;
        let catalog = settings
            .skills
            .as_ref()
            .map(SkillCatalog::load_shared)
            .transpose()?;
        match settings.settings {
            Some(
                jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::Settings::Ollama(
                    settings,
                ),
            ) => {
                let system_prompt = settings.system_prompt.clone();
                let ollama = OllamaChatService::new(
                    self.app.function_app.clone(),
                    self.app.function_set_app.clone(),
                    settings,
                )?
                .with_skill_catalog(catalog.clone());
                tracing::info!("{} loaded(ollama)", "LLM(chat)");
                self.ollama = Some(ollama);
                self.genai = None;
                self.skill_catalog = catalog;
                self.configured_system_prompt = system_prompt;
                Ok(())
            }
            Some(
                jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::Settings::Genai(
                    settings,
                ),
            ) => {
                let system_prompt = settings.system_prompt.clone();
                let genai = GenaiChatService::new_with_skills(
                    self.app.function_app.clone(),
                    self.app.function_set_app.clone(),
                    settings,
                    catalog.clone(),
                )
                .await?;
                tracing::info!("{} loaded(genai)", "LLM(chat)");
                self.genai = Some(genai);
                self.ollama = None;
                self.skill_catalog = catalog;
                self.configured_system_prompt = system_prompt;
                Ok(())
            }
            _ => Err(anyhow!("model_settings is not set")),
        }
    }

    async fn run(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
        _using: Option<&str>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        let cancellation_token = self.get_cancellation_token().await;

        // Early cancellation check prevents wasted LLM service calls
        if cancellation_token.is_cancelled() {
            return (
                Err(
                    JobWorkerError::CancelledError("LLM chat execution was cancelled".to_string())
                        .into(),
                ),
                metadata,
            );
        }

        let span = Self::otel_span_from_metadata(&metadata, APP_WORKER_NAME, "llm_chat_run");
        let cx = Context::current_with_span(span);

        let metadata_clone = metadata.clone();
        let result = async {
            let mut args = LlmChatArgs::decode(&mut Cursor::new(arg))
                .map_err(|e| anyhow!("decode error: {}", e))?;

            // Handle potentially escaped JSON schema string from grpc-web
            if let Some(json_schema) = &args.json_schema {
                let schema_value = serde_json::to_value(json_schema)
                    .map_err(|e| anyhow!("Invalid json_schema format: {}", e))?;
                let processed_schema = match schema_value {
                    serde_json::Value::String(json_str) => {
                        // Try to parse as JSON string (in case it's escaped)
                        match serde_json::from_str::<serde_json::Value>(&json_str) {
                            Ok(_) => json_str,             // Valid JSON string, use as-is
                            Err(_) => json_schema.clone(), // Parse failed, use original
                        }
                    }
                    _ => json_schema.clone(), // Not a string, use original
                };
                args.json_schema = Some(processed_schema);
            }
            self.prepare_skill_args(&mut args, false)?;

            if let Some(ollama) = self.ollama.as_mut() {
                // Race between LLM completion and cancellation signal
                let res = tokio::select! {
                    result = ollama.request_chat(args, cx, metadata_clone.clone()) => result?,
                    _ = cancellation_token.cancelled() => {
                        return Err(JobWorkerError::CancelledError("LLM chat (Ollama) request was cancelled".to_string()).into());
                    }
                };

                let mut buf = Vec::with_capacity(res.encoded_len());
                res.encode(&mut buf)
                    .map_err(|e| anyhow!("encode error: {}", e))?;
                Ok(buf)
            } else if let Some(genai) = self.genai.as_mut() {
                // Race between LLM completion and cancellation signal
                let res = tokio::select! {
                    result = genai.request_chat(args, cx, metadata_clone.clone()) => result?,
                    _ = cancellation_token.cancelled() => {
                        return Err(JobWorkerError::CancelledError("LLM chat (GenAI) request was cancelled".to_string()).into());
                    }
                };

                let mut buf = Vec::with_capacity(res.encoded_len());
                res.encode(&mut buf)
                    .map_err(|e| anyhow!("encode error: {}", e))?;
                Ok(buf)
            } else {
                Err(anyhow!("llm is not initialized"))
            }
        }
        .await;

        (result, metadata_clone)
    }

    async fn run_stream(
        &mut self,
        args: &[u8],
        metadata: HashMap<String, String>,
        _using: Option<&str>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        let cancellation_token = self.get_cancellation_token().await;

        let mut args = LlmChatArgs::decode(args).map_err(|e| anyhow!("decode error: {}", e))?;

        // Handle potentially escaped JSON schema string from grpc-web
        if let Some(json_schema) = &args.json_schema {
            let schema_value = serde_json::to_value(json_schema)
                .map_err(|e| anyhow!("Invalid json_schema format: {}", e))?;
            let processed_schema = match schema_value {
                serde_json::Value::String(json_str) => {
                    // Try to parse as JSON string (in case it's escaped)
                    match serde_json::from_str::<serde_json::Value>(&json_str) {
                        Ok(_) => json_str,             // Valid JSON string, use as-is
                        Err(_) => json_schema.clone(), // Parse failed, use original
                    }
                }
                _ => json_schema.clone(), // Not a string, use original
            };
            args.json_schema = Some(processed_schema);
        }
        self.prepare_skill_args(&mut args, true)?;

        // The Context owns the root BoxedSpan; we move it into the returned stream so the
        // span lives until the consumer drops the stream (BoxedSpan::Drop calls end()).
        let parent_cx = Context::current_with_span(Self::otel_span_from_metadata(
            &metadata,
            APP_WORKER_NAME,
            "llm_chat_run_stream",
        ));

        if let Some(ollama) = self.ollama.as_ref() {
            let stream = ollama
                .request_stream_chat_ref(args, metadata.clone(), Some(parent_cx.clone()))
                .await?;

            let req_meta = Arc::new(metadata.clone());
            let cancel_token = cancellation_token.clone();
            let root_cx = parent_cx;

            // Stream processing with mid-stream cancellation capability
            let output_stream = stream! {
                // async_stream::stream! does not support `move` capture, so binding the
                // Context inside the block keeps the root span alive for the stream's lifetime.
                let _root_cx = root_cx;
                tokio::pin!(stream);
                loop {
                    tokio::select! {
                        item = stream.next() => {
                            match item {
                                Some(completion_result) => {
                                    // Yield chunks that have content or tool execution results
                                    if should_forward_ollama_chunk(&completion_result) {
                                        let buf = ProstMessageCodec::serialize_message(&completion_result);
                                        if let Ok(buf) = buf {
                                            yield ResultOutputItem {
                                                item: Some(result_output_item::Item::Data(buf)),
                                            };
                                        } else {
                                            tracing::error!("Failed to serialize LLM completion result");
                                        }
                                    }

                                    if completion_result.done {
                                        yield ResultOutputItem {
                                            item: Some(result_output_item::Item::End(
                                                proto::jobworkerp::data::Trailer {
                                                    metadata: (*req_meta).clone(),
                                                },
                                            )),
                                        };
                                        break;
                                    }
                                }
                                None => break,
                            }
                        }
                        _ = cancel_token.cancelled() => {
                            tracing::info!("LLM chat stream was cancelled");
                            break;
                        }
                    }
                }
            }.boxed();

            Ok(output_stream)
        } else if let Some(genai) = self.genai.as_mut() {
            let stream = genai
                .request_chat_stream(args, metadata, Some(parent_cx.clone()))
                .await?;

            let cancel_token = cancellation_token.clone();
            let root_cx = parent_cx;
            let cancellable_stream = stream! {
                let _root_cx = root_cx;
                tokio::pin!(stream);
                loop {
                    tokio::select! {
                        item = stream.next() => {
                            match item {
                                Some(result_item) => yield result_item,
                                None => break,
                            }
                        }
                        _ = cancel_token.cancelled() => {
                            tracing::info!("LLM chat GenAI stream was cancelled");
                            break;
                        }
                    }
                }
            }
            .boxed();

            Ok(cancellable_stream)
        } else {
            Err(anyhow!("llm is not initialized"))
        }
    }
}

#[async_trait]
impl jobworkerp_runner::runner::cancellation::CancelMonitoring for LLMChatRunnerImpl {
    async fn setup_cancellation_monitoring(
        &mut self,
        job_id: proto::jobworkerp::data::JobId,
        job_data: &proto::jobworkerp::data::JobData,
    ) -> anyhow::Result<Option<proto::jobworkerp::data::JobResult>> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.setup_monitoring_impl(job_id, job_data).await
        } else {
            tracing::debug!(
                "No cancel monitoring configured for LLM Chat job {}",
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

    /// Signals cancellation token for LLMChatRunnerImpl
    async fn request_cancellation(&mut self) -> anyhow::Result<()> {
        if let Some(helper) = &self.cancel_helper {
            let token = helper.get_cancellation_token().await;
            if !token.is_cancelled() {
                token.cancel();
                tracing::debug!("LLMChatRunnerImpl: cancellation token signaled");
            }
        } else {
            tracing::warn!("LLMChatRunnerImpl: no cancellation helper available");
        }
        Ok(())
    }

    async fn reset_for_pooling(&mut self) -> anyhow::Result<()> {
        // Quick completion requires immediate cleanup to prevent resource leaks
        if let Some(helper) = &mut self.cancel_helper {
            helper.reset_for_pooling_impl().await?;
        } else {
            self.cleanup_cancellation_monitoring().await?;
        }

        tracing::debug!("LLMChatRunnerImpl reset for pooling");
        Ok(())
    }
}

#[cfg(test)]
mod skills_stream_tests {
    use super::*;
    use jobworkerp_runner::jobworkerp::runner::llm::{
        LlmChatResult, ToolExecutionResult, ToolExecutionStarted, llm_chat_args::FunctionOptions,
    };

    #[test]
    fn ollama_stream_forwards_tool_start_and_result_without_text() {
        let started = LlmChatResult {
            tool_execution_started: Some(ToolExecutionStarted {
                call_id: "call-1".into(),
                fn_name: "reader".into(),
                job_id: 123,
                fn_arguments: "{}".into(),
            }),
            ..Default::default()
        };
        assert!(should_forward_ollama_chunk(&started));

        let activated = LlmChatResult {
            tool_execution_results: vec![ToolExecutionResult {
                call_id: "call-2".into(),
                fn_name: "activate_skill".into(),
                result: "instructions".into(),
                job_id: None,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(should_forward_ollama_chunk(&activated));
        assert!(!should_forward_ollama_chunk(&LlmChatResult::default()));
    }

    #[test]
    fn skill_requests_require_server_tool_calling_and_reject_conflicting_knobs() {
        let mut args = LlmChatArgs::default();
        assert!(validate_skill_function_options(&args, false).is_err());
        args.function_options = Some(FunctionOptions {
            use_function_calling: true,
            ..Default::default()
        });
        assert!(validate_skill_function_options(&args, false).is_ok());
        assert!(validate_skill_function_options(&args, true).is_ok());

        args.function_options.as_mut().unwrap().is_auto_calling = Some(true);
        assert!(validate_skill_function_options(&args, false).is_ok());
        assert!(validate_skill_function_options(&args, true).is_err());
        args.function_options.as_mut().unwrap().is_auto_calling = Some(false);
        args.function_options
            .as_mut()
            .unwrap()
            .auto_select_function_set = Some(true);
        assert!(validate_skill_function_options(&args, false).is_err());
        args.function_options
            .as_mut()
            .unwrap()
            .auto_select_function_set = Some(false);
        args.function_options.as_mut().unwrap().client_tools_json = Some("[]".into());
        assert!(validate_skill_function_options(&args, false).is_err());
    }

    #[test]
    fn skill_validation_preserves_the_direct_ollama_empty_choice_exception() {
        let mut args = LlmChatArgs {
            function_options: Some(FunctionOptions {
                use_function_calling: true,
                tool_choice: Some(String::new()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(validate_skill_function_options(&args, false).is_err());
        assert!(validate_skill_function_options_for_ollama(&args, false).is_ok());
        args.function_options.as_mut().unwrap().tool_choice = Some("required".into());
        assert!(validate_skill_function_options_for_ollama(&args, false).is_err());
        args.function_options.as_mut().unwrap().tool_choice = Some("auto".into());
        assert!(validate_skill_function_options_for_ollama(&args, false).is_ok());
        args.function_options
            .as_mut()
            .unwrap()
            .use_workers_as_function = Some(true);
        assert!(validate_skill_function_options_for_ollama(&args, false).is_err());
    }

    #[test]
    fn shared_skill_validation_rejects_each_unsupported_option_in_both_modes() {
        let base = LlmChatArgs {
            function_options: Some(FunctionOptions {
                use_function_calling: true,
                tool_choice: Some("auto".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(validate_skill_function_options(&base, false).is_ok());
        assert!(validate_skill_function_options_for_ollama(&base, true).is_ok());

        let unsupported: [fn(&mut FunctionOptions); 6] = [
            |options| options.use_function_calling = false,
            |options| options.auto_select_function_set = Some(true),
            |options| options.client_tools_json = Some("[]".into()),
            |options| options.use_runners_as_function = Some(true),
            |options| options.use_workers_as_function = Some(true),
            |options| options.tool_choice = Some("required".into()),
        ];
        for change in unsupported {
            let mut args = base.clone();
            change(args.function_options.as_mut().unwrap());
            assert!(validate_skill_function_options(&args, false).is_err());
            assert!(validate_skill_function_options_for_ollama(&args, false).is_err());
        }
        let mut stream = base;
        stream.function_options.as_mut().unwrap().is_auto_calling = Some(true);
        assert!(validate_skill_function_options(&stream, false).is_ok());
        assert!(validate_skill_function_options_for_ollama(&stream, false).is_ok());
        assert!(validate_skill_function_options(&stream, true).is_err());
        assert!(validate_skill_function_options_for_ollama(&stream, true).is_err());
    }

    #[tokio::test]
    async fn common_manual_tool_result_preserves_direct_and_stream_error_contracts() {
        use app::app::function::EnqueuedFunction;
        use proto::jobworkerp::data::{
            JobId, JobResult, JobResultData, ResultOutput, ResultStatus,
        };

        let app = app::module::test::create_rdb_chan_test_app(false, false)
            .await
            .unwrap();
        let result = |status: ResultStatus| JobResult {
            data: Some(JobResultData {
                status: status as i32,
                output: Some(ResultOutput {
                    items: b"target changed".to_vec(),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let enqueued = |raw_result: Option<JobResult>,
                        value: Option<serde_json::Value>,
                        result_handle: Option<
            tokio::task::JoinHandle<app::app::function::JobListenResult>,
        >| EnqueuedFunction {
            job_id: JobId { value: 42 },
            runner_name: "COMMAND".into(),
            result: value,
            raw_result,
            is_streaming: result_handle.is_some(),
            result_handle,
            using: Some("run".into()),
        };

        let direct = enqueued(
            Some(result(ResultStatus::Success)),
            Some(serde_json::json!("read")),
            None,
        );
        assert_eq!(
            await_enqueued_tool_result(&app.function_app, direct, true)
                .await
                .unwrap(),
            serde_json::json!("read")
        );
        let fatal = enqueued(
            Some(result(ResultStatus::FatalError)),
            Some(serde_json::json!("must not succeed")),
            None,
        );
        assert!(
            await_enqueued_tool_result(&app.function_app, fatal, true)
                .await
                .is_err()
        );
        let legacy = enqueued(
            Some(result(ResultStatus::FatalError)),
            Some(serde_json::json!("legacy")),
            None,
        );
        assert_eq!(
            await_enqueued_tool_result(&app.function_app, legacy, false)
                .await
                .unwrap(),
            serde_json::json!("legacy")
        );
        let pending = enqueued(
            None,
            None,
            Some(tokio::spawn(async move {
                Ok((result(ResultStatus::FatalError), None))
            })),
        );
        assert!(
            await_enqueued_tool_result(&app.function_app, pending, true)
                .await
                .is_err()
        );
        let command_output = jobworkerp_runner::jobworkerp::runner::CommandResult {
            stdout: Some("reference".into()),
            exit_code: Some(0),
            ..Default::default()
        }
        .encode_to_vec();
        for skills_enabled in [false, true] {
            let successful = JobResult {
                data: Some(JobResultData {
                    status: ResultStatus::Success as i32,
                    output: Some(ResultOutput {
                        items: command_output.clone(),
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let pending = enqueued(
                None,
                None,
                Some(tokio::spawn(async move { Ok((successful, None)) })),
            );
            let value = await_enqueued_tool_result(&app.function_app, pending, skills_enabled)
                .await
                .unwrap();
            assert_eq!(value["stdout"], "reference");
        }
        assert!(
            await_enqueued_tool_result(&app.function_app, enqueued(None, None, None), true)
                .await
                .unwrap_err()
                .to_string()
                .contains("No result or result_handle")
        );
    }

    #[test]
    fn skill_catalog_replaces_only_configured_system_prompt_and_preserves_other_messages() {
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
            ChatMessage, ChatRole, MessageContent, message_content::Content,
        };
        let message = |role: ChatRole, text: &str| ChatMessage {
            role: role.into(),
            content: Some(MessageContent {
                content: Some(Content::Text(text.into())),
            }),
        };
        let mut args = LlmChatArgs {
            messages: vec![
                message(ChatRole::System, "client system"),
                message(ChatRole::User, "hello"),
            ],
            ..Default::default()
        };
        inject_skill_catalog(&mut args, Some("configured system"), "skill catalog");
        assert_eq!(args.messages.len(), 2);
        assert_eq!(
            args.messages[0].content.as_ref().unwrap().content,
            Some(Content::Text("configured system\n\nskill catalog".into()))
        );
        assert_eq!(args.messages[1], message(ChatRole::User, "hello"));

        let mut without_settings = LlmChatArgs {
            messages: vec![
                message(ChatRole::System, "client system"),
                message(ChatRole::User, "hello"),
            ],
            ..Default::default()
        };
        inject_skill_catalog(&mut without_settings, None, "skill catalog");
        assert_eq!(without_settings.messages.len(), 3);
        assert_eq!(
            without_settings.messages[0],
            message(ChatRole::System, "client system")
        );
        assert_eq!(
            without_settings.messages[1],
            message(ChatRole::System, "skill catalog")
        );
        assert_eq!(
            without_settings.messages[2],
            message(ChatRole::User, "hello")
        );
    }

    #[test]
    fn skill_catalog_groups_all_input_system_messages_before_normal_messages() {
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
            ChatMessage, ChatRole, MessageContent, message_content::Content,
        };
        let message = |role: ChatRole, text: &str| ChatMessage {
            role: role.into(),
            content: Some(MessageContent {
                content: Some(Content::Text(text.into())),
            }),
        };
        let mut args = LlmChatArgs {
            messages: vec![
                message(ChatRole::User, "first"),
                message(ChatRole::System, "late system"),
                message(ChatRole::User, "second"),
                message(ChatRole::System, "later system"),
            ],
            ..Default::default()
        };
        inject_skill_catalog(&mut args, None, "skill catalog");
        assert_eq!(
            args.messages,
            vec![
                message(ChatRole::System, "late system"),
                message(ChatRole::System, "later system"),
                message(ChatRole::System, "skill catalog"),
                message(ChatRole::User, "first"),
                message(ChatRole::User, "second"),
            ]
        );
    }
}
