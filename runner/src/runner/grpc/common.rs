use crate::jobworkerp::runner::grpc::{GrpcRunnerSettings, grpc_args};
use anyhow::{Result, anyhow};
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
    /// Default gRPC method from settings (takes priority over args method)
    pub(crate) settings_method: Option<String>,
    /// Default metadata from settings (takes priority over args metadata when non-empty)
    pub(crate) settings_metadata: HashMap<String, String>,
    /// Default job timeout from settings (takes priority over args job_timeout)
    pub(crate) settings_timeout: Option<u32>,
    /// Default as_json from settings (takes priority over args as_json)
    pub(crate) settings_as_json: Option<bool>,
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
            settings_method: None,
            settings_metadata: HashMap::new(),
            settings_timeout: None,
            settings_as_json: None,
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
        self.settings_method = None;
        self.settings_metadata = HashMap::new();
        self.settings_timeout = None;
        self.settings_as_json = None;
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

        if let Some(connection_timeout) = settings.connection_timeout {
            endpoint = endpoint.timeout(Duration::from_millis(connection_timeout as u64));
        }

        if let Some(max_size) = settings.max_message_size {
            self.max_message_size = Some(max_size as usize);
        }

        if settings.tls {
            let mut tls_config = ClientTlsConfig::new();

            if let Some(tls_settings) = &settings.tls_config {
                let domain = if !tls_settings.server_name_override.is_empty() {
                    tls_settings.server_name_override.clone()
                } else {
                    // Strip scheme prefix for SNI/certificate verification
                    Self::strip_scheme(host)
                };
                tls_config = tls_config.domain_name(domain);

                if !tls_settings.ca_cert_path.is_empty() {
                    let ca_cert = std::fs::read_to_string(&tls_settings.ca_cert_path)?;
                    tls_config = tls_config.ca_certificate(Certificate::from_pem(ca_cert));
                } else {
                    // Use system root CAs when no custom CA is specified
                    let _ = rustls::crypto::ring::default_provider().install_default();
                    tls_config = tls_config.with_enabled_roots();
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
                tls_config = tls_config
                    .domain_name(Self::strip_scheme(host))
                    .with_enabled_roots();
            }

            endpoint = endpoint.tls_config(tls_config)?;
        }

        let channel = endpoint.connect().await?;

        self.use_reflection = settings.use_reflection.unwrap_or(false);
        self.settings_method = settings.method.clone();
        self.settings_metadata = settings.metadata.clone();
        self.settings_timeout = settings.timeout;
        self.settings_as_json = settings.as_json;

        if self.use_reflection {
            let reflection_channel = channel.clone();
            self.reflection_client = Some(
                GrpcReflectionClient::connect(
                    endpoint,
                    reflection_channel,
                    settings
                        .connection_timeout
                        .map(|s| Duration::from_millis(s as u64)),
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

    /// Resolve the effective gRPC method: settings_method takes priority over args_method.
    pub fn resolve_effective_method(&self, args_method: &Option<String>) -> Result<String> {
        if let Some(ref m) = self.settings_method
            && !m.trim().is_empty()
        {
            return Ok(m.clone());
        }
        match args_method {
            Some(m) if !m.is_empty() => Ok(m.clone()),
            _ => Err(anyhow!(
                "No gRPC method specified: set method in GrpcRunnerSettings or GrpcArgs"
            )),
        }
    }

    /// Resolve effective metadata: merges args and settings, with settings keys taking priority.
    pub fn resolve_effective_metadata(
        &self,
        args_metadata: &HashMap<String, String>,
    ) -> HashMap<String, String> {
        if self.settings_metadata.is_empty() {
            return args_metadata.clone();
        }
        let mut merged = args_metadata.clone();
        merged.extend(self.settings_metadata.clone());
        merged
    }

    /// Resolve effective job timeout: settings_timeout takes priority over args_timeout.
    pub fn resolve_effective_timeout(&self, args_timeout: &Option<u32>) -> u32 {
        self.settings_timeout.or(*args_timeout).unwrap_or(0)
    }

    /// Resolve effective as_json: settings_as_json takes priority over args_as_json.
    pub fn resolve_effective_as_json(&self, args_as_json: &Option<bool>) -> bool {
        self.settings_as_json.or(*args_as_json).unwrap_or(false)
    }

    /// Strip http:// or https:// scheme prefix from a host string.
    fn strip_scheme(host: &str) -> String {
        host.strip_prefix("https://")
            .or_else(|| host.strip_prefix("http://"))
            .unwrap_or(host)
            .to_string()
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

    /// Resolve a method descriptor from the given method path, then extract a
    /// message descriptor using the provided extractor function.
    async fn get_method_message_descriptor<F>(
        &self,
        method_path: &str,
        extractor: F,
    ) -> Result<MessageDescriptor>
    where
        F: Fn(&prost_reflect::MethodDescriptor) -> MessageDescriptor,
    {
        let (service_name, method_name) = Self::parse_method_path(method_path)?;

        if let Some(ref reflection_client) = self.reflection_client {
            let pool = reflection_client
                .get_service_with_dependencies(&service_name)
                .await?;

            if let Some(service) = pool.get_service_by_name(&service_name)
                && let Some(method) = service.methods().find(|m| m.name() == method_name)
            {
                return Ok(extractor(&method));
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
                    return Ok(extractor(&method));
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

    pub async fn get_output_message_descriptor(
        &self,
        method_path: &str,
    ) -> Result<MessageDescriptor> {
        self.get_method_message_descriptor(method_path, |m| m.output())
            .await
    }

    pub async fn get_input_message_descriptor(
        &self,
        method_path: &str,
    ) -> Result<MessageDescriptor> {
        self.get_method_message_descriptor(method_path, |m| m.input())
            .await
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
                    // key already includes the "-bin" suffix (tonic restores it on iteration).
                    // as_encoded_bytes() returns base64-encoded ASCII bytes, so
                    // from_utf8_lossy is safe here (no replacement characters possible).
                    let value_str = String::from_utf8_lossy(value.as_encoded_bytes()).to_string();
                    result.insert(key.to_string(), value_str);
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
            if key.ends_with("-bin") {
                // Binary metadata: value is base64-encoded, decode and insert as binary
                // tonic's as_encoded_bytes() may omit padding, so use a pad-tolerant decoder
                const PAD_INDIFFERENT: base64::engine::GeneralPurpose =
                    base64::engine::GeneralPurpose::new(
                        &base64::alphabet::STANDARD,
                        base64::engine::GeneralPurposeConfig::new().with_decode_padding_mode(
                            base64::engine::DecodePaddingMode::Indifferent,
                        ),
                    );
                match base64::Engine::decode(&PAD_INDIFFERENT, value) {
                    Ok(decoded) => {
                        match tonic::metadata::MetadataKey::<tonic::metadata::Binary>::from_bytes(
                            key.as_bytes(),
                        ) {
                            Ok(k) => {
                                metadata_mut.insert_bin(k, MetadataValue::from_bytes(&decoded));
                            }
                            Err(_) => {
                                tracing::warn!("Invalid binary metadata key: {}", key);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to base64-decode binary metadata value for key {}: {}",
                            key,
                            e
                        );
                    }
                }
            } else {
                match MetadataValue::try_from(value.as_str()) {
                    Ok(val) => match tonic::metadata::MetadataKey::from_bytes(key.as_bytes()) {
                        Ok(key) => {
                            metadata_mut.insert(key, val);
                        }
                        Err(_) => {
                            tracing::warn!("Invalid metadata key: {}", key);
                        }
                    },
                    Err(_) => {
                        tracing::warn!("Invalid metadata value for key {}: {}", key, value);
                    }
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
            .field("settings_method", &self.settings_method)
            .field(
                "settings_metadata",
                &redact_sensitive_metadata(&self.settings_metadata),
            )
            .field("settings_timeout", &self.settings_timeout)
            .field("settings_as_json", &self.settings_as_json)
            .finish()
    }
}

const SENSITIVE_METADATA_KEYS: &[&str] =
    &["authorization", "api-key", "x-api-key", "token", "cookie"];

fn redact_sensitive_metadata(metadata: &HashMap<String, String>) -> HashMap<String, String> {
    metadata
        .iter()
        .map(|(k, v)| {
            let lower = k.to_ascii_lowercase();
            if SENSITIVE_METADATA_KEYS.iter().any(|s| lower == *s) {
                (k.clone(), "[REDACTED]".to_string())
            } else {
                (k.clone(), v.clone())
            }
        })
        .collect()
}

impl Default for GrpcConnection {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_scheme_https() {
        assert_eq!(
            GrpcConnection::strip_scheme("https://example.com"),
            "example.com"
        );
    }

    #[test]
    fn test_strip_scheme_http() {
        assert_eq!(
            GrpcConnection::strip_scheme("http://example.com"),
            "example.com"
        );
    }

    #[test]
    fn test_strip_scheme_none() {
        assert_eq!(GrpcConnection::strip_scheme("example.com"), "example.com");
    }

    #[test]
    fn test_parse_method_path_with_leading_slash() {
        let (svc, method) = GrpcConnection::parse_method_path("/my.Service/MyMethod").unwrap();
        assert_eq!(svc, "my.Service");
        assert_eq!(method, "MyMethod");
    }

    #[test]
    fn test_parse_method_path_without_leading_slash() {
        let (svc, method) = GrpcConnection::parse_method_path("my.Service/MyMethod").unwrap();
        assert_eq!(svc, "my.Service");
        assert_eq!(method, "MyMethod");
    }

    #[test]
    fn test_parse_method_path_invalid() {
        assert!(GrpcConnection::parse_method_path("invalid").is_err());
    }

    #[test]
    fn test_metadata_map_to_hashmap_ascii() {
        let mut map = tonic::metadata::MetadataMap::new();
        map.insert("content-type", "application/grpc".parse().unwrap());
        let result = GrpcConnection::metadata_map_to_hashmap(&map);
        assert_eq!(result.get("content-type").unwrap(), "application/grpc");
    }

    #[test]
    fn test_metadata_map_to_hashmap_binary() {
        let mut map = tonic::metadata::MetadataMap::new();
        map.insert_bin(
            "data-bin",
            tonic::metadata::MetadataValue::from_bytes(b"hello"),
        );
        let result = GrpcConnection::metadata_map_to_hashmap(&map);
        // Key should be "data-bin" (not "data-bin-bin")
        assert!(
            result.contains_key("data-bin"),
            "key should be 'data-bin', got: {:?}",
            result.keys().collect::<Vec<_>>()
        );
        assert!(
            !result.contains_key("data-bin-bin"),
            "should not have double -bin suffix"
        );
    }

    #[test]
    fn test_build_request_binary_metadata() {
        use base64::Engine;
        let conn = GrpcConnection::new();
        let mut metadata = HashMap::new();
        let original_bytes = b"hello binary";
        let encoded = base64::engine::general_purpose::STANDARD.encode(original_bytes);
        metadata.insert("trace-bin".to_string(), encoded);
        metadata.insert("x-custom".to_string(), "ascii-value".to_string());

        let request = conn.build_request(vec![], &metadata);
        let meta = request.metadata();

        // Binary key should be retrievable via get_bin
        let bin_val = meta.get_bin("trace-bin").expect("trace-bin should exist");
        assert_eq!(bin_val.to_bytes().unwrap().as_ref(), original_bytes);

        // ASCII key should be retrievable via get
        let ascii_val = meta.get("x-custom").expect("x-custom should exist");
        assert_eq!(ascii_val.to_str().unwrap(), "ascii-value");
    }

    #[test]
    fn test_build_request_binary_metadata_roundtrip() {
        // Simulate: server response metadata → hashmap → build_request
        let mut server_meta = tonic::metadata::MetadataMap::new();
        let original_bytes = b"\x00\x01\x02\xff";
        server_meta.insert_bin(
            "data-bin",
            tonic::metadata::MetadataValue::from_bytes(original_bytes),
        );
        let hashmap = GrpcConnection::metadata_map_to_hashmap(&server_meta);

        let conn = GrpcConnection::new();
        let request = conn.build_request(vec![], &hashmap);
        let bin_val = request
            .metadata()
            .get_bin("data-bin")
            .expect("data-bin should exist");
        assert_eq!(bin_val.to_bytes().unwrap().as_ref(), original_bytes);
    }

    #[test]
    fn test_resolve_effective_method_both_set_settings_wins() {
        let mut conn = GrpcConnection::new();
        conn.settings_method = Some("settings.Service/Method".to_string());
        let result = conn
            .resolve_effective_method(&Some("args.Service/Method".to_string()))
            .unwrap();
        assert_eq!(result, "settings.Service/Method");
    }

    #[test]
    fn test_resolve_effective_method_settings_only() {
        let mut conn = GrpcConnection::new();
        conn.settings_method = Some("settings.Service/Method".to_string());
        let result = conn.resolve_effective_method(&None).unwrap();
        assert_eq!(result, "settings.Service/Method");
    }

    #[test]
    fn test_resolve_effective_method_args_only() {
        let conn = GrpcConnection::new();
        let result = conn
            .resolve_effective_method(&Some("args.Service/Method".to_string()))
            .unwrap();
        assert_eq!(result, "args.Service/Method");
    }

    #[test]
    fn test_resolve_effective_method_neither_set() {
        let conn = GrpcConnection::new();
        let result = conn.resolve_effective_method(&None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No gRPC method"));
    }

    #[test]
    fn test_resolve_effective_method_args_empty_string() {
        let conn = GrpcConnection::new();
        let result = conn.resolve_effective_method(&Some("".to_string()));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No gRPC method"));
    }

    #[test]
    fn test_resolve_effective_metadata_settings_wins() {
        let mut conn = GrpcConnection::new();
        conn.settings_metadata = HashMap::from([("key".to_string(), "settings_val".to_string())]);
        let args_meta = HashMap::from([
            ("key".to_string(), "args_val".to_string()),
            ("args_only".to_string(), "args_only_val".to_string()),
        ]);
        let result = conn.resolve_effective_metadata(&args_meta);
        // settings key overwrites args key
        assert_eq!(result.get("key").unwrap(), "settings_val");
        // args-only key is preserved
        assert_eq!(result.get("args_only").unwrap(), "args_only_val");
    }

    #[test]
    fn test_resolve_effective_metadata_args_fallback() {
        let conn = GrpcConnection::new();
        let args_meta = HashMap::from([("key".to_string(), "args_val".to_string())]);
        let result = conn.resolve_effective_metadata(&args_meta);
        assert_eq!(result.get("key").unwrap(), "args_val");
    }

    #[test]
    fn test_resolve_effective_metadata_both_empty() {
        let conn = GrpcConnection::new();
        let result = conn.resolve_effective_metadata(&HashMap::new());
        assert!(result.is_empty());
    }

    #[test]
    fn test_resolve_effective_timeout_settings_wins() {
        let mut conn = GrpcConnection::new();
        conn.settings_timeout = Some(5000u32);
        assert_eq!(conn.resolve_effective_timeout(&Some(3000u32)), 5000u32);
    }

    #[test]
    fn test_resolve_effective_timeout_args_fallback() {
        let conn = GrpcConnection::new();
        assert_eq!(conn.resolve_effective_timeout(&Some(3000u32)), 3000u32);
    }

    #[test]
    fn test_resolve_effective_timeout_both_none() {
        let conn = GrpcConnection::new();
        assert_eq!(conn.resolve_effective_timeout(&None), 0u32);
    }

    #[test]
    fn test_resolve_effective_as_json_settings_wins() {
        let mut conn = GrpcConnection::new();
        conn.settings_as_json = Some(true);
        assert!(conn.resolve_effective_as_json(&Some(false)));
    }

    #[test]
    fn test_resolve_effective_as_json_args_fallback() {
        let conn = GrpcConnection::new();
        assert!(conn.resolve_effective_as_json(&Some(true)));
    }

    #[test]
    fn test_resolve_effective_as_json_both_none() {
        let conn = GrpcConnection::new();
        assert!(!conn.resolve_effective_as_json(&None));
    }

    #[test]
    fn test_normalize_method_path() {
        let p = GrpcConnection::normalize_method_path("my.Service/Method").unwrap();
        assert_eq!(p.as_str(), "/my.Service/Method");

        let p = GrpcConnection::normalize_method_path("/my.Service/Method").unwrap();
        assert_eq!(p.as_str(), "/my.Service/Method");
    }

    #[test]
    fn test_resolve_effective_method_settings_empty_falls_back_to_args() {
        let mut conn = GrpcConnection::new();
        conn.settings_method = Some("".to_string());
        let result = conn
            .resolve_effective_method(&Some("args.Service/Method".to_string()))
            .unwrap();
        assert_eq!(result, "args.Service/Method");
    }

    #[test]
    fn test_resolve_effective_method_settings_whitespace_falls_back_to_args() {
        let mut conn = GrpcConnection::new();
        conn.settings_method = Some("   ".to_string());
        let result = conn
            .resolve_effective_method(&Some("args.Service/Method".to_string()))
            .unwrap();
        assert_eq!(result, "args.Service/Method");
    }

    #[test]
    fn test_resolve_effective_method_settings_empty_args_empty_errors() {
        let mut conn = GrpcConnection::new();
        conn.settings_method = Some("".to_string());
        let result = conn.resolve_effective_method(&Some("".to_string()));
        assert!(result.is_err());
    }

    #[test]
    fn test_redact_sensitive_metadata() {
        let metadata = HashMap::from([
            ("Authorization".to_string(), "Bearer secret".to_string()),
            ("api-key".to_string(), "my-key".to_string()),
            ("x-api-key".to_string(), "another-key".to_string()),
            ("Token".to_string(), "tok123".to_string()),
            ("Cookie".to_string(), "session=abc".to_string()),
            ("x-request-id".to_string(), "req-123".to_string()),
        ]);
        let redacted = redact_sensitive_metadata(&metadata);
        assert_eq!(redacted.get("Authorization").unwrap(), "[REDACTED]");
        assert_eq!(redacted.get("api-key").unwrap(), "[REDACTED]");
        assert_eq!(redacted.get("x-api-key").unwrap(), "[REDACTED]");
        assert_eq!(redacted.get("Token").unwrap(), "[REDACTED]");
        assert_eq!(redacted.get("Cookie").unwrap(), "[REDACTED]");
        assert_eq!(redacted.get("x-request-id").unwrap(), "req-123");
    }
}
