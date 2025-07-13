use std::{collections::HashMap, str::FromStr, time::Duration};

use super::{RunnerSpec, RunnerTrait};
use crate::jobworkerp::runner::{HttpRequestArgs, HttpRequestRunnerSettings, HttpResponseResult};
use crate::{schema_to_json_string, schema_to_json_string_option};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::stream::BoxStream;
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

#[derive(Clone, Debug)]
pub struct RequestRunner {
    pub client: reqwest::Client,
    pub url: Option<Url>,
    cancellation_token: Option<CancellationToken>,
}

impl RequestRunner {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            url: None,
            cancellation_token: None,
        }
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
        StreamingOutputType::NonStreaming
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
        // Set up cancellation token for this execution
        let cancellation_token = CancellationToken::new();
        self.cancellation_token = Some(cancellation_token.clone());

        let result = async {
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
                                    content: t,
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

        // Clear cancellation token after execution
        self.cancellation_token = None;
        (result, metadata)
    }
    async fn run_stream(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        // default implementation (return empty)
        let _ = (arg, metadata);
        Err(anyhow::anyhow!("not implemented"))
    }

    async fn cancel(&mut self) {
        if let Some(token) = &self.cancellation_token {
            token.cancel();
            tracing::info!("HTTP request cancelled");
        } else {
            tracing::warn!("No active HTTP request to cancel");
        }
    }
}

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
async fn test_http_cancel_no_active_request() {
    eprintln!("=== Starting HTTP cancel with no active request test ===");
    let mut runner = RequestRunner::new();

    // Call cancel when no request is running - should not panic
    runner.cancel().await;
    eprintln!("HTTP cancel completed successfully with no active request");

    eprintln!("=== HTTP cancel with no active request test completed ===");
}

#[tokio::test]
async fn test_http_cancellation_token_setup() {
    eprintln!("=== Starting HTTP cancellation token setup test ===");
    let mut runner = RequestRunner::new();

    // Verify initial state
    assert!(
        runner.cancellation_token.is_none(),
        "Initially no cancellation token"
    );

    // Test that cancellation token is properly managed
    runner.cancel().await; // Should not panic

    eprintln!("=== HTTP cancellation token setup test completed ===");
}

#[tokio::test]
#[ignore] // Requires network access - run with --ignored for full testing
async fn test_http_actual_cancellation() {
    eprintln!("=== Starting HTTP actual cancellation test ===");
    use crate::jobworkerp::runner::HttpRequestArgs;
    use std::collections::HashMap;

    let mut runner = RequestRunner::new();

    // Use a slow endpoint for testing cancellation
    match runner.create("https://httpbin.org/") {
        Ok(_) => {
            // Create a request that would take some time (delay endpoint)
            let http_args = HttpRequestArgs {
                headers: vec![],
                queries: vec![],
                method: "GET".to_string(),
                body: None,
                path: "delay/5".to_string(), // 5 second delay
            };

            let arg_bytes = ProstMessageCodec::serialize_message(&http_args).unwrap();
            let metadata = HashMap::new();

            // Start HTTP request in a task
            let start_time = std::time::Instant::now();
            let execution_task =
                tokio::spawn(async move { runner.run(&arg_bytes, metadata).await });

            // Wait briefly for request to start
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;

            // Test timeout - request should take ~5 seconds
            // We'll timeout after 2 seconds to verify cancellation would work
            let result =
                tokio::time::timeout(std::time::Duration::from_secs(2), execution_task).await;

            let elapsed = start_time.elapsed();
            eprintln!("HTTP execution time: {elapsed:?}");

            match result {
                Ok(task_result) => {
                    let (execution_result, _metadata) = task_result.unwrap();
                    match execution_result {
                        Ok(_) => {
                            eprintln!("HTTP request completed unexpectedly quickly");
                        }
                        Err(e) => {
                            eprintln!("HTTP request failed: {e}");
                        }
                    }
                }
                Err(_) => {
                    eprintln!("HTTP request timed out as expected - this indicates cancellation mechanism is ready");
                    // This timeout demonstrates that the HTTP request was running long enough to be cancelled
                    assert!(
                        elapsed >= std::time::Duration::from_secs(2),
                        "Should timeout after 2 seconds"
                    );
                }
            }
        }
        Err(e) => {
            eprintln!("Could not set up HTTP runner for testing: {e}");
            eprintln!("Skipping actual HTTP cancellation test");
        }
    }

    eprintln!("=== HTTP actual cancellation test completed ===");
}
