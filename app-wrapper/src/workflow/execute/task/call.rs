use super::TaskExecutorTrait;
use crate::workflow::{
    definition::{
        transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
        workflow::{self, HttpOutput},
    },
    execute::{
        context::{TaskContext, WorkflowContext},
        expression::UseExpression,
        task::run::resolve_run_task_timeout_sec,
    },
};
use anyhow::{Result, anyhow};
use app::app::{
    job::execute::{JobExecutorWrapper, UseJobExecutor},
    runner::UseRunnerApp,
};
use command_utils::trace::Tracing;
use proto::jobworkerp::data::{QueueType, ResponseType, StreamingType, WorkerData};
use serde_json::{Map, Value, json};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::RwLock;

const HTTP_REQUEST_RUNNER_NAME: &str = "HTTP_REQUEST";

// Response fields emitted by the HTTP_REQUEST runner. The runner output shape is
// not fully stable, so several field names are probed defensively.
const RES_CONTENT: &str = "content";
const RES_BODY: &str = "body";
const RES_RESPONSE_DATA: &str = "responseData";
const RES_HEADERS: &str = "headers";
const RES_KEY: &str = "key";
const RES_VALUE: &str = "value";
const RES_STATUS_CODE_CAMEL: &str = "statusCode";
const RES_STATUS_CODE_SNAKE: &str = "status_code";
const HEADER_CONTENT_TYPE: &str = "content-type";
const CONTENT_TYPE_JSON: &str = "application/json";

pub struct CallTaskExecutor {
    workflow_context: Arc<RwLock<WorkflowContext>>,
    default_task_timeout: Duration,
    task: workflow::CallTask,
    job_executor_wrapper: Arc<JobExecutorWrapper>,
    metadata: Arc<HashMap<String, String>>,
}

impl UseExpression for CallTaskExecutor {}
impl UseJqAndTemplateTransformer for CallTaskExecutor {}
impl UseExpressionTransformer for CallTaskExecutor {}
impl Tracing for CallTaskExecutor {}

impl CallTaskExecutor {
    pub fn new(
        workflow_context: Arc<RwLock<WorkflowContext>>,
        default_task_timeout: Duration,
        job_executor_wrapper: Arc<JobExecutorWrapper>,
        task: workflow::CallTask,
        metadata: Arc<HashMap<String, String>>,
    ) -> Self {
        Self {
            workflow_context,
            default_task_timeout,
            task,
            job_executor_wrapper,
            metadata,
        }
    }

    fn unsupported(field: &str, message: String) -> Box<workflow::Error> {
        workflow::errors::ErrorFactory::new().bad_argument(
            format!("Unsupported call.http {field}: {message}"),
            None,
            None,
        )
    }

    fn endpoint_uri(endpoint: &workflow::HttpEndpoint) -> Result<String> {
        match serde_json::to_value(endpoint)? {
            Value::String(uri) => Ok(uri),
            Value::Object(mut object) => {
                if object.get("authentication").is_some() {
                    return Err(anyhow!("endpoint authentication is not supported"));
                }
                object
                    .remove("uri")
                    .and_then(|v| v.as_str().map(ToOwned::to_owned))
                    .ok_or_else(|| anyhow!("endpoint.uri must be a string"))
            }
            _ => Err(anyhow!("endpoint must be a string URI or object with uri")),
        }
    }

    // Validate Phase 1 constraints and return the resolved endpoint URI so the
    // caller can reuse it without re-serializing the endpoint.
    fn ensure_supported(call: &workflow::CallHttp) -> Result<String, Box<workflow::Error>> {
        if call.redirect.is_some() {
            return Err(Self::unsupported(
                "redirect",
                "redirect policy is not supported in Phase 1".to_string(),
            ));
        }
        let uri = Self::endpoint_uri(&call.endpoint)
            .map_err(|e| Self::unsupported("endpoint", e.to_string()))?;
        let parsed =
            url::Url::parse(&uri).map_err(|e| Self::unsupported("endpoint", e.to_string()))?;
        if parsed.fragment().is_some() {
            return Err(Self::unsupported(
                "endpoint",
                "URI fragments are not sent in HTTP requests".to_string(),
            ));
        }
        Ok(uri)
    }

