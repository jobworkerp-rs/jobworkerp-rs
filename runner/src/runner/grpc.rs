pub mod common;
pub mod streaming;
pub mod unary;

use crate::jobworkerp::runner::grpc::{GrpcArgs, GrpcRunnerSettings, GrpcStreamingResult};
use crate::schema_to_json_string;
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use common::GrpcConnection;
use futures::StreamExt;
use futures::stream::BoxStream;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
#[allow(unused_imports)]
use proto::jobworkerp::data::{
    JobData, JobId, JobResult, ResultOutputItem, RunnerType, StreamingOutputType,
    result_output_item,
};
use std::collections::HashMap;
use tokio_util::sync::CancellationToken;

use super::CollectStreamFuture;
#[allow(unused_imports)]
use super::cancellation::CancelMonitoring;
use super::cancellation_helper::{CancelMonitoringHelper, UseCancelMonitoringHelper};
use super::{RunnerSpec, RunnerTrait};

pub const METHOD_UNARY: &str = "unary";
pub const METHOD_STREAMING: &str = "streaming";

/// Multi-method gRPC runner supporting unary and server streaming calls.
#[derive(Debug, Clone)]
pub struct GrpcRunnerSpecImpl {
    connection: GrpcConnection,
    cancel_helper: Option<CancelMonitoringHelper>,
}

impl GrpcRunnerSpecImpl {
    pub fn new() -> Self {
        Self {
            connection: GrpcConnection::new(),
            cancel_helper: None,
        }
    }

    pub fn new_with_cancel_monitoring(cancel_helper: CancelMonitoringHelper) -> Self {
        Self {
            connection: GrpcConnection::new(),
            cancel_helper: Some(cancel_helper),
        }
    }

    async fn get_cancellation_token(&self) -> CancellationToken {
        if let Some(helper) = &self.cancel_helper {
            helper.get_cancellation_token().await
        } else {
            CancellationToken::new()
        }
    }

    pub fn resolve_method(using: Option<&str>) -> Result<&str> {
        match using {
            Some(METHOD_UNARY) | None => Ok(METHOD_UNARY),
            Some(METHOD_STREAMING) => Ok(METHOD_STREAMING),
            Some(other) => Err(anyhow!(
                "Unknown method '{}' for GRPC runner. Available methods: {}, {}",
                other,
                METHOD_UNARY,
                METHOD_STREAMING
            )),
        }
    }
}

impl Default for GrpcRunnerSpecImpl {
    fn default() -> Self {
        Self::new()
    }
}

/// Aggregate stream items into collected bodies and trailer metadata.
async fn collect_stream_items(
    stream: &mut BoxStream<'static, ResultOutputItem>,
) -> (Vec<Vec<u8>>, HashMap<String, String>, Option<Vec<u8>>) {
    let mut bodies: Vec<Vec<u8>> = Vec::new();
    let mut metadata = HashMap::new();

    while let Some(item) = stream.next().await {
        match item.item {
            Some(result_output_item::Item::Data(data)) => {
                bodies.push(data);
            }
            Some(result_output_item::Item::End(trailer)) => {
                metadata = trailer.metadata;
                break;
            }
            Some(result_output_item::Item::FinalCollected(data)) => {
                return (bodies, metadata, Some(data));
            }
            None => {}
        }
    }

    (bodies, metadata, None)
}

/// Build GrpcStreamingResult from collected stream data.
fn build_streaming_result(
    bodies: Vec<Vec<u8>>,
    metadata: HashMap<String, String>,
    json_body: Option<String>,
) -> GrpcStreamingResult {
    let code = metadata
        .get("grpc-status")
        .and_then(|v| v.parse::<i32>().ok())
        .unwrap_or(tonic::Code::Ok as i32);
    let message = metadata.get("grpc-message").cloned();

    GrpcStreamingResult {
        metadata,
        bodies,
        code,
        message,
        json_body,
    }
}

impl std::fmt::Display for GrpcRunnerSpecImpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "GrpcRunnerSpecImpl {{ reflection: {} }}",
            self.connection.use_reflection
        )
    }
}

impl RunnerSpec for GrpcRunnerSpecImpl {
    fn name(&self) -> String {
        RunnerType::Grpc.as_str_name().to_string()
    }

