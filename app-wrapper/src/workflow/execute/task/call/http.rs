//! HTTP-specific logic for the `call: http` task.
//!
//! Converts a `call: http` task into an HTTP_REQUEST runner job: builds the
//! runner settings/args from the endpoint, headers, query, and body; resolves
//! endpoint authentication into an Authorization header; and adapts the runner
//! response back into the task output (`content` / `response`).
//!
//! The HTTP_REQUEST runner itself succeeds for any received response. The spec
//! requires a call to fail on a status outside the accepted range, so that
//! check lives here, in the workflow adapter, not in the runner.

use super::CallTaskExecutor;
use crate::workflow::execute::expression::ExpressionContext;
use crate::workflow::{
    definition::workflow::{self, HttpOutput},
    execute::{
        context::TaskContext,
        secret::{self, MASKED_VALUE},
    },
};
use anyhow::{Result, anyhow};
use app::app::job::execute::SENSITIVE_HEADER_KEYS;
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
// HTTP_REQUEST runner settings/args fields, shared by the request builder and
// the `request` echoed by `output: response`.
const SETTINGS_BASE_URL: &str = "base_url";
const ARG_METHOD: &str = "method";
const ARG_PATH: &str = "path";
const ARG_HEADERS: &str = "headers";
const ARG_QUERIES: &str = "queries";
const ARG_BODY: &str = "body";
const HEADER_CONTENT_TYPE: &str = "content-type";
const HEADER_CONTENT_TYPE_TITLE: &str = "Content-Type";
const HEADER_AUTHORIZATION_TITLE: &str = "Authorization";
const CONTENT_TYPE_JSON: &str = "application/json";
// Separator for combining repeated header fields (RFC 9110 section 5.3); the
// spec types headers as map[string, string].
const HEADER_VALUE_SEPARATOR: &str = ", ";

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
        expression: &ExpressionContext,
    ) -> Result<(workflow::AuthenticationPolicy, Vec<String>), Box<workflow::Error>> {
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

    /// Resolve the endpoint authentication into the Authorization value to send,
    /// together with the values to mask for this call beyond the `with`
    /// secrets: URI userinfo, the secrets a named policy resolved, and the
    /// credentials the Authorization value carries.
    fn resolve_http_authorization(
        uri: &str,
        endpoint: &workflow::HttpEndpoint,
        named: &std::collections::HashMap<String, workflow::AuthenticationPolicy>,
        declared_secrets: &std::collections::HashSet<String>,
        raw_input: Arc<Value>,
        expression: &ExpressionContext,
    ) -> Result<(Option<String>, Vec<String>), Box<workflow::Error>> {
        let mut mask_values = Self::userinfo_credentials(uri);
        let Some(endpoint_auth) = Self::endpoint_authentication(endpoint) else {
            return Ok((None, mask_values));
        };
        let (policy, policy_secret_values) = Self::resolve_authentication(
            endpoint_auth,
            named,
            declared_secrets,
            raw_input,
            expression,
        )?;
        let authorization =
            secret::resolve_authorization(&policy, declared_secrets, &|k| std::env::var(k).ok())
                .map_err(|e| Self::secret_failure("authentication", e))?;
        mask_values.extend(policy_secret_values);
        mask_values.extend(authorization.credentials);
        Ok((Some(authorization.value), mask_values))
    }

    // URI userinfo is a credential even without `$secrets`. Each component is
    // returned decoded when it is valid UTF-8 (masking also matches encoded
    // forms) and as written otherwise, together with the Basic value the HTTP
    // client derives from it and sends as the Authorization header.
    fn userinfo_credentials(uri: &str) -> Vec<String> {
        let Ok(url) = url::Url::parse(uri) else {
            return Vec::new();
        };
        let decode = |text: &str| {
            percent_encoding::percent_decode_str(text)
                .decode_utf8()
                .ok()
                .map(|decoded| decoded.into_owned())
        };
        let mut credentials: Vec<String> = [Some(url.username()), url.password()]
            .into_iter()
            .flatten()
            .filter(|text| !text.is_empty())
            .map(|text| decode(text).unwrap_or_else(|| text.to_string()))
            .collect();
        // Mirrors reqwest's `extract_authority`: a username that is not UTF-8
        // disables the Basic header, and such a password is sent as absent.
        if let Some(username) = decode(url.username()) {
            let password = url.password().and_then(decode);
            if !username.is_empty() || password.is_some() {
                credentials.push(secret::basic_credential(
                    &username,
                    password.as_deref().unwrap_or_default(),
                ));
            }
        }
        credentials
    }

    // Validate the endpoint and return the resolved URI so the caller can reuse
    // it without re-serializing the endpoint.
    fn ensure_supported(call: &workflow::CallHttp) -> Result<String, Box<workflow::Error>> {
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

    fn response_body_ref(response: &Value) -> Option<&Value> {
        response
            .get(RES_CONTENT)
            .or_else(|| response.get(RES_BODY))
            .or_else(|| {
                response
                    .get(RES_RESPONSE_DATA)
                    .and_then(|v| v.get(RES_CONTENT))
            })
    }

    fn response_body(response: &Value) -> Value {
        Self::response_body_ref(response)
            .cloned()
            .unwrap_or(Value::Null)
    }

    fn header_map(pairs: impl Iterator<Item = (String, String)>) -> Value {
        let mut out = Map::new();
        for (key, value) in pairs {
            if key.is_empty() {
                continue;
            }
            match out.get_mut(&key) {
                Some(Value::String(existing)) => {
                    existing.push_str(HEADER_VALUE_SEPARATOR);
                    existing.push_str(&value);
                }
                _ => {
                    out.insert(key, Value::String(value));
                }
            }
        }
        Value::Object(out)
    }

    // Read a `{key, value}` pair array. Field names are passed in because the
    // request-args (ARG_*) and response (RES_*) contracts are kept separate.
    fn key_value_pairs<'a>(
        pairs: Option<&'a Value>,
        key_field: &'static str,
        value_field: &'static str,
    ) -> impl Iterator<Item = (String, String)> + 'a {
        pairs
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(move |pair| {
                let field = |name| {
                    pair.get(name)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                };
                (field(key_field), field(value_field))
            })
    }

    fn response_headers(response: &Value) -> Value {
        Self::header_map(Self::key_value_pairs(
            response.get(RES_HEADERS),
            RES_KEY,
            RES_VALUE,
        ))
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

    fn response_status(response: &Value) -> u64 {
        response
            .get(RES_STATUS_CODE_CAMEL)
            .or_else(|| response.get(RES_STATUS_CODE_SNAKE))
            .and_then(Value::as_u64)
            .unwrap_or_default()
    }

    // `redirect: false` accepts 2xx only; `redirect: true` also accepts 3xx.
    fn is_accepted_status(status: u64, redirect: bool) -> bool {
        let upper = if redirect { 400 } else { 300 };
        (200..upper).contains(&status)
    }

    /// The communication error raised for a response outside the accepted
    /// range. It keeps the origin's status (as the spec recommends) and the
    /// response body as `detail`, so a `catch` can filter on the status and
    /// inspect what the service returned.
    fn status_error(
        response: &Value,
        status: u64,
        position: Option<String>,
        secret_values: &[String],
    ) -> Box<workflow::Error> {
        let reason = u16::try_from(status)
            .ok()
            .and_then(|code| reqwest::StatusCode::from_u16(code).ok())
            .and_then(|code| code.canonical_reason());
        let title = match reason {
            Some(reason) => format!("HTTP {status} {reason}"),
            None => format!("HTTP {status}"),
        };
        // Borrowed so that, with no secrets to mask, an oversized body is not
        // copied before being cut.
        let detail = match Self::response_body_ref(response) {
            None | Some(Value::Null) => None,
            Some(Value::String(body)) if body.is_empty() => None,
            Some(Value::String(body)) => Some(Self::error_detail(body, secret_values)),
            Some(other) => Some(Self::error_detail(&other.to_string(), secret_values)),
        };
        workflow::errors::ErrorFactory::new().standard_with_status(
            workflow::errors::StandardErrorType::Communication,
            i64::try_from(status).unwrap_or(i64::MAX),
            title,
            position,
            detail,
        )
    }

    /// The URI sent for `base_url` plus `queries`, with credentials masked.
    ///
    /// Secrets are matched against the decoded path (as a whole, since a
    /// secret may contain `/`), each decoded query key/value, and the decoded
    /// fragment; only a component holding a secret is re-serialized. Userinfo
    /// is masked unconditionally because it is a credential even without
    /// `$secrets`.
    fn masked_request_uri(
        base_url: &str,
        queries: Vec<(String, String)>,
        secret_values: &[String],
    ) -> String {
        let Ok(mut url) = url::Url::parse(base_url) else {
            return secret::mask_encoded_secret_values(base_url, secret_values)
                .unwrap_or_else(|| base_url.to_string());
        };
        if !url.username().is_empty() {
            let _ = url.set_username(MASKED_VALUE);
        }
        if url.password().is_some() {
            let _ = url.set_password(Some(MASKED_VALUE));
        }
        // Append queries the way the runner's reqwest client does, before
        // masking, so they are checked in the encoded form actually sent.
        if !queries.is_empty() {
            url.query_pairs_mut().extend_pairs(queries);
        }
        if let Some(path) = secret::mask_encoded_secret_values(url.path(), secret_values) {
            url.set_path(&path);
        }
        Self::mask_query_secrets(&mut url, secret_values);
        if let Some(fragment) = url
            .fragment()
            .and_then(|fragment| secret::mask_encoded_secret_values(fragment, secret_values))
        {
            url.set_fragment(Some(&fragment));
        }
        // Catches raw occurrences in any other component (e.g. the host).
        secret::mask_secret_values(url.as_str(), secret_values)
    }

    fn mask_query_secrets(url: &mut url::Url, secret_values: &[String]) {
        let mut changed = false;
        let mut mask = |text: String| match secret::mask_encoded_secret_values(&text, secret_values)
        {
            Some(masked) => {
                changed = true;
                masked
            }
            None => text,
        };
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .into_owned()
            .map(|(key, value)| (mask(key), mask(value)))
            .collect();
        if changed {
            url.query_pairs_mut().clear().extend_pairs(pairs);
        }
    }

    /// The `request` part of `output: response`, rebuilt from the runner
    /// settings/args actually sent. Credentials are masked — credential headers
    /// and URI userinfo entirely, and resolved secret values
    /// wherever they appear — because this object becomes task output that may
    /// be stored and returned to clients.
    fn request_descriptor(settings: &Value, args: &Value, secret_values: &[String]) -> Value {
        let base_url = settings
            .get(SETTINGS_BASE_URL)
            .and_then(Value::as_str)
            .unwrap_or_default();
        let queries: Vec<(String, String)> =
            Self::key_value_pairs(args.get(ARG_QUERIES), ARG_KEY, ARG_VALUE).collect();
        let uri = Self::masked_request_uri(base_url, queries, secret_values);
        let headers = Self::header_map(
            Self::key_value_pairs(args.get(ARG_HEADERS), ARG_KEY, ARG_VALUE).map(|(key, value)| {
                let value = if SENSITIVE_HEADER_KEYS
                    .iter()
                    .any(|name| key.eq_ignore_ascii_case(name))
                {
                    MASKED_VALUE.to_string()
                } else {
                    secret::mask_encoded_secret_values(&value, secret_values).unwrap_or(value)
                };
                (key, value)
            }),
        );
        json!({
            "method": args[ARG_METHOD].clone(),
            "uri": uri,
            "headers": headers,
        })
    }

    fn adapt_output(response: Value, output: HttpOutput, request: Value) -> Value {
        match output {
            HttpOutput::Content => Self::content_output(&response),
            HttpOutput::Response => json!({
                "request": request,
                "statusCode": Self::response_status(&response),
                "headers": Self::response_headers(&response),
                "content": Self::content_output(&response),
            }),
        }
    }

    // `uri` is the endpoint already resolved by `ensure_supported`, so we avoid
    // serializing the endpoint a second time here.
    fn http_request_parts(
        uri: String,
        call: workflow::CallHttp,
        authorization: Option<String>,
    ) -> Result<(Value, Value, HttpOutput)> {
        let settings = json!({ SETTINGS_BASE_URL: uri });
        let mut headers = Self::map_to_key_values(call.headers, "headers")?;
        // An authentication policy wins over a hand-written Authorization header:
        // drop any existing one, then append the policy-derived value.
        if let Some(value) = authorization {
            headers.retain(|header| {
                !header
                    .get(ARG_KEY)
                    .and_then(Value::as_str)
                    .is_some_and(|key| key.eq_ignore_ascii_case(HEADER_AUTHORIZATION_TITLE))
            });
            headers.push(Self::key_value(HEADER_AUTHORIZATION_TITLE, &value));
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
            ARG_METHOD: call.method.to_ascii_uppercase(),
            ARG_PATH: "",
            ARG_HEADERS: headers,
            ARG_QUERIES: Self::map_to_key_values(call.query, "query")?,
        });
        if let Some(body) = body {
            args[ARG_BODY] = Value::String(body);
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
        secret_values: &[String],
        expression: &ExpressionContext,
        timeout_sec: u32,
    ) -> Result<Value, Box<workflow::Error>> {
        let uri = Self::ensure_supported(&call)?;
        let redirect = call.redirect.unwrap_or(false);
        let position = task_context.position.read().await.as_error_instance();
        let (authorization, mask_values) = Self::resolve_http_authorization(
            &uri,
            &call.endpoint,
            named_authentications,
            declared_secrets,
            task_context.input.clone(),
            expression,
        )?;
        let secret_values = secret::merge_secret_values(secret_values, mask_values);
        let (settings, args, output) = Self::http_request_parts(uri, call, authorization)
            .map_err(|e| Self::unsupported("with", e.to_string()))?;
        let request = match output {
            HttpOutput::Response => Self::request_descriptor(&settings, &args, &secret_values),
            HttpOutput::Content => Value::Null,
        };

        let raw_response = self
            .execute_runner(
                cx,
                HTTP_REQUEST_RUNNER_NAME,
                None,
                settings,
                args,
                task_name,
                timeout_sec,
                task_context.child_job_scope.as_ref(),
            )
            .await
            .map_err(|e| {
                Self::runner_failure(
                    e,
                    "Failed to execute call.http",
                    position.clone(),
                    &secret_values,
                )
            })?;
        let status = Self::response_status(&raw_response);
        if !Self::is_accepted_status(status, redirect) {
            return Err(Self::status_error(
                &raw_response,
                status,
                Some(position),
                &secret_values,
            ));
        }
        Ok(Self::adapt_output(raw_response, output, request))
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ERROR_DETAIL_MAX_BYTES, TRUNCATED_MARKER};
    use super::*;
    use proto::jobworkerp::data::RunnerType;

    fn secrets(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }

    fn empty_expression() -> ExpressionContext {
        ExpressionContext::new()
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
    fn response_headers_join_duplicate_values_into_one_string() {
        // The spec types response headers as map[string, string].
        let response = json!({
            "headers": [
                {"key": "cache-control", "value": "no-cache"},
                {"key": "cache-control", "value": "no-store"},
                {"key": "vary", "value": "accept"}
            ]
        });

        let headers = CallTaskExecutor::response_headers(&response);

        assert_eq!(
            headers,
            json!({
                "cache-control": "no-cache, no-store",
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
        let header = secret::resolve_authorization(&policy, &no_secrets(), &no_env).unwrap();
        assert_eq!(header.value, "Bearer abc123");
    }

    #[test]
    fn basic_authentication_base64_encodes_credentials() {
        let policy = workflow::AuthenticationPolicy::Basic(
            workflow::BasicAuthentication::BasicAuthenticationProperties {
                username: "Aladdin".to_string(),
                password: "open sesame".to_string(),
            },
        );
        let header = secret::resolve_authorization(&policy, &no_secrets(), &no_env).unwrap();
        // RFC 7617 §2 reference vector.
        assert_eq!(header.value, "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==");
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
        let value = secret::resolve_authorization(&policy, &declared, &getter)
            .unwrap()
            .value;
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
        let value = secret::resolve_authorization(&policy, &declared, &getter)
            .unwrap()
            .value;
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
        let err = secret::resolve_authorization(&policy, &no_secrets(), &getter)
            .err()
            .expect("an undeclared secret reference must be rejected");
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
        let (policy, _) = CallTaskExecutor::resolve_authentication(
            auth,
            &named,
            &no_secrets(),
            Arc::new(json!({})),
            &empty_expression(),
        )
        .unwrap();
        let value = secret::resolve_authorization(&policy, &no_secrets(), &no_env)
            .unwrap()
            .value;
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
        let (policy, _) = CallTaskExecutor::resolve_authentication(
            auth,
            &named,
            &no_secrets(),
            Arc::new(json!({})),
            &empty_expression(),
        )
        .unwrap();
        let value = secret::resolve_authorization(&policy, &no_secrets(), &no_env)
            .unwrap()
            .value;
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
        let (policy, _) = CallTaskExecutor::resolve_authentication(
            auth,
            &named,
            &declared(&["API_TOKEN"]),
            Arc::new(json!({})),
            &empty_expression(),
        )
        .unwrap();

        // SAFETY: tests run single-threaded (`--test-threads=1`).
        unsafe { std::env::remove_var(env_name) };

        let value = secret::resolve_authorization(&policy, &no_secrets(), &no_env)
            .unwrap()
            .value;
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
        let auth = Some("Bearer policy".to_string());
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

    fn sample_request() -> Value {
        json!({
            "method": "GET",
            "uri": "https://example.com/items",
            "headers": {}
        })
    }

    #[test]
    fn adapt_content_output_returns_body_only() {
        let response = json!({
            "statusCode": 201,
            "headers": [{"key": "content-type", "value": "application/json"}],
            "content": "{\"ok\":true}"
        });

        let output = CallTaskExecutor::adapt_output(response, HttpOutput::Content, Value::Null);

        assert_eq!(output, json!({"ok": true}));
    }

    #[test]
    fn adapt_response_output_uses_official_shape() {
        let response = json!({
            "statusCode": 201,
            "headers": [{"key": "content-type", "value": "application/json"}],
            "content": "{\"ok\":true}"
        });

        let output =
            CallTaskExecutor::adapt_output(response, HttpOutput::Response, sample_request());

        assert_eq!(
            output,
            json!({
                "request": {
                    "method": "GET",
                    "uri": "https://example.com/items",
                    "headers": {}
                },
                "statusCode": 201,
                "headers": {"content-type": "application/json"},
                "content": {"ok": true}
            })
        );
    }

    #[test]
    fn redirect_option_is_accepted() {
        let call: workflow::CallHttp = serde_json::from_value(json!({
            "method": "GET",
            "endpoint": "https://example.com/items",
            "redirect": true
        }))
        .unwrap();

        assert!(CallTaskExecutor::ensure_supported(&call).is_ok());
    }

    #[test]
    fn accepted_status_range_depends_on_redirect() {
        for (status, redirect, accepted) in [
            (199, false, false),
            (200, false, true),
            (299, false, true),
            (300, false, false),
            (399, false, false),
            (400, false, false),
            (500, false, false),
            (199, true, false),
            (200, true, true),
            (300, true, true),
            (399, true, true),
            (400, true, false),
        ] {
            assert_eq!(
                CallTaskExecutor::is_accepted_status(status, redirect),
                accepted,
                "status={status} redirect={redirect}"
            );
        }
    }

    fn assert_communication(error: &workflow::Error, status: i64) {
        assert_eq!(
            error.type_.0,
            workflow::errors::StandardErrorType::Communication.uri(),
            "{error:?}"
        );
        assert_eq!(error.status, status, "{error:?}");
    }

    #[test]
    fn status_error_carries_response_status_reason_and_body() {
        let response = json!({
            "statusCode": 404,
            "headers": [{"key": "content-type", "value": "text/plain"}],
            "content": "no such item"
        });

        let error = CallTaskExecutor::status_error(&response, 404, Some("/do/0/fetch".into()), &[]);

        assert_communication(&error, 404);
        assert_eq!(error.title.as_deref(), Some("HTTP 404 Not Found"));
        assert_eq!(error.detail.as_deref(), Some("no such item"));
        assert_eq!(error.instance.as_deref(), Some("/do/0/fetch"));
    }

    #[test]
    fn status_error_keeps_json_body_as_raw_text() {
        let response = json!({
            "statusCode": 503,
            "headers": [{"key": "content-type", "value": "application/json"}],
            "content": "{\"error\":\"busy\"}"
        });

        let error = CallTaskExecutor::status_error(&response, 503, None, &[]);

        assert_communication(&error, 503);
        assert_eq!(error.title.as_deref(), Some("HTTP 503 Service Unavailable"));
        assert_eq!(error.detail.as_deref(), Some("{\"error\":\"busy\"}"));
    }

    #[test]
    fn status_error_without_body_has_no_detail() {
        let response = json!({"statusCode": 500, "headers": [], "content": ""});

        let error = CallTaskExecutor::status_error(&response, 500, None, &[]);

        assert_communication(&error, 500);
        assert_eq!(error.detail, None);
    }

    #[test]
    fn status_error_for_unregistered_status_omits_reason() {
        let response = json!({"statusCode": 599, "headers": []});

        let error = CallTaskExecutor::status_error(&response, 599, None, &[]);

        assert_communication(&error, 599);
        assert_eq!(error.title.as_deref(), Some("HTTP 599"));
    }

    #[test]
    fn status_error_truncates_large_body() {
        let body = "x".repeat(ERROR_DETAIL_MAX_BYTES + 10);
        let response = json!({"statusCode": 500, "headers": [], "content": body});

        let error = CallTaskExecutor::status_error(&response, 500, None, &[]);

        let detail = error.detail.unwrap();
        assert!(detail.ends_with(TRUNCATED_MARKER));
        assert_eq!(
            detail.len(),
            ERROR_DETAIL_MAX_BYTES + TRUNCATED_MARKER.len()
        );
    }

    #[test]
    fn response_status_reads_camel_and_snake_case() {
        assert_eq!(
            CallTaskExecutor::response_status(&json!({"statusCode": 204})),
            204
        );
        assert_eq!(
            CallTaskExecutor::response_status(&json!({"status_code": 418})),
            418
        );
    }

    fn request_for(call: Value, auth: Option<String>, secret_values: &[&str]) -> Value {
        let call: workflow::CallHttp = serde_json::from_value(call).unwrap();
        let uri = CallTaskExecutor::ensure_supported(&call).unwrap();
        let (settings, args, _output) =
            CallTaskExecutor::http_request_parts(uri, call, auth).unwrap();
        CallTaskExecutor::request_descriptor(&settings, &args, &secrets(secret_values))
    }

    #[test]
    fn request_descriptor_reports_method_full_uri_and_headers() {
        let request = request_for(
            json!({
                "method": "get",
                "endpoint": "https://example.com/items?existing=1",
                "query": {"page": 2, "q": "a b"},
                "headers": {"Accept": "application/json"}
            }),
            None,
            &[],
        );

        assert_eq!(
            request,
            json!({
                "method": "GET",
                "uri": "https://example.com/items?existing=1&page=2&q=a+b",
                "headers": {"Accept": "application/json"}
            })
        );
    }

    #[test]
    fn request_descriptor_masks_authorization_headers() {
        let request = request_for(
            json!({
                "method": "GET",
                "endpoint": "https://example.com/items",
                "headers": {"Proxy-Authorization": "Basic cHJveHk6cHc="}
            }),
            Some("Bearer tok".to_string()),
            &[],
        );

        assert_eq!(request["headers"]["Authorization"], json!(MASKED_VALUE));
        assert_eq!(
            request["headers"]["Proxy-Authorization"],
            json!(MASKED_VALUE)
        );
    }

    #[test]
    fn request_descriptor_masks_secret_values_in_uri_and_headers() {
        let request = request_for(
            json!({
                "method": "GET",
                "endpoint": "https://example.com/s3cr3t-path/items",
                "query": {"api_key": "s3cr3t value"},
                "headers": {"X-Api-Key": "s3cr3t"}
            }),
            None,
            &["s3cr3t value", "s3cr3t"],
        );

        let uri = request["uri"].as_str().unwrap();
        assert!(!uri.contains("s3cr3t"), "{uri}");
        assert_eq!(request["headers"]["X-Api-Key"], json!(MASKED_VALUE));
    }

    fn assert_no_leak(uri: &str, fragments: &[&str]) {
        for fragment in fragments {
            assert!(!uri.contains(fragment), "{fragment} leaked in {uri}");
        }
    }

    #[test]
    fn request_descriptor_masks_percent_encoded_secret_in_endpoint_query() {
        // e.g. `${ "https://example.com/items?token=" + ($secrets.T | @uri) }`
        let request = request_for(
            json!({
                "method": "GET",
                "endpoint": "https://example.com/items?token=s3cr3t%2Fvalue&page=1"
            }),
            None,
            &["s3cr3t/value"],
        );

        let uri = request["uri"].as_str().unwrap();
        assert_no_leak(uri, &["s3cr3t"]);
        assert!(uri.contains("page=1"), "{uri}");
    }

    #[test]
    fn request_descriptor_masks_secret_with_lowercase_percent_encoding() {
        let request = request_for(
            json!({"method": "GET", "endpoint": "https://example.com/items?token=s3cr3t%2fvalue"}),
            None,
            &["s3cr3t/value"],
        );

        assert_no_leak(request["uri"].as_str().unwrap(), &["s3cr3t"]);
    }

    #[test]
    fn request_descriptor_masks_form_encoded_secret_in_endpoint_query() {
        let request = request_for(
            json!({"method": "GET", "endpoint": "https://example.com/items?token=s3cr3t+value"}),
            None,
            &["s3cr3t value"],
        );

        assert_no_leak(request["uri"].as_str().unwrap(), &["s3cr3t"]);
    }

    #[test]
    fn request_descriptor_masks_percent_encoded_secret_in_path() {
        let request = request_for(
            json!({"method": "GET", "endpoint": "https://example.com/keys/s3cr3t%2Fvalue/items"}),
            None,
            &["s3cr3t/value"],
        );

        let uri = request["uri"].as_str().unwrap();
        assert_no_leak(uri, &["s3cr3t"]);
        assert!(uri.ends_with("/items"), "{uri}");
    }

    #[test]
    fn request_descriptor_masks_userinfo_credentials() {
        let request = request_for(
            json!({"method": "GET", "endpoint": "https://alice:password@example.com/items"}),
            None,
            &[],
        );

        let uri = request["uri"].as_str().unwrap();
        assert_no_leak(uri, &["alice", "password"]);
        assert!(uri.contains("example.com/items"), "{uri}");
    }

    #[test]
    fn request_descriptor_keeps_endpoint_query_unchanged_without_secrets() {
        // Only a query that holds a secret is re-serialized.
        let request = request_for(
            json!({"method": "GET", "endpoint": "https://example.com/items?a=b%20c&flag"}),
            None,
            &["unrelated"],
        );

        assert_eq!(
            request["uri"],
            json!("https://example.com/items?a=b%20c&flag")
        );
    }

    #[test]
    fn request_descriptor_masks_pre_encoded_secret_in_query_map() {
        // `query.token: ${ $secrets.T | @uri }` is encoded again when sent.
        let request = request_for(
            json!({
                "method": "GET",
                "endpoint": "https://example.com/items",
                "query": {"token": "s3cr3t%2Fvalue", "page": 1}
            }),
            None,
            &["s3cr3t/value"],
        );

        let uri = request["uri"].as_str().unwrap();
        assert_no_leak(uri, &["s3cr3t"]);
        assert!(uri.contains("page=1"), "{uri}");
    }

    #[test]
    fn request_descriptor_masks_double_encoded_secret_in_endpoint_query() {
        let request = request_for(
            json!({"method": "GET", "endpoint": "https://example.com/items?token=s3cr3t%252Fvalue"}),
            None,
            &["s3cr3t/value"],
        );

        assert_no_leak(request["uri"].as_str().unwrap(), &["s3cr3t"]);
    }

    #[test]
    fn request_descriptor_masks_secret_spanning_path_segments() {
        let request = request_for(
            json!({"method": "GET", "endpoint": "https://example.com/s3cr3t/va%6Cue/items"}),
            None,
            &["s3cr3t/value"],
        );

        let uri = request["uri"].as_str().unwrap();
        assert_no_leak(uri, &["s3cr3t"]);
        assert!(uri.ends_with("/items"), "{uri}");
    }

    #[test]
    fn request_descriptor_masks_percent_encoded_secret_in_header() {
        let request = request_for(
            json!({
                "method": "GET",
                "endpoint": "https://example.com/items",
                "headers": {"X-Token": "s3cr3t%2Fvalue"}
            }),
            None,
            &["s3cr3t/value"],
        );

        assert_no_leak(&request["headers"]["X-Token"].to_string(), &["s3cr3t"]);
    }

    #[test]
    fn masked_request_uri_masks_encoded_secret_in_fragment() {
        // Endpoints with a fragment are rejected before execution; this guards
        // the masking itself.
        let secrets = secrets(&["s3cr3t/value"]);
        let uri = CallTaskExecutor::masked_request_uri(
            "https://example.com/items#token=s3cr3t%2Fvalue",
            Vec::new(),
            &secrets,
        );

        assert_no_leak(&uri, &["s3cr3t"]);
    }

    #[test]
    fn request_descriptor_masks_raw_and_encoded_secret_in_same_path() {
        let request = request_for(
            json!({"method": "GET", "endpoint": "https://example.com/s3cr3t/value/s3cr3t%2Fvalue"}),
            None,
            &["s3cr3t/value"],
        );

        assert_no_leak(request["uri"].as_str().unwrap(), &["s3cr3t"]);
    }

    #[test]
    fn request_descriptor_masks_raw_and_encoded_secret_in_same_query_value() {
        let request = request_for(
            json!({
                "method": "GET",
                "endpoint": "https://example.com/items",
                "query": {"token": "s3cr3t/value,s3cr3t%2Fvalue"}
            }),
            None,
            &["s3cr3t/value"],
        );

        assert_no_leak(request["uri"].as_str().unwrap(), &["s3cr3t"]);
    }

    #[test]
    fn request_descriptor_masks_raw_and_encoded_secret_in_same_header() {
        let request = request_for(
            json!({
                "method": "GET",
                "endpoint": "https://example.com/items",
                "headers": {"X-Token": "s3cr3t/value s3cr3t%2Fvalue"}
            }),
            None,
            &["s3cr3t/value"],
        );

        assert_no_leak(&request["headers"]["X-Token"].to_string(), &["s3cr3t"]);
    }

    #[test]
    fn request_descriptor_masks_cookie_header() {
        let request = request_for(
            json!({
                "method": "GET",
                "endpoint": "https://example.com/items",
                "headers": {"Cookie": "session=abc123"}
            }),
            None,
            &[],
        );

        assert_eq!(request["headers"]["Cookie"], json!(MASKED_VALUE));
    }

    #[test]
    fn status_error_masks_secret_echoed_in_body() {
        let response = json!({
            "statusCode": 403,
            "headers": [],
            "content": "invalid key s3cr3t/value (s3cr3t%2Fvalue)"
        });
        let secrets = secrets(&["s3cr3t/value"]);

        let error = CallTaskExecutor::status_error(&response, 403, None, &secrets);

        let detail = error.detail.unwrap();
        assert_no_leak(&detail, &["s3cr3t"]);
        assert!(detail.contains("invalid key"), "{detail}");
    }

    #[test]
    fn status_error_masks_secret_before_truncating_body() {
        // A secret straddling the size limit must not leave its head exposed.
        let limit = ERROR_DETAIL_MAX_BYTES;
        let body = format!("{}s3cr3t/value", "x".repeat(limit - 3));
        let response = json!({"statusCode": 500, "headers": [], "content": body});
        let secrets = secrets(&["s3cr3t/value"]);

        let error = CallTaskExecutor::status_error(&response, 500, None, &secrets);

        assert_no_leak(&error.detail.unwrap(), &["s3c"]);
    }

    #[test]
    fn use_reference_reports_secrets_resolved_in_named_policy() {
        let call: workflow::CallHttp = serde_json::from_value(json!({
            "method": "GET",
            "endpoint": {
                "uri": "https://example.com/items",
                "authentication": {"use": "basicAuth"}
            }
        }))
        .unwrap();
        let mut named = std::collections::HashMap::new();
        named.insert(
            "basicAuth".to_string(),
            workflow::AuthenticationPolicy::Basic(
                workflow::BasicAuthentication::BasicAuthenticationProperties {
                    username: "${ $secrets.API_USER }".to_string(),
                    password: "pw".to_string(),
                },
            ),
        );
        let env_name = "WORKFLOW_SECRET_API_USER";
        // SAFETY: tests run single-threaded (`--test-threads=1`).
        unsafe { std::env::set_var(env_name, "svc-user") };

        let auth = CallTaskExecutor::endpoint_authentication(&call.endpoint).unwrap();
        let resolved = CallTaskExecutor::resolve_authentication(
            auth,
            &named,
            &declared(&["API_USER"]),
            Arc::new(json!({})),
            &empty_expression(),
        );

        // SAFETY: tests run single-threaded (`--test-threads=1`).
        unsafe { std::env::remove_var(env_name) };
        let (_, policy_secrets) = resolved.unwrap();
        assert_eq!(policy_secrets, vec!["svc-user".to_string()]);
    }

    #[test]
    fn userinfo_credentials_are_reported_from_endpoint() {
        assert_eq!(
            CallTaskExecutor::userinfo_credentials("https://alice:pa%20ss@example.com/items"),
            // Decoded (masking also matches the percent-encoded form), plus the
            // Basic value the client derives from them: base64("alice:pa ss").
            vec![
                "alice".to_string(),
                "pa ss".to_string(),
                "YWxpY2U6cGEgc3M=".to_string()
            ]
        );
        assert_eq!(
            CallTaskExecutor::userinfo_credentials("https://tok@example.com/items"),
            // base64("tok:")
            vec!["tok".to_string(), "dG9rOg==".to_string()]
        );
        assert!(CallTaskExecutor::userinfo_credentials("https://example.com/items").is_empty());
    }

    #[test]
    fn userinfo_credentials_follow_the_client_for_non_utf8_components() {
        // A password that is not UTF-8 is dropped by the client, which then
        // sends base64("alice:"); the password is reported as written.
        assert_eq!(
            CallTaskExecutor::userinfo_credentials("https://alice:pa%FFss@example.com/"),
            vec![
                "alice".to_string(),
                "pa%FFss".to_string(),
                "YWxpY2U6".to_string()
            ]
        );
        // A username that is not UTF-8 disables the client's Basic header.
        assert_eq!(
            CallTaskExecutor::userinfo_credentials("https://al%FFice:pw@example.com/"),
            vec!["al%FFice".to_string(), "pw".to_string()]
        );
    }
}
