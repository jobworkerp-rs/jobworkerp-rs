//! HTTP-specific logic for the `call: http` task.
//!
//! Converts a `call: http` task into an HTTP_REQUEST runner job: builds the
//! runner settings/args from the endpoint, headers, query, and body; resolves
//! endpoint authentication into an Authorization header; and adapts the runner
//! response back into the task output (`content` / `response`).

use super::CallTaskExecutor;
use crate::workflow::{
    definition::workflow::{self, HttpOutput},
    execute::{context::TaskContext, secret},
};
use anyhow::{Result, anyhow};
use serde_json::{Map, Value, json};
use std::sync::Arc;

// Canonical name of the built-in HTTP_REQUEST runner. Must equal
// `RunnerType::HttpRequest.as_str_name()`; a unit test guards against drift.
pub(super) const HTTP_REQUEST_RUNNER_NAME: &str = "HTTP_REQUEST";

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
// Key/value field names of the {key, value} pairs the HTTP_REQUEST runner accepts
// for request headers and queries. Kept separate from the RES_* response contract.
const ARG_KEY: &str = "key";
const ARG_VALUE: &str = "value";
const HEADER_CONTENT_TYPE: &str = "content-type";
const HEADER_CONTENT_TYPE_TITLE: &str = "Content-Type";
const CONTENT_TYPE_JSON: &str = "application/json";

impl CallTaskExecutor {
    fn endpoint_uri(endpoint: &workflow::HttpEndpoint) -> Result<String> {
        let (workflow::HttpEndpoint::Uri(uri) | workflow::HttpEndpoint::Object { uri, .. }) =
            endpoint;
        Ok(uri.clone())
    }

    fn endpoint_authentication(
        endpoint: &workflow::HttpEndpoint,
    ) -> Option<&workflow::HttpEndpointAuthentication> {
        match endpoint {
            workflow::HttpEndpoint::Object {
                authentication: Some(auth),
                ..
            } => Some(auth),
            _ => None,
        }
    }

