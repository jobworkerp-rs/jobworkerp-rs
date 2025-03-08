use crate::jobworkerp::runner::{GrpcUnaryArgs, GrpcUnaryRunnerSettings};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::stream::BoxStream;
use jobworkerp_base::{
    codec::{ProstMessageCodec, UseProstCodec},
    error::JobWorkerError,
};
use proto::jobworkerp::data::{ResultOutputItem, RunnerType};
use tonic::{transport::Channel, IntoRequest};

use super::RunnerTrait;

/// grpc unary request runner.
/// specify protobuf payload as arg in enqueue.
/// return response as single byte vector payload (not interpret, not extract vector etc).
#[derive(Debug, Clone)]
pub struct GrpcUnaryRunner {
    pub client: Option<tonic::client::Grpc<Channel>>,
}

impl GrpcUnaryRunner {
    // TODO Error type
    pub fn new() -> Self {
        Self { client: None }
    }
    pub async fn create(&mut self, host: &str, port: &u32) -> Result<()> {
        let conn = tonic::transport::Endpoint::new(format!("{}:{}", host, port))?
            .connect()
            .await?;
        self.client = Some(tonic::client::Grpc::new(conn));
        Ok(())
    }
}

impl Default for GrpcUnaryRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RunnerTrait for GrpcUnaryRunner {
    fn name(&self) -> String {
        RunnerType::GrpcUnary.as_str_name().to_string()
    }
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        let req = ProstMessageCodec::deserialize_message::<GrpcUnaryRunnerSettings>(&settings)?;
        self.create(&req.host, &req.port).await
    }
    // args: {headers:{<headers map>}, queries:[<query string array>], body: <body string or struct>}
    async fn run(&mut self, args: &[u8]) -> Result<Vec<Vec<u8>>> {
        if let Some(client) = self.client.as_mut() {
            let req = ProstMessageCodec::deserialize_message::<GrpcUnaryArgs>(args)?;
            let codec = tonic::codec::ProstCodec::default();
            // todo
            // let mut client = tonic::client::Grpc::new(self.conn.clone());
            // let bytes = bytes::Bytes::from(req.request);
            client.ready().await.map_err(|e| {
                tonic::Status::new(
                    tonic::Code::Unknown,
                    format!("Service was not ready: {:?}", e),
                )
            })?;
            client
                .unary(
                    req.request.clone().into_request(),
                    req.path.clone().try_into()?,
                    codec,
                )
                .await
                .map_err(|e| {
                    tracing::warn!("grpc request error: status={:?}", e);
                    JobWorkerError::TonicClientError(e).into()
                })
                .map(|r| {
                    tracing::info!("grpc unary runner result: {:?}", &r);
                    vec![r.into_inner()]
                })
        } else {
            Err(anyhow!("grpc client is not initialized"))
        }
    }
    async fn run_stream(&mut self, arg: &[u8]) -> Result<BoxStream<'static, ResultOutputItem>> {
        // default implementation (return empty)
        let _ = arg;
        Err(anyhow::anyhow!("not implemented"))
    }

    async fn cancel(&mut self) {
        tracing::warn!("cannot cancel grpc request until timeout")
    }
    fn runner_settings_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/grpc_unary_runner.proto").to_string()
    }
    fn job_args_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/grpc_unary_args.proto").to_string()
    }
    // TODO resolve by reflection api if possible
    fn result_output_proto(&self) -> Option<String> {
        None
    }
    fn output_as_stream(&self) -> Option<bool> {
        Some(false)
    }
}

#[tokio::test]
#[ignore] // need to start front server and fix handling empty stream...
async fn run_request() -> Result<()> {
    // common::util::tracing::tracing_init_test(tracing::Level::INFO);
    let mut runner = GrpcUnaryRunner::new();
    runner.create("http://localhost", &9000u32).await?;
    let arg = crate::jobworkerp::runner::GrpcUnaryArgs {
        path: "/jobworkerp.service.JobService/Count".to_string(),
        // path: "/jobworkerp.service.WorkerService/FindList".to_string(),
        request: b"".to_vec(),
    };
    let arg = ProstMessageCodec::serialize_message(&arg)?;
    let res = runner.run(&arg).await;
    println!("arg: {:?}, res: {:?}", arg, res); // XXX missing response error
                                                // TODO
    assert!(res.is_ok());
    Ok(())
}
