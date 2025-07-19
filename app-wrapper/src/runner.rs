pub mod cancellation;

use crate::llm::chat::LLMChatRunnerImpl;
use crate::modules::AppWrapperModule;
use crate::workflow::runner::reusable::ReusableWorkflowRunner;
use crate::{
    llm::completion::LLMCompletionRunnerImpl, workflow::runner::inline::InlineWorkflowRunner,
};
use anyhow::Result;
use app::module::AppModule;
use jobworkerp_runner::runner::mcp::proxy::McpServerFactory;
use jobworkerp_runner::runner::mcp::McpServerRunnerImpl;
use jobworkerp_runner::runner::{
    command::CommandRunnerImpl,
    docker::{DockerExecRunner, DockerRunner},
    grpc_unary::GrpcUnaryRunner,
    plugins::{PluginLoader, PluginMetadata, Plugins},
    python::PythonCommandRunner,
    request::RequestRunner,
    slack::SlackPostMessageRunner,
    RunnerTrait,
};
use proto::jobworkerp::data::RunnerType;
use std::sync::Arc;
// Note: RunnerCancellationManagerImpl removed in favor of unified approach

// CommandRunnerWrapper removed - will use direct CommandRunnerImpl with AppModule support

#[derive(Debug)]
pub struct RunnerFactory {
    app_module: Arc<AppModule>,
    app_wrapper_module: Arc<AppWrapperModule>,
    plugins: Arc<Plugins>,
    pub mcp_clients: Arc<McpServerFactory>,
}

// same as RunnerSpecFactory
impl RunnerFactory {
    pub fn new(
        app_module: Arc<AppModule>,
        app_wrapper_module: Arc<AppWrapperModule>,
        mcp_clients: Arc<McpServerFactory>,
    ) -> Self {
        Self {
            app_module: app_module.clone(),
            app_wrapper_module: app_wrapper_module.clone(),
            plugins: app_module.config_module.runner_factory.plugins.clone(),
            mcp_clients,
        }
    }
    pub async fn load_plugins_from(&self, dir: &str) -> Vec<PluginMetadata> {
        self.plugins.load_plugin_files(dir).await
    }
    pub async fn unload_plugins(&self, name: &str) -> Result<bool> {
        self.plugins.runner_plugins().write().await.unload(name)
    }
    // use_static: need to specify correctly to create for running
    pub async fn create_by_name(
        &self,
        name: &str,
        use_static: bool,
    ) -> Option<Box<dyn RunnerTrait + Send + Sync>> {
        // Storage設定に応じたキャンセル監視Helper作成
        let create_cancel_helper = || {
            use jobworkerp_runner::runner::cancellation_helper::CancelMonitoringHelper;
            let cancellation_repository = self.app_module.job_queue_cancellation_repository();
            let cancellation_manager = Box::new(
                crate::runner::cancellation::RunnerCancellationManager::new_with_repository(
                    cancellation_repository,
                ),
            );
            CancelMonitoringHelper::new(cancellation_manager)
        };

        match RunnerType::from_str_name(name) {
            Some(RunnerType::Command) => Some(Box::new(
                CommandRunnerImpl::new_with_cancel_monitoring(create_cancel_helper()),
            ) as Box<dyn RunnerTrait + Send + Sync>),
            Some(RunnerType::PythonCommand) => Some(Box::new(
                PythonCommandRunner::new_with_cancel_monitoring(create_cancel_helper()),
            )
                as Box<dyn RunnerTrait + Send + Sync>),
            Some(RunnerType::Docker) if use_static => {
                // TODO: DockerExecRunnerにもキャンセル監視対応を追加予定
                Some(Box::new(DockerExecRunner::new()) as Box<dyn RunnerTrait + Send + Sync>)
            }
            Some(RunnerType::Docker) => {
                // TODO: DockerRunnerにもキャンセル監視対応を追加予定
                Some(Box::new(DockerRunner::new()) as Box<dyn RunnerTrait + Send + Sync>)
            }
            Some(RunnerType::GrpcUnary) => {
                // TODO: GrpcUnaryRunnerにもキャンセル監視対応を追加予定
                Some(Box::new(GrpcUnaryRunner::new()) as Box<dyn RunnerTrait + Send + Sync>)
            }
            Some(RunnerType::HttpRequest) => Some(Box::new(
                RequestRunner::new_with_cancel_monitoring(create_cancel_helper()),
            )
                as Box<dyn RunnerTrait + Send + Sync>),
            Some(RunnerType::SlackPostMessage) => {
                // TODO: SlackPostMessageRunnerにもキャンセル監視対応を追加予定
                Some(Box::new(SlackPostMessageRunner::new()) as Box<dyn RunnerTrait + Send + Sync>)
            }
            Some(RunnerType::InlineWorkflow) => {
                match InlineWorkflowRunner::new(
                    self.app_wrapper_module.clone(),
                    self.app_module.clone(),
                ) {
                    Ok(runner) => Some(Box::new(runner) as Box<dyn RunnerTrait + Send + Sync>),
                    Err(err) => {
                        tracing::error!("Failed to create InlineWorkflowRunner: {}", err);
                        None
                    }
                }
            }
            Some(RunnerType::ReusableWorkflow) => {
                match ReusableWorkflowRunner::new(
                    self.app_wrapper_module.clone(),
                    self.app_module.clone(),
                ) {
                    Ok(runner) => Some(Box::new(runner) as Box<dyn RunnerTrait + Send + Sync>),
                    Err(err) => {
                        tracing::error!("Failed to create ReusableWorkflowRunner: {}", err);
                        None
                    }
                }
            }
            Some(RunnerType::LlmCompletion) => {
                // TODO: LLMCompletionRunnerImplにもキャンセル監視対応を追加予定
                Some(Box::new(LLMCompletionRunnerImpl::new()) as Box<dyn RunnerTrait + Send + Sync>)
            }
            Some(RunnerType::LlmChat) => {
                // TODO: LLMChatRunnerImplにもキャンセル監視対応を追加予定
                Some(Box::new(LLMChatRunnerImpl::new(self.app_module.clone()))
                    as Box<dyn RunnerTrait + Send + Sync>)
            }
            _ => {
                if let Ok(server) = self.mcp_clients.connect_server(name).await {
                    // TODO: McpServerRunnerImplにもキャンセル監視対応を追加予定
                    Some(Box::new(McpServerRunnerImpl::new(server))
                        as Box<dyn RunnerTrait + Send + Sync>)
                } else {
                    // TODO: Plugin Runnersにもキャンセル監視対応を追加予定
                    self.plugins
                        .runner_plugins()
                        .write()
                        .await
                        .find_plugin_runner_by_name(name)
                        .map(|r| Box::new(r) as Box<dyn RunnerTrait + Send + Sync>)
                }
            }
        }
    }
}

pub trait UseRunnerFactory {
    fn runner_factory(&self) -> &RunnerFactory;
}