    fn value_to_http_strings(value: Value) -> Result<Vec<String>> {
        match value {
            Value::String(s) => Ok(vec![s]),
            Value::Number(n) => Ok(vec![n.to_string()]),
            Value::Bool(b) => Ok(vec![b.to_string()]),
            Value::Null => Ok(Vec::new()),
            Value::Array(values) => {
                let mut out = Vec::new();
                for value in values {
                    out.extend(Self::value_to_http_strings(value)?);
                }
                Ok(out)
            }
            Value::Object(_) => Err(anyhow!("object values are not supported")),
        }
    }

    fn map_to_key_values(map: Map<String, Value>, field: &str) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        for (key, value) in map {
            for value in
                Self::value_to_http_strings(value).map_err(|e| anyhow!("{field}.{key}: {e}"))?
            {
                out.push(json!({ "key": key, "value": value }));
            }
        }
        Ok(out)
    }

    fn body_to_string(body: Option<Value>) -> Result<(Option<String>, bool)> {
        match body {
            Some(Value::String(s)) => Ok((Some(s), false)),
            Some(value) => Ok((Some(serde_json::to_string(&value)?), true)),
            None => Ok((None, false)),
        }
    }

    fn response_body(response: &Value) -> Value {
        response
            .get(RES_CONTENT)
            .or_else(|| response.get(RES_BODY))
            .or_else(|| {
                response
                    .get(RES_RESPONSE_DATA)
                    .and_then(|v| v.get(RES_CONTENT))
            })
            .cloned()
            .unwrap_or(Value::Null)
    }

    fn response_headers(response: &Value) -> Value {
        let Some(headers) = response.get(RES_HEADERS).and_then(Value::as_array) else {
            return json!({});
        };
        let mut out = Map::new();
        for header in headers {
            let key = header
                .get(RES_KEY)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let value = header
                .get(RES_VALUE)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if !key.is_empty() {
                match out.get_mut(&key) {
                    Some(Value::Array(values)) => values.push(Value::String(value)),
                    Some(existing) => {
                        let first = std::mem::replace(existing, Value::Null);
                        *existing = Value::Array(vec![first, Value::String(value)]);
                    }
                    None => {
                        out.insert(key, Value::String(value));
                    }
                }
            }
        }
        Value::Object(out)
    }

    fn response_header_value<'a>(response: &'a Value, header_name: &str) -> Option<&'a str> {
        response
            .get(RES_HEADERS)
            .and_then(Value::as_array)?
            .iter()
            .find(|header| {
                header
                    .get(RES_KEY)
                    .and_then(Value::as_str)
                    .is_some_and(|key| key.eq_ignore_ascii_case(header_name))
            })?
            .get(RES_VALUE)
            .and_then(Value::as_str)
    }

    fn content_output(response: &Value) -> Value {
        let body = Self::response_body(response);
        let Some(content_type) = Self::response_header_value(response, HEADER_CONTENT_TYPE) else {
            return body;
        };
        if !content_type
            .to_ascii_lowercase()
            .contains(CONTENT_TYPE_JSON)
        {
            return body;
        }
        match body {
            Value::String(s) => serde_json::from_str(&s).unwrap_or(Value::String(s)),
            other => other,
        }
    }

    fn adapt_output(response: Value, output: HttpOutput) -> Value {
        match output {
            HttpOutput::Content => Self::content_output(&response),
            HttpOutput::Response => json!({
                "statusCode": response
                    .get(RES_STATUS_CODE_CAMEL)
                    .or_else(|| response.get(RES_STATUS_CODE_SNAKE))
                    .and_then(Value::as_u64)
                    .unwrap_or_default(),
                "headers": Self::response_headers(&response),
                "body": Self::content_output(&response),
            }),
        }
    }

    fn merge_task_metadata(
        workflow_metadata: &HashMap<String, String>,
        task_metadata: &serde_json::Map<String, Value>,
    ) -> HashMap<String, String> {
        let mut metadata = workflow_metadata.clone();
        for (key, value) in task_metadata {
            if !metadata.contains_key(key)
                && let Some(value) = value.as_str()
            {
                metadata.insert(key.clone(), value.to_string());
            }
        }
        metadata
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_http_runner(
        &self,
        cx: Arc<opentelemetry::Context>,
        settings: Value,
        job_args: Value,
        worker_name: &str,
        timeout_sec: u32,
    ) -> Result<Value> {
        let runner = self
            .job_executor_wrapper
            .runner_app()
            .find_runner_by_name(HTTP_REQUEST_RUNNER_NAME)
            .await?
            .ok_or_else(|| anyhow!("Runner '{HTTP_REQUEST_RUNNER_NAME}' not found"))?;
        let settings = self
            .job_executor_wrapper
            .setup_runner_and_settings(&runner, Some(settings))
            .await?;
        let (rid, rdata) = runner
            .id
            .zip(runner.data)
            .ok_or_else(|| anyhow!("Runner '{HTTP_REQUEST_RUNNER_NAME}' missing id/data"))?;
        let job_args = self
            .job_executor_wrapper
            .transform_job_args(&rid, &rdata, &job_args, None)
            .await?;

        let mut metadata = Self::merge_task_metadata(&self.metadata, &self.task.metadata);
        Self::inject_metadata_from_context(&mut metadata, &cx);
        let worker_data = WorkerData {
            name: worker_name.to_string(),
            description: "Serverless Workflow call.http temporary worker".to_string(),
            runner_id: Some(rid),
            runner_settings: settings,
            queue_type: QueueType::Normal as i32,
            response_type: ResponseType::Direct as i32,
            store_failure: true,
            broadcast_results: true,
            ..Default::default()
        };
        let (job_id, result_fut) = self
            .job_executor_wrapper
            .enqueue_with_worker_or_temp_channel(
                Arc::new(metadata),
                None,
                worker_data,
                job_args,
                None,
                timeout_sec,
                StreamingType::None,
                None,
            )
            .await?;

        self.workflow_context
            .read()
            .await
            .register_running_job(&job_id)
            .await;
        let wait_result = result_fut.await;
        self.workflow_context
            .read()
            .await
            .unregister_running_job(&job_id)
            .await;

        let (res, _stream) = wait_result?;
        let Some(res) = res else {
            return Err(anyhow!("Failed to enqueue job or job result not found"));
        };
        let output = self.job_executor_wrapper.extract_job_result_output(res)?;
        self.job_executor_wrapper
            .transform_raw_output(&rid, &rdata, output.as_slice(), None)
            .await
    }

    // `uri` is the endpoint already resolved by `ensure_supported`, so we avoid
    // serializing the endpoint a second time here.
    fn http_request_parts(
        uri: String,
        call: workflow::CallHttp,
    ) -> Result<(Value, Value, HttpOutput)> {
        let settings = json!({ "base_url": uri });
        let mut headers = Self::map_to_key_values(call.headers, "headers")?;
        let (body, json_body) = Self::body_to_string(call.body)?;
        if json_body
            && !headers.iter().any(|header| {
                header
                    .get(RES_KEY)
                    .and_then(Value::as_str)
                    .is_some_and(|key| key.eq_ignore_ascii_case(HEADER_CONTENT_TYPE))
            })
        {
            headers.push(json!({ "key": "Content-Type", "value": CONTENT_TYPE_JSON }));
        }
        let mut args = json!({
            "method": call.method.to_ascii_uppercase(),
            "path": "",
            "headers": headers,
            "queries": Self::map_to_key_values(call.query, "query")?,
        });
        if let Some(body) = body {
            args["body"] = Value::String(body);
        }
        Ok((settings, args, call.output))
    }
}

