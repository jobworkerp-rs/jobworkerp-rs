use super::common::GrpcConnection;
use crate::jobworkerp::runner::grpc::GrpcArgs;
use anyhow::{Result, anyhow};
use futures::stream::BoxStream;
use jobworkerp_base::error::JobWorkerError;
use net_utils::grpc::RawBytesCodec;
use proto::jobworkerp::data::{ResultOutputItem, Trailer, result_output_item};
use std::collections::HashMap;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

impl GrpcConnection {
    /// Execute a gRPC server streaming call and return a stream of ResultOutputItem.
    pub async fn call_server_streaming(
        &mut self,
        args: &GrpcArgs,
        cancellation_token: CancellationToken,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
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
            "Sending gRPC server streaming request to {}, payload size: {} bytes",
            args.method,
            request_len
        );

        // Initiate server streaming call with cancellation support
        let response = if args.timeout > 0 {
            let timeout_duration = Duration::from_millis(args.timeout as u64);
            tokio::select! {
                timeout_result = tokio::time::timeout(timeout_duration, client.server_streaming(request, method, codec)) => {
                    timeout_result
                        .map(|r| r.inspect_err(|e| tracing::warn!("grpc streaming request error: status={:?}", e)))
                        .map_err(|_| tonic::Status::new(tonic::Code::DeadlineExceeded, format!("Request timed out after {} ms", args.timeout)))?
                }
                _ = cancellation_token.cancelled() => {
                    return Err(JobWorkerError::CancelledError("gRPC streaming request was cancelled".to_string()).into());
                }
            }
        } else {
            tokio::select! {
                response_result = client.server_streaming(request, method, codec) => {
                    response_result.inspect_err(|e| tracing::warn!("grpc streaming request error: status={:?}", e))
                }
                _ = cancellation_token.cancelled() => {
                    return Err(JobWorkerError::CancelledError("gRPC streaming request was cancelled".to_string()).into());
                }
            }
        };

        match response {
            Ok(response) => {
                let initial_metadata = GrpcConnection::metadata_map_to_hashmap(response.metadata());
                tracing::debug!(
                    "gRPC server streaming started, initial metadata: {:?}",
                    initial_metadata
                );

                let mut streaming = response.into_inner();

                let stream = async_stream::stream! {
                    loop {
                        tokio::select! {
                            msg = streaming.message() => {
                                match msg {
                                    Ok(Some(data)) => {
                                        yield ResultOutputItem {
                                            item: Some(result_output_item::Item::Data(data)),
                                        };
                                    }
                                    Ok(None) => {
                                        // Stream ended normally
                                        let trailer_metadata = streaming.trailers().await
                                            .map(|t| t.map(|m| GrpcConnection::metadata_map_to_hashmap(&m)).unwrap_or_default())
                                            .unwrap_or_default();
                                        yield ResultOutputItem {
                                            item: Some(result_output_item::Item::End(Trailer {
                                                metadata: trailer_metadata,
                                            })),
                                        };
                                        break;
                                    }
                                    Err(status) => {
                                        tracing::warn!("gRPC streaming error: status={:?}", status);
                                        let mut trailer_metadata = GrpcConnection::metadata_map_to_hashmap(status.metadata());
                                        // Propagate gRPC status code and message via metadata
                                        trailer_metadata.insert(
                                            "grpc-status".to_string(),
                                            (status.code() as i32).to_string(),
                                        );
                                        if !status.message().is_empty() {
                                            trailer_metadata.insert(
                                                "grpc-message".to_string(),
                                                status.message().to_string(),
                                            );
                                        }
                                        yield ResultOutputItem {
                                            item: Some(result_output_item::Item::End(Trailer {
                                                metadata: trailer_metadata,
                                            })),
                                        };
                                        break;
                                    }
                                }
                            }
                            _ = cancellation_token.cancelled() => {
                                tracing::info!("gRPC streaming cancelled");
                                let mut cancel_metadata = HashMap::new();
                                cancel_metadata.insert(
                                    "grpc-status".to_string(),
                                    (tonic::Code::Cancelled as i32).to_string(),
                                );
                                cancel_metadata.insert(
                                    "grpc-message".to_string(),
                                    "Stream cancelled by client".to_string(),
                                );
                                yield ResultOutputItem {
                                    item: Some(result_output_item::Item::End(Trailer {
                                        metadata: cancel_metadata,
                                    })),
                                };
                                break;
                            }
                        }
                    }
                };

                Ok(Box::pin(stream) as BoxStream<'static, ResultOutputItem>)
            }
            Err(status) => {
                tracing::warn!("gRPC server streaming failed: status={:?}", status);
                Err(status.into())
            }
        }
    }
}
