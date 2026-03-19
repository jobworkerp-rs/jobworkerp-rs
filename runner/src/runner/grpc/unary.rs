use super::common::GrpcConnection;
use crate::jobworkerp::runner::grpc::{GrpcArgs, GrpcUnaryResult};
use anyhow::{Result, anyhow};
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_base::error::JobWorkerError;
use net_utils::grpc::RawBytesCodec;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

impl GrpcConnection {
    /// Execute a gRPC unary call and return serialized GrpcUnaryResult.
    pub async fn call_unary(
        &mut self,
        args: &GrpcArgs,
        cancellation_token: CancellationToken,
    ) -> Result<Vec<u8>> {
        let mut client = self
            .client
            .clone()
            .ok_or_else(|| anyhow!("grpc client is not initialized"))?;

        let codec = RawBytesCodec;

        if let Some(size) = self.max_message_size {
            client = client
                .max_decoding_message_size(size)
                .max_encoding_message_size(size);
        }

        client.ready().await.map_err(|e| {
            tonic::Status::new(
                tonic::Code::Unknown,
                format!("Service was not ready: {e:?}"),
            )
        })?;

        let request_bytes = self
            .prepare_request_bytes(&args.method, &args.request)
            .await?;
        let request_len = request_bytes.len();
        let request = self.build_request(request_bytes, &args.metadata);
        let method = GrpcConnection::normalize_method_path(&args.method)?;

        tracing::debug!(
            "Sending gRPC unary request to {}, payload size: {} bytes",
            args.method,
            request_len
        );

        let response = if args.timeout > 0 {
            let timeout_duration = Duration::from_millis(args.timeout as u64);
            tokio::select! {
                timeout_result = tokio::time::timeout(timeout_duration, client.unary(request, method, codec)) => {
                    timeout_result
                        .map(|r| r.inspect_err(|e| tracing::warn!("grpc request error: status={:?}", e)))
                        .map_err(|_| tonic::Status::new(tonic::Code::DeadlineExceeded, format!("Request timed out after {} ms", args.timeout)))?
                }
                _ = cancellation_token.cancelled() => {
                    return Err(JobWorkerError::CancelledError("gRPC request was cancelled".to_string()).into());
                }
            }
        } else {
            tokio::select! {
                response_result = client.unary(request, method, codec) => {
                    response_result.inspect_err(|e| tracing::warn!("grpc request error: status={:?}", e))
                }
                _ = cancellation_token.cancelled() => {
                    return Err(JobWorkerError::CancelledError("gRPC request was cancelled".to_string()).into());
                }
            }
        };

        let res = match response {
            Ok(response) => {
                let metadata = GrpcConnection::metadata_map_to_hashmap(response.metadata());
                let response_body = response.into_inner();
                let mut json_body = None;

                if args.as_json && self.use_reflection && self.reflection_client.is_some() {
                    match self
                        .convert_response_to_json(&args.method, &response_body)
                        .await
                    {
                        Ok(json_str) => {
                            tracing::debug!("Converted response to JSON: {}", json_str);
                            json_body = Some(json_str);
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Failed to convert response to JSON, using raw bytes: {}",
                                e
                            );
                        }
                    }
                } else if args.as_json {
                    tracing::warn!(
                        "JSON conversion requested but reflection not available, returning raw bytes"
                    );
                }

                GrpcUnaryResult {
                    metadata,
                    body: response_body,
                    code: tonic::Code::Ok as i32,
                    message: None,
                    json_body,
                }
            }
            Err(e) => {
                tracing::warn!("grpc request error: status={:?}", e);
                GrpcUnaryResult {
                    metadata: GrpcConnection::metadata_map_to_hashmap(e.metadata()),
                    body: e.details().to_vec(),
                    code: e.code() as i32,
                    message: Some(e.message().to_string()),
                    json_body: None,
                }
            }
        };

        tracing::info!(
            "grpc unary runner completed: code={}, body_size={} bytes",
            res.code,
            res.body.len()
        );
        tracing::debug!("grpc unary runner result detail: {:?}", &res);
        ProstMessageCodec::serialize_message(&res)
    }
}