impl TaskExecutorTrait<'_> for CallTaskExecutor {
    async fn execute(
        &self,
        cx: Arc<opentelemetry::Context>,
        task_name: &str,
        mut task_context: TaskContext,
    ) -> Result<TaskContext, Box<workflow::Error>> {
        task_context.add_position_name("call".to_string()).await;

        let expression = Self::expression(
            &*self.workflow_context.read().await,
            Arc::new(task_context.clone()),
        )
        .await?;

        let call_value = serde_json::to_value(&self.task.with)
            .map_err(|e| Self::unsupported("with", format!("serialization failed: {e}")))?;
        let transformed_call =
            match Self::transform_value(task_context.input.clone(), call_value, &expression) {
                Ok(value) => value,
                Err(mut e) => {
                    task_context.add_position_name("with".to_string()).await;
                    let pos = task_context.position.read().await;
                    e.position(&pos);
                    return Err(e);
                }
            };
        let call: workflow::CallHttp = serde_json::from_value(transformed_call)
            .map_err(|e| Self::unsupported("with", format!("invalid transformed call: {e}")))?;
        let uri = Self::ensure_supported(&call)?;
        let (settings, args, output) = Self::http_request_parts(uri, call)
            .map_err(|e| Self::unsupported("with", e.to_string()))?;
        let timeout_sec =
            resolve_run_task_timeout_sec(self.task.timeout.as_ref(), self.default_task_timeout);

        let raw_response = self
            .execute_http_runner(cx, settings, args, task_name, timeout_sec)
            .await
            .map_err(|e| {
                workflow::errors::ErrorFactory::new().service_unavailable(
                    "Failed to execute call.http".to_string(),
                    None,
                    Some(e.to_string()),
                )
            })?;
        task_context.set_raw_output(Self::adapt_output(raw_response, output));
        task_context.remove_position().await;
        Ok(task_context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn map_to_key_values_stringifies_scalar_values() {
        let map = serde_json::Map::from_iter([
            ("a".to_string(), json!("x")),
            ("b".to_string(), json!(1)),
            ("c".to_string(), json!(true)),
        ]);

        let values = CallTaskExecutor::map_to_key_values(map, "query").unwrap();

        assert_eq!(
            values,
            vec![
                json!({"key": "a", "value": "x"}),
                json!({"key": "b", "value": "1"}),
                json!({"key": "c", "value": "true"}),
            ]
        );
    }

    #[test]
    fn map_to_key_values_rejects_nested_values() {
        let map = serde_json::Map::from_iter([("a".to_string(), json!({"x": 1}))]);

        let err = CallTaskExecutor::map_to_key_values(map, "headers").unwrap_err();

        assert!(err.to_string().contains("headers.a"));
    }

    #[test]
    fn map_to_key_values_expands_arrays_and_skips_nulls() {
        let map = serde_json::Map::from_iter([
            ("tag".to_string(), json!(["a", "b"])),
            ("cursor".to_string(), Value::Null),
        ]);

        let values = CallTaskExecutor::map_to_key_values(map, "query").unwrap();

        assert_eq!(
            values,
            vec![
                json!({"key": "tag", "value": "a"}),
                json!({"key": "tag", "value": "b"}),
            ]
        );
    }

    #[test]
    fn http_request_parts_normalizes_method_and_adds_json_content_type() {
        let call: workflow::CallHttp = serde_json::from_value(json!({
            "method": "post",
            "endpoint": "https://example.com/items",
            "body": {"name": "test"}
        }))
        .unwrap();

        let uri = CallTaskExecutor::ensure_supported(&call).unwrap();
        let (_settings, args, _output) = CallTaskExecutor::http_request_parts(uri, call).unwrap();

        assert_eq!(args["method"], json!("POST"));
        assert_eq!(
            args["headers"],
            json!([{"key": "Content-Type", "value": "application/json"}])
        );
        assert_eq!(args["body"], json!("{\"name\":\"test\"}"));
    }

    #[test]
    fn http_request_parts_keeps_explicit_content_type_for_json_body() {
        let call: workflow::CallHttp = serde_json::from_value(json!({
            "method": "POST",
            "endpoint": "https://example.com/items",
            "headers": {"content-type": "application/merge-patch+json"},
            "body": {"name": "test"}
        }))
        .unwrap();

        let uri = CallTaskExecutor::ensure_supported(&call).unwrap();
        let (_settings, args, _output) = CallTaskExecutor::http_request_parts(uri, call).unwrap();

        assert_eq!(
            args["headers"],
            json!([{"key": "content-type", "value": "application/merge-patch+json"}])
        );
    }

    #[test]
    fn http_request_parts_uses_full_endpoint_as_base_url_and_empty_path() {
        let call: workflow::CallHttp = serde_json::from_value(json!({
            "method": "GET",
            "endpoint": "https://example.com/items?existing=1",
            "query": {"page": 2}
        }))
        .unwrap();

        let uri = CallTaskExecutor::ensure_supported(&call).unwrap();
        let (settings, args, output) = CallTaskExecutor::http_request_parts(uri, call).unwrap();

        assert_eq!(
            settings,
            json!({"base_url": "https://example.com/items?existing=1"})
        );
        assert_eq!(args["path"], json!(""));
        assert_eq!(args["queries"], json!([{"key": "page", "value": "2"}]));
        assert_eq!(output, HttpOutput::Content);
    }

    #[test]
    fn response_headers_preserve_duplicate_values_as_array() {
        let response = json!({
            "headers": [
                {"key": "set-cookie", "value": "a=1"},
                {"key": "set-cookie", "value": "b=2"},
                {"key": "vary", "value": "accept"}
            ]
        });

        let headers = CallTaskExecutor::response_headers(&response);

        assert_eq!(
            headers,
            json!({
                "set-cookie": ["a=1", "b=2"],
                "vary": "accept"
            })
        );
    }

    #[test]
    fn merge_task_metadata_preserves_workflow_metadata_precedence() {
        let mut base = HashMap::new();
        base.insert("trace".to_string(), "workflow".to_string());
        let task_metadata = serde_json::Map::from_iter([
            ("trace".to_string(), json!("task")),
            ("task".to_string(), json!("call")),
            ("ignored".to_string(), json!(1)),
        ]);

        let merged = CallTaskExecutor::merge_task_metadata(&base, &task_metadata);

        assert_eq!(merged.get("trace"), Some(&"workflow".to_string()));
        assert_eq!(merged.get("task"), Some(&"call".to_string()));
        assert!(!merged.contains_key("ignored"));
    }

    #[test]
    fn endpoint_authentication_is_rejected_by_adapter() {
        let call: workflow::CallHttp = serde_json::from_value(json!({
            "method": "GET",
            "endpoint": {
                "uri": "https://example.com/items",
                "authentication": {"bearer": "token"}
            }
        }))
        .unwrap();

        let err = CallTaskExecutor::ensure_supported(&call).unwrap_err();

        assert!(err.to_string().contains("authentication"));
    }

    #[test]
    fn adapt_content_output_returns_body_only() {
        let response = json!({
            "statusCode": 201,
            "headers": [{"key": "content-type", "value": "application/json"}],
            "content": "{\"ok\":true}"
        });

        let output = CallTaskExecutor::adapt_output(response, HttpOutput::Content);

        assert_eq!(output, json!({"ok": true}));
    }

    #[test]
    fn adapt_response_output_uses_official_shape() {
        let response = json!({
            "statusCode": 201,
            "headers": [{"key": "content-type", "value": "application/json"}],
            "content": "{\"ok\":true}"
        });

        let output = CallTaskExecutor::adapt_output(response, HttpOutput::Response);

        assert_eq!(
            output,
            json!({
                "statusCode": 201,
                "headers": {"content-type": "application/json"},
                "body": {"ok": true}
            })
        );
    }
}
