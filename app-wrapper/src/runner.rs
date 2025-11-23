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
use jobworkerp_runner::runner::mcp_tool::McpToolRunnerImpl;
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

    /// Create MCP Tool runner from runner name (format: "server___tool")
    ///
    /// # Arguments
    /// * `runner_name` - Runner name in format "server___tool"
    /// * `cancel_helper` - Cancellation monitoring helper
    ///
    /// # Returns
    /// * `Some(runner)` if successfully created
    /// * `None` if failed (invalid format, server connection failed, tool not found)
    async fn create_mcp_tool_runner(
        &self,
        runner_name: &str,
        cancel_helper: jobworkerp_runner::runner::cancellation_helper::CancelMonitoringHelper,
    ) -> Option<Box<dyn CancellableRunner + Send + Sync>> {
        const DELIMITER: &str = "___";

        // Parse runner name to extract server_name and tool_name
        let parts: Vec<&str> = runner_name.splitn(2, DELIMITER).collect();
        if parts.len() != 2 {
            tracing::error!(
                "Invalid MCP tool runner name format: '{}'. Expected format: 'server___tool'",
                runner_name
            );
            return None;
        }

        let server_name = parts[0];
        let tool_name = parts[1];

        // Connect to MCP server
        let server = match self.mcp_clients.connect_server(server_name).await {
            Ok(s) => Arc::new(s),
            Err(e) => {
                tracing::error!(
                    "Failed to connect to MCP server '{}' for tool '{}': {:?}",
                    server_name,
                    tool_name,
                    e
                );
                return None;
            }
        };

        // Load tools from MCP server
        let tools = match server.load_tools().await {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(
                    "Failed to load tools from MCP server '{}': {:?}",
                    server_name,
                    e
                );
                return None;
            }
        };

        // Find the target tool
        let tool = match tools.iter().find(|t| t.name == tool_name) {
            Some(t) => t,
            None => {
                tracing::error!(
                    "Tool '{}' not found in MCP server '{}'. Available tools: {:?}",
                    tool_name,
                    server_name,
                    tools.iter().map(|t| t.name.as_ref()).collect::<Vec<_>>()
                );
                return None;
            }
        };

        // Create McpToolRunnerImpl
        // Convert Arc<Map<String, Value>> to Value::Object
        let tool_schema = serde_json::Value::Object((*tool.input_schema).clone());
        match McpToolRunnerImpl::new_with_cancel_monitoring(
            server,
            server_name.to_string(),
            tool_name.to_string(),
            tool_schema,
            cancel_helper,
        ) {
            Ok(runner) => {
                tracing::debug!(
                    "Successfully created MCP tool runner: {}::{}",
                    server_name,
                    tool_name
                );
                Some(Box::new(runner) as Box<dyn CancellableRunner + Send + Sync>)
            }
            Err(e) => {
                tracing::error!(
                    "Failed to create MCP tool runner '{}::{}': {:?}",
                    server_name,
                    tool_name,
                    e
                );
                None
            }
        }
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
                // Try MCP Tool runner (type=8, format: "server___tool")
                const DELIMITER: &str = "___";
                if name.contains(DELIMITER) {
                    if let Some(runner) = self
                        .create_mcp_tool_runner(name, create_cancel_helper())
                        .await
                    {
                        return Some(runner);
                    }
                }

                // Try Plugin runner
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

pub trait UseRunnerFactory {
    fn runner_factory(&self) -> &RunnerFactory;
}
