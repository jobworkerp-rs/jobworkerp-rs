//! Masking of credentials and secret values in logs, shared by the job layer
//! and the workflow engine: job arguments by credential key name, the
//! `_secrets` of a WORKFLOW job's context, and the secret values of the
//! enclosing workflow run. Also the credential header names that the workflow
//! engine masks in `call.http` task output.

use infra::workflow::definition::expression_context::CONTEXT_SECRETS_VARIABLE;
use serde_json::{Map, Value};
use std::sync::Arc;

/// Placeholder substituted for redacted sensitive values in logs.
pub const REDACTED: &str = "***REDACTED***";

tokio::task_local! {
    /// Secret values of the workflow run enqueueing jobs, longest first. Key
    /// names cannot catch them: a workflow passes a token as e.g. a COMMAND
    /// `stdin`.
    static LOG_SECRET_VALUES: Arc<[String]>;
}

/// Run `future` with `secret_values` (longest first) masked in the job
/// argument logs it writes. Only `future` itself sees them, not tasks it
/// spawns, and a job enqueued outside such a scope gets no value masking: the
/// workflow engine must wrap every call that converts child job arguments.
pub async fn with_log_secret_values<F: std::future::Future>(
    secret_values: Arc<[String]>,
    future: F,
) -> F::Output {
    LOG_SECRET_VALUES.scope(secret_values, future).await
}

/// The secret values of the enclosing [`with_log_secret_values`], if any.
fn scoped_log_secret_values() -> Arc<[String]> {
    LOG_SECRET_VALUES
        .try_with(Arc::clone)
        .unwrap_or_else(|_| Arc::from([]))
}

/// Non-empty `values` without duplicates, longest first so that masking one
/// never leaves part of a longer one exposed.
pub fn longest_first(mut values: Vec<String>) -> Vec<String> {
    values.retain(|value| !value.is_empty());
    values.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    values.dedup();
    values
}

/// Every distinct non-empty string leaf of `value`, longest first.
pub fn secret_strings(value: &Value) -> Vec<String> {
    fn collect(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::String(s) => out.push(s.clone()),
            Value::Object(map) => map.values().for_each(|v| collect(v, out)),
            Value::Array(values) => values.iter().for_each(|v| collect(v, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    collect(value, &mut out);
    longest_first(out)
}

/// Log secret values shorter than this many characters are masked only where a
/// whole string equals them: as substrings they would match inside almost every
/// log string and make the log unreadable.
const MIN_SUBSTRING_LOG_SECRET_CHARS: usize = 4;

/// `text` with every occurrence of `secret_values` (longest first) replaced by
/// `placeholder`.
pub fn mask_secret_text<'a>(
    text: &str,
    secret_values: impl IntoIterator<Item = &'a String>,
    placeholder: &str,
) -> String {
    secret_values
        .into_iter()
        .filter(|secret| text.contains(secret.as_str()))
        .fold(text.to_string(), |text, secret| {
            text.replace(secret.as_str(), placeholder)
        })
}

/// `text` for a log: [`REDACTED`] when it equals a secret value, otherwise with
/// the occurrences of the values of at least [`MIN_SUBSTRING_LOG_SECRET_CHARS`]
/// characters (longest first) replaced by [`REDACTED`].
pub fn mask_log_text(text: &str, secret_values: &[String]) -> String {
    if secret_values.iter().any(|secret| secret == text) {
        return REDACTED.to_string();
    }
    let substring_values = secret_values
        .iter()
        .filter(|secret| secret.chars().count() >= MIN_SUBSTRING_LOG_SECRET_CHARS);
    mask_secret_text(text, substring_values, REDACTED)
}

/// A copy of `value` with `f` applied to every string leaf. Keys and other
/// leaves are kept.
fn map_string_leaves(value: &Value, f: &impl Fn(&str) -> String) -> Value {
    match value {
        Value::String(text) => Value::String(f(text)),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), map_string_leaves(v, f)))
                .collect(),
        ),
        Value::Array(items) => {
            Value::Array(items.iter().map(|v| map_string_leaves(v, f)).collect())
        }
        other => other.clone(),
    }
}

