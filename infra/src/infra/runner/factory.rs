use crate::infra::plugins::{PluginLoader, Plugins};
use anyhow::Result;
use jobworkerp_runner::runner::command::CommandRunnerImpl;
use jobworkerp_runner::runner::docker::{DockerExecRunner, DockerRunner};
use jobworkerp_runner::runner::grpc_unary::GrpcUnaryRunner;
use jobworkerp_runner::runner::python::PythonCommandRunner;
use jobworkerp_runner::runner::request::RequestRunner;
use jobworkerp_runner::runner::slack::SlackPostMessageRunner;
use jobworkerp_runner::runner::RunnerTrait;
use proto::jobworkerp::data::RunnerType;

#[derive(Debug)]
pub struct RunnerFactory {
    // TODO to map?
    plugins: Plugins,
}

impl RunnerFactory {
    pub fn new() -> Self {
        Self {
            plugins: Plugins::new(),
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

impl Default for RunnerFactory {
    fn default() -> Self {
        Self::new()
    }
}

pub trait UseRunnerFactory {
    fn runner_factory(&self) -> &RunnerFactory;
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn test_new() {
        std::env::set_var("PLUGINS_RUNNER_DIR", "../target/debug");
        let runner_factory = RunnerFactory::new();
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
                .create_by_name(RunnerType::GrpcUnary.as_str_name(), false)
                .await
                .unwrap()
                .name(),
            "GRPC_UNARY"
        );
        // from plugins
        assert_eq!(
            runner_factory
                .create_by_name("Test", false)
                .await
                .unwrap()
                .name(),
            "Test"
        );
    }

    #[tokio::test]
    async fn test_create_by_name() {
        let runner_factory = RunnerFactory::new();
        runner_factory.load_plugins().await;
        let runner = runner_factory
            .create_by_name(RunnerType::Command.as_str_name(), false)
            .await
            .unwrap();
        assert_eq!(runner.name(), "COMMAND");
    }
}
