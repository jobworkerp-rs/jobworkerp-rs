use anyhow::Result;
use app::module::AppModule;
use jobworkerp_runner::runner::{
    command::CommandRunnerImpl,
    docker::{DockerExecRunner, DockerRunner},
    grpc_unary::GrpcUnaryRunner,
    plugins::{PluginLoader, Plugins},
    python::PythonCommandRunner,
    request::RequestRunner,
    slack::SlackPostMessageRunner,
    RunnerTrait,
};
use proto::jobworkerp::data::RunnerType;
use std::sync::Arc;

use crate::{llm::LLMRunnerImpl, simple_workflow::runner::SimpleWorkflowRunner};

#[derive(Debug)]
pub struct RunnerFactory {
    app_module: Arc<AppModule>,
    plugins: Arc<Plugins>,
}

// same as RunnerSpecFactory
impl RunnerFactory {
    pub fn new(app_module: Arc<AppModule>) -> Self {
        Self {
            app_module: app_module.clone(),
            plugins: app_module.config_module.runner_factory.plugins.clone(),
        }
    }
    pub async fn load_plugins(&self) -> Vec<(String, String)> {
        self.plugins.load_plugin_files_from_env().await
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
        match RunnerType::from_str_name(name) {
            Some(RunnerType::Command) => {
                Some(Box::new(CommandRunnerImpl::new()) as Box<dyn RunnerTrait + Send + Sync>)
            }
            Some(RunnerType::PythonCommand) => {
                Some(Box::new(PythonCommandRunner::new()) as Box<dyn RunnerTrait + Send + Sync>)
            }
            Some(RunnerType::Docker) if use_static => {
                Some(Box::new(DockerExecRunner::new()) as Box<dyn RunnerTrait + Send + Sync>)
            }
            Some(RunnerType::Docker) => {
                Some(Box::new(DockerRunner::new()) as Box<dyn RunnerTrait + Send + Sync>)
            }
            Some(RunnerType::GrpcUnary) => {
                Some(Box::new(GrpcUnaryRunner::new()) as Box<dyn RunnerTrait + Send + Sync>)
            }
            Some(RunnerType::HttpRequest) => {
                Some(Box::new(RequestRunner::new()) as Box<dyn RunnerTrait + Send + Sync>)
            }
            Some(RunnerType::SlackPostMessage) => {
                Some(Box::new(SlackPostMessageRunner::new()) as Box<dyn RunnerTrait + Send + Sync>)
            }
            Some(RunnerType::SimpleWorkflow) => {
                match SimpleWorkflowRunner::new(self.app_module.clone()) {
                    Ok(runner) => Some(Box::new(runner) as Box<dyn RunnerTrait + Send + Sync>),
                    Err(err) => {
                        tracing::error!("Failed to create SimpleWorkflowRunner: {}", err);
                        None
                    }
                }
            }
            Some(RunnerType::Llm) => {
                Some(Box::new(LLMRunnerImpl::new()) as Box<dyn RunnerTrait + Send + Sync>)
            }
            // _ => self.runner_factory().create_plugin_by_name(name).await,
            _ => self
                .plugins
                .runner_plugins()
                .write()
                .await
                .find_plugin_runner_by_name(name)
                .map(|r| Box::new(r) as Box<dyn RunnerTrait + Send + Sync>),
        }
    }
}

pub trait UseRunnerFactory {
    fn runner_factory(&self) -> &RunnerFactory;
}
