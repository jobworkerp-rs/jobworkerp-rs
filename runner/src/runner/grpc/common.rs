use crate::jobworkerp::runner::grpc::{GrpcRunnerSettings, grpc_args};
use anyhow::{Result, anyhow};
use base64::Engine;
use command_utils::protobuf::ProtobufDescriptor;
use net_utils::grpc::reflection::GrpcReflectionClient;
use prost_reflect::{DescriptorPool, MessageDescriptor};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tonic::{
    metadata::MetadataValue,
    transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity},
};

/// Shared gRPC connection state used by both unary and streaming methods.
#[derive(Clone)]
pub struct GrpcConnection {
    pub(crate) client: Option<tonic::client::Grpc<Channel>>,
    pub(crate) reflection_client: Option<GrpcReflectionClient>,
    /// Reserved for future use: local proto file-based descriptor pool as fallback
    /// when reflection is unavailable. Currently only set to None; used as fallback
    /// path in get_input/output_message_descriptor().
    pub(crate) descriptor_pool: Option<Arc<DescriptorPool>>,
    pub(crate) max_message_size: Option<usize>,
    pub(crate) auth_token: Option<String>,
    pub(crate) use_reflection: bool,
}

impl GrpcConnection {
    pub fn new() -> Self {
        Self {
            client: None,
            reflection_client: None,
            descriptor_pool: None,
            max_message_size: None,
            auth_token: None,
            use_reflection: false,
        }
    }

    /// Reset all connection state to initial values.
    pub fn clear(&mut self) {
        self.client = None;
        self.reflection_client = None;
        self.descriptor_pool = None;
        self.max_message_size = None;
        self.auth_token = None;
        self.use_reflection = false;
    }

    pub async fn create(&mut self, settings: &GrpcRunnerSettings) -> Result<()> {
        self.clear();

        let host = &settings.host;
        let port = &settings.port;
        let prtcl = if host.starts_with("http://") || host.starts_with("https://") {
            ""
        } else if settings.tls {
            "https://"
        } else {
            "http://"
        };

        let mut endpoint = Endpoint::new(format!("{prtcl}{host}:{port}"))?;

        if let Some(timeout_ms) = settings.timeout_ms {
            endpoint = endpoint.timeout(Duration::from_millis(timeout_ms as u64));
        }

        if let Some(max_size) = settings.max_message_size {
            self.max_message_size = Some(max_size as usize);
        }

        if settings.tls {
            let mut tls_config = ClientTlsConfig::new();

            if let Some(tls_settings) = &settings.tls_config {
                if !tls_settings.server_name_override.is_empty() {
                    tls_config = tls_config.domain_name(tls_settings.server_name_override.clone());
                } else {
                    tls_config = tls_config.domain_name(host.clone());
                }

                if !tls_settings.ca_cert_path.is_empty() {
                    let ca_cert = std::fs::read_to_string(&tls_settings.ca_cert_path)?;
                    tls_config = tls_config.ca_certificate(Certificate::from_pem(ca_cert));
                }

                if !tls_settings.client_cert_path.is_empty()
                    && !tls_settings.client_key_path.is_empty()
                {
                    let client_cert = std::fs::read_to_string(&tls_settings.client_cert_path)?;
                    let client_key = std::fs::read_to_string(&tls_settings.client_key_path)?;
                    tls_config = tls_config.identity(Identity::from_pem(client_cert, client_key));
                }
            } else {
                let _ = rustls::crypto::ring::default_provider().install_default();
                tls_config = tls_config.with_enabled_roots();
            }

            endpoint = endpoint.tls_config(tls_config)?;
        }

        let channel = endpoint.connect().await?;

        self.use_reflection = settings.use_reflection.unwrap_or(false);

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

            if let Some(ref reflection_client) = self.reflection_client {
                let services = reflection_client.list_services().await?;
                tracing::debug!("Available gRPC services: {:?}", services);
                tracing::info!("Successfully initialized gRPC reflection client");
            }
        }

        if let Some(auth_token) = &settings.auth_token
            && !auth_token.is_empty()
        {
            self.auth_token = Some(auth_token.clone());
            tracing::debug!("Authorization token set for future requests");
        }

        self.client = Some(tonic::client::Grpc::new(channel));