/// A copy of `value` whose string leaves have `secret_values` replaced by
/// [`REDACTED`]. A value shorter than 4 characters is masked only where a whole
/// string leaf equals it.
pub fn mask_secret_strings(value: &Value, secret_values: &[String]) -> Value {
    map_string_leaves(value, &|text| mask_log_text(text, secret_values))
}

/// Header names whose values carry credentials and must never be logged or
/// echoed into task output.
/// Matched case-insensitively against the `key` of `{key, value}` pairs (the
/// shape the HTTP_REQUEST runner uses for headers/queries). This is a generic
/// HTTP-credential rule, not tied to any single runner type.
const SENSITIVE_HEADER_KEYS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
];

/// Whether `key` names a credential-bearing header (`Authorization`,
/// `Proxy-Authorization`, `Cookie`, `Set-Cookie`), case-insensitively.
pub fn is_sensitive_header_key(key: &str) -> bool {
    SENSITIVE_HEADER_KEYS
        .iter()
        .any(|name| key.eq_ignore_ascii_case(name))
}

/// Object field names whose string value carries a credential and must never be
/// logged. Matched case-insensitively against object keys (the flat shape used
/// by runner settings, e.g. the GRPC runner's `auth_token` resolved from a
/// `WORKFLOW_SECRET_*`). Only string values are masked so that structural keys
/// of the same name are left intact.
const SENSITIVE_FLAT_KEYS: &[&str] = &[
    "auth_token",
    "token",
    "access_token",
    "refresh_token",
    "password",
    "secret",
    "api_key",
    "apikey",
];

/// Return a copy of `args` with credential-bearing values masked, for safe
/// logging. Redacted are `{key, value}` pairs whose `key` is a sensitive
/// header name (e.g. a transformed `call.http`'s `Authorization` header derived
/// from a `WORKFLOW_SECRET_*`), flat object fields whose key is a sensitive
/// credential name with a string value (e.g. the GRPC runner's `auth_token`),
/// the `_secrets` of a WORKFLOW job's `workflow_context`, and the secret values
/// of an enclosing [`with_log_secret_values`].
/// All other content is preserved so the log stays useful.
pub(crate) fn redact_sensitive_args(args: &Value) -> Value {
    redact_sensitive_keys(args, &scoped_log_secret_values())
}

/// The `workflow_context` JSON string of WORKFLOW job args with every string
/// under its `_secrets` redacted, or `None` when it holds no `_secrets`.
fn redact_workflow_context(context: &str) -> Option<String> {
    let mut context: Map<String, Value> = serde_json::from_str(context).ok()?;
    let secrets = context.get_mut(CONTEXT_SECRETS_VARIABLE)?;
    *secrets = map_string_leaves(secrets, &|_| REDACTED.to_string());
    serde_json::to_string(&context).ok()
}

fn redact_sensitive_keys(args: &Value, secret_values: &[String]) -> Value {
    match args {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    // Mask a flat credential field (string value only, so that a
                    // structural key of the same name is not corrupted).
                    if matches!(v, Value::String(_))
                        && SENSITIVE_FLAT_KEYS
                            .iter()
                            .any(|s| k.eq_ignore_ascii_case(s))
                    {
                        (k.clone(), Value::String(REDACTED.to_string()))
                    } else if let Value::String(context) = v
                        && k == "workflow_context"
                        && let Some(redacted) = redact_workflow_context(context)
                    {
                        (k.clone(), Value::String(redacted))
                    } else {
                        (k.clone(), redact_sensitive_keys(v, secret_values))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| redact_key_value_or_recurse(item, secret_values))
                .collect(),
        ),
        Value::String(text) => Value::String(mask_log_text(text, secret_values)),
        other => other.clone(),
    }
}

