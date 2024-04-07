use super::impls::builtin;
use super::Runner;
use crate::plugins::{runner::UsePluginRunner, Plugins};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use command_utils::util::option::FlatMap as _;
use debug_stub_derive::DebugStub;
use libloading::Library;
use proto::jobworkerp::data::worker_operation::Operation;
use proto::jobworkerp::data::WorkerData;
use std::sync::Arc;
use tracing;

#[async_trait]
pub trait RunnerFactory: UsePluginRunner + Send + Sync {
    // only use non static worker
    async fn create(&self, worker: &WorkerData) -> Result<Box<dyn Runner + Send + Sync>> {
        let operation = worker.operation.clone();
        match operation.flat_map(|o| o.operation) {
            Some(Operation::SlackInternal(_)) => Ok(Box::new(builtin::SLACK_RUNNER.clone())),
            Some(Operation::Command(c)) => Ok(Box::new(super::impls::command::CommandRunnerImpl {
                process: None,
                command: Box::new(c.name.clone()),
            })),
            Some(Operation::Plugin(p)) => {
                tracing::debug!("plugin runner: {}", &p.name);
                self.find_and_init_plugin_runner_by_name(&p.name)
                    .ok_or_else(|| {
                        tracing::error!("plugin not found: {:?}", &p);
                        anyhow!("plugin not found: {:?}", &worker)
                    })
            }
            // Some(WorkerType::K8s) => Err(anyhow!("k8s not implemented")),
            Some(Operation::GrpcUnary(g)) => {
                super::impls::grpc_unary::GrpcUnaryRunner::new(&g.host, &g.port)
                    .await
                    .map(|r| Box::new(r) as Box<dyn Runner + Send + Sync>)
            }
            Some(Operation::HttpRequest(h)) => {
                let base_url = h.base_url.clone();
                super::impls::request::RequestRunner::new(&base_url)
                    .map(|r| Box::new(r) as Box<dyn Runner + Send + Sync>)
            }
            Some(Operation::Docker(d)) => {
                if worker.use_static {
                    // XXX not tested
                    super::impls::docker::DockerExecRunner::new(&d.into())
                        .await
                        .map(|r| Box::new(r) as Box<dyn Runner + Send + Sync>)
                } else {
                    // create docker one-time runner (TODO exec runner for static)
                    super::impls::docker::DockerRunner::new(&d.into())
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
