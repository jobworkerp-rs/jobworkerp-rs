use crate::jobworkerp::runner::{GrpcUnaryArgs, GrpcUnaryResult, GrpcUnaryRunnerSettings};
use crate::{schema_to_json_string, schema_to_json_string_option};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use base64::Engine;
use command_utils::protobuf::ProtobufDescriptor;
use futures::stream::BoxStream;
use infra_utils::infra::net::grpc::reflection::GrpcReflectionClient;
use infra_utils::infra::net::grpc::RawBytesCodec;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use prost_reflect::{DescriptorPool, MessageDescriptor};
use proto::jobworkerp::data::{ResultOutputItem, RunnerType, StreamingOutputType};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tonic::{
    metadata::MetadataValue,
    transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity},
};

use super::common::cancellation_helper::{handle_run_result, CancellationHelper};
use super::{RunnerSpec, RunnerTrait};

/// grpc unary request runner.
/// specify protobuf payload as JSON in enqueue.
/// return response as single byte vector payload (not interpret, not extract vector etc).
#[derive(Debug, Clone)]
pub struct GrpcUnaryRunner {
    pub client: Option<tonic::client::Grpc<Channel>>,
    reflection_client: Option<GrpcReflectionClient>,
    descriptor_pool: Option<Arc<DescriptorPool>>,
    max_message_size: Option<usize>,
    auth_token: Option<String>,
    use_reflection: bool,
    cancellation_helper: CancellationHelper,
}

impl GrpcUnaryRunner {
    // TODO Error type
    pub fn new() -> Self {
        Self {
            client: None,
            reflection_client: None,
            descriptor_pool: None,
            max_message_size: None,
            auth_token: None,
            use_reflection: false,
            cancellation_helper: CancellationHelper::new(),
        }
    }

    /// Set a cancellation token for this runner instance
    /// This allows external control over cancellation behavior (for test)
    #[cfg(test)]
    pub(crate) fn set_cancellation_token(&mut self, token: tokio_util::sync::CancellationToken) {
        self.cancellation_helper.set_cancellation_token(token);
    }

    pub async fn create(&mut self, settings: &GrpcUnaryRunnerSettings) -> Result<()> {
        let host = &settings.host;
        let port = &settings.port;

        // Create the base endpoint
        let mut endpoint = Endpoint::new(format!("{host}:{port}"))?;

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

        // Set use_reflection flag
        self.use_reflection = settings.use_reflection.unwrap_or(false);

        // If reflection is enabled, initialize the reflection client
        if self.use_reflection {
            let reflection_channel = channel.clone();
            self.reflection_client = Some(
                GrpcReflectionClient::connect(
                    endpoint,
                    reflection_channel,
                    settings.timeout_ms.map(|s| Duration::from_millis(s as u64)),
                )
                .await?,
            );

            // Initialize reflection client and verify connection
            if let Some(ref reflection_client) = self.reflection_client {
                // Test the connection by listing services
                let services = reflection_client.list_services().await?;
                tracing::debug!("Available gRPC services: {:?}", services);

                // We don't need to initialize the descriptor pool in advance anymore
                // The new GrpcReflectionClient API will handle this on demand
                tracing::info!("Successfully initialized gRPC reflection client");
            }
        }

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

        tracing::debug!(
            "GrpcUnaryRunner initialized, use_reflection={}",
            self.use_reflection
        );
        Ok(())
    }

