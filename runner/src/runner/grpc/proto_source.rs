use crate::jobworkerp::runner::grpc::{GrpcArgsProtoSource, GrpcSettingsProtoSource};
use anyhow::{Context, Result, anyhow, bail};
use reqwest::Url;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(crate) const GRPC_PROTO_ALLOWED_DIR: &str = "GRPC_PROTO_ALLOWED_DIR";
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PROTO_SOURCE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProtoSourceRef<'a> {
    source: &'a str,
    fetch_headers: &'a HashMap<String, String>,
    allow_insecure_http: bool,
}

impl<'a> ProtoSourceRef<'a> {
    pub(crate) fn from_settings(source: &'a GrpcSettingsProtoSource) -> Self {
        Self {
            source: &source.source,
            fetch_headers: &source.fetch_headers,
            allow_insecure_http: source.allow_insecure_http.unwrap_or(false),
        }
    }

    pub(crate) fn from_args(source: &'a GrpcArgsProtoSource) -> Self {
        Self {
            source: &source.source,
            fetch_headers: &source.fetch_headers,
            allow_insecure_http: source.allow_insecure_http.unwrap_or(false),
        }
    }
}

pub(crate) async fn fetch_proto_source(
    client: &reqwest::Client,
    source: ProtoSourceRef<'_>,
) -> Result<String> {
    ensure_not_empty(source.source)?;

    match Url::parse(source.source) {
        Ok(url) => match url.scheme() {
            "https" => fetch_http_proto_source(client, url, source.fetch_headers).await,
            "http" if source.allow_insecure_http => {
                fetch_http_proto_source(client, url, source.fetch_headers).await
            }
            "http" => bail!("insecure http proto source is disabled: {}", source.source),
            "file" => read_file_proto_source(&url),
            scheme => bail!("unsupported proto source URI scheme: {scheme}"),
        },
        Err(_) => string_within_limit(source.source, "inline proto source"),
    }
}

fn ensure_not_empty(source: &str) -> Result<()> {
    if source.trim().is_empty() {
        bail!("proto source must not be empty");
    }
    Ok(())
}

async fn fetch_http_proto_source(
    client: &reqwest::Client,
    url: Url,
    headers: &HashMap<String, String>,
) -> Result<String> {
    let mut request = client.get(url.clone()).timeout(FETCH_TIMEOUT);
    for (key, value) in headers {
        request = request.header(key, value);
    }

    let mut response = request
        .send()
        .await
        .with_context(|| format!("failed to fetch proto source from {url}"))?
        .error_for_status()
        .with_context(|| format!("proto source fetch failed for {url}"))?;

    if let Some(length) = response.content_length()
        && length > MAX_PROTO_SOURCE_BYTES as u64
    {
        bail!(
            "proto source size exceeds limit: {} bytes > {} bytes",
            length,
            MAX_PROTO_SOURCE_BYTES
        );
    }

    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .with_context(|| format!("failed to read proto source response from {url}"))?
    {
        bytes.extend_from_slice(&chunk);
        ensure_size_limit(bytes.len(), "proto source response")?;
    }

    String::from_utf8(bytes).context("proto source response is not valid UTF-8")
}

fn read_file_proto_source(url: &Url) -> Result<String> {
    let path = url
        .to_file_path()
        .map_err(|_| anyhow!("invalid file proto source URL: {url}"))?;
    let canonical_path = path.canonicalize().with_context(|| {
        format!(
            "failed to canonicalize proto source file: {}",
            path.display()
        )
    })?;
    ensure_allowed_file_path(&canonical_path)?;

    let metadata = std::fs::metadata(&canonical_path).with_context(|| {
        format!(
            "failed to read proto source metadata: {}",
            canonical_path.display()
        )
    })?;
    if metadata.len() > MAX_PROTO_SOURCE_BYTES as u64 {
        bail!(
            "proto source file size exceeds limit: {} bytes > {} bytes",
            metadata.len(),
            MAX_PROTO_SOURCE_BYTES
        );
    }

    let bytes = std::fs::read(&canonical_path).with_context(|| {
        format!(
            "failed to read proto source file: {}",
            canonical_path.display()
        )
    })?;
    ensure_size_limit(bytes.len(), "proto source file")?;
    String::from_utf8(bytes).context("proto source file is not valid UTF-8")
}

