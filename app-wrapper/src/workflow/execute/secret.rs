//! Secret resolution for workflow task execution.
//!
//! Secret values are never written in the workflow definition. A workflow
//! declares secret names under `use.secrets`, and the runtime resolves each
//! declared name from the `WORKFLOW_SECRET_<UPPER_SNAKE_NAME>` environment
//! variable. Only declared names may be resolved; referencing an undeclared or
//! unset secret is a [`SecretAccessError`], which the specification requires to
//! surface as an authorization error.
//!
//! Resolved values are kept local to the task and are never logged or
//! embedded in error messages (only the secret name is surfaced).

use crate::workflow::definition::{
    transform::{self, TransformExpression},
    workflow,
};
use crate::workflow::execute::expression::ExpressionContext;
use anyhow::{Result, anyhow};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use infra::workflow::definition::expression_context::SECRETS_VARIABLE;
use serde_json::{Map, Value};
use std::{collections::HashSet, sync::Arc};

/// Placeholder that replaces credentials in task output.
pub(crate) const MASKED_VALUE: &str = "***";

// Bound on repeated percent-decoding when looking for secrets, enough to undo
// a value encoded before being placed into a request and encoded again when
// sent.
const MAX_SECRET_DECODE_PASSES: usize = 4;

/// A secret the workflow may not read: it is not declared in `use.secrets` or
/// its value is not available. Carried inside `anyhow::Error` so callers can
/// tell it apart (via `downcast_ref`) from malformed secret values.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(crate) struct SecretAccessError(String);

fn undeclared_secret(name: &str) -> anyhow::Error {
    SecretAccessError(format!(
        "secret '{name}' is not declared in use.secrets; declare it before referencing it"
    ))
    .into()
}

fn unset_secret(name: &str, env_name: &str) -> anyhow::Error {
    SecretAccessError(format!(
        "secret '{name}' is not set; expected environment variable {env_name}"
    ))
    .into()
}

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
        return Err(undeclared_secret(name));
    }
    let env_name = env_var_name(name);
    match getter(&env_name) {
        Some(value) if !value.is_empty() => Ok(value),
        _ => Err(unset_secret(name, &env_name)),
    }
}