    fn parse_method_path(method_path: &str) -> Result<(String, String)> {
        let parts: Vec<&str> = method_path.split('/').collect();
        if parts.len() == 3 && parts[0].is_empty() {
            Ok((parts[1].to_string(), parts[2].to_string()))
        } else if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
            Ok((parts[0].to_string(), parts[1].to_string()))
        } else {
            Err(anyhow!("Invalid method path format: {}", method_path))
        }
    }

    // Helper method to get input message descriptor for a given method
    async fn get_input_message_descriptor(&self, method_path: &str) -> Result<MessageDescriptor> {
        let (service_name, method_name) = Self::parse_method_path(method_path)?;

        // First check if we have a reflection client available
        if let Some(ref reflection_client) = self.reflection_client {
            // Get service descriptor pool using the reflection client
            let pool = reflection_client
                .get_service_with_dependencies(&service_name)
                .await?;

            // Find the service in the pool
            if let Some(service) = pool.get_service_by_name(&service_name) {
                // Find the method
                if let Some(method) = service.methods().find(|m| m.name() == method_name) {
                    return Ok(method.input());
                }
            }
            Err(anyhow!(
                "Method {} not found in service {}",
                method_name,
                service_name
            ))
        } else if let Some(pool) = &self.descriptor_pool {
            // Fallback to using cached pool if reflection client is not available
            // Find the service
            for service in pool.services() {
                if service.full_name() == service_name {
                    // Find the method
                    if let Some(method) = service.methods().find(|m| m.name() == method_name) {
                        return Ok(method.input());
                    }
                }
            }

            Err(anyhow!(
                "Method {} not found in service {}",
                method_name,
                service_name
            ))
        } else {
            Err(anyhow!("No reflection client or descriptor pool available"))
        }
    }

    // Helper method to convert JSON to protobuf bytes using reflection
    async fn json_to_protobuf(&self, method_path: &str, json_str: &str) -> Result<Vec<u8>> {
        let input_descriptor = self.get_input_message_descriptor(method_path).await?;
        tracing::debug!(
            "Input message descriptor for {}:\n json:{},\n descriptor: {:?}",
            method_path,
            json_str,
            input_descriptor
        );

        // Use ProtobufDescriptor utility to convert JSON to protobuf bytes
        let bytes = ProtobufDescriptor::json_to_message(input_descriptor, json_str)?;

        tracing::debug!(
            "Converted JSON to protobuf message for {}: {} bytes",
            method_path,
            bytes.len()
        );

        Ok(bytes)
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

                    result.insert(format!("{key}-bin"), value_str);
                }
            }
        }
        result
    }
}

