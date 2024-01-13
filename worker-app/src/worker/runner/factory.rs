use super::impls::builtin::BuiltinRunner;
use super::impls::builtin::BuiltinRunnerTrait;
use super::Runner;
use crate::plugins::{runner::UsePluginRunner, Plugins};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use command_utils::util::result::ToOption;
use debug_stub_derive::DebugStub;
use infra::error::JobWorkerError;
use libloading::Library;
use proto::jobworkerp::data::{RunnerType, WorkerData};
use std::sync::Arc;
use tracing;

#[async_trait]
pub trait RunnerFactory: UsePluginRunner + Send + Sync {
    // only use non static worker
    async fn create(&self, worker: &WorkerData) -> Result<Box<dyn Runner + Send + Sync>> {
        let operation = worker.operation.clone();
        match RunnerType::try_from(worker.r#type).to_option() {
            Some(RunnerType::Builtin) => BuiltinRunner::find_runner_by_operation(&operation).ok_or(
                JobWorkerError::InvalidParameter(format!(
                    "built-in worker not found: {:?}",
                    worker
                ))
                .into(),
            ),
            Some(RunnerType::Command) => Ok(Box::new(super::impls::command::CommandRunnerImpl {
                process: None,
                command: Box::new(operation.clone()),
            })),
            Some(RunnerType::Plugin) => {
                tracing::debug!("plugin runner: {}", &operation);
                self.find_and_init_plugin_runner_by_name(&operation)
                    .ok_or_else(|| {
                        tracing::error!("plugin not found: {:?}", &operation);
                        anyhow!("plugin not found: {:?}", &worker)
                    })
            }
            // Some(WorkerType::K8s) => Err(anyhow!("k8s not implemented")),
            Some(RunnerType::GrpcUnary) => {
                super::impls::grpc_unary::GrpcUnaryRunner::new(operation.as_str())
                    .await
                    .map(|r| Box::new(r) as Box<dyn Runner + Send + Sync>)
            }
            Some(RunnerType::Request) => super::impls::request::RequestRunner::new(&operation)
                .map(|r| Box::new(r) as Box<dyn Runner + Send + Sync>),
            Some(RunnerType::Docker) =>
            // create docker one-time runner (TODO exec runner)
            {
                super::impls::docker::DockerRunner::new(&operation)
                    .await
                    .map(|r| Box::new(r) as Box<dyn Runner + Send + Sync>)
            }
            None => Err(anyhow!("not implemented")),
        }
    }
}

#[derive(DebugStub, Clone)]
pub struct RunnerFactoryImpl {
    #[debug_stub = "[Plugins]"]
    plugins: Arc<Plugins>,
}

impl UsePluginRunner for RunnerFactoryImpl {
    fn runner_plugins(&self) -> &Vec<(String, Library)> {
        self.plugins.runner_plugins()
    }
}
impl RunnerFactory for RunnerFactoryImpl {}

impl RunnerFactoryImpl {
    pub fn new(plugins: Arc<Plugins>) -> Self {
        Self { plugins }
    }
}