    fn runner_settings_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/grpc/runner.proto").to_string()
    }

    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        let mut schemas = HashMap::new();

        schemas.insert(
            METHOD_UNARY.to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: include_str!("../../protobuf/jobworkerp/runner/grpc/args.proto")
                    .to_string(),
                result_proto: include_str!(
                    "../../protobuf/jobworkerp/runner/grpc/unary_result.proto"
                )
                .to_string(),
                description: Some("Execute gRPC unary request".to_string()),
                output_type: StreamingOutputType::NonStreaming as i32,
                ..Default::default()
            },
        );

        schemas.insert(
            METHOD_STREAMING.to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: include_str!("../../protobuf/jobworkerp/runner/grpc/args.proto")
                    .to_string(),
                result_proto: include_str!(
                    "../../protobuf/jobworkerp/runner/grpc/streaming_result.proto"
                )
                .to_string(),
                description: Some("Execute gRPC server streaming request".to_string()),
                output_type: StreamingOutputType::Both as i32,
                ..Default::default()
            },
        );

        schemas
    }

    fn settings_schema(&self) -> String {
        schema_to_json_string!(GrpcRunnerSettings, "settings_schema")
    }

    /// Collect streaming results into a single GrpcStreamingResult.
    ///
    /// NOTE: json_body is always None in this path because collect_stream cannot
    /// access GrpcConnection for reflection-based JSON conversion.
    /// Use run() with METHOD_STREAMING for JSON conversion support (as_json=true).
    fn collect_stream(
        &self,
        stream: BoxStream<'static, ResultOutputItem>,
        _using: Option<&str>,
    ) -> CollectStreamFuture {
        Box::pin(async move {
            let mut stream = stream;
            let (bodies, metadata, final_collected) = collect_stream_items(&mut stream).await;

            if let Some(data) = final_collected {
                return Ok((data, metadata));
            }

            let result = build_streaming_result(bodies, metadata, None);
            let serialized = ProstMessageCodec::serialize_message(&result)?;
            Ok((serialized, result.metadata))
        })
    }
}

#[async_trait]
impl RunnerTrait for GrpcRunnerSpecImpl {
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        let settings = ProstMessageCodec::deserialize_message::<GrpcRunnerSettings>(&settings)?;
        self.connection.create(&settings).await
    }

    async fn run(
        &mut self,
        args: &[u8],
        metadata: HashMap<String, String>,
        using: Option<&str>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        let cancellation_token = self.get_cancellation_token().await;

        let result = async {
            let req = ProstMessageCodec::deserialize_message::<GrpcArgs>(args)?;
            let method = Self::resolve_method(using)?;

            match method {
                METHOD_UNARY => self.connection.call_unary(&req, cancellation_token).await,
                METHOD_STREAMING => {
                    let mut stream = self
                        .connection
                        .call_server_streaming(&req, cancellation_token)
                        .await?;

                    let (bodies, trailer_metadata, final_collected) =
                        collect_stream_items(&mut stream).await;

                    if let Some(data) = final_collected {
                        return Ok(data);
                    }

                    // Build JSON body if reflection is available and as_json is requested
                    let mut json_body = None;
                    if req.as_json
                        && self.connection.use_reflection
                        && self.connection.reflection_client.is_some()
                    {
                        let mut json_parts = Vec::new();
                        for body in &bodies {
                            match self
                                .connection
                                .convert_response_to_json(&req.method, body)
                                .await
                            {
                                Ok(json_str) => json_parts.push(json_str),
                                Err(e) => {
                                    tracing::warn!(
                                        "Failed to convert streaming response to JSON: {}",
                                        e
                                    );
                                    break;
                                }
                            }
                        }
                        if json_parts.len() == bodies.len() {
                            json_body = Some(format!("[{}]", json_parts.join(",")));
                        }
                    }

                    let result = build_streaming_result(bodies, trailer_metadata, json_body);
                    Ok(ProstMessageCodec::serialize_message(&result)?)
                }
                _ => unreachable!(),
            }
        }
        .await;

        (result, metadata)
    }

    async fn run_stream(
        &mut self,
        args: &[u8],
        _metadata: HashMap<String, String>,
        using: Option<&str>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        let cancellation_token = self.get_cancellation_token().await;
        let req = ProstMessageCodec::deserialize_message::<GrpcArgs>(args)?;
        let method = Self::resolve_method(using)?;

        match method {
            METHOD_STREAMING => {
                self.connection
                    .call_server_streaming(&req, cancellation_token)
                    .await
            }
            _ => Err(anyhow!(
                "run_stream is not supported for '{}' method",
                method
            )),
        }
    }
}

