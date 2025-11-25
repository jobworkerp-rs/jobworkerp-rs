pub mod cancellation;

use crate::llm::chat::LLMChatRunnerImpl;
use crate::modules::AppWrapperModule;
use crate::workflow::create_workflow::CreateWorkflowRunnerImpl;
use crate::workflow::runner::reusable::ReusableWorkflowRunner;
use crate::{
    llm::completion::LLMCompletionRunnerImpl, workflow::runner::inline::InlineWorkflowRunner,
};
use anyhow::Result;
use app::module::AppModule;
use jobworkerp_runner::runner::mcp::proxy::McpServerFactory;
use jobworkerp_runner::runner::mcp::McpServerRunnerImpl;
use jobworkerp_runner::runner::{
    cancellation::CancellableRunner,
    command::CommandRunnerImpl,
    docker::{DockerExecRunner, DockerRunner},
    grpc_unary::GrpcUnaryRunner,
    plugins::{PluginLoader, PluginMetadata, Plugins},
    python::PythonCommandRunner,
    request::RequestRunner,
    slack::SlackPostMessageRunner,
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
    ) -> Option<Box<dyn CancellableRunner + Send + Sync>> {
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
            )
                as Box<dyn CancellableRunner + Send + Sync>),
            Some(RunnerType::PythonCommand) => Some(Box::new(
                PythonCommandRunner::new_with_cancel_monitoring(create_cancel_helper()),
            )
                as Box<dyn CancellableRunner + Send + Sync>),
            Some(RunnerType::Docker) if use_static => Some(Box::new(
                DockerExecRunner::new_with_cancel_monitoring(create_cancel_helper()),
            )
                as Box<dyn CancellableRunner + Send + Sync>),
            Some(RunnerType::Docker) => Some(Box::new(DockerRunner::new_with_cancel_monitoring(
                create_cancel_helper(),
            ))
                as Box<dyn CancellableRunner + Send + Sync>),
            Some(RunnerType::GrpcUnary) => Some(Box::new(
                GrpcUnaryRunner::new_with_cancel_monitoring(create_cancel_helper()),
            )
                as Box<dyn CancellableRunner + Send + Sync>),
            Some(RunnerType::HttpRequest) => Some(Box::new(
                RequestRunner::new_with_cancel_monitoring(create_cancel_helper()),
            )
                as Box<dyn CancellableRunner + Send + Sync>),
            Some(RunnerType::SlackPostMessage) => Some(Box::new(
                SlackPostMessageRunner::new_with_cancel_monitoring(create_cancel_helper()),
            )
                as Box<dyn CancellableRunner + Send + Sync>),
            Some(RunnerType::InlineWorkflow) => {
                match InlineWorkflowRunner::new_with_cancel_monitoring(
                    self.app_wrapper_module.clone(),
                    self.app_module.clone(),
                    create_cancel_helper(),
                ) {
                    Ok(runner) => {
                        Some(Box::new(runner) as Box<dyn CancellableRunner + Send + Sync>)
                    }
                    Err(err) => {
                        tracing::error!("Failed to create InlineWorkflowRunner: {}", err);
                        None
                    }
                }
            }
            Some(RunnerType::ReusableWorkflow) => {
                match ReusableWorkflowRunner::new_with_cancel_monitoring(
                    self.app_wrapper_module.clone(),
                    self.app_module.clone(),
                    create_cancel_helper(),
                ) {
                    Ok(runner) => {
                        Some(Box::new(runner) as Box<dyn CancellableRunner + Send + Sync>)
                    }
                    Err(err) => {
                        tracing::error!("Failed to create ReusableWorkflowRunner: {}", err);
                        None
                    }
                }
            }
            Some(RunnerType::CreateWorkflow) => Some(Box::new(
                CreateWorkflowRunnerImpl::new_with_cancel_monitoring(
                    self.app_module.clone(),
                    create_cancel_helper(),
                ),
            )
                as Box<dyn CancellableRunner + Send + Sync>),
            Some(RunnerType::LlmCompletion) => Some(Box::new(
                LLMCompletionRunnerImpl::new_with_cancel_monitoring(
                    self.app_module.clone(),
                    create_cancel_helper(),
                ),
            )
                as Box<dyn CancellableRunner + Send + Sync>),
            Some(RunnerType::LlmChat) => {
                Some(Box::new(LLMChatRunnerImpl::new_with_cancel_monitoring(
                    self.app_module.clone(),
                    create_cancel_helper(),
                ))
                    as Box<dyn CancellableRunner + Send + Sync>)
            }
            _ => {
                if let Ok(server) = self.mcp_clients.connect_server(name).await {
                    match McpServerRunnerImpl::new(server, Some(create_cancel_helper())).await {
                        Ok(mcp_runner) => {
                            Some(Box::new(mcp_runner) as Box<dyn CancellableRunner + Send + Sync>)
                        }
                        Err(e) => {
                            tracing::error!(
                                "Failed to initialize MCP runner '{}': {}",
                                name,
                                e
                            );
                            None
                        }
                    }
                } else {
                    // TODO: Add cancellation monitoring support to Plugin Runners
                    self.plugins
                        .runner_plugins()
                        .write()
                        .await
                        .find_plugin_runner_by_name(name)
                        .await
                        .map(|r| Box::new(r) as Box<dyn CancellableRunner + Send + Sync>)
                }
            }
        }
    }
}

pub trait UseRunnerFactory {
    fn runner_factory(&self) -> &RunnerFactory;
}
