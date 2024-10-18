use anyhow::Result;
use tonic::async_trait;

#[async_trait]
pub trait Runner: Send + Sync {
    async fn name(&self) -> String;
    async fn run(&mut self, arg: &[u8]) -> Result<Vec<Vec<u8>>>;
    async fn cancel(&mut self);
    fn operation_proto(&self) -> String;
    fn job_args_proto(&self) -> String;
    fn use_job_result(&self) -> bool;
}