    /// Resolve an endpoint authentication into a concrete policy by delegating to
    /// the shared [`CallTaskExecutor::resolve_named_policy`] after destructuring
    /// the HTTP-specific enum into `(inline, use_ref)`.
    fn resolve_authentication(
        endpoint_auth: &workflow::HttpEndpointAuthentication,
        named: &std::collections::HashMap<String, workflow::AuthenticationPolicy>,
        declared_secrets: &std::collections::HashSet<String>,
        raw_input: Arc<Value>,
        expression: &std::collections::BTreeMap<String, Arc<Value>>,
    ) -> Result<workflow::AuthenticationPolicy, Box<workflow::Error>> {
        let (inline, use_ref) = match endpoint_auth {
            workflow::HttpEndpointAuthentication::Variant0(policy) => (Some(policy), None),
            workflow::HttpEndpointAuthentication::Variant1 { use_ } => (None, Some(use_.as_str())),
        };
        Self::resolve_named_policy(
            inline,
            use_ref,
            named,
            declared_secrets,
            raw_input,
            expression,
        )
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

    // Build a {key, value} pair in the shape the HTTP_REQUEST runner expects for
    // a single header or query argument.
    fn key_value(key: &str, value: &str) -> Value {
        json!({ ARG_KEY: key, ARG_VALUE: value })
    }

    fn map_to_key_values(map: Map<String, Value>, field: &str) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        for (key, value) in map {
            for value in
                Self::value_to_http_strings(value).map_err(|e| anyhow!("{field}.{key}: {e}"))?
            {
                out.push(Self::key_value(&key, &value));
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

    // `uri` is the endpoint already resolved by `ensure_supported`, so we avoid
    // serializing the endpoint a second time here.
    fn http_request_parts(
        uri: String,
        call: workflow::CallHttp,
        auth_header: Option<(String, String)>,
    ) -> Result<(Value, Value, HttpOutput)> {
        let settings = json!({ "base_url": uri });
        let mut headers = Self::map_to_key_values(call.headers, "headers")?;
        // An authentication policy wins over a hand-written Authorization header:
        // drop any existing one, then append the policy-derived value.
        if let Some((name, value)) = auth_header {
            headers.retain(|header| {
                !header
                    .get(ARG_KEY)
                    .and_then(Value::as_str)
                    .is_some_and(|key| key.eq_ignore_ascii_case(&name))
            });
            headers.push(Self::key_value(&name, &value));
        }
        let (body, json_body) = Self::body_to_string(call.body)?;
        if json_body
            && !headers.iter().any(|header| {
                header
                    .get(ARG_KEY)
                    .and_then(Value::as_str)
                    .is_some_and(|key| key.eq_ignore_ascii_case(HEADER_CONTENT_TYPE))
            })
        {
            headers.push(Self::key_value(
                HEADER_CONTENT_TYPE_TITLE,
                CONTENT_TYPE_JSON,
            ));
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

    // Execute an HTTP call through the HTTP_REQUEST runner and return its
    // adapted output. Resolves endpoint authentication into an Authorization
    // header (inline policy already expanded by the caller's `with` transform,
    // or a `use:` reference expanded here with the same context).
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn execute_http(
        &self,
        cx: Arc<opentelemetry::Context>,
        task_name: &str,
        task_context: &mut TaskContext,
        call: workflow::CallHttp,
        named_authentications: &std::collections::HashMap<String, workflow::AuthenticationPolicy>,
        declared_secrets: &std::collections::HashSet<String>,
        expression: &std::collections::BTreeMap<String, Arc<Value>>,
        timeout_sec: u32,
    ) -> Result<Value, Box<workflow::Error>> {
        let uri = Self::ensure_supported(&call)?;
        let auth_header = match Self::endpoint_authentication(&call.endpoint) {
            Some(endpoint_auth) => {
                let policy = Self::resolve_authentication(
                    endpoint_auth,
                    named_authentications,
                    declared_secrets,
                    task_context.input.clone(),
                    expression,
                )?;
                let header = secret::authentication_header(&policy, declared_secrets, &|k| {
                    std::env::var(k).ok()
                })
                .map_err(|e| Self::unsupported("authentication", e.to_string()))?;
                Some(header)
            }
            None => None,
        };
        let (settings, args, output) = Self::http_request_parts(uri, call, auth_header)
            .map_err(|e| Self::unsupported("with", e.to_string()))?;

        let raw_response = self
            .execute_runner(
                cx,
                HTTP_REQUEST_RUNNER_NAME,
                None,
                settings,
                args,
                task_name,
                timeout_sec,
            )
            .await
            .map_err(|e| {
                workflow::errors::ErrorFactory::new().service_unavailable(
                    "Failed to execute call.http".to_string(),
                    None,
                    Some(e.to_string()),
                )
            })?;
        Ok(Self::adapt_output(raw_response, output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::jobworkerp::data::RunnerType;

    fn empty_expression() -> std::collections::BTreeMap<String, Arc<Value>> {
        std::collections::BTreeMap::new()
    }

    fn no_secrets() -> std::collections::HashSet<String> {
        std::collections::HashSet::new()
    }

    // Inline policies don't read secrets; this getter must never be consulted.
    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn bearer_inline(token: &str) -> workflow::AuthenticationPolicy {
        workflow::AuthenticationPolicy::Bearer(
            workflow::BearerAuthentication::BearerAuthenticationProperties {
                token: token.to_string(),
            },
        )
    }

    fn declared(names: &[&str]) -> std::collections::HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn http_request_runner_name_matches_proto_enum() {
        assert_eq!(
            HTTP_REQUEST_RUNNER_NAME,
            RunnerType::HttpRequest.as_str_name()
        );
    }

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
        let (_settings, args, _output) =
            CallTaskExecutor::http_request_parts(uri, call, None).unwrap();

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
        let (_settings, args, _output) =
            CallTaskExecutor::http_request_parts(uri, call, None).unwrap();

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
        let (settings, args, output) =
            CallTaskExecutor::http_request_parts(uri, call, None).unwrap();

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
        let mut base = std::collections::HashMap::new();
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
    fn bearer_authentication_becomes_authorization_header() {
        let policy = bearer_inline("abc123");
        let (name, value) = secret::authentication_header(&policy, &no_secrets(), &no_env).unwrap();
        assert_eq!(name, "Authorization");
        assert_eq!(value, "Bearer abc123");
    }

    #[test]
    fn basic_authentication_base64_encodes_credentials() {
        let policy = workflow::AuthenticationPolicy::Basic(
            workflow::BasicAuthentication::BasicAuthenticationProperties {
                username: "Aladdin".to_string(),
                password: "open sesame".to_string(),
            },
        );
        let (name, value) = secret::authentication_header(&policy, &no_secrets(), &no_env).unwrap();
        assert_eq!(name, "Authorization");
        // RFC 7617 §2 reference vector.
        assert_eq!(value, "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==");
    }

    #[test]
    fn bearer_use_reference_resolves_secret_from_env() {
        // `{ bearer: { use: API_TOKEN } }` resolves from the declared-secret env.
        let policy = workflow::AuthenticationPolicy::Bearer(
            workflow::BearerAuthentication::SecretBasedAuthenticationPolicy(
                workflow::SecretBasedAuthenticationPolicy {
                    use_: "API_TOKEN".parse().unwrap(),
                },
            ),
        );
        let declared: std::collections::HashSet<String> =
            ["API_TOKEN".to_string()].into_iter().collect();
        let getter = |k: &str| (k == "WORKFLOW_SECRET_API_TOKEN").then(|| "env-token".to_string());
        let (_, value) = secret::authentication_header(&policy, &declared, &getter).unwrap();
        assert_eq!(value, "Bearer env-token");
    }

    #[test]
    fn basic_use_reference_resolves_secret_from_env() {
        let policy = workflow::AuthenticationPolicy::Basic(
            workflow::BasicAuthentication::SecretBasedAuthenticationPolicy(
                workflow::SecretBasedAuthenticationPolicy {
                    use_: "CREDS".parse().unwrap(),
                },
            ),
        );
        let declared: std::collections::HashSet<String> =
            ["CREDS".to_string()].into_iter().collect();
        let getter = |k: &str| {
            (k == "WORKFLOW_SECRET_CREDS")
                .then(|| r#"{"username": "Aladdin", "password": "open sesame"}"#.to_string())
        };
        let (_, value) = secret::authentication_header(&policy, &declared, &getter).unwrap();
        assert_eq!(value, "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==");
    }

    #[test]
    fn use_reference_to_undeclared_secret_is_rejected_without_leaking() {
        let policy = workflow::AuthenticationPolicy::Bearer(
            workflow::BearerAuthentication::SecretBasedAuthenticationPolicy(
                workflow::SecretBasedAuthenticationPolicy {
                    use_: "API_TOKEN".parse().unwrap(),
                },
            ),
        );
        // Not declared in use.secrets -> error, env never read.
        let getter = |_: &str| Some("leaked".to_string());
        let err = secret::authentication_header(&policy, &no_secrets(), &getter).unwrap_err();
        assert!(err.to_string().contains("API_TOKEN"));
        assert!(!err.to_string().contains("leaked"));
    }

    #[test]
    fn inline_endpoint_authentication_is_accepted() {
        let call: workflow::CallHttp = serde_json::from_value(json!({
            "method": "GET",
            "endpoint": {
                "uri": "https://example.com/items",
                "authentication": {"bearer": {"token": "tok"}}
            }
        }))
        .unwrap();

        // No longer rejected.
        let uri = CallTaskExecutor::ensure_supported(&call).unwrap();
        assert_eq!(uri, "https://example.com/items");

        let auth = CallTaskExecutor::endpoint_authentication(&call.endpoint).unwrap();
        let named = std::collections::HashMap::new();
        let policy = CallTaskExecutor::resolve_authentication(
            auth,
            &named,
            &no_secrets(),
            Arc::new(json!({})),
            &empty_expression(),
        )
        .unwrap();
        let (_, value) = secret::authentication_header(&policy, &no_secrets(), &no_env).unwrap();
        assert_eq!(value, "Bearer tok");
    }

    #[test]
    fn use_reference_resolves_named_authentication() {
        let call: workflow::CallHttp = serde_json::from_value(json!({
            "method": "GET",
            "endpoint": {
                "uri": "https://example.com/items",
                "authentication": {"use": "apiAuth"}
            }
        }))
        .unwrap();

        let mut named = std::collections::HashMap::new();
        named.insert("apiAuth".to_string(), bearer_inline("named-tok"));

        let auth = CallTaskExecutor::endpoint_authentication(&call.endpoint).unwrap();
        let policy = CallTaskExecutor::resolve_authentication(
            auth,
            &named,
            &no_secrets(),
            Arc::new(json!({})),
            &empty_expression(),
        )
        .unwrap();
        let (_, value) = secret::authentication_header(&policy, &no_secrets(), &no_env).unwrap();
        assert_eq!(value, "Bearer named-tok");
    }

    #[test]
    fn use_reference_expands_expressions_in_named_policy() {
        // A named policy referencing `$secrets` must be expanded with the call's
        // expression context, not sent as the literal `${ $secrets.X }` string.
        let call: workflow::CallHttp = serde_json::from_value(json!({
            "method": "GET",
            "endpoint": {
                "uri": "https://example.com/items",
                "authentication": {"use": "apiAuth"}
            }
        }))
        .unwrap();

        let mut named = std::collections::HashMap::new();
        named.insert(
            "apiAuth".to_string(),
            bearer_inline("${ $secrets.API_TOKEN }"),
        );

        // The policy references `$secrets.API_TOKEN`, so the name must be declared
        // in `use.secrets`; `resolve_authentication` resolves it from the env
        // (test-threads=1 makes the env mutation safe here).
        let env_name = "WORKFLOW_SECRET_API_TOKEN";
        // SAFETY: tests run single-threaded (`--test-threads=1`).
        unsafe { std::env::set_var(env_name, "resolved-secret") };

        let auth = CallTaskExecutor::endpoint_authentication(&call.endpoint).unwrap();
        let policy = CallTaskExecutor::resolve_authentication(
            auth,
            &named,
            &declared(&["API_TOKEN"]),
            Arc::new(json!({})),
            &empty_expression(),
        )
        .unwrap();

        // SAFETY: tests run single-threaded (`--test-threads=1`).
        unsafe { std::env::remove_var(env_name) };

        let (_, value) = secret::authentication_header(&policy, &no_secrets(), &no_env).unwrap();
        assert_eq!(value, "Bearer resolved-secret");
    }

    #[test]
    fn undefined_use_reference_is_rejected() {
        let call: workflow::CallHttp = serde_json::from_value(json!({
            "method": "GET",
            "endpoint": {
                "uri": "https://example.com/items",
                "authentication": {"use": "missing"}
            }
        }))
        .unwrap();

        let auth = CallTaskExecutor::endpoint_authentication(&call.endpoint).unwrap();
        let named = std::collections::HashMap::new();
        let err = CallTaskExecutor::resolve_authentication(
            auth,
            &named,
            &no_secrets(),
            Arc::new(json!({})),
            &empty_expression(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("missing"));
    }

    #[test]
    fn authentication_overrides_handwritten_authorization_header() {
        let call: workflow::CallHttp = serde_json::from_value(json!({
            "method": "GET",
            "endpoint": "https://example.com/items",
            "headers": {"Authorization": "Bearer handwritten"}
        }))
        .unwrap();

        let uri = CallTaskExecutor::ensure_supported(&call).unwrap();
        let auth = Some(("Authorization".to_string(), "Bearer policy".to_string()));
        let (_settings, args, _output) =
            CallTaskExecutor::http_request_parts(uri, call, auth).unwrap();

        let headers = args["headers"].as_array().unwrap();
        let auth_values: Vec<&str> = headers
            .iter()
            .filter(|h| h["key"].as_str() == Some("Authorization"))
            .map(|h| h["value"].as_str().unwrap())
            .collect();
        // Exactly one Authorization header, the policy value (not the handwritten one).
        assert_eq!(auth_values, vec!["Bearer policy"]);
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
