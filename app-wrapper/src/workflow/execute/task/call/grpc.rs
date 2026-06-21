//! gRPC-specific logic for the `call: grpc` task (unary only).
//!
//! Converts a `call: grpc` task into a GRPC runner unary job: builds the runner
//! settings (host/port/auth_token/use_reflection) and args (`service/method`
//! path, `arguments` as a JSON body resolved via reflection), and adapts the
//! unary result into the task output (`{ code, body, metadata, message? }`).

use super::CallTaskExecutor;
use crate::workflow::{
    definition::workflow,
    execute::{context::TaskContext, secret},
};
use anyhow::Result;
use serde_json::{Value, json};
use std::sync::Arc;

// Canonical name of the built-in GRPC runner and its unary method, mirrored from
// `RunnerType::Grpc.as_str_name()` and `runner::grpc::METHOD_UNARY`. The GRPC
// runner is multi-method (unary/streaming), so `using` must be set explicitly; a
// unit test guards against drift.
pub(super) const GRPC_RUNNER_NAME: &str = "GRPC";
pub(super) const GRPC_METHOD_UNARY: &str = "unary";

impl CallTaskExecutor {
    fn grpc_authentication(
        service: &workflow::GrpcService,
    ) -> Option<&workflow::GrpcServiceAuthentication> {
        service.authentication.as_ref()
    }

