//! Secret resolution for workflow task execution.
//!
//! Secret values are never written in the workflow definition. A workflow
//! declares secret names under `use.secrets`, and the runtime resolves each
//! declared name from the `WORKFLOW_SECRET_<UPPER_SNAKE_NAME>` environment
//! variable. Only declared names may be resolved; an undeclared reference is a
//! configuration error.
//!
//! Resolved values are kept local to the task and are never logged or
//! embedded in error messages (only the secret name is surfaced).

use crate::workflow::definition::{
    transform::{self, TransformExpression},
    workflow,
};
use anyhow::{Result, anyhow};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde_json::{Map, Value};
use std::{collections::HashSet, sync::Arc};

/// Map a declared secret name to its environment variable name.
/// Non-alphanumeric characters become `_`, then the whole name is uppercased
/// and prefixed with `WORKFLOW_SECRET_` (e.g. `github-token` ->
/// `WORKFLOW_SECRET_GITHUB_TOKEN`).
pub(crate) fn env_var_name(secret_name: &str) -> String {
    let sanitized: String = secret_name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("WORKFLOW_SECRET_{}", sanitized.to_ascii_uppercase())
}

/// Resolve a declared secret name to its raw environment value.
///
/// `getter` reads the environment (inject `std::env::var(..).ok()` in
/// production; a closure in tests). Errors never include the secret value.
pub(crate) fn resolve_secret<F>(name: &str, declared: &HashSet<String>, getter: F) -> Result<String>
where
    F: Fn(&str) -> Option<String>,
{
    if !declared.contains(name) {
        return Err(anyhow!(
            "secret '{name}' is not declared in use.secrets; declare it before referencing it"
        ));
    }
    let env_name = env_var_name(name);
    match getter(&env_name) {
        Some(value) if !value.is_empty() => Ok(value),
        _ => Err(anyhow!(
            "secret '{name}' is not set; expected environment variable {env_name}"
        )),
    }
}

/// Merge newly resolved secrets into the `$secrets` object of an expression
/// context, preserving any already present (e.g. those referenced by `with`).
pub(crate) fn merge_secrets(
    expression: &mut std::collections::BTreeMap<String, Arc<Value>>,
    secrets: Value,
) {
    let Value::Object(new_secrets) = secrets else {
        return;
    };
    if new_secrets.is_empty() {
        return;
    }
    let mut merged = expression
        .get("secrets")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    merged.extend(new_secrets);
    expression.insert("secrets".to_string(), Arc::new(Value::Object(merged)));
}

/// Build the `$secrets` object for expression evaluation, resolving only the
/// declared secrets actually referenced in a serialized workflow task payload.
///
/// `use.secrets` is a declaration, not a demand to load every value: a workflow
/// may declare several secrets and use only some of them. Resolving all of them
/// eagerly would fail the task when an unused secret's env var is absent. We
/// therefore resolve a declared name only when the payload references it inside
/// a `${ ... }` / `$${ ... }` expression span. An undeclared name is still
/// rejected, and a referenced-but-unset secret still errors.
#[cfg(test)]
pub(crate) fn resolve_declared_secrets(
    declared: &std::collections::HashSet<String>,
    serialized: &str,
) -> Result<Value> {
    let referenced = referenced_secret_names(serialized);
    resolve_referenced_secrets(declared, &referenced)
}

/// Scan `serialized` once for `$secrets` references, reject any undeclared
/// reference, then resolve the referenced declared secrets into a `$secrets`
/// object.
pub(crate) fn resolve_secrets_for(
    declared: &std::collections::HashSet<String>,
    serialized: &str,
) -> Result<Value> {
    let referenced = referenced_secret_names(serialized);
    reject_undeclared_secret_references(declared, &referenced)?;
    resolve_referenced_secrets(declared, &referenced)
}

fn resolve_referenced_secrets(
    declared: &std::collections::HashSet<String>,
    referenced: &std::collections::HashSet<String>,
) -> Result<Value> {
    let mut map = Map::new();
    for name in declared {
        if !referenced.contains(name) {
            continue;
        }
        let raw = resolve_secret(name, declared, |k| std::env::var(k).ok())?;
        let value = if raw.trim_start().starts_with('{') {
            serde_json::from_str(&raw).unwrap_or(Value::String(raw))
        } else {
            Value::String(raw)
        };
        map.insert(name.clone(), value);
    }
    Ok(Value::Object(map))
}