/// Merge newly resolved secrets into the `$secrets` object of an expression
/// context, preserving any already present (e.g. those referenced by `with`).
pub(crate) fn merge_secrets(expression: &mut ExpressionContext, secrets: Value) {
    let Value::Object(new_secrets) = secrets else {
        return;
    };
    if new_secrets.is_empty() {
        return;
    }
    // `secrets` is only ever inserted eagerly, so lookup cannot fail.
    let mut merged = expression
        .get(SECRETS_VARIABLE)
        .ok()
        .flatten()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    merged.extend(new_secrets);
    expression.insert(SECRETS_VARIABLE, Arc::new(Value::Object(merged)));
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

/// Every non-empty string leaf of a resolved `$secrets` object, longest first.
pub(crate) fn secret_values(secrets: &Value) -> Vec<String> {
    fn collect(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::String(s) => out.push(s.clone()),
            Value::Object(map) => map.values().for_each(|v| collect(v, out)),
            Value::Array(values) => values.iter().for_each(|v| collect(v, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    collect(secrets, &mut out);
    longest_first(out)
}

/// `secret_values` plus further values to mask that `$secrets` alone does not
/// cover: credentials resolved by an authentication step (named policies and
/// `{ use: <name> }` references resolve their secrets later, and inline tokens
/// are credentials too) and URI userinfo.
pub(crate) fn merge_secret_values(secret_values: &[String], more: Vec<String>) -> Vec<String> {
    longest_first(secret_values.iter().cloned().chain(more).collect())
}

/// Non-empty values, longest first so that masking one never leaves part of a
/// longer one exposed.
fn longest_first(mut values: Vec<String>) -> Vec<String> {
    values.retain(|value| !value.is_empty());
    values.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    values.dedup();
    values
}

fn contains_secret_value(text: &str, secret_values: &[String]) -> bool {
    secret_values
        .iter()
        .any(|secret| text.contains(secret.as_str()))
}

/// `text` with every raw occurrence of a secret value replaced by
/// [`MASKED_VALUE`].
pub(crate) fn mask_secret_values(text: &str, secret_values: &[String]) -> String {
    secret_values.iter().fold(text.to_string(), |text, secret| {
        text.replace(secret.as_str(), MASKED_VALUE)
    })
}

/// `text` with every secret occurrence masked, whether raw or percent-encoded
/// (possibly more than once), or `None` when no secret is found (so callers
/// keep the original text). Decoding repeats because a value encoded before
/// being placed into a request (e.g. with jq's `@uri`) is encoded again when
/// the URI is serialized. Each level is masked before it is decoded further,
/// so raw and encoded occurrences in the same text are all covered, and a
/// secret that itself looks percent-encoded is masked before decoding alters
/// it. A masked result is in decoded form.
pub(crate) fn mask_encoded_secret_values(text: &str, secret_values: &[String]) -> Option<String> {
    if secret_values.is_empty() {
        return None;
    }
    let mut current = text.to_string();
    let mut found = false;
    for _ in 0..MAX_SECRET_DECODE_PASSES {
        if contains_secret_value(&current, secret_values) {
            found = true;
            current = mask_secret_values(&current, secret_values);
        }
        // Borrowed means nothing was decoded, so further passes cannot change it.
        match percent_encoding::percent_decode_str(&current).decode_utf8_lossy() {
            std::borrow::Cow::Borrowed(_) => break,
            std::borrow::Cow::Owned(decoded) => current = decoded,
        }
    }
    if contains_secret_value(&current, secret_values) {
        found = true;
        current = mask_secret_values(&current, secret_values);
    }
    found.then_some(current)
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
            return Err(undeclared_secret(name));
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
            for tag in transform::liquid_tag_bodies(span) {
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

/// An authentication policy resolved into the Authorization field value it
/// sends. Deliberately not `Debug`: both fields hold credentials, which must
/// never reach logs.
pub(crate) struct ResolvedAuthorization {
    pub value: String,
    /// Credential values carried by `value` (a bearer token; a basic username,
    /// password and encoded `user:password`), to be masked in any text a
    /// service may echo them into (see [`merge_secret_values`]).
    pub credentials: Vec<String>,
}

/// Convert an authentication policy into an Authorization field value,
/// resolving any `{ use: <name> }` secret reference.
pub(crate) fn resolve_authorization(
    policy: &workflow::AuthenticationPolicy,
    declared_secrets: &std::collections::HashSet<String>,
    getter: &impl Fn(&str) -> Option<String>,
) -> Result<ResolvedAuthorization> {
    match policy {
        workflow::AuthenticationPolicy::Bearer(bearer) => {
            let token = bearer_token(bearer, declared_secrets, getter)?;
            Ok(ResolvedAuthorization {
                value: format!("Bearer {token}"),
                credentials: vec![token],
            })
        }
        workflow::AuthenticationPolicy::Basic(basic) => {
            let (username, password) = basic_credentials(basic, declared_secrets, getter)?;
            let encoded = basic_credential(&username, &password);
            // The username is collected as sent: an expression may derive it
            // from a secret, and the original secret value would not match it.
            Ok(ResolvedAuthorization {
                value: format!("Basic {encoded}"),
                credentials: vec![encoded, password, username],
            })
        }
    }
}

/// The Basic authentication credential `base64(username:password)` (RFC 7617).
pub(crate) fn basic_credential(username: &str, password: &str) -> String {
    BASE64_STANDARD.encode(format!("{username}:{password}"))
}

/// Extract a raw bearer token (without the `Bearer ` prefix) from a policy,
/// resolving any `{ use: <name> }` secret reference, for callers that need the
/// bare token rather than a full `Authorization` header value.
///
/// Basic authentication is rejected: a single bare token cannot represent
/// `username:password` credentials. The value must not be logged.
pub(crate) fn bearer_token_value(
    policy: &workflow::AuthenticationPolicy,
    declared_secrets: &std::collections::HashSet<String>,
    getter: &impl Fn(&str) -> Option<String>,
) -> Result<String> {
    match policy {
        workflow::AuthenticationPolicy::Bearer(bearer) => {
            bearer_token(bearer, declared_secrets, getter)
        }
        workflow::AuthenticationPolicy::Basic(_) => Err(anyhow!(
            "basic authentication is not supported for gRPC; use bearer"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

        let value = resolve_authorization(&policy, &declared(&[]), &|_| None)
            .unwrap()
            .value;

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

        let value = resolve_authorization(&policy, &declared(&[]), &|_| None)
            .unwrap()
            .value;

        assert_eq!(value, "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==");
    }

    #[test]
    fn bearer_token_value_returns_bare_token_without_prefix() {
        // The gRPC runner prepends `Bearer ` itself, so the adapter must supply
        // the bare token (no `Bearer ` prefix) to avoid double-prefixing.
        let policy = workflow::AuthenticationPolicy::Bearer(
            workflow::BearerAuthentication::BearerAuthenticationProperties {
                token: "abc123".to_string(),
            },
        );

        let value = bearer_token_value(&policy, &declared(&[]), &|_| None).unwrap();

        assert_eq!(value, "abc123");
    }

    #[test]
    fn bearer_token_value_resolves_use_secret_from_env() {
        let policy = workflow::AuthenticationPolicy::Bearer(
            workflow::BearerAuthentication::SecretBasedAuthenticationPolicy(
                workflow::SecretBasedAuthenticationPolicy {
                    use_: "API_TOKEN".parse().unwrap(),
                },
            ),
        );

        let value = bearer_token_value(&policy, &declared(&["API_TOKEN"]), &|k| {
            (k == "WORKFLOW_SECRET_API_TOKEN").then(|| "env-token".to_string())
        })
        .unwrap();

        assert_eq!(value, "env-token");
    }

    #[test]
    fn bearer_token_value_rejects_basic_policy() {
        let policy = workflow::AuthenticationPolicy::Basic(
            workflow::BasicAuthentication::BasicAuthenticationProperties {
                username: "admin".to_string(),
                password: "s3cret".to_string(),
            },
        );

        let err = bearer_token_value(&policy, &declared(&[]), &|_| None).unwrap_err();
        assert!(err.to_string().contains("basic"));
        // The password must never leak into the error.
        assert!(!err.to_string().contains("s3cret"));
    }

    #[test]
    fn secret_values_collects_non_empty_string_leaves_longest_first() {
        let secrets = json!({
            "API_TOKEN": "tok",
            "CREDS": {"username": "admin", "password": "p4ssword"},
            "EMPTY": "",
        });
        assert_eq!(
            secret_values(&secrets),
            vec![
                "p4ssword".to_string(),
                "admin".to_string(),
                "tok".to_string()
            ]
        );
    }

    #[test]
    fn mask_encoded_secret_values_masks_secret_containing_percent_sequence() {
        // A raw secret that itself looks percent-encoded must be masked before
        // decoding changes it.
        let secrets = vec!["a%41b".to_string()];
        let masked = mask_encoded_secret_values("x=a%41b", &secrets).unwrap();
        assert!(
            !masked.contains("a%41b") && !masked.contains("aAb"),
            "{masked}"
        );
    }

    #[test]
    fn mask_encoded_secret_values_without_secrets_keeps_text() {
        assert_eq!(mask_encoded_secret_values("a%2Fb", &[]), None);
        let secrets = vec!["other".to_string()];
        assert_eq!(mask_encoded_secret_values("a%2Fb", &secrets), None);
    }

    fn bearer_use(name: &str) -> workflow::AuthenticationPolicy {
        workflow::AuthenticationPolicy::Bearer(
            workflow::BearerAuthentication::SecretBasedAuthenticationPolicy(
                workflow::SecretBasedAuthenticationPolicy {
                    use_: name.parse().unwrap(),
                },
            ),
        )
    }

    #[test]
    fn resolve_authorization_reports_the_resolved_bearer_token() {
        let getter = |k: &str| (k == "WORKFLOW_SECRET_API_TOKEN").then(|| "env-token".to_string());
        let resolved =
            resolve_authorization(&bearer_use("API_TOKEN"), &declared(&["API_TOKEN"]), &getter)
                .unwrap();
        assert_eq!(resolved.value, "Bearer env-token");
        assert_eq!(resolved.credentials, vec!["env-token".to_string()]);
    }

    #[test]
    fn resolve_authorization_reports_basic_credentials_as_sent() {
        let policy = workflow::AuthenticationPolicy::Basic(
            workflow::BasicAuthentication::BasicAuthenticationProperties {
                username: "Aladdin".to_string(),
                password: "open sesame".to_string(),
            },
        );
        let resolved = resolve_authorization(&policy, &declared(&[]), &|_: &str| None).unwrap();
        assert_eq!(resolved.value, "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==");
        assert_eq!(
            resolved.credentials,
            vec![
                "QWxhZGRpbjpvcGVuIHNlc2FtZQ==".to_string(),
                "open sesame".to_string(),
                "Aladdin".to_string()
            ]
        );
    }

    #[test]
    fn resolve_authorization_reports_username_from_basic_secret_reference() {
        let policy = workflow::AuthenticationPolicy::Basic(
            workflow::BasicAuthentication::SecretBasedAuthenticationPolicy(
                workflow::SecretBasedAuthenticationPolicy {
                    use_: "CREDS".parse().unwrap(),
                },
            ),
        );
        let getter = |k: &str| {
            (k == "WORKFLOW_SECRET_CREDS")
                .then(|| r#"{"username": "svc-user", "password": "pw"}"#.to_string())
        };
        let credentials = resolve_authorization(&policy, &declared(&["CREDS"]), &getter)
            .unwrap()
            .credentials;
        // A username read from a secret is part of that secret.
        assert!(
            credentials.contains(&"svc-user".to_string()),
            "{credentials:?}"
        );
        assert!(credentials.contains(&"pw".to_string()), "{credentials:?}");
    }

    #[test]
    fn merge_secret_values_merges_longest_first_without_duplicates_or_empties() {
        let merged = merge_secret_values(
            &["tok".to_string(), "p4ssword".to_string()],
            vec![
                "tok".to_string(),
                "a-longer-token".to_string(),
                String::new(),
            ],
        );
        assert_eq!(
            merged,
            vec![
                "a-longer-token".to_string(),
                "p4ssword".to_string(),
                "tok".to_string()
            ]
        );
    }

    #[test]
    fn secret_values_of_no_secrets_is_empty() {
        assert!(secret_values(&json!({})).is_empty());
        assert!(secret_values(&Value::Null).is_empty());
    }
}