/// For array elements: if the element is a `{key, value}` pair with a sensitive
/// `key`, replace its `value` with the placeholder; otherwise recurse.
fn redact_key_value_or_recurse(item: &Value, secret_values: &[String]) -> Value {
    if let Value::Object(obj) = item
        && let Some(Value::String(key)) = obj.get("key")
        && obj.contains_key("value")
        && is_sensitive_header_key(key)
    {
        let mut redacted = obj.clone();
        redacted.insert("value".to_string(), Value::String(REDACTED.to_string()));
        return Value::Object(redacted);
    }
    redact_sensitive_keys(item, secret_values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mask_secret_strings_replaces_values_inside_string_leaves_only() {
        let value = json!({
            "stdin": "tok-123",
            "args": ["--header", "Authorization: token tok-123"],
            "tok-123": 7,
        });
        let masked = mask_secret_strings(&value, &["tok-123".to_string()]);
        assert_eq!(
            masked,
            json!({
                "stdin": REDACTED,
                "args": ["--header", format!("Authorization: token {REDACTED}")],
                "tok-123": 7,
            })
        );
    }

    #[test]
    fn short_secret_values_are_masked_only_as_whole_strings() {
        let secret_values = longest_first(vec!["a".to_string(), "tok-123".to_string()]);
        let value = json!({
            "stdin": "a",
            "command": "cat",
            "args": ["--token", "x tok-123 a"],
        });
        assert_eq!(
            mask_secret_strings(&value, &secret_values),
            json!({
                "stdin": REDACTED,
                "command": "cat",
                "args": ["--token", format!("x {REDACTED} a")],
            })
        );
    }

    #[test]
    fn the_substring_limit_counts_characters_not_bytes() {
        let secret_values = longest_first(vec!["秘密の鍵".to_string(), "秘密鍵".to_string()]);
        assert_eq!(
            mask_secret_strings(&json!(["x秘密の鍵x", "x秘密鍵x"]), &secret_values),
            json!([format!("x{REDACTED}x"), "x秘密鍵x"])
        );
    }

    #[test]
    fn secret_strings_are_distinct_non_empty_leaves_longest_first() {
        let value = json!({"a": "tok", "b": ["longer-tok", "tok", ""], "n": 1});
        assert_eq!(
            secret_strings(&value),
            vec!["longer-tok".to_string(), "tok".to_string()]
        );
    }

    #[test]
    fn masking_a_longer_secret_first_leaves_no_part_of_it() {
        let values = longest_first(vec!["tok".to_string(), "tok-long".to_string()]);
        assert_eq!(mask_secret_text("x tok-long tok", &values, "*"), "x * *");
    }

    #[test]
    fn redact_masks_secrets_in_a_workflow_context_string() {
        // WORKFLOW job args carry the context as a JSON string.
        let context = json!({
            "_secrets": {"token": "ctx-s3cret", "nested": {"key": "k-s3cret"}},
            "repo": "acme/widget",
        });
        let args = json!({
            "input": "{}",
            "workflow_context": context.to_string(),
        });
        let redacted = redact_sensitive_args(&args);
        let redacted_context: Value =
            serde_json::from_str(redacted["workflow_context"].as_str().unwrap()).unwrap();
        assert_eq!(redacted_context["_secrets"]["token"], REDACTED);
        assert_eq!(redacted_context["_secrets"]["nested"]["key"], REDACTED);
        assert_eq!(redacted_context["repo"], "acme/widget");
        assert!(!redacted.to_string().contains("s3cret"));
    }

    #[test]
    fn redact_leaves_other_workflow_contexts_unchanged() {
        for context in [r#"{"repo":"acme/widget"}"#, "not json", r#"["_secrets"]"#] {
            let args = json!({"workflow_context": context});
            assert_eq!(redact_sensitive_args(&args), args, "{context}");
        }
    }

    #[tokio::test]
    async fn redact_masks_log_secret_values_only_inside_their_scope() {
        // Key names cannot tell that a COMMAND stdin carries a token.
        let args = json!({"command": "git", "stdin": "tok-123"});
        let secret_values = Arc::from(vec!["tok-123".to_string()]);
        let scoped =
            with_log_secret_values(secret_values, async { redact_sensitive_args(&args) }).await;
        assert_eq!(scoped["stdin"], REDACTED);
        assert_eq!(scoped["command"], "git");
        assert_eq!(redact_sensitive_args(&args), args);
    }

    #[tokio::test]
    async fn redact_masks_short_log_secret_values_only_as_whole_strings() {
        let args = json!({"command": "git", "stdin": "1", "args": ["log", "-n", "10"]});
        let secret_values = Arc::from(vec!["1".to_string()]);
        let scoped =
            with_log_secret_values(secret_values, async { redact_sensitive_args(&args) }).await;
        assert_eq!(scoped["stdin"], REDACTED);
        assert_eq!(scoped["args"], json!(["log", "-n", "10"]));
    }

    #[test]
    fn redact_masks_authorization_header_value() {
        let args = json!({
            "method": "GET",
            "headers": [
                {"key": "Authorization", "value": "Bearer s3cret-token"},
                {"key": "Content-Type", "value": "application/json"},
            ],
        });
        let redacted = redact_sensitive_args(&args);
        let headers = redacted["headers"].as_array().unwrap();
        let auth = headers
            .iter()
            .find(|h| h["key"] == "Authorization")
            .unwrap();
        assert_eq!(auth["value"], REDACTED);
        // The secret must not survive anywhere in the redacted output.
        assert!(!redacted.to_string().contains("s3cret-token"));
        // Non-sensitive headers are preserved verbatim.
        let ct = headers.iter().find(|h| h["key"] == "Content-Type").unwrap();
        assert_eq!(ct["value"], "application/json");
    }

    #[test]
    fn redact_matches_header_key_case_insensitively() {
        let args = json!({
            "headers": [{"key": "authorization", "value": "Basic dXNlcjpwYXNz"}],
        });
        let redacted = redact_sensitive_args(&args);
        assert_eq!(redacted["headers"][0]["value"], REDACTED);
        assert!(!redacted.to_string().contains("dXNlcjpwYXNz"));
    }

    #[test]
    fn redact_preserves_non_sensitive_args() {
        // A plain value resembling a header key but not in {key,value} form must
        // be left untouched.
        let args = json!({
            "queries": [{"key": "q", "value": "authorization"}],
            "body": "authorization details",
        });
        let redacted = redact_sensitive_args(&args);
        assert_eq!(redacted, args);
    }

    #[test]
    fn redact_handles_nested_structures() {
        let args = json!({
            "outer": {
                "headers": [{"key": "Cookie", "value": "session=abc"}],
            },
        });
        let redacted = redact_sensitive_args(&args);
        assert_eq!(redacted["outer"]["headers"][0]["value"], REDACTED);
        assert!(!redacted.to_string().contains("session=abc"));
    }

    #[test]
    fn redact_masks_flat_auth_token_in_runner_settings() {
        // Shape of GRPC runner settings with a bearer secret resolved into a flat
        // `auth_token` field.
        let settings = json!({
            "host": "example.com",
            "port": 50051,
            "use_reflection": true,
            "auth_token": "Bearer-or-bare-s3cret",
        });
        let redacted = redact_sensitive_args(&settings);
        assert_eq!(redacted["auth_token"], REDACTED);
        assert!(!redacted.to_string().contains("s3cret"));
        // Non-sensitive fields are preserved.
        assert_eq!(redacted["host"], "example.com");
        assert_eq!(redacted["port"], 50051);
        assert_eq!(redacted["use_reflection"], true);
    }

    #[test]
    fn redact_masks_flat_credential_keys_case_insensitively() {
        let settings = json!({
            "Password": "p@ss",
            "API_KEY": "ak-123",
            "nested": {"access_token": "at-456"},
        });
        let redacted = redact_sensitive_args(&settings);
        assert_eq!(redacted["Password"], REDACTED);
        assert_eq!(redacted["API_KEY"], REDACTED);
        assert_eq!(redacted["nested"]["access_token"], REDACTED);
        for leaked in ["p@ss", "ak-123", "at-456"] {
            assert!(!redacted.to_string().contains(leaked));
        }
    }

    #[test]
    fn redact_does_not_corrupt_non_string_credential_keys() {
        // A key named like a credential but holding structural (non-string) data
        // must be recursed into, not blindly masked, so its contents remain
        // inspectable while any nested credential strings are still masked.
        let args = json!({
            "token": {"id": 7, "value": "nested-secret", "auth_token": "deep-s3cret"},
        });
        let redacted = redact_sensitive_args(&args);
        // The object value is preserved as an object (not replaced by a string).
        assert!(redacted["token"].is_object());
        assert_eq!(redacted["token"]["id"], 7);
        // A nested flat credential field is still masked.
        assert_eq!(redacted["token"]["auth_token"], REDACTED);
        assert!(!redacted.to_string().contains("deep-s3cret"));
    }
}
