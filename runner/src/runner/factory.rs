use std::sync::Arc;

use super::{
    command::CommandRunnerImpl,
    docker::{DockerExecRunner, DockerRunner},
    grpc_unary::GrpcUnaryRunner,
    plugins::{PluginLoader, Plugins},
    python::PythonCommandRunner,
    request::RequestRunner,
    simple_workflow::SimpleWorkflowRunnerSpecImpl,
    slack::SlackPostMessageRunner,
    RunnerSpec,
};
use anyhow::Result;
use proto::jobworkerp::data::RunnerType;

#[derive(Debug)]
pub struct RunnerSpecFactory {
    // TODO to map?
    pub plugins: Arc<Plugins>,
}

impl RunnerSpecFactory {
    pub fn new(plugins: Arc<Plugins>) -> Self {
        Self { plugins }
    }
    pub async fn load_plugins(&self) -> Vec<(String, String)> {
        self.plugins.load_plugin_files_from_env().await
    }
    pub async fn unload_plugins(&self, name: &str) -> Result<bool> {
        self.plugins.runner_plugins().write().await.unload(name)
    }
    // use_static: need to specify correctly to create for running
    pub async fn create_plugin_by_name(
        &self,
        name: &str,
        use_static: bool,
    ) -> Option<Box<dyn RunnerSpec + Send + Sync>> {
        match RunnerType::from_str_name(name) {
            Some(RunnerType::Command) => {
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
            Some(RunnerType::SimpleWorkflow) => {
                Some(Box::new(SimpleWorkflowRunnerSpecImpl::new())
                    as Box<dyn RunnerSpec + Send + Sync>)
            }
            _ => self
                .plugins
                .runner_plugins()
                .write()
                .await
                .find_plugin_runner_by_name(name)
                .map(|r| Box::new(r) as Box<dyn RunnerSpec + Send + Sync>),
        }
    }
}

pub trait UseRunnerSpecFactory {
    fn plugin_runner_factory(&self) -> &RunnerSpecFactory;
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn test_new() {
        let runner_factory = RunnerSpecFactory::new(Arc::new(Plugins::new()));
        runner_factory.load_plugins().await;
        assert_eq!(
            runner_factory
                .plugins
                .runner_plugins()
                .read()
                .await
                .plugin_loaders()
                .len(),
            2 // Test, Hello
        );
        // from builtins
        assert_eq!(
            runner_factory
                .create_plugin_by_name(RunnerType::GrpcUnary.as_str_name(), false)
                .await
                .unwrap()
                .name(),
            "GRPC_UNARY"
        );
        // from plugins
        assert_eq!(
            runner_factory
                .create_plugin_by_name("Test", false)
                .await
                .unwrap()
                .name(),
            "Test"
        );
    }

    #[tokio::test]
    async fn test_create_by_name() {
        let runner_factory = RunnerSpecFactory::new(Arc::new(Plugins::new()));
        runner_factory.load_plugins().await;
        let runner = runner_factory
            .create_plugin_by_name(RunnerType::Command.as_str_name(), false)
            .await
            .unwrap();
        assert_eq!(runner.name(), "COMMAND");
    }
}
