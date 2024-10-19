use super::super::Runner;
use anyhow::Result;
use async_trait::async_trait;
use infra::{
    error::JobWorkerError,
    infra::job::rows::{JobqueueAndCodec, UseJobqueueAndCodec},
};
use proto::jobworkerp::data::GrpcUnaryArg;
use tonic::{transport::Channel, IntoRequest};

/// grpc unary request runner.
/// specify url(http or https) and path (grpc rpc method) as json as 'operation' value in creation of worker.
/// specify protobuf payload as arg in enqueue.
/// return response as single byte vector payload (not interpret, not extract vector etc).
#[derive(Debug, Clone)]
pub struct GrpcUnaryRunner {
    pub client: tonic::client::Grpc<Channel>,
}

impl GrpcUnaryRunner {
    // TODO Error type
    // operation: parse as json string: url
    pub async fn new(host: &str, port: &u32) -> Result<GrpcUnaryRunner> {
        let conn = tonic::transport::Endpoint::new(format!("{}:{}", host, port))?
            .connect()
            .await?;
        Ok(GrpcUnaryRunner {
            client: tonic::client::Grpc::new(conn),
        })
    }
}

#[async_trait]
// arg: {headers:{<headers map>}, queries:[<query string array>], body: <body string or struct>}
impl Runner for GrpcUnaryRunner {
    async fn name(&self) -> String {
        format!("GrpcUnaryRunner")
    }
    async fn run(&mut self, arg: &[u8]) -> Result<Vec<Vec<u8>>> {
        let req = JobqueueAndCodec::deserialize_message::<GrpcUnaryArg>(arg)?;
        let codec = tonic::codec::ProstCodec::default();
        // todo
        // let mut client = tonic::client::Grpc::new(self.conn.clone());
        // let bytes = bytes::Bytes::from(req.request);
        self.client.ready().await.map_err(|e| {
            tonic::Status::new(
                tonic::Code::Unknown,
                format!("Service was not ready: {:?}", e),
            )
        })?;
        self.client
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
    }

    async fn cancel(&mut self) {
        tracing::warn!("cannot cancel grpc request until timeout")
    }
    fn operation_proto(&self) -> String {
        include_str!("../../../../protobuf/grpc_unary_operation.proto").to_string()
    }
    fn job_args_proto(&self) -> String {
        include_str!("../../../../protobuf/grpc_unary_args.proto").to_string()
    }
    fn use_job_result(&self) -> bool {
        false
    }
}

#[tokio::test]
#[ignore] // need to start front server and fix handling empty stream...
async fn run_request() -> Result<()> {
    // common::util::tracing::tracing_init_test(tracing::Level::INFO);
    use proto::jobworkerp;
    let mut runner = GrpcUnaryRunner::new("http://localhost", &9000u32).await?;
    let arg = jobworkerp::data::GrpcUnaryArg {
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
