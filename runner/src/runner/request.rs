use std::{str::FromStr, time::Duration};

use super::RunnerTrait;
use crate::jobworkerp::runner::{
    http_request_result::KeyValue, HttpRequestArgs, HttpRequestResult, HttpRequestRunnerSettings,
};
use anyhow::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
use jobworkerp_base::{
    codec::{ProstMessageCodec, UseProstCodec},
    error::JobWorkerError,
};
use proto::jobworkerp::data::{ResultOutputItem, RunnerType};
use reqwest::{
    header::{HeaderMap, HeaderName},
    Method, Url,
};

#[derive(Clone, Debug)]
pub struct RequestRunner {
    pub client: reqwest::Client,
    pub url: Option<Url>,
}

impl RequestRunner {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            url: None,
        }
    }
    // TODO Error type
    // settings: base url (+ arg.path)
    pub fn create(&mut self, base_url: &str) -> Result<()> {
        let u = Url::from_str(base_url).map_err(|e| {
            JobWorkerError::ParseError(format!(
                "cannot parse url from: {}, error= {:?}",
                base_url, e
            ))
        })?;
        // TODO http client option from settings
        let c = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10)) // set default header?
            .build()
            .map_err(|e| JobWorkerError::OtherError(format!("http client build error: {:?}", e)))?;
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

// arg: {headers:{<headers map>}, queries:[<query string array>], body: <body string or struct>}
// res: vec![result_bytes]  (fixed size 1)
#[async_trait]
impl RunnerTrait for RequestRunner {
    fn name(&self) -> String {
        RunnerType::HttpRequest.as_str_name().to_string()
    }
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        let op = ProstMessageCodec::deserialize_message::<HttpRequestRunnerSettings>(&settings)?;
        self.create(op.base_url.as_str())
    }
    async fn run(&mut self, args: &[u8]) -> Result<Vec<Vec<u8>>> {
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
                        JobWorkerError::ParseError(format!("header value error: {:?}", e))
                    })?;
                    let v1 = kv.value.parse().map_err(|e| {
                        JobWorkerError::ParseError(format!("header value error: {:?}", e))
                    })?;
                    hm.append(k1, v1);
                }
                req.headers(hm)
            };
            // send request and await
            let res = req.send().await.map_err(JobWorkerError::ReqwestError)?;
            let h = res.headers().clone();
            let s = res.status().as_u16();
            let t = res.text().await.map_err(JobWorkerError::ReqwestError)?;
            let mes = HttpRequestResult {
                status_code: s as u32,
                headers: h
                    .iter()
                    .map(|(k, v)| KeyValue {
                        key: k.as_str().to_string(),
                        value: v.to_str().unwrap().to_string(),
                    })
                    .collect(),
                content: t,
            };
            Ok(vec![ProstMessageCodec::serialize_message(&mes)?])
        } else {
            Err(JobWorkerError::RuntimeError("url is not set".to_string()).into())
        }
    }
    async fn run_stream(&mut self, arg: &[u8]) -> Result<BoxStream<'static, ResultOutputItem>> {
        // default implementation (return empty)
        let _ = arg;
        Err(anyhow::anyhow!("not implemented"))
    }

    async fn cancel(&mut self) {
        tracing::warn!("cannot cancel request until timeout")
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
    fn output_as_stream(&self) -> Option<bool> {
        Some(false)
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

    let res = runner.run(&arg).await;

    let out = res.as_ref().unwrap().first().unwrap();
    println!(
        "arg: {:?}, res: {:?}",
        arg,
        String::from_utf8_lossy(out.as_slice()),
    );
    assert!(res.is_ok());
}