        tracing::debug!(
            "GrpcConnection initialized, use_reflection={}",
            self.use_reflection
        );
        Ok(())
    }

    pub fn parse_method_path(method_path: &str) -> Result<(String, String)> {
        let parts: Vec<&str> = method_path.split('/').collect();
        if parts.len() == 3 && parts[0].is_empty() {
            Ok((parts[1].to_string(), parts[2].to_string()))
        } else if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
            Ok((parts[0].to_string(), parts[1].to_string()))
        } else {
            Err(anyhow!("Invalid method path format: {}", method_path))
        }
    }

    pub async fn get_output_message_descriptor(
        &self,
        method_path: &str,
    ) -> Result<MessageDescriptor> {
        let (service_name, method_name) = Self::parse_method_path(method_path)?;

        if let Some(ref reflection_client) = self.reflection_client {
            let pool = reflection_client
                .get_service_with_dependencies(&service_name)
                .await?;

            if let Some(service) = pool.get_service_by_name(&service_name)
                && let Some(method) = service.methods().find(|m| m.name() == method_name)
            {
                return Ok(method.output());
            }
            Err(anyhow!(
                "Method {} not found in service {}",
                method_name,
                service_name
            ))
        } else if let Some(pool) = &self.descriptor_pool {
            for service in pool.services() {
                if service.full_name() == service_name
                    && let Some(method) = service.methods().find(|m| m.name() == method_name)
                {
                    return Ok(method.output());
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

    pub async fn get_input_message_descriptor(
        &self,
        method_path: &str,
    ) -> Result<MessageDescriptor> {
        let (service_name, method_name) = Self::parse_method_path(method_path)?;

        if let Some(ref reflection_client) = self.reflection_client {
            let pool = reflection_client
                .get_service_with_dependencies(&service_name)
                .await?;

            if let Some(service) = pool.get_service_by_name(&service_name)
                && let Some(method) = service.methods().find(|m| m.name() == method_name)
            {
                return Ok(method.input());
            }
            Err(anyhow!(
                "Method {} not found in service {}",
                method_name,
                service_name
            ))
        } else if let Some(pool) = &self.descriptor_pool {
            for service in pool.services() {
                if service.full_name() == service_name
                    && let Some(method) = service.methods().find(|m| m.name() == method_name)
                {
                    return Ok(method.input());
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

    pub async fn json_to_protobuf(&self, method_path: &str, json_str: &str) -> Result<Vec<u8>> {
        let input_descriptor = self.get_input_message_descriptor(method_path).await?;
        tracing::debug!(
            "Input message descriptor for {}:\n json:{},\n descriptor: {:?}",
            method_path,
            json_str,
            input_descriptor
        );

        let bytes = ProtobufDescriptor::json_to_message(input_descriptor, json_str, true)?;

        tracing::debug!(
            "Converted JSON to protobuf message for {}: {} bytes",
            method_path,
            bytes.len()
        );

        Ok(bytes)
    }

    pub async fn convert_response_to_json(
        &self,
        method_path: &str,
        response_bytes: &[u8],
    ) -> Result<String> {
        if let Some(ref reflection_client) = self.reflection_client {
            let output_descriptor = self.get_output_message_descriptor(method_path).await?;
            let message_name = output_descriptor.full_name();

            tracing::debug!(
                "Converting response to JSON for method {} with output type {}",
                method_path,
                message_name
            );

            reflection_client
                .parse_bytes_to_json(message_name, response_bytes)
                .await
        } else {
            Err(anyhow!("Reflection client not available"))
        }
    }

    pub fn metadata_map_to_hashmap(
        metadata: &tonic::metadata::MetadataMap,
    ) -> HashMap<String, String> {
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
                    let value_str =
                        base64::engine::general_purpose::STANDARD.encode(value.as_encoded_bytes());
                    result.insert(format!("{key}-bin"), value_str);
                }
            }
        }
        result
    }

    /// Prepare request bytes from GrpcArgs oneof request field.
    /// - `body`: use raw bytes as-is (pre-serialized protobuf)
    /// - `json_body`: convert JSON to protobuf using reflection
    /// - `None`: treat as empty request
    pub async fn prepare_request_bytes(
        &self,
        method_path: &str,
        request: &Option<grpc_args::Request>,
    ) -> Result<Vec<u8>> {
        match request {
            Some(grpc_args::Request::Body(bytes)) => {
                tracing::debug!(
                    "Using raw protobuf bytes for request ({} bytes)",
                    bytes.len()
                );
                Ok(bytes.clone())
            }
            Some(grpc_args::Request::JsonBody(json_str)) => {
                if self.use_reflection && self.reflection_client.is_some() {
                    self.json_to_protobuf(method_path, json_str).await
                } else {
                    Err(anyhow!(
                        "json_body requires reflection to be enabled and available"
                    ))
                }
            }
            None => {
                tracing::debug!("No request payload, using empty bytes");
                Ok(Vec::new())
            }
        }
    }

    /// Build a tonic::Request with metadata and auth token.
    pub fn build_request(
        &self,
        request_bytes: Vec<u8>,
        req_metadata: &HashMap<String, String>,
    ) -> tonic::Request<Vec<u8>> {
        let mut request = tonic::Request::new(request_bytes);

        let metadata_mut = request.metadata_mut();
        for (key, value) in req_metadata {
            match MetadataValue::try_from(value.as_str()) {
                Ok(val) => match tonic::metadata::MetadataKey::from_bytes(key.as_bytes()) {
                    Ok(key) => {
                        metadata_mut.insert(key, val);
                    }
                    _ => {
                        tracing::warn!("Invalid metadata key: {}", key);
                    }
                },
                _ => {
                    tracing::warn!("Invalid metadata value for key {}: {}", key, value);
                }
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

        request
    }

    /// Normalize method path to start with '/'.
    pub fn normalize_method_path(method: &str) -> Result<http::uri::PathAndQuery> {
        let method_path = if method.starts_with('/') {
            method.to_string()
        } else {
            format!("/{}", method)
        };
        http::uri::PathAndQuery::try_from(method_path)
            .map_err(|e| anyhow!("Invalid URI path: {}", e))
    }
}

impl std::fmt::Debug for GrpcConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrpcConnection")
            .field("client", &self.client.is_some())
            .field("reflection_client", &self.reflection_client.is_some())
            .field("descriptor_pool", &self.descriptor_pool.is_some())
            .field("max_message_size", &self.max_message_size)
            .field(
                "auth_token",
                &self.auth_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("use_reflection", &self.use_reflection)
            .finish()
    }
}

impl Default for GrpcConnection {
    fn default() -> Self {
        Self::new()
    }
}
