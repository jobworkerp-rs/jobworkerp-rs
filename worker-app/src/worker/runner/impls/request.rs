use std::{collections::HashMap, str::FromStr, time::Duration};

use super::super::Runner;
use anyhow::Result;
use async_trait::async_trait;
use infra::error::JobWorkerError;
use reqwest::{
    header::{HeaderMap, HeaderName},
    Method, Url,
};
use serde::Deserialize;

#[derive(Clone, Debug)]
pub struct RequestRunner {
    pub client: reqwest::Client,
    pub url: Url,
}

#[derive(Deserialize, Debug, Clone)]
struct Arg {
    headers: Option<HashMap<String, Vec<String>>>,
    queries: Option<Vec<(String, String)>>,
    method: Option<String>,
    body: Option<String>,
    path: Option<String>,
}

impl RequestRunner {
    // TODO Error type
    // operation: base url (+ arg.path)
    pub fn new(operation: &str) -> Result<RequestRunner> {
        let u = Url::from_str(operation).map_err(|e| {
            JobWorkerError::ParseError(format!(
                "cannot parse url from operation: {}, error= {:?}",
                operation, e
            ))
        })?;
        // TODO http client option from settings
        let c = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10)) // set default header?
            .build()
            .map_err(|e| JobWorkerError::OtherError(format!("http client build error: {:?}", e)))?;
        Ok(RequestRunner { client: c, url: u })
    }
}

#[async_trait]
// arg: {headers:{<headers map>}, queries:[<query string array>], body: <body string or struct>}
// res: vec![result_bytes]  (fixed size 1)
impl Runner for RequestRunner {
    async fn name(&self) -> String {
        format!("RequestRunner: url {}", self.url)
    }
    async fn run(&mut self, arg: Vec<u8>) -> Result<Vec<Vec<u8>>> {
        // parse arg
        let res: Arg = serde_json::from_slice(arg.as_ref())?;
        let met = Method::from_str(res.method.as_deref().unwrap_or("GET"))?;
        let u = self.url.join(res.path.as_deref().unwrap_or(""))?;
        // create request
        let req = self.client.request(met, u);
        // set body
        let req = if let Some(b) = res.body {
            req.body(b)
        } else {
            req
        };
        // set queries
        let req = if let Some(q) = res.queries {
            req.query(&q)
        } else {
            req
        };
        // set headers
        let req = if let Some(headers) = res.headers {
            let mut hm = HeaderMap::new();
            for (k, vec) in headers.into_iter() {
                for v in vec.iter() {
                    let k1: HeaderName = k.parse().map_err(|e| {
                        JobWorkerError::ParseError(format!("header value error: {:?}", e))
                    })?;
                    let v1 = v.parse().map_err(|e| {
                        JobWorkerError::ParseError(format!("header value error: {:?}", e))
                    })?;
                    hm.append(k1, v1);
                }
            }
            req.headers(hm)
        } else {
            req
        };
        // send request and await
        req.send()
            .await
            .map_err(JobWorkerError::ReqwestError)?
            .bytes()
            .await
            .map(|v| vec![v.to_vec()])
            .map_err(|e| JobWorkerError::ReqwestError(e).into())
    }

    async fn cancel(&mut self) {
        tracing::warn!("cannot cancel request until timeout")
    }
}

#[tokio::test]
async fn run_request() {
    let mut runner = RequestRunner::new("https://www.google.com/").unwrap();
    let arg = r#"{"headers":{"Content-Type":["plain/text"]},"queries":[["q","rust async"],["ie","UTF-8"]],"path":"search","method":"GET"}"#
        .as_bytes()
        .to_vec();
    let _r: Result<Arg, serde_json::Error> = serde_json::from_slice(arg.as_ref());

    let res = runner.run(arg.clone()).await;
    assert!(res.is_ok());

    let out = res.as_ref().unwrap().first().unwrap();
    println!(
        "arg: {}, res: {:?}",
        String::from_utf8_lossy(arg.as_slice()),
        String::from_utf8_lossy(out.as_slice()),
    );
}
