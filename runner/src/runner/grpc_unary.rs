use crate::jobworkerp::runner::{GrpcUnaryArgs, GrpcUnaryResult, GrpcUnaryRunnerSettings};
use crate::schema_to_json_string;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use base64::Engine;
use futures::stream::BoxStream;
use infra_utils::infra::net::grpc::RawBytesCodec;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use proto::jobworkerp::data::{ResultOutputItem, RunnerType, StreamingOutputType};
use std::collections::HashMap;
use std::time::Duration;
use tonic::{
    metadata::MetadataValue,
    transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity},
};

use super::{RunnerSpec, RunnerTrait};

/// grpc unary request runner.
/// specify protobuf payload as arg in enqueue.
/// return response as single byte vector payload (not interpret, not extract vector etc).
#[derive(Debug, Clone)]
pub struct GrpcUnaryRunner {
    pub client: Option<tonic::client::Grpc<Channel>>,
    max_message_size: Option<usize>,
    auth_token: Option<String>,
}

impl GrpcUnaryRunner {
    // TODO Error type
    pub fn new() -> Self {
        Self {
            client: None,
            max_message_size: None,
            auth_token: None,
        }
    }

    pub async fn create(&mut self, settings: &GrpcUnaryRunnerSettings) -> Result<()> {
        let host = &settings.host;
        let port = &settings.port;

        // Create the base endpoint
        let mut endpoint = Endpoint::new(format!("{}:{}", host, port))?;

        // Apply timeout if specified
        if let Some(timeout_ms) = settings.timeout_ms {
            endpoint = endpoint.timeout(Duration::from_millis(timeout_ms as u64));
        }

        // Apply max message size if specified
        if let Some(max_size) = settings.max_message_size {
            self.max_message_size = Some(max_size as usize);
        }

        // Apply TLS configuration if enabled
        if settings.tls {
            let mut tls_config = ClientTlsConfig::new();

            // Apply TLS configuration settings if available
            if let Some(tls_settings) = &settings.tls_config {
                // Set server name override if provided
                if !tls_settings.server_name_override.is_empty() {
                    tls_config = tls_config.domain_name(tls_settings.server_name_override.clone());
                } else {
                    tls_config = tls_config.domain_name(host.clone());
                }

                // Load CA certificate if provided
                if !tls_settings.ca_cert_path.is_empty() {
                    let ca_cert = std::fs::read_to_string(&tls_settings.ca_cert_path)?;
                    tls_config = tls_config.ca_certificate(Certificate::from_pem(ca_cert));
                }

                // Load client certificate and key for mutual TLS if provided
                if !tls_settings.client_cert_path.is_empty()
                    && !tls_settings.client_key_path.is_empty()
                {
                    let client_cert = std::fs::read_to_string(&tls_settings.client_cert_path)?;
                    let client_key = std::fs::read_to_string(&tls_settings.client_key_path)?;
                    tls_config = tls_config.identity(Identity::from_pem(client_cert, client_key));
                }

                // Apply skip verification if set
                // if tls_settings.skip_verification {
                //     // TODO
                // }
            } else {
                // Default to using system roots
                // https://github.com/rustls/rustls/issues/1938
                let _ = rustls::crypto::ring::default_provider().install_default();
                tls_config = tls_config.with_enabled_roots();
            }

            endpoint = endpoint.tls_config(tls_config)?;
        }

        // Establish the connection
        let channel = endpoint.connect().await?;

        // Apply authentication token if provided
        if let Some(auth_token) = &settings.auth_token {
            if !auth_token.is_empty() {
                // Store the auth token for later use in request metadata
                self.auth_token = Some(auth_token.clone());
                tracing::debug!("Authorization token set for future requests");
            }
        }

        // Create the client
        self.client = Some(tonic::client::Grpc::new(channel));

        Ok(())
    }