    /// Resolve a gRPC service authentication into a concrete policy by delegating
    /// to the shared [`CallTaskExecutor::resolve_named_policy`] after
    /// destructuring the gRPC-specific enum into `(inline, use_ref)`.
    fn resolve_grpc_authentication(
        grpc_auth: &workflow::GrpcServiceAuthentication,
        named: &std::collections::HashMap<String, workflow::AuthenticationPolicy>,
        declared_secrets: &std::collections::HashSet<String>,
        raw_input: Arc<Value>,
        expression: &std::collections::BTreeMap<String, Arc<Value>>,
    ) -> Result<workflow::AuthenticationPolicy, Box<workflow::Error>> {
        let (inline, use_ref) = match grpc_auth {
            workflow::GrpcServiceAuthentication::Variant0(policy) => (Some(policy), None),
            workflow::GrpcServiceAuthentication::Variant1 { use_ } => (None, Some(use_.as_str())),
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

    // Reject gRPC `with` features the jobworkerp adapter does not execute. The
    // official `proto` (externalResource) is not used because the adapter relies
    // on server reflection instead.
    fn ensure_supported_grpc(call: &workflow::CallGrpc) -> Result<(), Box<workflow::Error>> {
        if call.proto.is_some() {
            return Err(Self::unsupported(
                "proto",
                "proto resource references are not supported; the gRPC adapter uses server reflection".to_string(),
            ));
        }
        Ok(())
    }

    // Build GRPC runner settings and unary args from a resolved gRPC call.
    // `method` is the runner's "service/method" path; `arguments` is sent as a
    // JSON body resolved to protobuf via reflection (`as_json=true`).
    fn grpc_request_parts(
        call: workflow::CallGrpc,
        auth_token: Option<String>,
    ) -> Result<(Value, Value)> {
        let mut settings = json!({
            "host": call.service.host,
            "use_reflection": true,
        });
        if let Some(port) = call.service.port {
            settings["port"] = json!(port);
        }
        if let Some(token) = auth_token {
            settings["auth_token"] = Value::String(token);
        }
        let method = format!("{}/{}", call.service.name, call.method);
        // `arguments` is a serde_json::Map, which serializes directly to a JSON
        // object string without an intermediate Value::Object wrapper.
        let json_body = serde_json::to_string(&call.arguments)?;
        let args = json!({
            "method": method,
            "json_body": json_body,
            "as_json": true,
        });
        Ok((settings, args))
    }

    // Adapt the GRPC unary runner result into the task output. The runner emits
    // `{ metadata, code, message?, body|jsonBody }`; the `response_data` oneof is
    // serialized to JSON in camelCase (`jsonBody`), but snake_case is also probed
    // defensively. `jsonBody` is a JSON string that is parsed into structured
    // `body`. Runner-internal field names are not exposed beyond this contract.
    fn adapt_grpc_output(response: Value) -> Value {
        let body = match response
            .get("jsonBody")
            .or_else(|| response.get("json_body"))
            .and_then(Value::as_str)
        {
            Some(json) => {
                serde_json::from_str(json).unwrap_or_else(|_| Value::String(json.to_string()))
            }
            None => response.get("body").cloned().unwrap_or(Value::Null),
        };
        let mut out = json!({
            "code": response.get("code").and_then(Value::as_i64).unwrap_or_default(),
            "body": body,
        });
        if let Some(metadata) = response.get("metadata") {
            out["metadata"] = metadata.clone();
        }
        if let Some(message) = response.get("message").and_then(Value::as_str)
            && !message.is_empty()
        {
            out["message"] = Value::String(message.to_string());
        }
        out
    }

    // Execute a gRPC unary call through the GRPC runner and return its adapted
    // output. The `arguments` map is sent as a JSON request body and resolved to
    // protobuf by the runner via reflection.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn execute_grpc(
        &self,
        cx: Arc<opentelemetry::Context>,
        task_name: &str,
        task_context: &mut TaskContext,
        call: workflow::CallGrpc,
        named_authentications: &std::collections::HashMap<String, workflow::AuthenticationPolicy>,
        declared_secrets: &std::collections::HashSet<String>,
        expression: &std::collections::BTreeMap<String, Arc<Value>>,
        timeout_sec: u32,
    ) -> Result<Value, Box<workflow::Error>> {
        Self::ensure_supported_grpc(&call)?;
        // Resolve service authentication into a bare bearer token. The GRPC
        // runner prepends `Bearer ` to `auth_token` itself, so the adapter must
        // pass the unprefixed token; basic is rejected (single-token field).
        let auth_token = match Self::grpc_authentication(&call.service) {
            Some(grpc_auth) => {
                let policy = Self::resolve_grpc_authentication(
                    grpc_auth,
                    named_authentications,
                    declared_secrets,
                    task_context.input.clone(),
                    expression,
                )?;
                let token = secret::bearer_token_value(&policy, declared_secrets, &|k| {
                    std::env::var(k).ok()
                })
                .map_err(|e| Self::unsupported("authentication", e.to_string()))?;
                Some(token)
            }
            None => None,
        };
        let (settings, args) = Self::grpc_request_parts(call, auth_token)
            .map_err(|e| Self::unsupported("with", e.to_string()))?;

        let raw_response = self
            .execute_runner(
                cx,
                GRPC_RUNNER_NAME,
                Some(GRPC_METHOD_UNARY),
                settings,
                args,
                task_name,
                timeout_sec,
            )
            .await
            .map_err(|e| {
                workflow::errors::ErrorFactory::new().service_unavailable(
                    "Failed to execute call.grpc".to_string(),
                    None,
                    Some(e.to_string()),
                )
            })?;
        Ok(Self::adapt_grpc_output(raw_response))
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

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn grpc_call(value: serde_json::Value) -> workflow::CallGrpc {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn grpc_runner_name_matches_proto_enum() {
        assert_eq!(GRPC_RUNNER_NAME, RunnerType::Grpc.as_str_name());
    }

    #[test]
    fn grpc_request_parts_builds_method_path_and_json_body() {
        let call = grpc_call(json!({
            "service": {"name": "helloworld.Greeter", "host": "localhost", "port": 50051},
            "method": "SayHello",
            "arguments": {"name": "world"}
        }));

        let (settings, args) = CallTaskExecutor::grpc_request_parts(call, None).unwrap();

        assert_eq!(settings["host"], json!("localhost"));
        assert_eq!(settings["port"], json!(50051));
        assert_eq!(settings["use_reflection"], json!(true));
        assert!(settings.get("auth_token").is_none());
        assert_eq!(args["method"], json!("helloworld.Greeter/SayHello"));
        assert_eq!(args["as_json"], json!(true));
        // arguments are serialized to a JSON string body.
        assert_eq!(args["json_body"], json!("{\"name\":\"world\"}"));
    }

    #[test]
    fn grpc_request_parts_omits_port_when_absent_and_sets_bare_auth_token() {
        let call = grpc_call(json!({
            "service": {"name": "pkg.Svc", "host": "example.com"},
            "method": "Do"
        }));

        let (settings, args) =
            CallTaskExecutor::grpc_request_parts(call, Some("raw-token".to_string())).unwrap();

        assert!(settings.get("port").is_none());
        // The runner prepends `Bearer ` itself; the adapter must pass the bare token.
        assert_eq!(settings["auth_token"], json!("raw-token"));
        // Empty arguments serialize to an empty JSON object.
        assert_eq!(args["json_body"], json!("{}"));
    }

    #[test]
    fn ensure_supported_grpc_rejects_proto_resource() {
        let call = grpc_call(json!({
            "service": {"name": "pkg.Svc", "host": "example.com"},
            "method": "Do",
            "proto": {"endpoint": "file://app/greet.proto"}
        }));

        let err = CallTaskExecutor::ensure_supported_grpc(&call).unwrap_err();
        assert!(err.to_string().contains("proto"));
    }

    #[test]
    fn ensure_supported_grpc_accepts_minimal_call() {
        let call = grpc_call(json!({
            "service": {"name": "pkg.Svc", "host": "example.com"},
            "method": "Do"
        }));
        assert!(CallTaskExecutor::ensure_supported_grpc(&call).is_ok());
    }

    #[test]
    fn grpc_inline_bearer_resolves_to_bare_token() {
        let call = grpc_call(json!({
            "service": {
                "name": "pkg.Svc",
                "host": "example.com",
                "authentication": {"bearer": {"token": "tok"}}
            },
            "method": "Do"
        }));

        let grpc_auth = CallTaskExecutor::grpc_authentication(&call.service).unwrap();
        let policy = CallTaskExecutor::resolve_grpc_authentication(
            grpc_auth,
            &std::collections::HashMap::new(),
            &no_secrets(),
            Arc::new(json!({})),
            &empty_expression(),
        )
        .unwrap();
        let token = secret::bearer_token_value(&policy, &no_secrets(), &no_env).unwrap();
        // Bare token, no `Bearer ` prefix (the runner adds it).
        assert_eq!(token, "tok");
    }

    #[test]
    fn grpc_basic_authentication_is_rejected() {
        let policy = workflow::AuthenticationPolicy::Basic(
            workflow::BasicAuthentication::BasicAuthenticationProperties {
                username: "admin".to_string(),
                password: "s3cret".to_string(),
            },
        );
        let err = secret::bearer_token_value(&policy, &no_secrets(), &no_env).unwrap_err();
        assert!(err.to_string().contains("basic"));
        assert!(!err.to_string().contains("s3cret"));
    }

    #[test]
    fn adapt_grpc_output_parses_camel_case_json_body() {
        // The runner serializes the response_data oneof in camelCase (`jsonBody`).
        let response = json!({
            "code": 0,
            "jsonBody": "{\"message\":\"hello world\"}",
            "metadata": {"content-type": "application/grpc"}
        });

        let output = CallTaskExecutor::adapt_grpc_output(response);

        assert_eq!(
            output,
            json!({
                "code": 0,
                "body": {"message": "hello world"},
                "metadata": {"content-type": "application/grpc"}
            })
        );
    }

    #[test]
    fn adapt_grpc_output_parses_snake_case_json_body() {
        // snake_case `json_body` is probed defensively as a fallback.
        let response = json!({
            "code": 0,
            "json_body": "{\"message\":\"hi\"}",
        });

        let output = CallTaskExecutor::adapt_grpc_output(response);

        assert_eq!(output["body"], json!({"message": "hi"}));
    }

    #[test]
    fn adapt_grpc_output_includes_nonempty_message_and_falls_back_to_body() {
        let response = json!({
            "code": 5,
            "body": null,
            "message": "not found"
        });

        let output = CallTaskExecutor::adapt_grpc_output(response);

        assert_eq!(output["code"], json!(5));
        assert_eq!(output["body"], json!(null));
        assert_eq!(output["message"], json!("not found"));
    }
}
