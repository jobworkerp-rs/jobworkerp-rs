use std::{collections::HashMap, str::FromStr, sync::Arc, time::Duration};

use super::cancellation::RunnerCancellationManager;
use super::{RunnerSpec, RunnerTrait};
use crate::jobworkerp::runner::{HttpRequestArgs, HttpRequestRunnerSettings, HttpResponseResult};
use crate::{schema_to_json_string, schema_to_json_string_option};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use jobworkerp_base::{
    codec::{ProstMessageCodec, UseProstCodec},
    error::JobWorkerError,
};
use proto::jobworkerp::data::{ResultOutputItem, RunnerType, StreamingOutputType};
use reqwest::{
    header::{HeaderMap, HeaderName},
    Method, Url,
};
use tokio_util::sync::CancellationToken;

/// HTTP request runner.
/// Handles HTTP requests with streaming support.
///
/// **Response Format**:
/// - `run()` method: Returns `string content` for text-based responses
/// - `run_stream()` method: Returns `bytes chunk` to preserve UTF-8 integrity
///
/// The protobuf uses `oneof response_data` to distinguish between the two response types.
#[derive(Debug)]
pub struct RequestRunner {
    pub client: reqwest::Client,
    pub url: Option<Url>,
    cancellation_manager: Option<Arc<tokio::sync::Mutex<Box<dyn RunnerCancellationManager>>>>,
}

impl RequestRunner {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            url: None,
            cancellation_manager: None,
        }
    }

    /// 統一されたtoken取得メソッド
    async fn get_cancellation_token(&self) -> CancellationToken {
        if let Some(manager) = &self.cancellation_manager {
            manager.lock().await.get_token().await
        } else {
            // fallback: basic token
            CancellationToken::new()
        }
    }

    pub fn set_cancellation_manager(
        &mut self,
        cancellation_manager: Box<dyn RunnerCancellationManager>,
    ) {
        self.cancellation_manager = Some(Arc::new(tokio::sync::Mutex::new(cancellation_manager)));
    }

    // TODO Error type
    // settings: base url (+ arg.path)
    pub fn create(&mut self, base_url: &str) -> Result<()> {
        let u = Url::from_str(base_url).map_err(|e| {
            JobWorkerError::ParseError(format!("cannot parse url from: {base_url}, error= {e:?}"))
        })?;
        // TODO http client option from settings
        let c = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10)) // set default header?
            .build()
            .map_err(|e| JobWorkerError::OtherError(format!("http client build error: {e:?}")))?;
        self.client = c;
        self.url = Some(u);
        Ok(())
    }
}

