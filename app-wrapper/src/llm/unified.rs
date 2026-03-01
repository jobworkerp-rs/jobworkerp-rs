//! Unified LLM Runner implementation for app-wrapper
//!
//! This module provides a unified LLM runner that supports both 'completion' and 'chat' methods
//! via the `using` parameter.

use super::chat::LLMChatRunnerImpl;
use super::completion::LLMCompletionRunnerImpl;
use anyhow::{Result, anyhow};
use app::module::AppModule;
use async_trait::async_trait;
use futures::stream::BoxStream;
use jobworkerp_runner::runner::cancellation::CancelMonitoring;
use jobworkerp_runner::runner::cancellation_helper::{
    CancelMonitoringHelper, UseCancelMonitoringHelper,
};
use jobworkerp_runner::runner::llm_unified::{
    LLMUnifiedRunnerSpecImpl, METHOD_CHAT, METHOD_COMPLETION,
};
use jobworkerp_runner::runner::{RunnerSpec, RunnerTrait};
use proto::jobworkerp::data::{JobData, JobId, JobResult, ResultOutputItem};
use std::collections::HashMap;
use std::sync::Arc;

/// Unified LLM Runner implementation that delegates to completion or chat runners
pub struct LLMUnifiedRunnerImpl {
    completion_runner: LLMCompletionRunnerImpl,
    chat_runner: LLMChatRunnerImpl,
    spec: LLMUnifiedRunnerSpecImpl,
    cancel_helper: Option<CancelMonitoringHelper>,
}

impl LLMUnifiedRunnerImpl {
    pub fn new(app_module: Arc<AppModule>) -> Self {
        Self {
            completion_runner: LLMCompletionRunnerImpl::new(app_module.clone()),
            chat_runner: LLMChatRunnerImpl::new(app_module),
            spec: LLMUnifiedRunnerSpecImpl::new(),
            cancel_helper: None,
        }
    }

    pub fn new_with_cancel_monitoring(
        app_module: Arc<AppModule>,
        cancel_helper: CancelMonitoringHelper,
    ) -> Self {
        Self {
            completion_runner: LLMCompletionRunnerImpl::new_with_cancel_monitoring(
                app_module.clone(),
                cancel_helper.clone(),
            ),
            chat_runner: LLMChatRunnerImpl::new_with_cancel_monitoring(
                app_module,
                cancel_helper.clone(),
            ),
            spec: LLMUnifiedRunnerSpecImpl::new(),
            cancel_helper: Some(cancel_helper),
        }
    }
}

impl UseCancelMonitoringHelper for LLMUnifiedRunnerImpl {
    fn cancel_monitoring_helper(&self) -> Option<&CancelMonitoringHelper> {
        self.cancel_helper.as_ref()
    }
}

impl RunnerSpec for LLMUnifiedRunnerImpl {
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

    fn method_json_schema_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodJsonSchema> {
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
impl RunnerTrait for LLMUnifiedRunnerImpl {
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        // Load settings into both runners (they share the same settings schema)
        self.completion_runner.load(settings.clone()).await?;
        self.chat_runner.load(settings).await?;
        Ok(())
    }

    async fn run(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
        using: Option<&str>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        match LLMUnifiedRunnerSpecImpl::resolve_method(using) {
            Ok(METHOD_COMPLETION) => self.completion_runner.run(arg, metadata, None).await,
            Ok(METHOD_CHAT) => self.chat_runner.run(arg, metadata, None).await,
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
        match LLMUnifiedRunnerSpecImpl::resolve_method(using) {
            Ok(METHOD_COMPLETION) => self.completion_runner.run_stream(arg, metadata, None).await,
            Ok(METHOD_CHAT) => self.chat_runner.run_stream(arg, metadata, None).await,
            Ok(_) => Err(anyhow!("Internal error: unknown method after validation")),
            Err(e) => Err(e),
        }
    }
}

#[async_trait]
impl CancelMonitoring for LLMUnifiedRunnerImpl {
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
        assert!(LLMUnifiedRunnerSpecImpl::resolve_method(Some("completion")).is_ok());
        assert!(LLMUnifiedRunnerSpecImpl::resolve_method(Some("chat")).is_ok());
        assert!(LLMUnifiedRunnerSpecImpl::resolve_method(None).is_err());
        assert!(LLMUnifiedRunnerSpecImpl::resolve_method(Some("unknown")).is_err());
    }

    #[test]
    fn test_runner_spec_name() {
        let spec = LLMUnifiedRunnerSpecImpl::new();
        assert_eq!(spec.name(), "LLM");
    }

    #[test]
    fn test_method_proto_map_has_both_methods() {
        let spec = LLMUnifiedRunnerSpecImpl::new();
        let methods = spec.method_proto_map();

        assert!(methods.contains_key("completion"));
        assert!(methods.contains_key("chat"));
        assert_eq!(methods.len(), 2);

        // Verify schemas are not empty
        let completion = methods.get("completion").unwrap();
        assert!(!completion.args_proto.is_empty());
        assert!(!completion.result_proto.is_empty());

        let chat = methods.get("chat").unwrap();
        assert!(!chat.args_proto.is_empty());
        assert!(!chat.result_proto.is_empty());
    }

    #[test]
    fn test_method_json_schema_map_has_both_methods() {
        let spec = LLMUnifiedRunnerSpecImpl::new();
        let schemas = spec.method_json_schema_map();

        assert!(schemas.contains_key("completion"));
        assert!(schemas.contains_key("chat"));
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