fn ensure_allowed_file_path(path: &Path) -> Result<()> {
    let allowed_dirs = allowed_proto_dirs()?;
    if allowed_dirs.is_empty() {
        bail!("{GRPC_PROTO_ALLOWED_DIR} is not set; file:// proto sources are disabled");
    }

    if allowed_dirs.iter().any(|dir| path.starts_with(dir)) {
        Ok(())
    } else {
        bail!(
            "file:// proto source is outside allowed directories: {}",
            path.display()
        )
    }
}

fn allowed_proto_dirs() -> Result<Vec<PathBuf>> {
    let Some(value) = std::env::var_os(GRPC_PROTO_ALLOWED_DIR) else {
        return Ok(Vec::new());
    };
    let value = value.to_string_lossy();
    value
        .split(':')
        .filter(|entry| !entry.trim().is_empty())
        .map(|entry| {
            PathBuf::from(entry).canonicalize().with_context(|| {
                format!("failed to canonicalize {GRPC_PROTO_ALLOWED_DIR}: {entry}")
            })
        })
        .collect()
}

fn string_within_limit(value: &str, label: &str) -> Result<String> {
    ensure_size_limit(value.len(), label)?;
    Ok(value.to_string())
}

fn ensure_size_limit(size: usize, label: &str) -> Result<()> {
    if size > MAX_PROTO_SOURCE_BYTES {
        bail!("{label} size exceeds limit: {size} bytes > {MAX_PROTO_SOURCE_BYTES} bytes");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc;
    use std::thread;
    use tempfile::TempDir;

    const TEST_PROTO: &str = r#"syntax = "proto3";
package example;
message EchoRequest { string text = 1; }
message EchoResponse { string text = 1; }
service EchoService { rpc Echo(EchoRequest) returns (EchoResponse); }
"#;

    struct EnvGuard {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let original = std::env::var_os(key);
            // SAFETY: These tests are intended to run with --test-threads=1.
            unsafe { std::env::set_var(key, value) };
            Self { key, original }
        }

        fn unset(key: &'static str) -> Self {
            let original = std::env::var_os(key);
            // SAFETY: These tests are intended to run with --test-threads=1.
            unsafe { std::env::remove_var(key) };
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(value) => {
                    // SAFETY: These tests are intended to run with --test-threads=1.
                    unsafe { std::env::set_var(self.key, value) };
                }
                None => {
                    // SAFETY: These tests are intended to run with --test-threads=1.
                    unsafe { std::env::remove_var(self.key) };
                }
            }
        }
    }

    fn source_ref<'a>(
        source: &'a str,
        headers: &'a HashMap<String, String>,
        allow_insecure_http: bool,
    ) -> ProtoSourceRef<'a> {
        ProtoSourceRef {
            source,
            fetch_headers: headers,
            allow_insecure_http,
        }
    }

    #[tokio::test]
    async fn fetch_inline_proto_source() {
        let headers = HashMap::new();
        let fetched = fetch_proto_source(
            &reqwest::Client::new(),
            source_ref(TEST_PROTO, &headers, false),
        )
        .await
        .unwrap();
        assert_eq!(TEST_PROTO, fetched);
    }

    #[tokio::test]
    async fn reject_http_without_explicit_allowance() {
        let headers = HashMap::new();
        let err = fetch_proto_source(
            &reqwest::Client::new(),
            source_ref("http://127.0.0.1:1/echo.proto", &headers, false),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("insecure http"));
    }

    #[tokio::test]
    async fn fetch_http_proto_source_with_headers_when_allowed() {
        let (url, received) = start_http_server(TEST_PROTO.as_bytes().to_vec());
        let mut headers = HashMap::new();
        headers.insert("authorization".to_string(), "Bearer test-token".to_string());

        let fetched = fetch_proto_source(&reqwest::Client::new(), source_ref(&url, &headers, true))
            .await
            .unwrap();

        assert_eq!(TEST_PROTO, fetched);
        let request = received.recv().unwrap();
        assert!(request.contains("authorization: Bearer test-token"));
    }

    #[tokio::test]
    async fn reject_inline_proto_source_over_size_limit() {
        let headers = HashMap::new();
        let oversized = "a".repeat(MAX_PROTO_SOURCE_BYTES + 1);
        let err = fetch_proto_source(
            &reqwest::Client::new(),
            source_ref(&oversized, &headers, false),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("size exceeds limit"));
    }

    #[tokio::test]
    async fn reject_http_response_over_size_limit() {
        let (url, _received) = start_http_server(vec![b'a'; MAX_PROTO_SOURCE_BYTES + 1]);
        let headers = HashMap::new();
        let err = fetch_proto_source(&reqwest::Client::new(), source_ref(&url, &headers, true))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("size exceeds limit"));
    }

    #[tokio::test]
    async fn reject_file_proto_source_when_allowed_dir_is_unset() {
        let _guard = EnvGuard::unset(GRPC_PROTO_ALLOWED_DIR);
        let temp_dir = TempDir::new().unwrap();
        let proto_path = temp_dir.path().join("echo.proto");
        std::fs::write(&proto_path, TEST_PROTO).unwrap();
        let headers = HashMap::new();

        let err = fetch_proto_source(
            &reqwest::Client::new(),
            source_ref(&file_url(&proto_path), &headers, false),
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains(GRPC_PROTO_ALLOWED_DIR));
    }

    #[tokio::test]
    async fn fetch_file_proto_source_inside_allowed_dir() {
        let temp_dir = TempDir::new().unwrap();
        let _guard = EnvGuard::set(GRPC_PROTO_ALLOWED_DIR, temp_dir.path());
        let proto_path = temp_dir.path().join("echo.proto");
        std::fs::write(&proto_path, TEST_PROTO).unwrap();
        let headers = HashMap::new();

        let fetched = fetch_proto_source(
            &reqwest::Client::new(),
            source_ref(&file_url(&proto_path), &headers, false),
        )
        .await
        .unwrap();

        assert_eq!(TEST_PROTO, fetched);
    }

    #[tokio::test]
    async fn reject_file_proto_source_outside_allowed_dir() {
        let allowed_dir = TempDir::new().unwrap();
        let outside_dir = TempDir::new().unwrap();
        let _guard = EnvGuard::set(GRPC_PROTO_ALLOWED_DIR, allowed_dir.path());
        let proto_path = outside_dir.path().join("echo.proto");
        std::fs::write(&proto_path, TEST_PROTO).unwrap();
        let headers = HashMap::new();

        let err = fetch_proto_source(
            &reqwest::Client::new(),
            source_ref(&file_url(&proto_path), &headers, false),
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("outside allowed directories"));
    }

    #[ignore]
    #[tokio::test]
    async fn fetch_https_proto_source_with_test_ca_and_headers() {
        use rcgen::{CertifiedKey, generate_simple_self_signed};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener as TokioTcpListener;
        use tokio_rustls::TlsAcceptor;
        use tokio_rustls::rustls::{
            ServerConfig,
            crypto::ring,
            pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
        };

        let _ = ring::default_provider().install_default();
        let CertifiedKey { cert, signing_key } =
            generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let cert_der = CertificateDer::from(cert.der().to_vec());
        let key_der = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(signing_key.serialize_der()));
        let server_config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der.clone()], key_der)
            .unwrap();
        let acceptor = TlsAcceptor::from(std::sync::Arc::new(server_config));
        let listener = TokioTcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (received_tx, received_rx) = mpsc::channel();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = acceptor.accept(stream).await.unwrap();
            let mut buf = vec![0_u8; 4096];
            let n = stream.read(&mut buf).await.unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            received_tx.send(request).unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{}",
                TEST_PROTO.len(),
                TEST_PROTO
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.shutdown().await.unwrap();
        });

        let cert = reqwest::Certificate::from_der(cert_der.as_ref()).unwrap();
        let client = reqwest::Client::builder()
            .add_root_certificate(cert)
            .build()
            .unwrap();
        let mut headers = HashMap::new();
        headers.insert(
            "authorization".to_string(),
            "Bearer https-token".to_string(),
        );

        let fetched = fetch_proto_source(
            &client,
            source_ref(
                &format!("https://localhost:{}/echo.proto", addr.port()),
                &headers,
                false,
            ),
        )
        .await
        .unwrap();

        assert_eq!(TEST_PROTO, fetched);
        assert!(
            received_rx
                .recv()
                .unwrap()
                .contains("authorization: Bearer https-token")
        );
    }

    fn start_http_server(body: Vec<u8>) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            tx.send(request).unwrap();
            write_http_response(&mut stream, &body);
        });
        (format!("http://{addr}/echo.proto"), rx)
    }

    fn read_http_request(stream: &mut TcpStream) -> String {
        let mut buf = [0_u8; 4096];
        let n = stream.read(&mut buf).unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    }

    fn write_http_response(stream: &mut TcpStream, body: &[u8]) {
        let header = format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n", body.len());
        stream.write_all(header.as_bytes()).unwrap();
        stream.write_all(body).unwrap();
    }

    fn file_url(path: &Path) -> String {
        Url::from_file_path(path).unwrap().to_string()
    }
}