impl std::fmt::Display for GrpcUnaryRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "GrpcUnaryRunner {{ reflection: {} }}",
            self.use_reflection
        )
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
        Some(include_str!("../../protobuf/jobworkerp/runner/grpc_unary_result.proto").to_string())
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
        schema_to_json_string_option!(GrpcUnaryResult, "output_schema")
    }
}
#[async_trait]
impl RunnerTrait for GrpcUnaryRunner {
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        let settings =
            ProstMessageCodec::deserialize_message::<GrpcUnaryRunnerSettings>(&settings)?;
        self.create(&settings).await
    }
    async fn run(
        &mut self,
        args: &[u8],
        metadata: HashMap<String, String>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        // Set up cancellation token using helper
        let cancellation_token = match self.cancellation_helper.setup_execution_token() {
            Ok(token) => token,
            Err(e) => return (Err(e), metadata),
        };

        let result = async {
            if let Some(mut client) = self.client.clone() {
                let req = ProstMessageCodec::deserialize_message::<GrpcUnaryArgs>(args)?;
                let codec = RawBytesCodec;

                if let Some(size) = self.max_message_size {
                    client = client
                        .max_decoding_message_size(size)
                        .max_encoding_message_size(size);
                }

                // Wait for the client to be ready (to avoid buffer full errors)
                client.ready().await.map_err(|e| {
                    tonic::Status::new(
                        tonic::Code::Unknown,
                        format!("Service was not ready: {e:?}"),
                    )
                })?;

                let request_bytes = if self.use_reflection && self.reflection_client.is_some() {
                    self.json_to_protobuf(&req.method, &req.request).await?
                } else {
                    if self.use_reflection {
                        tracing::warn!(
                            "Requested reflection unavailable. Treating request as raw data."
                        );
                    } else {
                        tracing::debug!("Using raw bytes(base64) for request");
                    }
                    match base64::engine::general_purpose::STANDARD.decode(&req.request) {
                        Ok(bytes) => bytes,
                        Err(_) => req.request.into_bytes(),
                    }
                };
                let request_len = request_bytes.len();

                let mut request = tonic::Request::new(request_bytes);

                let metadata_mut = request.metadata_mut();
                for (key, value) in req.metadata.clone() {
                    if let Ok(val) = MetadataValue::try_from(value.as_str()) {
                        if let Ok(key) = tonic::metadata::MetadataKey::from_bytes(key.as_bytes()) {
                            metadata_mut.insert(key, val);
                        } else {
                            tracing::warn!("Invalid metadata key: {}", key);
                        }
                    } else {
                        tracing::warn!("Invalid metadata value for key {}: {}", key, value);
                    }
                }

                if let Some(token) = &self.auth_token {
                    let token_str = format!("Bearer {token}");
                    if let Ok(val) = MetadataValue::try_from(token_str.as_str()) {
                        metadata_mut.insert(
                            tonic::metadata::MetadataKey::from_static("authorization"),
                            val,
                        );
                    } else {
                        tracing::warn!("Failed to create authorization metadata");
                    }
                }

                let method = http::uri::PathAndQuery::try_from(req.method.clone())
                    .map_err(|e| anyhow!("Invalid URI path: {}", e))?;

                tracing::debug!(
                    "Sending gRPC request to {}, payload size: {} bytes",
                    req.method,
                    request_len
                );

                // Send gRPC request with cancellation support
                let response = if req.timeout > 0 {
                    let timeout_duration = Duration::from_millis(req.timeout as u64);
                    tokio::select! {
                        timeout_result = tokio::time::timeout(timeout_duration, client.unary(request, method, codec)) => {
                            timeout_result
                                .map(|r| r.inspect_err(|e| tracing::warn!("grpc request error: status={:?}", e)))
                                .map_err(|_| tonic::Status::new(tonic::Code::DeadlineExceeded, format!("Request timed out after {} ms", req.timeout)))?
                        }
                        _ = cancellation_token.cancelled() => {
                            return Err(anyhow!("gRPC request was cancelled"));
                        }
                    }
                } else {
                    tokio::select! {
                        response_result = client.unary(request, method, codec) => {
                            response_result.inspect_err(|e| tracing::warn!("grpc request error: status={:?}", e))
                        }
                        _ = cancellation_token.cancelled() => {
                            return Err(anyhow!("gRPC request was cancelled"));
                        }
                    }
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
                        GrpcUnaryResult {
                            metadata: Self::metadata_map_to_hashmap(e.metadata()),
                            body: e.details().to_vec(),
                            code: e.code() as i32,
                            message: Some(e.message().to_string()),
                        }
                    }
                };

                tracing::info!("grpc unary runner result: {:?}", &res);
                Ok(ProstMessageCodec::serialize_message(&res)?)
            } else {
                Err(anyhow!("grpc client is not initialized"))
            }
        }.await;

        handle_run_result(&mut self.cancellation_helper, result, metadata)
    }

    async fn run_stream(
        &mut self,
        _arg: &[u8],
        _metadata: HashMap<String, String>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        unimplemented!("gRPC unary does not support streaming")
    }

    async fn cancel(&mut self) {
        self.cancellation_helper.cancel();
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::jobworkerp::data::Runner;

    #[tokio::test]
    #[ignore = "Requires a running gRPC server"]
    async fn test_grpc_pre_execution_cancellation() {
        let mut runner = GrpcUnaryRunner::new();
        runner
            .load(
                ProstMessageCodec::serialize_message(&GrpcUnaryRunnerSettings {
                    host: "http://localhost".to_string(),
                    port: 9000,
                    tls: false,
                    timeout_ms: Some(5000),
                    max_message_size: Some(1024 * 1024), // 1 MB
                    auth_token: None,
                    tls_config: None,
                    use_reflection: Some(false),
                })
                .unwrap(),
            )
            .await
            .unwrap();

        // Set up cancellation token and cancel it immediately
        let cancellation_token = tokio_util::sync::CancellationToken::new();
        runner.set_cancellation_token(cancellation_token.clone());
        cancellation_token.cancel();

        use crate::jobworkerp::runner::GrpcUnaryArgs;
        let grpc_args = GrpcUnaryArgs {
            method: "/test.Service/TestMethod".to_string(),
            request: "{}".to_string(),
            metadata: HashMap::new(),
            timeout: 5000,
        };

        let start_time = std::time::Instant::now();
        let (result, _) = runner
            .run(
                &ProstMessageCodec::serialize_message(&grpc_args).unwrap(),
                HashMap::new(),
            )
            .await;
        let elapsed = start_time.elapsed();

        // Should fail immediately due to pre-execution cancellation
        assert!(result.is_err());
        assert!(elapsed < std::time::Duration::from_millis(100));

        let error_msg = result.unwrap_err().to_string();
        assert!(
            error_msg.contains("gRPC request was cancelled"),
            "Expected cancellation error, got: {error_msg}"
        );
    }

    #[tokio::test]
    #[ignore] // need to start front server and fix handling empty stream...
    async fn run_request() -> Result<()> {
        use prost::Message;
        command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
        let mut runner = GrpcUnaryRunner::new();

        let settings = GrpcUnaryRunnerSettings {
            host: "http://localhost".to_string(),
            port: 9000,
            tls: false,
            timeout_ms: None,
            max_message_size: None,
            auth_token: None,
            tls_config: None,
            use_reflection: Some(false),
        };

        runner
            .load(ProstMessageCodec::serialize_message(&settings)?)
            .await?;

        // Create properly encoded protobuf message
        let runner_id = proto::jobworkerp::data::RunnerId { value: 1 };
        let mut buf = Vec::with_capacity(runner_id.encoded_len());
        runner_id.encode(&mut buf)?;

        // Convert binary protobuf to base64 for backward compatibility
        let base64_encoded = base64::engine::general_purpose::STANDARD.encode(&buf);

        let arg = crate::jobworkerp::runner::GrpcUnaryArgs {
            method: "jobworkerp.service.RunnerService/Find".to_string(),
            request: base64_encoded,
            metadata: Default::default(),
            timeout: 0,
        };

        let arg = ProstMessageCodec::serialize_message(&arg)?;
        let res = runner.run(&arg, HashMap::new()).await;

        match res.0 {
            Ok(data) => {
                if !data.is_empty() {
                    // Try to deserialize the response as a Runner message
                    match GrpcUnaryResult::decode(data.as_slice()) {
                        Ok(result) => {
                            #[derive(Clone, PartialEq, prost::Message)]
                            pub struct OptionRunner {
                                #[prost(message, optional, tag = "1")]
                                data: Option<Runner>,
                            }
                            println!("Successfully received runner: {result:#?}");
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
                            println!("Failed to decode response as Runner: {e:?}");
                            println!("Raw response: {data:?}");
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

    #[tokio::test]
    #[ignore] // Requires a running gRPC server with reflection enabled
    async fn test_function_set_crud_via_reflection() -> Result<()> {
        // Initialize the runner with reflection enabled
        let mut runner = GrpcUnaryRunner::new();
        let settings = GrpcUnaryRunnerSettings {
            host: "http://localhost".to_string(),
            port: 9000,
            tls: false,
            timeout_ms: Some(5000), // 5 second timeout
            max_message_size: None,
            auth_token: None,
            tls_config: None,
            use_reflection: Some(true),
        };

        runner
            .load(ProstMessageCodec::serialize_message(&settings)?)
            .await?;
        assert!(
            runner.reflection_client.is_some(),
            "Reflection client should be initialized"
        );

        // 1. Verify FunctionSetService is available
        let services = {
            let reflection_client = runner.reflection_client.as_ref().unwrap();
            let services = reflection_client.list_services().await?;
            println!("Available services: {services:?}");
            services
        };

        let service_name = "jobworkerp.function.service.FunctionSetService";
        assert!(
            services.contains(&service_name.to_string()),
            "FunctionSetService should exist on the server"
        );

        // 2. Get FunctionSetData template for creation
        let function_set_data_template = {
            let reflection_client = runner.reflection_client.as_ref().unwrap();
            let message_name = "jobworkerp.function.data.FunctionSetData";
            let template = reflection_client.get_message_template(message_name).await?;
            println!("FunctionSet template: {template}");
            template
        };

        // 3. Create a test function set with a unique name
        let test_set_name = format!("test-reflection-set-{}", uuid::Uuid::new_v4());
        let mut set_data: serde_json::Value = serde_json::from_str(&function_set_data_template)?;

        if let Some(obj) = set_data.as_object_mut() {
            obj.insert("name".to_string(), test_set_name.clone().into());
            obj.insert(
                "description".to_string(),
                "Function set created via reflection test".into(),
            );
            obj.insert("category".to_string(), 1.into());

            // We could add targets here if needed
            // "targets": []
        }

        println!("Creating function set with name: {test_set_name}");

        // 4. Create the function set
        let create_request = GrpcUnaryArgs {
            method: format!("/{service_name}/Create"),
            request: serde_json::to_string(&set_data)?,
            metadata: HashMap::new(),
            timeout: 5000,
        };

        let create_result = {
            let result = runner
                .run(
                    &ProstMessageCodec::serialize_message(&create_request)?,
                    HashMap::new(),
                )
                .await;
            let response = ProstMessageCodec::deserialize_message::<GrpcUnaryResult>(&result.0?)?;

            assert_eq!(
                response.code,
                tonic::Code::Ok as i32,
                "Expected OK response for Create"
            );

            // Parse the response to extract the created function set ID
            let reflection_client = runner.reflection_client.as_ref().unwrap();
            let response_message = "jobworkerp.function.service.CreateFunctionSetResponse";
            let json_response = reflection_client
                .parse_bytes_to_json(response_message, &response.body)
                .await?;

            println!("Created function set response: {json_response}");
            json_response
        };

        // 5. Extract the function set ID from the creation response
        let function_set_id = {
            let response_value: serde_json::Value = serde_json::from_str(&create_result)?;
            let id = response_value
                .as_object()
                .and_then(|obj| obj.get("id").and_then(|data| data.get("value")))
                .ok_or_else(|| anyhow!("Could not find id in response"))?
                .as_str()
                .ok_or_else(|| anyhow!("Could not find id in response"))?
                .parse::<i64>()?;
            println!("Created function set with ID: {id}");
            id
        };

        // 6. Find the created function set by ID
        let find_request = GrpcUnaryArgs {
            method: format!("/{service_name}/Find"),
            request: format!(r#"{{"value": {function_set_id}}}"#),
            metadata: HashMap::new(),
            timeout: 5000,
        };

        let find_result = {
            let result = runner
                .run(
                    &ProstMessageCodec::serialize_message(&find_request)?,
                    HashMap::new(),
                )
                .await;
            let response = ProstMessageCodec::deserialize_message::<GrpcUnaryResult>(&result.0?)?;

            assert_eq!(
                response.code,
                tonic::Code::Ok as i32,
                "Expected OK response for Find"
            );

            // Parse the response to verify the function set
            let reflection_client = runner.reflection_client.as_ref().unwrap();
            let response_message = "jobworkerp.function.service.OptionalFunctionSetResponse";
            let json_response = reflection_client
                .parse_bytes_to_json(response_message, &response.body)
                .await?;

            println!("Found function set: {json_response}");

            // Verify correct function set was found
            let response_value: serde_json::Value = serde_json::from_str(&json_response)?;
            assert!(
                response_value["data"].is_object(),
                "Function set data should exist"
            );
            assert_eq!(
                response_value["data"]["data"]["name"]
                    .as_str()
                    .unwrap_or(""),
                test_set_name,
                "Function set name should match"
            );

            json_response
        };

        // 7. Find the created function set by name
        let find_by_name_request = GrpcUnaryArgs {
            method: format!("/{service_name}/FindByName"),
            request: format!(r#"{{"name": "{test_set_name}"}}"#),
            metadata: HashMap::new(),
            timeout: 5000,
        };

        let _find_by_name_result = {
            let result = runner
                .run(
                    &ProstMessageCodec::serialize_message(&find_by_name_request)?,
                    HashMap::new(),
                )
                .await;
            let response = ProstMessageCodec::deserialize_message::<GrpcUnaryResult>(&result.0?)?;

            assert_eq!(
                response.code,
                tonic::Code::Ok as i32,
                "Expected OK response for FindByName"
            );

            // Parse the response
            let reflection_client = runner.reflection_client.as_ref().unwrap();
            let response_message = "jobworkerp.function.service.OptionalFunctionSetResponse";
            let json_response = reflection_client
                .parse_bytes_to_json(response_message, &response.body)
                .await?;

            println!("Found function set by name: {json_response}");

            // Verify correct function set was found
            let response_value: serde_json::Value = serde_json::from_str(&json_response)?;
            assert!(
                response_value["data"].is_object(),
                "Function set data should exist"
            );
            assert_eq!(
                response_value["data"]["id"]["value"]
                    .as_str()
                    .unwrap()
                    .parse()
                    .unwrap_or(0),
                function_set_id,
                "Found function set ID should match created ID"
            );

            json_response
        };

        // 8. Update the function set
        let update_request = {
            let mut function_set: serde_json::Value = serde_json::from_str(&find_result)?;
            // Extract the "data" object which is the FunctionSet
            if let Some(data) = function_set.get_mut("data") {
                if let Some(data_obj) = data.as_object_mut() {
                    // Update the description
                    if let Some(function_data) = data_obj.get_mut("data") {
                        if let Some(function_data_obj) = function_data.as_object_mut() {
                            function_data_obj.insert(
                                "description".to_string(),
                                "Updated via reflection test".into(),
                            );
                        }
                    }
                }
            }

            GrpcUnaryArgs {
                method: format!("/{service_name}/Update"),
                request: serde_json::to_string(&function_set["data"])?,
                metadata: HashMap::new(),
                timeout: 5000,
            }
        };

        {
            let result = runner
                .run(
                    &ProstMessageCodec::serialize_message(&update_request)?,
                    HashMap::new(),
                )
                .await;
            let response = ProstMessageCodec::deserialize_message::<GrpcUnaryResult>(&result.0?)?;

            assert_eq!(
                response.code,
                tonic::Code::Ok as i32,
                "Expected OK response for Update"
            );

            println!("Successfully updated function set");
        }

        // 9. Verify the update worked
        {
            let result = runner
                .run(
                    &ProstMessageCodec::serialize_message(&find_request)?,
                    HashMap::new(),
                )
                .await;
            let response = ProstMessageCodec::deserialize_message::<GrpcUnaryResult>(&result.0?)?;

            let reflection_client = runner.reflection_client.as_ref().unwrap();
            let response_message = "jobworkerp.function.service.OptionalFunctionSetResponse";
            let json_response = reflection_client
                .parse_bytes_to_json(response_message, &response.body)
                .await?;

            // Verify the description was updated
            let response_value: serde_json::Value = serde_json::from_str(&json_response)?;
            assert_eq!(
                response_value["data"]["data"]["description"]
                    .as_str()
                    .unwrap_or(""),
                "Updated via reflection test",
                "Function set description should be updated"
            );

            println!("Update verified successfully");
        };

        // 10. Delete the function set
        let delete_request = GrpcUnaryArgs {
            method: format!("/{service_name}/Delete"),
            request: format!(r#"{{"value": {function_set_id}}}"#),
            metadata: HashMap::new(),
            timeout: 5000,
        };

        {
            let result = runner
                .run(
                    &ProstMessageCodec::serialize_message(&delete_request)?,
                    HashMap::new(),
                )
                .await;
            let response = ProstMessageCodec::deserialize_message::<GrpcUnaryResult>(&result.0?)?;

            assert_eq!(
                response.code,
                tonic::Code::Ok as i32,
                "Expected OK response for Delete"
            );

            println!("Successfully deleted function set");
        }

        // 11. Verify the function set was deleted
        {
            let result = runner
                .run(
                    &ProstMessageCodec::serialize_message(&find_request)?,
                    HashMap::new(),
                )
                .await;
            let response = ProstMessageCodec::deserialize_message::<GrpcUnaryResult>(&result.0?)?;

            let reflection_client = runner.reflection_client.as_ref().unwrap();
            let response_message = "jobworkerp.function.service.OptionalFunctionSetResponse";
            let json_response = reflection_client
                .parse_bytes_to_json(response_message, &response.body)
                .await?;

            let response_value: serde_json::Value = serde_json::from_str(&json_response)?;

            // Check that data field is null after deletion
            assert!(
                response_value["data"].is_null(),
                "Function set should be deleted"
            );

            println!("Deletion verified successfully");
        };

        println!("CRUD operations for function set completed successfully");
        Ok(())
    }
}
