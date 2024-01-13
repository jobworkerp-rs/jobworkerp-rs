use std::str::FromStr;

use super::super::Runner;
use anyhow::Result;
use async_trait::async_trait;
use infra::error::JobWorkerError;
use serde::Deserialize;
use tonic::{transport::Channel, IntoRequest};

/// grpc unary request runner.
/// specify url(http or https) and path (grpc rpc method) as json as 'operation' value in creation of worker.
/// specify protobuf payload as arg in enqueue.
/// return response as single byte vector payload (not interpret, not extract vector etc).
#[derive(Debug, Clone)]
pub struct GrpcUnaryRunner {
    pub client: tonic::client::Grpc<Channel>,
    pub path: http::uri::PathAndQuery,
}

#[derive(Deserialize, Debug, Clone)]
struct Operation {
    url: String,
    path: String,
}

impl GrpcUnaryRunner {
    // TODO Error type
    // operation: parse as json string: url
    pub async fn new(operation: &str) -> Result<GrpcUnaryRunner> {
        let operation: Operation = serde_json::from_str(operation)?;

        let conn = tonic::transport::Endpoint::new(operation.url.clone())?
            .connect()
            .await?;
        let path = http::uri::PathAndQuery::from_str(
            if operation.path.starts_with('/') {
                operation.path
            } else {
                format!("/{}", operation.path)
            }
            .as_str(),
        )?;
        Ok(GrpcUnaryRunner {
            client: tonic::client::Grpc::new(conn),
            path,
        })
    }
}

#[async_trait]
// arg: {headers:{<headers map>}, queries:[<query string array>], body: <body string or struct>}
impl Runner for GrpcUnaryRunner {
    async fn name(&self) -> String {
        format!("GrpcUnaryRunner: {}", self.path)
    }
    async fn run(&mut self, arg: Vec<u8>) -> Result<Vec<Vec<u8>>> {
        let codec = tonic::codec::ProstCodec::default();
        // todo
        // let mut client = tonic::client::Grpc::new(self.conn.clone());
        let bytes = bytes::Bytes::from(arg);
        self.client.ready().await.map_err(|e| {
            tonic::Status::new(
                tonic::Code::Unknown,
                format!("Service was not ready: {:?}", e),
            )
        })?;
        self.client
            .unary(bytes.into_request(), self.path.clone(), codec)
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
}

#[tokio::test]
#[ignore] // need to start front server and fix handling empty stream...
async fn run_request() -> Result<()> {
    // common::util::tracing::tracing_init_test(tracing::Level::INFO);
    use prost::Message;
    use proto::jobworkerp;
    let mut runner = GrpcUnaryRunner::new(
        r#"{"url":"http://localhost:9000","path":"jobworkerp.service.WorkerService/FindList"}"#,
    )
    .await?;
    let arg = jobworkerp::data::Empty {};
    let res = runner.run(arg.encode_to_vec()).await;
    println!("arg: {:?}, res: {:?}", arg, res); // XXX missing response error
                                                // TODO
    assert!(res.is_ok());
    Ok(())
}