impl Default for RequestRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl RunnerSpec for RequestRunner {
    fn name(&self) -> String {
        RunnerType::HttpRequest.as_str_name().to_string()
    }
    fn runner_settings_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/http_request_runner.proto").to_string()
    }
    fn job_args_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/http_request_args.proto").to_string()
    }
    fn result_output_proto(&self) -> Option<String> {
        Some(include_str!("../../protobuf/jobworkerp/runner/http_request_result.proto").to_string())
    }
    fn output_type(&self) -> StreamingOutputType {
        StreamingOutputType::Both
    }
    fn settings_schema(&self) -> String {
        schema_to_json_string!(HttpRequestRunnerSettings, "settings_schema")
    }
    fn arguments_schema(&self) -> String {
        schema_to_json_string!(HttpRequestArgs, "arguments_schema")
    }
    fn output_schema(&self) -> Option<String> {
        schema_to_json_string_option!(HttpResponseResult, "output_schema")
    }
}
// arg: {headers:{<headers map>}, queries:[<query string array>], body: <body string or struct>}
// res: vec![result_bytes]  (fixed size 1)
#[async_trait]
impl RunnerTrait for RequestRunner {
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        let op = ProstMessageCodec::deserialize_message::<HttpRequestRunnerSettings>(&settings)?;
        self.create(op.base_url.as_str())
    }
    async fn run(
        &mut self,
        args: &[u8],
        metadata: HashMap<String, String>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        // 明確で簡潔なtoken取得
        let cancellation_token = self.get_cancellation_token().await;

        let result = async {
            // Check for cancellation before starting
            if cancellation_token.is_cancelled() {
                return Err(anyhow!("HTTP request was cancelled before execution"));
            }

            if let Some(url) = self.url.as_ref() {
                let args = ProstMessageCodec::deserialize_message::<HttpRequestArgs>(args)?;
                let met = Method::from_str(args.method.as_str())?;
                let u = url.join(args.path.as_str())?;
                // create request
                let req = self.client.request(met, u);
                // set body
                let req = if let Some(b) = &args.body {
                    req.body(b.to_owned())
                } else {
                    req
                };
                // set queries
                let req = if args.queries.is_empty() {
                    req
                } else {
                    req.query(
                        &args
                            .queries
                            .iter()
                            .map(|kv| (kv.key.as_str(), kv.value.as_str()))
                            .collect::<Vec<_>>(),
                    )
                };
                // set headers
                let req = if args.headers.is_empty() {
                    req
                } else {
                    let mut hm = HeaderMap::new();
                    for kv in args.headers.iter() {
                        let k1: HeaderName = kv.key.parse().map_err(|e| {
                            JobWorkerError::ParseError(format!("header value error: {e:?}"))
                        })?;
                        let v1 = kv.value.parse().map_err(|e| {
                            JobWorkerError::ParseError(format!("header value error: {e:?}"))
                        })?;
                        hm.append(k1, v1);
                    }
                    req.headers(hm)
                };
                // Send request with cancellation support
                let result = tokio::select! {
                    response_result = req.send() => {
                        match response_result {
                            Ok(res) => {
                                let h = res.headers().clone();
                                let s = res.status().as_u16();
                                let t = res.text().await.map_err(JobWorkerError::ReqwestError)?;
                                let mes = HttpResponseResult {
                                    status_code: s as u32,
                                    headers: h
                                        .iter()
                                        .map(
                                            |(k, v)| crate::jobworkerp::runner::http_response_result::KeyValue {
                                                key: k.as_str().to_string(),
                                                value: v.to_str().unwrap().to_string(),
                                            },
                                        )
                                        .collect(),
                                    response_data: Some(crate::jobworkerp::runner::http_response_result::ResponseData::Content(t)),
                                };
                                Ok(ProstMessageCodec::serialize_message(&mes)?)
                            }
                            Err(e) => Err(JobWorkerError::ReqwestError(e).into())
                        }
                    }
                    _ = cancellation_token.cancelled() => {
                        Err(anyhow!("HTTP request was cancelled"))
                    }
                };
                result
            } else {
                Err(JobWorkerError::RuntimeError("url is not set".to_string()).into())
            }
        }.await;

        // 結果処理も簡素化
        (result, metadata)
    }
    async fn run_stream(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        // Set up cancellation token for pre-execution cancellation check
        let cancellation_token = self.get_cancellation_token().await;

        let url = self.url.clone().ok_or_else(|| anyhow!("url is not set"))?;
        let client = self.client.clone();
        let args = ProstMessageCodec::deserialize_message::<HttpRequestArgs>(arg)?;

        use async_stream::stream;
        use proto::jobworkerp::data::{result_output_item::Item, Trailer};

        let trailer = Arc::new(Trailer {
            metadata: metadata.clone(),
        });

        let stream = stream! {
            // Build the request
            let method = Method::from_str(&args.method);
            let url_result = url.join(&args.path);

            match (method, url_result) {
                (Ok(met), Ok(u)) => {
                    // Create request
                    let mut req = client.request(met, u);

                    // Set body
                    if let Some(b) = &args.body {
                        req = req.body(b.to_owned());
                    }

                    // Set queries
                    if !args.queries.is_empty() {
                        req = req.query(
                            &args.queries
                                .iter()
                                .map(|kv| (kv.key.as_str(), kv.value.as_str()))
                                .collect::<Vec<_>>(),
                        );
                    }

                    // Set headers
                    if !args.headers.is_empty() {
                        let mut hm = HeaderMap::new();
                        let mut header_ok = true;
                        for kv in args.headers.iter() {
                            match kv.key.parse::<HeaderName>() {
                                Ok(k1) => {
                                    match kv.value.parse() {
                                        Ok(v1) => {
                                            hm.append(k1, v1);
                                        }
                                        Err(e) => {
                                            tracing::error!("Invalid header value: {}", e);
                                            header_ok = false;
                                            break;
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::error!("Invalid header name: {}", e);
                                    header_ok = false;
                                    break;
                                }
                            }
                        }
                        if header_ok {
                            req = req.headers(hm);
                        }
                    }

                    // Send request with cancellation support
                    let response_result = tokio::select! {
                        result = req.send() => result,
                        _ = cancellation_token.cancelled() => {
                            tracing::info!("HTTP stream request was cancelled");
                            yield ResultOutputItem {
                                item: Some(Item::End((*trailer).clone())),
                            };
                            return;
                        }
                    };

                    match response_result {
                        Ok(res) => {
                            let h = res.headers().clone();
                            let s = res.status().as_u16();

                            // Get response as bytes stream for streaming support
                            let mut bytes_stream = res.bytes_stream();
                            let mut content_bytes = Vec::new();

                            // Process the stream with cancellation support
                            loop {
                                let chunk_result = tokio::select! {
                                    chunk = bytes_stream.next() => chunk,
                                    _ = cancellation_token.cancelled() => {
                                        tracing::info!("HTTP stream response reading was cancelled");
                                        yield ResultOutputItem {
                                            item: Some(Item::End((*trailer).clone())),
                                        };
                                        return;
                                    }
                                };

                                match chunk_result {
                                    Some(Ok(chunk)) => {
                                        content_bytes.extend_from_slice(&chunk);
                                        // Continue collecting chunks without yielding intermediate results
                                        // This allows for proper streaming while avoiding excessive intermediate yields
                                    }
                                    Some(Err(e)) => {
                                        tracing::error!("HTTP response stream error: {}", e);
                                        break;
                                    }
                                    None => {
                                        // Stream ended, yield the final complete response as bytes
                                        // Using bytes chunk preserves UTF-8 integrity by avoiding
                                        // premature string conversion of potentially incomplete UTF-8 sequences
                                        let mes = HttpResponseResult {
                                            status_code: s as u32,
                                            headers: h
                                                .iter()
                                                .map(|(k, v)| crate::jobworkerp::runner::http_response_result::KeyValue {
                                                    key: k.as_str().to_string(),
                                                    value: v.to_str().unwrap_or_default().to_string(),
                                                })
                                                .collect(),
                                            response_data: Some(crate::jobworkerp::runner::http_response_result::ResponseData::Chunk(content_bytes)),
                                        };

                                        // Serialize and yield the final result
                                        match ProstMessageCodec::serialize_message(&mes) {
                                            Ok(serialized) => {
                                                yield ResultOutputItem {
                                                    item: Some(Item::Data(serialized)),
                                                };
                                            }
                                            Err(e) => {
                                                tracing::error!("Failed to serialize HTTP response: {}", e);
                                            }
                                        }
                                        break;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!("HTTP request failed: {}", e);
                        }
                    }
                }
                (Err(e), _) => {
                    tracing::error!("Invalid HTTP method: {}", e);
                }
                (_, Err(e)) => {
                    tracing::error!("Invalid URL: {}", e);
                }
            }

            // Send end marker
            yield ResultOutputItem {
                item: Some(Item::End((*trailer).clone())),
            };
        };

        // Keep cancellation token for potential mid-stream cancellation
        // Note: The token will be cleared when cancel() is called
        Ok(Box::pin(stream))
    }

    async fn cancel(&mut self) {
        // Cancel using manager
        if let Some(manager) = &self.cancellation_manager {
            let manager = manager.lock().await;
            if manager.is_cancelled() {
                tracing::info!("RequestRunner execution is already cancelled");
            } else {
                tracing::info!(
                    "RequestRunner cancellation requested, but Manager handles token internally"
                );
            }
        } else {
            tracing::warn!("No cancellation manager set, cannot cancel");
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

// CancelMonitoring implementation for RequestRunner
#[async_trait]
impl super::cancellation::CancelMonitoring for RequestRunner {
    /// Initialize cancellation monitoring for specific job
    async fn setup_cancellation_monitoring(
        &mut self,
        job_id: proto::jobworkerp::data::JobId,
        _job_data: &proto::jobworkerp::data::JobData,
    ) -> Result<Option<proto::jobworkerp::data::JobResult>> {
        tracing::debug!(
            "Setting up cancellation monitoring for RequestRunner job {}",
            job_id.value
        );

        // For RequestRunner, we use the same pattern as CommandRunner
        // The actual cancellation monitoring will be handled by the CancellationHelper
        // HTTP requests will be cancelled automatically when the token is cancelled

        tracing::trace!("Cancellation monitoring started for job {}", job_id.value);
        Ok(None) // Continue with normal execution
    }

    /// Cleanup cancellation monitoring
    async fn cleanup_cancellation_monitoring(&mut self) -> Result<()> {
        tracing::trace!("Cleaning up cancellation monitoring for RequestRunner");

        // Clear the cancellation helper
        // Note: token cleanup is handled by Manager

        Ok(())
    }
}

// CancelMonitoringCapable implementation for RequestRunner
impl super::cancellation::CancelMonitoringCapable for RequestRunner {
    fn as_cancel_monitoring(&mut self) -> &mut dyn super::cancellation::CancelMonitoring {
        self
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    // Use common mock from test_common module

    #[tokio::test]
    async fn run_request() {
        use crate::jobworkerp::runner::{http_request_args::KeyValue, HttpRequestArgs};

        let mut runner = RequestRunner::new();
        runner.create("https://www.google.com/").unwrap();
        let arg = ProstMessageCodec::serialize_message(&HttpRequestArgs {
            headers: vec![KeyValue {
                key: "Content-Type".to_string(),
                value: "plain/text".to_string(),
            }],
            queries: vec![
                KeyValue {
                    key: "q".to_string(),
                    value: "rust async".to_string(),
                },
                KeyValue {
                    key: "ie".to_string(),
                    value: "UTF-8".to_string(),
                },
            ],
            method: "GET".to_string(),
            body: None,
            path: "search".to_string(),
        })
        .unwrap();

        let res = runner.run(&arg, HashMap::new()).await;

        let out = &res.0.as_ref().unwrap();
        println!(
            "arg: {:?}, res: {:?}",
            arg,
            String::from_utf8_lossy(out.as_slice()),
        );
        assert!(res.0.is_ok());
    }

    #[tokio::test]
    async fn test_http_pre_execution_cancellation() {
        let mut runner = RequestRunner::new();

        // Note: In unified architecture, Manager handles token internally
        // let cancellation_token = tokio_util::sync::CancellationToken::new();
        // cancellation_token.cancel();

        use crate::jobworkerp::runner::HttpRequestArgs;
        let http_args = HttpRequestArgs {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers: vec![],
            queries: vec![],
            body: None,
        };

        let start_time = std::time::Instant::now();
        let (result, _) = runner
            .run(
                &ProstMessageCodec::serialize_message(&http_args).unwrap(),
                HashMap::new(),
            )
            .await;
        let elapsed = start_time.elapsed();

        // Should fail immediately due to pre-execution cancellation
        assert!(result.is_err());
        assert!(elapsed < std::time::Duration::from_millis(100));

        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("cancelled before"));
    }

    // Note: Complex cancellation tests moved to app-wrapper integration tests
    // runner crate level tests focus on basic functionality only
}