/// Reject any `$secrets.<name>` / `secrets[...]` reference whose name is not
/// declared in `use.secrets`.
#[cfg(test)]
pub(crate) fn ensure_no_undeclared_secret_references(
    declared: &std::collections::HashSet<String>,
    serialized: &str,
) -> Result<()> {
    reject_undeclared_secret_references(declared, &referenced_secret_names(serialized))
}

fn reject_undeclared_secret_references(
    declared: &std::collections::HashSet<String>,
    referenced: &std::collections::HashSet<String>,
) -> Result<()> {
    for name in referenced {
        if !declared.contains(name) {
            return Err(anyhow!(
                "secret '{name}' is not declared in use.secrets; declare it before referencing it"
            ));
        }
    }
    Ok(())
}

/// Extract every secret name referenced via `$secrets`/`secrets` in dot or
/// bracket notation, only from string values that the shared transformer treats
/// as `${ ... }` (jq) or `$${ ... }` (Liquid) expressions.
pub(crate) fn referenced_secret_names(serialized: &str) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    match serde_json::from_str::<Value>(serialized) {
        Ok(value) => collect_secret_names_from_value(&value, &mut names),
        Err(_) => collect_secret_names_from_string(serialized, &mut names),
    }
    names
}

fn collect_secret_names_from_value(value: &Value, names: &mut std::collections::HashSet<String>) {
    match value {
        Value::String(s) => collect_secret_names_from_string(s, names),
        Value::Array(values) => {
            for value in values {
                collect_secret_names_from_value(value, names);
            }
        }
        Value::Object(map) => {
            for value in map.values() {
                collect_secret_names_from_value(value, names);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn collect_secret_names_from_string(value: &str, names: &mut std::collections::HashSet<String>) {
    match transform::transform_expression_body(value) {
        Some(TransformExpression::Jq(span)) => {
            let unquoted = without_quoted_literals(span);
            collect_dot_notation_names(&unquoted, "$secrets.", false, names);
            collect_bracket_names(span, "$secrets['", "']", false, names);
            collect_bracket_names(span, "$secrets[\"", "\"]", false, names);
        }
        Some(TransformExpression::Liquid(span)) => {
            for tag in transform::liquid_template_tag_bodies(span) {
                let unquoted = without_quoted_literals(tag);
                collect_dot_notation_names(&unquoted, "secrets.", true, names);
                collect_bracket_names(tag, "secrets['", "']", true, names);
                collect_bracket_names(tag, "secrets[\"", "\"]", true, names);
            }
        }
        None => {}
    }
}

fn without_quoted_literals(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in value.chars() {
        if let Some(q) = quote {
            out.push(' ');
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        if c == '"' || c == '\'' {
            quote = Some(c);
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

fn collect_dot_notation_names(
    serialized: &str,
    needle: &str,
    require_leading_boundary: bool,
    names: &mut std::collections::HashSet<String>,
) {
    let mut from = 0;
    while let Some(rel) = serialized[from..].find(needle) {
        let needle_start = from + rel;
        let start = needle_start + needle.len();
        if require_leading_boundary && !has_secret_variable_boundary(serialized, needle_start) {
            from = start;
            continue;
        }
        let name: String = serialized[start..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        from = start + name.len();
        if !name.is_empty() {
            names.insert(name);
        }
    }
}

fn has_secret_variable_boundary(serialized: &str, needle_start: usize) -> bool {
    serialized[..needle_start]
        .chars()
        .next_back()
        .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_' && c != '.')
}

fn collect_bracket_names(
    serialized: &str,
    open: &str,
    close: &str,
    require_leading_boundary: bool,
    names: &mut std::collections::HashSet<String>,
) {
    let mut from = 0;
    while let Some(rel) = serialized[from..].find(open) {
        let open_start = from + rel;
        let start = open_start + open.len();
        if is_inside_quoted_literal(serialized, open_start) {
            from = start;
            continue;
        }
        if require_leading_boundary && !has_secret_variable_boundary(serialized, open_start) {
            from = start;
            continue;
        }
        let Some(end_rel) = serialized[start..].find(close) else {
            break;
        };
        let name = &serialized[start..start + end_rel];
        from = start + end_rel + close.len();
        if !name.is_empty() {
            names.insert(name.to_string());
        }
    }
}

fn is_inside_quoted_literal(value: &str, index: usize) -> bool {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in value.char_indices() {
        if i >= index {
            break;
        }
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        if c == '"' || c == '\'' {
            quote = Some(c);
        }
    }
    quote.is_some()
}

/// Interpret a resolved bearer secret value as a token string. The raw value is
/// the token itself, or a `{"token": "..."}` JSON object. `name` is only used
/// for error messages — the value is never included.
pub(crate) fn interpret_bearer_secret(name: &str, raw: &str) -> Result<String> {
    if raw.trim_start().starts_with('{') {
        let obj: serde_json::Value = serde_json::from_str(raw)
            .map_err(|_| anyhow!("bearer secret '{name}' is not valid JSON"))?;
        obj.get("token")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("bearer secret '{name}' is missing a string 'token' field"))
    } else {
        Ok(raw.to_string())
    }
}

/// Interpret a resolved basic secret value as `(username, password)`. The raw
/// value must be a `{"username": "...", "password": "..."}` JSON object. `name`
/// is only used for error messages — the value is never included.
pub(crate) fn interpret_basic_secret(name: &str, raw: &str) -> Result<(String, String)> {
    let obj: serde_json::Value = serde_json::from_str(raw).map_err(|_| {
        anyhow!("basic secret '{name}' must be a JSON object with username/password")
    })?;
    let username = obj
        .get("username")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("basic secret '{name}' is missing a string 'username' field"))?;
    let password = obj
        .get("password")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("basic secret '{name}' is missing a string 'password' field"))?;
    Ok((username.to_string(), password.to_string()))
}

fn bearer_token(
    bearer: &workflow::BearerAuthentication,
    declared_secrets: &std::collections::HashSet<String>,
    getter: &impl Fn(&str) -> Option<String>,
) -> Result<String> {
    match bearer {
        workflow::BearerAuthentication::BearerAuthenticationProperties { token } => {
            Ok(token.clone())
        }
        workflow::BearerAuthentication::SecretBasedAuthenticationPolicy(secret_ref) => {
            let name = secret_ref.use_.as_str();
            let raw = resolve_secret(name, declared_secrets, getter)?;
            interpret_bearer_secret(name, &raw)
        }
    }
}

fn basic_credentials(
    basic: &workflow::BasicAuthentication,
    declared_secrets: &std::collections::HashSet<String>,
    getter: &impl Fn(&str) -> Option<String>,
) -> Result<(String, String)> {
    match basic {
        workflow::BasicAuthentication::BasicAuthenticationProperties { username, password } => {
            Ok((username.clone(), password.clone()))
        }
        workflow::BasicAuthentication::SecretBasedAuthenticationPolicy(secret_ref) => {
            let name = secret_ref.use_.as_str();
            let raw = resolve_secret(name, declared_secrets, getter)?;
            interpret_basic_secret(name, &raw)
        }
    }
}

/// Convert an authentication policy into an Authorization field value, resolving
/// any `{ use: <name> }` secret reference. The value must not be logged.
pub(crate) fn authorization_value(
    policy: &workflow::AuthenticationPolicy,
    declared_secrets: &std::collections::HashSet<String>,
    getter: &impl Fn(&str) -> Option<String>,
) -> Result<String> {
    match policy {
        workflow::AuthenticationPolicy::Bearer(bearer) => {
            let token = bearer_token(bearer, declared_secrets, getter)?;
            Ok(format!("Bearer {token}"))
        }
        workflow::AuthenticationPolicy::Basic(basic) => {
            let (username, password) = basic_credentials(basic, declared_secrets, getter)?;
            let encoded = BASE64_STANDARD.encode(format!("{username}:{password}"));
            Ok(format!("Basic {encoded}"))
        }
    }
}

/// Convert an authentication policy into the `(name, value)` Authorization
/// header shape used by HTTP runner arguments.
pub(crate) fn authentication_header(
    policy: &workflow::AuthenticationPolicy,
    declared_secrets: &std::collections::HashSet<String>,
    getter: &impl Fn(&str) -> Option<String>,
) -> Result<(String, String)> {
    let value = authorization_value(policy, declared_secrets, getter)?;
    Ok(("Authorization".to_string(), value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn env_var_name_uppercases_and_sanitizes() {
        assert_eq!(env_var_name("github-token"), "WORKFLOW_SECRET_GITHUB_TOKEN");
        assert_eq!(env_var_name("api.key"), "WORKFLOW_SECRET_API_KEY");
        assert_eq!(env_var_name("simple"), "WORKFLOW_SECRET_SIMPLE");
        assert_eq!(env_var_name("a1_b2"), "WORKFLOW_SECRET_A1_B2");
    }

    #[test]
    fn resolves_declared_secret_from_getter() {
        let declared = declared(&["token"]);
        let value = resolve_secret("token", &declared, |k| {
            (k == "WORKFLOW_SECRET_TOKEN").then(|| "s3cret".to_string())
        })
        .unwrap();
        assert_eq!(value, "s3cret");
    }

    #[test]
    fn undeclared_secret_is_rejected_without_reading_env() {
        let declared = declared(&["other"]);
        let read = std::cell::Cell::new(false);
        let err = resolve_secret("token", &declared, |_| {
            read.set(true);
            Some("leaked".to_string())
        })
        .unwrap_err();
        assert!(!read.get(), "env must not be read for undeclared secret");
        // Error names the secret but never its value.
        assert!(err.to_string().contains("token"));
        assert!(!err.to_string().contains("leaked"));
    }

    #[test]
    fn missing_env_is_an_error_naming_the_var() {
        let declared = declared(&["token"]);
        let err = resolve_secret("token", &declared, |_| None).unwrap_err();
        assert!(err.to_string().contains("WORKFLOW_SECRET_TOKEN"));
        assert!(err.to_string().contains("token"));
    }

    #[test]
    fn empty_env_value_is_treated_as_missing() {
        let declared = declared(&["token"]);
        let err = resolve_secret("token", &declared, |_| Some(String::new())).unwrap_err();
        assert!(err.to_string().contains("is not set"));
    }

    #[test]
    fn bearer_secret_plain_string_is_the_token() {
        assert_eq!(
            interpret_bearer_secret("api", "raw-token").unwrap(),
            "raw-token"
        );
    }

    #[test]
    fn bearer_secret_json_token_field_is_extracted() {
        assert_eq!(
            interpret_bearer_secret("api", r#"{"token": "json-token"}"#).unwrap(),
            "json-token"
        );
    }

    #[test]
    fn bearer_secret_json_without_token_errors_without_value() {
        let err = interpret_bearer_secret("api", r#"{"other": "leaked"}"#).unwrap_err();
        assert!(err.to_string().contains("api"));
        assert!(!err.to_string().contains("leaked"));
    }

    #[test]
    fn basic_secret_json_yields_username_password() {
        let (u, p) =
            interpret_basic_secret("creds", r#"{"username": "alice", "password": "pw"}"#).unwrap();
        assert_eq!(u, "alice");
        assert_eq!(p, "pw");
    }

    #[test]
    fn basic_secret_missing_password_errors_without_value() {
        let err = interpret_basic_secret("creds", r#"{"username": "alice"}"#).unwrap_err();
        assert!(err.to_string().contains("creds"));
        assert!(err.to_string().contains("password"));
    }

    #[test]
    fn basic_secret_non_json_errors() {
        let err = interpret_basic_secret("creds", "not-json").unwrap_err();
        assert!(err.to_string().contains("creds"));
    }

    #[test]
    fn authorization_value_formats_bearer_policy() {
        let policy = workflow::AuthenticationPolicy::Bearer(
            workflow::BearerAuthentication::BearerAuthenticationProperties {
                token: "abc123".to_string(),
            },
        );

        let value = authorization_value(&policy, &declared(&[]), &|_| None).unwrap();

        assert_eq!(value, "Bearer abc123");
    }

    #[test]
    fn authorization_value_formats_basic_policy() {
        let policy = workflow::AuthenticationPolicy::Basic(
            workflow::BasicAuthentication::BasicAuthenticationProperties {
                username: "Aladdin".to_string(),
                password: "open sesame".to_string(),
            },
        );

        let value = authorization_value(&policy, &declared(&[]), &|_| None).unwrap();

        assert_eq!(value, "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==");
    }
}
