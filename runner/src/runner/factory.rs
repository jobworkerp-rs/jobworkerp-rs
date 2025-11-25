use super::{
    command::CommandRunnerImpl,
    docker::{DockerExecRunner, DockerRunner},
    grpc_unary::GrpcUnaryRunner,
    llm::LLMCompletionRunnerSpecImpl,
    llm_chat::LLMChatRunnerSpecImpl,
    mcp::{
        config::McpServerConfig,
        proxy::{McpServerFactory, McpServerProxy},
        McpServerRunnerImpl,
    },
    plugins::{PluginLoader, PluginMetadata, Plugins},
    python::PythonCommandRunner,
    request::RequestRunner,
    slack::SlackPostMessageRunner,
    workflow::{InlineWorkflowRunnerSpecImpl, ReusableWorkflowRunnerSpecImpl},
    RunnerSpec,
};
use anyhow::Result;
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::RunnerType;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct RunnerSpecFactory {
    // TODO to map?
    pub plugins: Arc<Plugins>,
    pub mcp_clients: Arc<McpServerFactory>,
}

impl RunnerSpecFactory {
    pub fn new(plugins: Arc<Plugins>, mcp_clients: Arc<McpServerFactory>) -> Self {
        Self {
            plugins,
            mcp_clients,
        }
    }
    pub async fn load_plugins_from(&self, dir: &str) -> Vec<PluginMetadata> {
        self.plugins.load_plugin_files(dir).await
    }
    pub async fn load_plugin(
        &self,
        name: Option<&str>,
        filepath: &str,
        overwrite: bool,
    ) -> Result<PluginMetadata> {
        self.plugins
            .load_plugin_file(name, filepath, overwrite)
            .await
    }
    pub async fn unload_plugins(&self, name: &str) -> Result<bool> {
        self.plugins.runner_plugins().write().await.unload(name)
    }

    pub async fn load_mcp_server(
        &self,
        name: &str,
        description: &str,
        definition: &str, // transport
    ) -> Result<McpServerProxy> {
        if self.mcp_clients.find_server_config(name).await.is_some() {
            tracing::debug!("MCP server {} already exists", name);
            Err(JobWorkerError::AlreadyExists(format!("MCP server {name} already exists.")).into())
        } else {
            let config = McpServerConfig {
                name: name.to_string(),
                description: Some(description.to_string()),
                transport: toml::from_str(definition)
                    .or_else(|e| {
                        tracing::debug!(
                            "Failed to parse as toml definition as mcp transport: {}",
                            e
                        );
                        serde_json::from_str(definition)
                    })
                    .or_else(|e| {
                        tracing::debug!(
                            "Failed to parse as json definition as mcp transport: {}",
                            e
                        );
                        serde_yaml::from_str(definition)
                    })
                    .map_err(|e| {
                        tracing::debug!(
                            "Failed to parse as yaml definition as mcp transport: {}",
                            e
                        );
                        JobWorkerError::InvalidParameter(
                            "Failed to parse definition as mcp transport.".to_string(),
                        )
                    })?,
            };
            // load mcp server for test (setup connection)
            match self.mcp_clients.add_server(config).await {
                Ok(p) => {
                    tracing::info!("MCP server {} can be connected", name);
                    Ok(p)
                }
                Err(e) => {
                    tracing::error!("Failed to connect to {}: {:#?}", &name, e);
                    Err(e)
                }
            }
        }
    }
    pub async fn unload_mcp_server(&self, name: &str) -> Result<bool> {
        self.mcp_clients.remove_server(name).await
    }

