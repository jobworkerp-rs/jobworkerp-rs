use super::impls::builtin;
use anyhow::Result;
use async_trait::async_trait;
use debug_stub_derive::DebugStub;
use infra::infra::{
    job::rows::{JobqueueAndCodec, UseJobqueueAndCodec},
    plugins::{Plugins, UsePlugins},
    runner::Runner,
};
use proto::jobworkerp::data::{
    CommandOperation, DockerOperation, GrpcUnaryOperation, HttpRequestOperation, OperationType,
    WorkerData, WorkerSchemaData,
};
use std::sync::Arc;
use tracing;

#[async_trait]
pub trait RunnerFactory: UsePlugins + Send + Sync {
    // only use non static worker
    async fn create(
        &self,
        schema: &WorkerSchemaData,
        worker: &WorkerData,
    ) -> Result<Box<dyn Runner + Send + Sync>> {
        // TODO treat all runner as plugins
        match OperationType::from_str_name(schema.name.as_str()) {
            Some(OperationType::SlackInternal) => Ok(Box::new(builtin::SLACK_RUNNER.clone())),
            Some(OperationType::Command) => {
                let op =
                    JobqueueAndCodec::deserialize_message::<CommandOperation>(&worker.operation)?;
                Ok(Box::new(super::impls::command::CommandRunnerImpl {
                    process: None,
                    command: Box::new(op.name.clone()),
                }))
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
            Some(OperationType::Plugin) | None => {
                // let op =
                //     JobqueueAndCodec::deserialize_message::<PluginOperation>(&worker.operation)?;
                tracing::debug!("plugin runner: {}", &schema.name);
                // only load plugin
                self.plugins()
                    .runner_plugins()
                    .write()
                    .await
                    .load_runner_by_name(&schema.name, worker.operation.clone())
                    .await
            } //
              // None => Err(anyhow!("not implemented")),
        }
    }
}

#[derive(DebugStub, Clone)]
pub struct RunnerFactoryImpl {
    #[debug_stub = "[Plugins]"]
    plugins: Arc<Plugins>,
}

impl UsePlugins for RunnerFactoryImpl {
    fn plugins(&self) -> &Plugins {
        &self.plugins
    }
}

impl RunnerFactory for RunnerFactoryImpl {}

impl RunnerFactoryImpl {
    pub fn new(plugins: Arc<Plugins>) -> Self {
        Self { plugins }
    }
}
