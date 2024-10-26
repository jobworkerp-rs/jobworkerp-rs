use crate::jobworkerp::runner::{GrpcUnaryArg, GrpcUnaryOperation};
use crate::{
    error::JobWorkerError,
    infra::job::rows::{JobqueueAndCodec, UseJobqueueAndCodec},
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use proto::jobworkerp::data::RunnerType;
use tonic::{transport::Channel, IntoRequest};

use super::Runner;

/// grpc unary request runner.
/// specify url(http or https) and path (grpc rpc method) as json as 'operation' value in creation of worker.
/// specify protobuf payload as arg in enqueue.
/// return response as single byte vector payload (not interpret, not extract vector etc).
#[derive(Debug, Clone)]
pub struct GrpcUnaryRunner {
    pub client: Option<tonic::client::Grpc<Channel>>,
}

impl GrpcUnaryRunner {
    // TODO Error type
    // operation: parse as json string: url
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

// arg: {headers:{<headers map>}, queries:[<query string array>], body: <body string or struct>}
#[async_trait]
impl Runner for GrpcUnaryRunner {
    fn name(&self) -> String {
        RunnerType::GrpcUnary.as_str_name().to_string()
    }
    async fn load(&mut self, operation: Vec<u8>) -> Result<()> {
        let req = JobqueueAndCodec::deserialize_message::<GrpcUnaryOperation>(&operation)?;
        self.create(&req.host, &req.port).await
    }
    async fn run(&mut self, arg: &[u8]) -> Result<Vec<Vec<u8>>> {
        if let Some(client) = self.client.as_mut() {
            let req = JobqueueAndCodec::deserialize_message::<GrpcUnaryArg>(arg)?;
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

    async fn cancel(&mut self) {
        tracing::warn!("cannot cancel grpc request until timeout")
    }
    fn operation_proto(&self) -> String {
        include_str!("../../../protobuf/jobworkerp/runner/grpc_unary_operation.proto").to_string()
    }
    fn job_args_proto(&self) -> String {
        include_str!("../../../protobuf/jobworkerp/runner/grpc_unary_args.proto").to_string()
    }
    // TODO resolve by reflection api if possible
    fn result_output_proto(&self) -> Option<String> {
        None
    }
    fn use_job_result(&self) -> bool {
        false
    }
}

#[tokio::test]
#[ignore] // need to start front server and fix handling empty stream...
async fn run_request() -> Result<()> {
    // common::util::tracing::tracing_init_test(tracing::Level::INFO);
    let mut runner = GrpcUnaryRunner::new();
    runner.create("http://localhost", &9000u32).await?;
    let arg = crate::jobworkerp::runner::GrpcUnaryArg {
        path: "/jobworkerp.service.JobService/Count".to_string(),
        // path: "/jobworkerp.service.WorkerService/FindList".to_string(),
        request: b"".to_vec(),
    };
    let arg = JobqueueAndCodec::serialize_message(&arg);
    let res = runner.run(&arg).await;
    println!("arg: {:?}, res: {:?}", arg, res); // XXX missing response error
                                                // TODO
    assert!(res.is_ok());
    Ok(())
}