    // Helper function to convert MetadataMap to HashMap<String, String>
    fn metadata_map_to_hashmap(metadata: &tonic::metadata::MetadataMap) -> HashMap<String, String> {
        let mut result = HashMap::new();
        for key_and_value in metadata.iter() {
            match key_and_value {
                tonic::metadata::KeyAndValueRef::Ascii(key, value) => {
                    if let Ok(value_str) = value.to_str() {
                        result.insert(key.to_string(), value_str.to_string());
                    } else {
                        tracing::warn!(
                            "Failed to convert ASCII metadata value to string for key: {}",
                            key
                        );
                    }
                }
                tonic::metadata::KeyAndValueRef::Binary(key, value) => {
                    // For binary values, we could use base64 encoding
                    let value_str =
                        base64::engine::general_purpose::STANDARD.encode(value.as_encoded_bytes());

                    result.insert(format!("{}-bin", key), value_str);
                }
            }
        }
        result
    }
}

impl Default for GrpcUnaryRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl RunnerSpec for GrpcUnaryRunner {
    fn name(&self) -> String {
        RunnerType::GrpcUnary.as_str_name().to_string()
    }
    fn runner_settings_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/grpc_unary_runner.proto").to_string()
    }
    fn job_args_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/grpc_unary_args.proto").to_string()
    }
    fn result_output_proto(&self) -> Option<String> {
        None
    }
    fn output_type(&self) -> StreamingOutputType {
        StreamingOutputType::NonStreaming
    }
    fn settings_schema(&self) -> String {
        schema_to_json_string!(GrpcUnaryRunnerSettings, "settings_schema")
    }
    fn arguments_schema(&self) -> String {
        schema_to_json_string!(GrpcUnaryArgs, "arguments_schema")
    }
    fn output_schema(&self) -> Option<String> {
        // plain string with title
        let mut schema = schemars::schema_for!(String);
        schema.insert(
            "title".to_string(),
            serde_json::Value::String("Command stdout".to_string()),
        );
        match serde_json::to_string(&schema) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::error!("error in output_json_schema: {:?}", e);
                None
            }
        }
    }
}
#[async_trait]
impl RunnerTrait for GrpcUnaryRunner {
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        let settings =
            ProstMessageCodec::deserialize_message::<GrpcUnaryRunnerSettings>(&settings)?;
        self.create(&settings).await
    }
    // args: {headers:{<headers map>}, queries:[<query string array>], body: <body string or struct>}
    async fn run(&mut self, args: &[u8]) -> Result<Vec<Vec<u8>>> {
        if let Some(mut client) = self.client.clone() {
            let req = ProstMessageCodec::deserialize_message::<GrpcUnaryArgs>(args)?;
            // Use our custom BytesCodec instead of ProstCodec to handle raw byte data correctly
            let codec = RawBytesCodec;

            // Setup the message size limits if needed
            if let Some(size) = self.max_message_size {
                // Set max message size using the cloned instance
                client = client
                    .max_decoding_message_size(size)
                    .max_encoding_message_size(size);
            }

            // Wait for the client to be ready - important to avoid buffer full errors
            client.ready().await.map_err(|e| {
                tonic::Status::new(
                    tonic::Code::Unknown,
                    format!("Service was not ready: {:?}", e),
                )
            })?;

            // Prepare the request - use request field as raw bytes
            let mut request = tonic::Request::new(req.request.clone());

            // Apply metadata from GrpcUnaryArgs by cloning values to avoid lifetime issues
            let metadata = request.metadata_mut();
            for (key, value) in req.metadata.clone() {
                if let Ok(val) = MetadataValue::try_from(value.as_str()) {
                    // Use String type to solve lifetime issues with metadata keys
                    if let Ok(key) = tonic::metadata::MetadataKey::from_bytes(key.as_bytes()) {
                        metadata.insert(key, val);
                    } else {
                        tracing::warn!("Invalid metadata key: {}", key);
                    }
                } else {
                    tracing::warn!("Invalid metadata value for key {}: {}", key, value);
                }
            }

            // Add authorization token if present
            if let Some(token) = &self.auth_token {
                let token_str = format!("Bearer {}", token);
                if let Ok(val) = MetadataValue::try_from(token_str.as_str()) {
                    // Use a static string to avoid lifetime issues
                    metadata.insert(
                        tonic::metadata::MetadataKey::from_static("authorization"),
                        val,
                    );
                } else {
                    tracing::warn!("Failed to create authorization metadata");
                }
            }

            // Convert method string to URI path
            let method = http::uri::PathAndQuery::try_from(req.method.clone())
                .map_err(|e| anyhow!("Invalid URI path: {}", e))?;

            tracing::debug!(
                "Sending gRPC request to {}, payload size: {} bytes",
                req.method,
                req.request.len()
            );

            // For better clarity, handle timeout differently
            let response = if req.timeout > 0 {
                let timeout_duration = Duration::from_millis(req.timeout as u64);
                // Use tokio timeout to wrap the entire gRPC call
                tokio::time::timeout(timeout_duration, client.unary(request, method, codec))
                    .await
                    .map(|r| {
                        r.inspect_err(|e| tracing::warn!("grpc request error: status={:?}", e))
                    })
                    .map_err(|_| anyhow!("Request timed out after {} ms", req.timeout))?
            } else {
                // Send the unary request without timeout
                client
                    .unary(request, method, codec)
                    .await
                    .inspect_err(|e| tracing::warn!("grpc request error: status={:?}", e))
            };
            let res = match response {
                Ok(response) => GrpcUnaryResult {
                    metadata: Self::metadata_map_to_hashmap(response.metadata()),
                    body: response.into_inner(),
                    code: tonic::Code::Ok as i32,
                    message: None,
                },
                Err(e) => {
                    tracing::warn!("grpc request error: status={:?}", e);
                    let res = GrpcUnaryResult {
                        metadata: Self::metadata_map_to_hashmap(e.metadata()),
                        body: e.details().to_vec(),
                        code: e.code() as i32,
                        message: Some(e.message().to_string()),
                    };
                    res
                }
            };

            tracing::info!("grpc unary runner result: {:?}", &res);
            Ok(vec![ProstMessageCodec::serialize_message(&res)?])
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::jobworkerp::data::Runner;

    #[tokio::test]
    #[ignore] // need to start front server and fix handling empty stream...
    async fn run_request() -> Result<()> {
        use prost::Message;
        // common::util::tracing::tracing_init_test(tracing::Level::INFO);
        let mut runner = GrpcUnaryRunner::new();

        let settings = GrpcUnaryRunnerSettings {
            host: "http://localhost".to_string(),
            port: 9000,
            tls: false,
            timeout_ms: None,
            max_message_size: None,
            auth_token: None,
            tls_config: None,
        };

        runner
            .load(ProstMessageCodec::serialize_message(&settings)?)
            .await?;

        // Create properly encoded protobuf message
        let runner_id = proto::jobworkerp::data::RunnerId { value: 1 };
        let mut buf = Vec::with_capacity(runner_id.encoded_len());
        runner_id.encode(&mut buf)?;

        let arg = crate::jobworkerp::runner::GrpcUnaryArgs {
            method: "/jobworkerp.service.RunnerService/Find".to_string(),
            request: buf,
            metadata: Default::default(),
            timeout: 0,
        };

        let arg = ProstMessageCodec::serialize_message(&arg)?;
        let res = runner.run(&arg).await;

        match res {
            Ok(data) => {
                if !data.is_empty() {
                    // Try to deserialize the response as a Runner message
                    match GrpcUnaryResult::decode(data[0].as_slice()) {
                        Ok(result) => {
                            #[derive(Clone, PartialEq, prost::Message)]
                            pub struct OptionRunner {
                                #[prost(message, optional, tag = "1")]
                                data: Option<Runner>,
                            }
                            println!("Successfully received runner: {:#?}", result);
                            assert!(result.code == tonic::Code::Ok as i32);
                            assert!(!result.body.is_empty());
                            println!(
                                "runner: {:#?}",
                                ProstMessageCodec::deserialize_message::<OptionRunner>(
                                    result.body.as_slice()
                                )
                                .unwrap()
                            );
                        }
                        Err(e) => {
                            println!("Failed to decode response as Runner: {:?}", e);
                            println!("Raw response: {:?}", data[0]);
                            unreachable!()
                        }
                    }
                } else {
                    println!("Received empty response");
                }
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
}