#[async_trait]
impl super::cancellation::CancelMonitoring for GrpcRunnerSpecImpl {
    async fn setup_cancellation_monitoring(
        &mut self,
        job_id: proto::jobworkerp::data::JobId,
        job_data: &proto::jobworkerp::data::JobData,
    ) -> Result<Option<proto::jobworkerp::data::JobResult>> {
        tracing::debug!(
            "Setting up cancellation monitoring for GrpcRunnerSpecImpl job {}",
            job_id.value
        );

        if let Some(helper) = &mut self.cancel_helper {
            helper.setup_monitoring_impl(job_id, job_data).await
        } else {
            tracing::trace!("No cancel helper available, continuing with normal execution");
            Ok(None)
        }
    }

    async fn cleanup_cancellation_monitoring(&mut self) -> Result<()> {
        tracing::trace!("Cleaning up cancellation monitoring for GrpcRunnerSpecImpl");
        Ok(())
    }

    async fn request_cancellation(&mut self) -> Result<()> {
        if let Some(helper) = &self.cancel_helper {
            let token = helper.get_cancellation_token().await;
            if !token.is_cancelled() {
                token.cancel();
                tracing::info!("GrpcRunnerSpecImpl: cancellation token signaled");
            }
        } else {
            tracing::warn!("GrpcRunnerSpecImpl: no cancellation helper available");
        }
        Ok(())
    }
}

impl UseCancelMonitoringHelper for GrpcRunnerSpecImpl {
    fn cancel_monitoring_helper(&self) -> Option<&CancelMonitoringHelper> {
        self.cancel_helper.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_method_unary() {
        let result = GrpcRunnerSpecImpl::resolve_method(Some("unary"));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "unary");
    }

    #[test]
    fn test_resolve_method_streaming() {
        let result = GrpcRunnerSpecImpl::resolve_method(Some("streaming"));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "streaming");
    }

    #[test]
    fn test_resolve_method_none_defaults_to_unary() {
        let result = GrpcRunnerSpecImpl::resolve_method(None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "unary");
    }

    #[test]
    fn test_resolve_method_unknown() {
        let result = GrpcRunnerSpecImpl::resolve_method(Some("unknown"));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Unknown method 'unknown'")
        );
    }

    #[test]
    fn test_runner_name() {
        let runner = GrpcRunnerSpecImpl::new();
        assert_eq!(runner.name(), "GRPC");
    }

    #[test]
    fn test_method_proto_map_has_both_methods() {
        let runner = GrpcRunnerSpecImpl::new();
        let schemas = runner.method_proto_map();

        assert!(schemas.contains_key("unary"));
        assert!(schemas.contains_key("streaming"));
        assert_eq!(schemas.len(), 2);

        let unary = schemas.get("unary").unwrap();
        assert!(unary.description.as_ref().unwrap().contains("unary"));
        assert!(!unary.args_proto.is_empty());
        assert!(!unary.result_proto.is_empty());
        assert_eq!(unary.output_type, StreamingOutputType::NonStreaming as i32);

        let streaming = schemas.get("streaming").unwrap();
        assert!(
            streaming
                .description
                .as_ref()
                .unwrap()
                .contains("streaming")
        );
        assert!(!streaming.args_proto.is_empty());
        assert!(!streaming.result_proto.is_empty());
        assert_eq!(streaming.output_type, StreamingOutputType::Both as i32);
    }

    #[test]
    fn test_method_json_schema_map_has_both_methods() {
        let runner = GrpcRunnerSpecImpl::new();
        let schemas = runner.method_json_schema_map();

        assert!(schemas.contains_key("unary"));
        assert!(schemas.contains_key("streaming"));
        assert_eq!(schemas.len(), 2);
    }
}
