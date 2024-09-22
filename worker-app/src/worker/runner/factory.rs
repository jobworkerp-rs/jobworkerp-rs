use super::impls::builtin;
use super::Runner;
use crate::plugins::{runner::UsePluginRunner, Plugins};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use command_utils::util::result::ToOption;
use debug_stub_derive::DebugStub;
use infra::infra::job::rows::{JobqueueAndCodec, UseJobqueueAndCodec};
use libloading::Library;
use proto::jobworkerp::data::{
    CommandOperation, DockerOperation, GrpcUnaryOperation, HttpRequestOperation, OperationType,
    PluginOperation, WorkerData, WorkerSchemaData,
};
use std::sync::Arc;
use tracing;

#[async_trait]
pub trait RunnerFactory: UsePluginRunner + Send + Sync {
    // only use non static worker
    async fn create(
        &self,
        schema: &WorkerSchemaData,
        worker: &WorkerData,
    ) -> Result<Box<dyn Runner + Send + Sync>> {
        match OperationType::try_from(schema.operation_type).to_option() {
            Some(OperationType::SlackInternal) => Ok(Box::new(builtin::SLACK_RUNNER.clone())),
            Some(OperationType::Command) => {
                let op =
                    JobqueueAndCodec::deserialize_message::<CommandOperation>(&worker.operation)?;
                Ok(Box::new(super::impls::command::CommandRunnerImpl {
                    process: None,
                    command: Box::new(op.name.clone()),
                }))
            }
            Some(OperationType::Plugin) => {
                let op =
                    JobqueueAndCodec::deserialize_message::<PluginOperation>(&worker.operation)?;
                tracing::debug!("plugin runner: {}", &op.name);
                // only load plugin
                self.find_and_init_plugin_runner_by_name(&op.name)
                    .ok_or_else(|| {
                        tracing::error!("plugin not found: {:?}", &op);
                        anyhow!("plugin not found: {:?}", &worker)
                    })
            }
            // Some(WorkerType::K8s) => Err(anyhow!("k8s not implemented")),
            Some(OperationType::GrpcUnary) => {
                let op =
                    JobqueueAndCodec::deserialize_message::<GrpcUnaryOperation>(&worker.operation)?;
                super::impls::grpc_unary::GrpcUnaryRunner::new(&op.host, &op.port)
                    .await
                    .map(|r| Box::new(r) as Box<dyn Runner + Send + Sync>)
            }
            Some(OperationType::HttpRequest) => {
                let op = JobqueueAndCodec::deserialize_message::<HttpRequestOperation>(
                    &worker.operation,
                )?;
                super::impls::request::RequestRunner::new(&op.base_url)
                    .map(|r| Box::new(r) as Box<dyn Runner + Send + Sync>)
            }
            Some(OperationType::Docker) => {
                let op =
                    JobqueueAndCodec::deserialize_message::<DockerOperation>(&worker.operation)?;
                if worker.use_static {
                    // XXX not tested
                    super::impls::docker::DockerExecRunner::new(&op.into())
                        .await
                        .map(|r| Box::new(r) as Box<dyn Runner + Send + Sync>)
                } else {
                    // create docker one-time runner (TODO exec runner for static)
                    super::impls::docker::DockerRunner::new(&op.into())
                        .await
                        .map(|r| Box::new(r) as Box<dyn Runner + Send + Sync>)
                }
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