    // use_static: need to specify correctly to create for running (now unused here)
    pub async fn create_runner_spec_by_name(
        &self,
        name: &str,
        use_static: bool,
    ) -> Option<Box<dyn RunnerSpec + Send + Sync>> {
        match RunnerType::from_str_name(name) {
            Some(RunnerType::Command) => {
                // Create a dummy cancellation manager for RunnerSpec purposes
                #[derive(Debug)]
                #[allow(dead_code)]
                struct DummyCancellationManager;

                #[async_trait::async_trait]
                impl crate::runner::cancellation::RunnerCancellationManager for DummyCancellationManager {
                    async fn setup_monitoring(
                        &mut self,
                        _job_id: &proto::jobworkerp::data::JobId,
                        _job_data: &proto::jobworkerp::data::JobData,
                    ) -> anyhow::Result<crate::runner::cancellation::CancellationSetupResult>
                    {
                        Ok(crate::runner::cancellation::CancellationSetupResult::MonitoringStarted)
                    }

                    async fn cleanup_monitoring(&mut self) -> anyhow::Result<()> {
                        Ok(())
                    }

                    async fn get_token(&self) -> tokio_util::sync::CancellationToken {
                        tokio_util::sync::CancellationToken::new()
                    }

                    fn is_cancelled(&self) -> bool {
                        false
                    }
                }

                Some(Box::new(CommandRunnerImpl::new()) as Box<dyn RunnerSpec + Send + Sync>)
            }
            Some(RunnerType::PythonCommand) => {
                Some(Box::new(PythonCommandRunner::new()) as Box<dyn RunnerSpec + Send + Sync>)
            }
            Some(RunnerType::Docker) if use_static => {
                Some(Box::new(DockerExecRunner::new()) as Box<dyn RunnerSpec + Send + Sync>)
            }
            Some(RunnerType::Docker) => {
                Some(Box::new(DockerRunner::new()) as Box<dyn RunnerSpec + Send + Sync>)
            }
            Some(RunnerType::GrpcUnary) => {
                Some(Box::new(GrpcUnaryRunner::new()) as Box<dyn RunnerSpec + Send + Sync>)
            }
            Some(RunnerType::HttpRequest) => {
                Some(Box::new(RequestRunner::new()) as Box<dyn RunnerSpec + Send + Sync>)
            }
            Some(RunnerType::SlackPostMessage) => {
                Some(Box::new(SlackPostMessageRunner::new()) as Box<dyn RunnerSpec + Send + Sync>)
            }
            Some(RunnerType::InlineWorkflow) => {
                Some(Box::new(InlineWorkflowRunnerSpecImpl::new())
                    as Box<dyn RunnerSpec + Send + Sync>)
            }
            Some(RunnerType::ReusableWorkflow) => {
                Some(Box::new(ReusableWorkflowRunnerSpecImpl::new())
                    as Box<dyn RunnerSpec + Send + Sync>)
            }
            Some(RunnerType::CreateWorkflow) => Some(Box::new(
                crate::runner::create_workflow::CreateWorkflowRunnerSpecImpl {},
            )
                as Box<dyn RunnerSpec + Send + Sync>),
            Some(RunnerType::LlmChat) => {
                Some(Box::new(LLMChatRunnerSpecImpl::new()) as Box<dyn RunnerSpec + Send + Sync>)
            }
            Some(RunnerType::LlmCompletion) => {
                Some(Box::new(LLMCompletionRunnerSpecImpl::new())
                    as Box<dyn RunnerSpec + Send + Sync>)
            }
            _ => {
                if let Ok(server) = self.mcp_clients.as_ref().connect_server(name).await {
                    tracing::debug!("MCP server found: {}", &name);
                    // Create and initialize MCP runner
                    match McpServerRunnerImpl::new(server, None).await {
                        Ok(mcp_runner) => {
                            Some(Box::new(mcp_runner) as Box<dyn RunnerSpec + Send + Sync>)
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
                    self.plugins
                        .runner_plugins()
                        .write()
                        .await
                        .find_plugin_runner_by_name(name)
                        .await
                        .map(|r| Box::new(r) as Box<dyn RunnerSpec + Send + Sync>)
                }
            }
        }
    }
}

pub trait UseRunnerSpecFactory {
    fn runner_spec_factory(&self) -> &RunnerSpecFactory;
}

#[cfg(test)]
mod test {
    use super::*;
    pub const TEST_PLUGIN_DIR: &str =
        "./target/debug,../target/debug,../target/release,./target/release";

    #[tokio::test]
    async fn test_new() {
        let runner_factory = RunnerSpecFactory::new(
            Arc::new(Plugins::new()),
            Arc::new(McpServerFactory::default()),
        );
        runner_factory.load_plugins_from(TEST_PLUGIN_DIR).await;
        assert_eq!(
            runner_factory
                .plugins
                .runner_plugins()
                .read()
                .await
                .active_plugin_info()
                .len(),
            2 // Test, Hello
        );
        // from builtins
        assert_eq!(
            runner_factory
                .create_runner_spec_by_name(RunnerType::GrpcUnary.as_str_name(), false)
                .await
                .unwrap()
                .name(),
            "GRPC_UNARY"
        );
        // from plugins
        assert_eq!(
            runner_factory
                .create_runner_spec_by_name("Test", false)
                .await
                .unwrap()
                .name(),
            "Test"
        );
    }

    #[tokio::test]
    async fn test_create_by_name() {
        let runner_factory = RunnerSpecFactory::new(
            Arc::new(Plugins::new()),
            Arc::new(McpServerFactory::default()),
        );
        runner_factory.load_plugins_from(TEST_PLUGIN_DIR).await;
        let runner = runner_factory
            .create_runner_spec_by_name(RunnerType::Command.as_str_name(), false)
            .await
            .unwrap();
        assert_eq!(runner.name(), "COMMAND");
    }
}
