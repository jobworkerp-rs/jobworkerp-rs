use super::common::GrpcConnection;
use crate::jobworkerp::runner::grpc::{GrpcArgs, GrpcUnaryResult, grpc_unary_result};
use anyhow::{Result, anyhow};
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_base::error::JobWorkerError;
use net_utils::grpc::RawBytesCodec;
use std::collections::HashMap;
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

        let method_path = GrpcConnection::normalize_method_path(&args.method)?;

        // Prepare request (ready + serialization) and execute call within timeout/cancellation scope
        let call_fut = async {
            client.ready().await.map_err(|e| {
                anyhow::anyhow!(tonic::Status::new(
                    tonic::Code::Unknown,
                    format!("Service was not ready: {e:?}"),
                ))
            })?;

            let request_bytes = self
                .prepare_request_bytes(&args.method, &args.request)
                .await?;
            let request_len = request_bytes.len();
            let request = self.build_request(request_bytes, &args.metadata);

            tracing::debug!(
                "Sending gRPC unary request to {}, payload size: {} bytes",
                args.method,
                request_len
            );

            client
                .unary(request, method_path, codec)
                .await
                .inspect_err(|e| tracing::warn!("grpc request error: status={:?}", e))
                .map_err(Into::into)
        };

        let response: Result<tonic::Response<Vec<u8>>> = if args.timeout > 0 {
            let timeout_duration = Duration::from_millis(args.timeout as u64);
            tokio::select! {
                timeout_result = tokio::time::timeout(timeout_duration, call_fut) => {
                    timeout_result
                        .map_err(|_| anyhow::anyhow!(tonic::Status::new(tonic::Code::DeadlineExceeded, format!("Request timed out after {} ms", args.timeout))))?
                }
                _ = cancellation_token.cancelled() => {
                    return Err(JobWorkerError::CancelledError("gRPC request was cancelled".to_string()).into());
                }
            }
        } else {
            tokio::select! {
                response_result = call_fut => {
                    response_result
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
                let response_data = if args.as_json
                    && self.use_reflection
                    && self.reflection_client.is_some()
                {
                    match self
                        .convert_response_to_json(&args.method, &response_body)
                        .await
                    {
                        Ok(json_str) => {
                            tracing::debug!("Converted response to JSON: {}", json_str);
                            Some(grpc_unary_result::ResponseData::JsonBody(json_str))
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Failed to convert response to JSON, using raw bytes: {}",
                                e
                            );
                            Some(grpc_unary_result::ResponseData::Body(response_body))
                        }
                    }
                } else {
                    if args.as_json {
                        tracing::warn!(
                            "JSON conversion requested but reflection not available, returning raw bytes"
                        );
                    }
                    Some(grpc_unary_result::ResponseData::Body(response_body))
                };

                GrpcUnaryResult {
                    metadata,
                    code: tonic::Code::Ok as i32,
                    message: None,
                    response_data,
                }
            }
            // gRPC errors are wrapped in GrpcUnaryResult (not propagated as Err) so that
            // the job system records the runner as "executed successfully" while the
            // actual gRPC status is preserved in the result's code/message fields.
            Err(e) => {
                tracing::warn!("grpc request error: {:?}", e);
                if let Some(status) = e.downcast_ref::<tonic::Status>() {
                    GrpcUnaryResult {
                        metadata: GrpcConnection::metadata_map_to_hashmap(status.metadata()),
                        code: status.code() as i32,
                        message: Some(status.message().to_string()),
                        response_data: Some(grpc_unary_result::ResponseData::Body(
                            status.details().to_vec(),
                        )),
                    }
                } else {
                    // Non-tonic errors (e.g. connection failures) have no gRPC details
                    GrpcUnaryResult {
                        metadata: HashMap::new(),
                        code: tonic::Code::Internal as i32,
                        message: Some(e.to_string()),
                        response_data: None,
                    }
                }
            }
        };

        let body_size = match &res.response_data {
            Some(grpc_unary_result::ResponseData::Body(b)) => b.len(),
            Some(grpc_unary_result::ResponseData::JsonBody(j)) => j.len(),
            None => 0,
        };
        tracing::info!(
            "grpc unary runner completed: code={}, body_size={} bytes",
            res.code,
            body_size
        );
        tracing::debug!("grpc unary runner result detail: {:?}", &res);
        ProstMessageCodec::serialize_message(&res)
    }
}
