use anyhow::Result;
use futures::stream::BoxStream;
use proto::jobworkerp::data::ResultOutputItem;
use tonic::async_trait;

pub mod command;
pub mod docker;
pub mod grpc_unary;
pub mod k8s_job;
pub mod plugins;
pub mod python;
pub mod request;
pub mod slack;

#[async_trait]
pub trait RunnerTrait: Send + Sync {
    fn name(&self) -> String;
    async fn load(&mut self, settings: Vec<u8>) -> Result<()>;
    async fn run(&mut self, arg: &[u8]) -> Result<Vec<Vec<u8>>>;
    // only implement for stream runner (output_as_stream() == true)
    async fn run_stream(&mut self, arg: &[u8]) -> Result<BoxStream<'static, ResultOutputItem>>;
    async fn cancel(&mut self);
    fn runner_settings_proto(&self) -> String;
    fn job_args_proto(&self) -> String;
    fn result_output_proto(&self) -> Option<String>;
    // run with run_stream() if true
    fn output_as_stream(&self) -> Option<bool>;
}
