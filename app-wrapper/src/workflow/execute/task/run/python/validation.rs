use anyhow::{anyhow, Context, Result};
use once_cell::sync::Lazy;
use regex::Regex;
use std::time::Duration;

// Compile regex patterns once at startup for performance
static DANGEROUS_FUNC_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(eval|exec|compile|__import__|open|input|execfile)\s*\(")
        .expect("Invalid regex pattern for dangerous functions")
});

static SHELL_COMMAND_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(os\.system|subprocess\.|commands\.|popen)")
        .expect("Invalid regex pattern for shell commands")
});

static DUNDER_ACCESS_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"__[a-zA-Z_]+__").expect("Invalid regex pattern for dunder attributes")
});

/// Maximum recursion depth for nested JSON validation (= max json nest depth)
pub const MAX_RECURSIVE_DEPTH: usize = 21;

/// Python keyword constants
const PYTHON_KEYWORDS: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "False", "finally", "for", "from", "global", "if", "import", "in", "is",
    "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return", "True", "try", "while",
    "with", "yield",
];

/// Safe dunder attribute patterns
const SAFE_DUNDERS: &[&str] = &["__name__", "__doc__", "__version__", "__file__"];

/// Maximum script size (1MB)
const MAX_SCRIPT_SIZE: usize = 1024 * 1024;

/// Download timeout for external scripts
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);

/// Validate Python variable name against keywords and identifier rules
///
/// This function ensures that variable names can be safely used as Python identifiers
/// when injected into the generated script. It prevents:
/// - Invalid syntax (starting with digit, special characters)
/// - Python keywords (if, for, def, etc.)
/// - Dunder attributes (__init__, __class__, etc.) which could override special methods
///
/// Note: Single or double leading/trailing underscores (_var, var_, __var) are allowed
/// as they are common Python conventions, but dunder attributes (__*__) are forbidden.
pub fn is_valid_python_identifier(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }

    // Must not start with digit
    if s.chars().next().unwrap().is_numeric() {
        return false;
    }

    // Must be alphanumeric or underscore
    if !s.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return false;
    }

    // Must not be Python keyword
    if PYTHON_KEYWORDS.contains(&s) {
        return false;
    }

    // Must not be dunder attribute (__*__) to prevent overriding special methods
    // Examples: __init__, __class__, __name__, __file__, etc.
    // Single/double underscores are allowed: _private, __private, private_
    if s.starts_with("__") && s.ends_with("__") && s.len() > 4 {
        return false;
    }

    true
}

/// Sanitize arguments to prevent code injection with enhanced security validation
///
/// # Security Model
///
/// This function validates variable names and nested object keys for Python syntax compliance.
/// **Value content validation is NOT performed** because all values are Base64-encoded before
/// injection into Python scripts (see script.rs:164-166).
///
/// Base64 encoding ensures that any string content, including code-like patterns such as
/// `eval()`, `__import__`, or shell commands, is treated as pure data and cannot be executed.
/// This allows safe handling of legitimate data like Python documentation, code examples,
/// or any text containing these patterns.
///
/// ## Why no value validation?
///
/// The current implementation uses Base64 encoding for argument injection:
/// ```python
/// import json
/// import base64
/// variable = json.loads(base64.b64decode('base64_encoded_value').decode('utf-8'))
/// ```
///
/// This approach provides complete protection against code injection attacks, making
/// content-based validation unnecessary and preventing false positives on legitimate data.
pub fn sanitize_python_variable(key: &str, value: &serde_json::Value) -> Result<()> {
    if !is_valid_python_identifier(key) {
        return Err(anyhow!(
            "Invalid Python variable name: '{}'. Must be alphanumeric with underscores only.",
            key
        ));
    }

    // Only validate nested object keys for Python identifier rules
    // Value content is NOT validated because Base64 encoding provides complete protection
    validate_keys_recursive(value, 0)?;

    Ok(())
}

/// Validate only the keys in nested objects (for Python identifier rules)
///
/// This function recursively validates object keys to ensure they can be used as
/// Python identifiers. String values are NOT validated because Base64 encoding
/// provides complete protection against code injection.
fn validate_keys_recursive(value: &serde_json::Value, depth: usize) -> Result<()> {
    if depth > MAX_RECURSIVE_DEPTH {
        return Err(anyhow!("Maximum nesting depth exceeded"));
    }

    match value {
        serde_json::Value::Object(obj) => {
            for (k, v) in obj {
                if !is_valid_python_identifier(k) {
                    return Err(anyhow!(
                        "Invalid Python identifier in nested object key: '{}'",
                        k
                    ));
                }
                validate_keys_recursive(v, depth + 1)?;
            }
            Ok(())
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                validate_keys_recursive(item, depth + 1)?;
            }
            Ok(())
        }
        _ => Ok(()), // Primitives and strings are not validated
    }
}

/// Recursively validate JSON values for security threats
///
/// # DEPRECATED - This function is no longer used
///
/// This function is kept for backward compatibility but is NOT called in the current
/// implementation. Value content validation is unnecessary because Base64 encoding
/// provides complete protection against code injection attacks.
#[allow(dead_code)]
pub fn validate_value_recursive(value: &serde_json::Value, depth: usize) -> Result<()> {
    if depth > MAX_RECURSIVE_DEPTH {
        return Err(anyhow!("Maximum nesting depth exceeded"));
    }

    match value {
        serde_json::Value::String(s) => validate_string_content(s),
        serde_json::Value::Array(arr) => {
            for item in arr {
                validate_value_recursive(item, depth + 1)?;
            }
            Ok(())
        }
        serde_json::Value::Object(obj) => {
            for (k, v) in obj {
                if !is_valid_python_identifier(k) {
                    return Err(anyhow!(
                        "Invalid Python identifier in nested object key: '{}'",
                        k
                    ));
                }
                validate_value_recursive(v, depth + 1)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Validate string content for dangerous patterns using regex
///
/// # DEPRECATED - This function is no longer used
///
/// This function is kept for backward compatibility but is NOT called in the current
/// implementation. String content validation is unnecessary because Base64 encoding
/// provides complete protection against code injection attacks.
///
/// Base64 encoding ensures that strings containing patterns like `eval()`, `__import__`,
/// or shell commands are treated as pure data and cannot be executed. This allows
/// legitimate use cases such as:
/// - Python documentation with code examples
/// - Log messages mentioning dangerous functions
/// - Text containing dunder attributes for educational purposes
#[allow(dead_code)]
pub fn validate_string_content(s: &str) -> Result<()> {
    // 1. Dangerous function calls (eval, exec, etc.) with flexible whitespace
    if DANGEROUS_FUNC_REGEX.is_match(s) {
        return Err(anyhow!(
            "Dangerous function call detected in argument value: matches pattern for eval/exec/compile/__import__/open/input/execfile"
        ));
    }

    // 2. Dunder attribute access (excluding safe common ones)
    if let Some(matched) = DUNDER_ACCESS_REGEX.find(s) {
        let dunder = matched.as_str();
        // Allow common safe patterns
        if !SAFE_DUNDERS.contains(&dunder) {
            return Err(anyhow!(
                "Dunder attribute access not allowed in argument value: {}",
                dunder
            ));
        }
    }

    // 3. Shell command execution patterns
    if SHELL_COMMAND_REGEX.is_match(s) {
        return Err(anyhow!(
            "Shell command execution pattern detected in argument value: matches os.system/subprocess/commands/popen"
        ));
    }

    Ok(())
}

/// Download script from external URL with comprehensive security validation
pub async fn download_script_secure(uri: &str) -> Result<String> {
    // 1. URL schema validation
    let url = reqwest::Url::parse(uri).context("Invalid URL format")?;

    if url.scheme() != "https" {
        return Err(anyhow!(
            "Only HTTPS URLs are allowed for external scripts (got: {})",
            url.scheme()
        ));
    }

    // 2. Download with size limit and timeout
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(false) // Explicitly enable TLS verification
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .context("Failed to build HTTP client")?;

    let response = client
        .get(uri)
        .send()
        .await
        .context(format!("Failed to download script from: {}", uri))?;

    if !response.status().is_success() {
        return Err(anyhow!(
            "Failed to download script: HTTP {} from {}",
            response.status(),
            uri
        ));
    }

    // 3. Content-Type validation (optional but recommended)
    if let Some(content_type) = response.headers().get("content-type") {
        let ct_str = content_type
            .to_str()
            .context("Invalid Content-Type header")?;

        if !ct_str.starts_with("text/") && !ct_str.contains("python") && !ct_str.contains("plain") {
            tracing::warn!(
                "Unexpected Content-Type for script: {} (expected text/* or application/x-python)",
                ct_str
            );
        }
    }

    // 4. Stream download with size limit
    let bytes = response
        .bytes()
        .await
        .context("Failed to read response body")?;

    if bytes.len() > MAX_SCRIPT_SIZE {
        return Err(anyhow!(
            "Script size exceeds limit: {} bytes (max: {} bytes)",
            bytes.len(),
            MAX_SCRIPT_SIZE
        ));
    }

    let content = String::from_utf8(bytes.to_vec()).context("Script contains invalid UTF-8")?;

    tracing::info!(
        "Downloaded external script from {} ({} bytes)",
        uri,
        content.len()
    );

    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========== Tests for is_valid_python_identifier ==========

    #[test]
    fn test_is_valid_python_identifier_valid() {
        assert!(is_valid_python_identifier("valid_var"));
        assert!(is_valid_python_identifier("_private"));
        assert!(is_valid_python_identifier("camelCase123"));
        assert!(is_valid_python_identifier("snake_case_var"));
        assert!(is_valid_python_identifier("a"));
        assert!(is_valid_python_identifier("_"));
    }

    #[test]
    fn test_is_valid_python_identifier_invalid_start_with_digit() {
        assert!(!is_valid_python_identifier("123var"));
        assert!(!is_valid_python_identifier("0test"));
    }

    #[test]
    fn test_is_valid_python_identifier_invalid_empty() {
        assert!(!is_valid_python_identifier(""));
    }

    #[test]
    fn test_is_valid_python_identifier_invalid_special_chars() {
        assert!(!is_valid_python_identifier("var-name"));
        assert!(!is_valid_python_identifier("var.name"));
        assert!(!is_valid_python_identifier("var name"));
        assert!(!is_valid_python_identifier("var@name"));
    }

    #[test]
    fn test_is_valid_python_identifier_keywords() {
        assert!(!is_valid_python_identifier("if"));
        assert!(!is_valid_python_identifier("for"));
        assert!(!is_valid_python_identifier("def"));
        assert!(!is_valid_python_identifier("class"));
        assert!(!is_valid_python_identifier("import"));
        assert!(!is_valid_python_identifier("None"));
        assert!(!is_valid_python_identifier("True"));
        assert!(!is_valid_python_identifier("False"));
    }

    #[test]
    fn test_is_valid_python_identifier_dunder_attributes() {
        // Dunder attributes should be rejected
        assert!(!is_valid_python_identifier("__init__"));
        assert!(!is_valid_python_identifier("__class__"));
        assert!(!is_valid_python_identifier("__name__"));
        assert!(!is_valid_python_identifier("__file__"));
        assert!(!is_valid_python_identifier("__dict__"));
        assert!(!is_valid_python_identifier("__doc__"));
        assert!(!is_valid_python_identifier("__version__"));
        assert!(!is_valid_python_identifier("__main__"));
        assert!(!is_valid_python_identifier("__builtins__"));
        assert!(!is_valid_python_identifier("__import__"));
        assert!(!is_valid_python_identifier("__debug__"));
    }

    #[test]
    fn test_is_valid_python_identifier_underscores_allowed() {
        // Single and double leading/trailing underscores are allowed (Python conventions)
        assert!(is_valid_python_identifier("_private"));
        assert!(is_valid_python_identifier("__private"));
        assert!(is_valid_python_identifier("private_"));
        assert!(is_valid_python_identifier("_"));
        assert!(is_valid_python_identifier("__"));
        assert!(is_valid_python_identifier("___"));
        assert!(is_valid_python_identifier("____")); // Four underscores (not dunder pattern)

        // But dunder pattern (__*__) with content should be rejected
        assert!(!is_valid_python_identifier("__x__")); // Shortest dunder (len=5)
    }

    #[test]
    fn test_is_valid_python_identifier_edge_cases() {
        // Edge cases for dunder check
        assert!(is_valid_python_identifier("__")); // Only two underscores (len=2)
        assert!(is_valid_python_identifier("___")); // Only three underscores (len=3)
        assert!(is_valid_python_identifier("____")); // Four underscores (len=4)
        assert!(!is_valid_python_identifier("__a__")); // Dunder pattern (len=5)
        assert!(!is_valid_python_identifier("__ab__")); // Dunder pattern (len=6)

        // Not dunder pattern
        assert!(is_valid_python_identifier("__private_var")); // Leading __ only
        assert!(is_valid_python_identifier("public_var__")); // Trailing __ only
    }

    // ========== Tests for sanitize_python_variable (Base64-protected) ==========
    //
    // NOTE: Value content validation is NO LONGER performed because Base64 encoding
    // provides complete protection. These tests verify that legitimate data containing
    // code-like patterns is accepted.

    #[test]
    fn test_sanitize_python_variable_with_code_patterns_should_pass() {
        // Strings containing dangerous patterns should be accepted because
        // they will be Base64-encoded and cannot be executed
        let test_cases = vec![
            ("eval(code)", "eval function call"),
            ("exec(command)", "exec function call"),
            ("__import__('os')", "__import__ pattern"),
            ("os.system('ls')", "shell command pattern"),
            ("__class__", "dunder attribute"),
        ];

        for (value_str, description) in test_cases {
            let value = serde_json::json!(value_str);
            let result = sanitize_python_variable("var", &value);
            assert!(
                result.is_ok(),
                "{} should be accepted (Base64-protected): {:?}",
                description,
                result
            );
        }
    }

    #[test]
    fn test_sanitize_python_variable_with_article_containing_code_examples() {
        // Real-world scenario: Python documentation with code examples
        let article = r#"
# Python Security Best Practices

## Avoid using eval()

Never use eval(user_input) in production code as it can execute arbitrary code.
Instead, use safer alternatives like ast.literal_eval().

Bad example:
```python
result = eval("__import__('os').system('rm -rf /')")
```

Good example:
```python
import ast
result = ast.literal_eval("{'key': 'value'}")
```

## Functions to Avoid

The following functions should be avoided in production:
- eval: executes arbitrary Python code
- exec: similar to eval but for statements
- compile: compiles source code
- __import__: dynamically imports modules

## Shell Commands

Never use os.system() or subprocess without proper validation.
"#;

        let value = serde_json::json!(article);
        let result = sanitize_python_variable("documentation", &value);

        assert!(
            result.is_ok(),
            "Python documentation with code examples should be accepted: {:?}",
            result
        );
    }

    // ========== Tests for validate_keys_recursive (used in current implementation) ==========

    #[test]
    fn test_validate_keys_recursive_primitives() {
        // Primitives don't have keys, so they should always pass
        assert!(validate_keys_recursive(&serde_json::json!(123), 0).is_ok());
        assert!(validate_keys_recursive(&serde_json::json!(true), 0).is_ok());
        assert!(validate_keys_recursive(&serde_json::json!(null), 0).is_ok());
        assert!(
            validate_keys_recursive(&serde_json::json!("any string including eval()"), 0).is_ok()
        );
    }

    #[test]
    fn test_validate_keys_recursive_array_with_any_values() {
        // Arrays can contain any values (including dangerous patterns)
        let value = serde_json::json!(["safe", "eval(code)", "__import__", 123]);
        assert!(validate_keys_recursive(&value, 0).is_ok());
    }

    #[test]
    fn test_validate_keys_recursive_object_safe_keys() {
        let value = serde_json::json!({
            "safe_key": "value can contain eval() or __import__",
            "another_key": "os.system('ls')"
        });
        assert!(validate_keys_recursive(&value, 0).is_ok());
    }

    #[test]
    fn test_validate_keys_recursive_object_invalid_key() {
        let value = serde_json::json!({
            "invalid-key": "value doesn't matter"
        });
        assert!(validate_keys_recursive(&value, 0).is_err());
    }

    #[test]
    fn test_validate_keys_recursive_nested_object_with_dangerous_values() {
        // Nested objects with safe keys but dangerous values should pass
        let value = serde_json::json!({
            "level1": {
                "level2": {
                    "level3": "eval(__import__('os').system('ls'))"
                }
            }
        });
        assert!(validate_keys_recursive(&value, 0).is_ok());
    }

    #[test]
    fn test_validate_keys_recursive_max_depth_exceeded() {
        let mut value = serde_json::json!("deep");
        for _ in 0..MAX_RECURSIVE_DEPTH + 1 {
            value = serde_json::json!([value]);
        }
        assert!(validate_keys_recursive(&value, 0).is_err());
    }

    #[test]
    fn test_validate_keys_recursive_max_depth_exact_limit() {
        let mut value = serde_json::json!("deep");
        for _ in 0..MAX_RECURSIVE_DEPTH {
            value = serde_json::json!([value]);
        }
        assert!(validate_keys_recursive(&value, 0).is_ok());
    }

    // ========== Tests for sanitize_python_variable (current implementation) ==========

    #[test]
    fn test_sanitize_python_variable_valid_name_and_value() {
        // Any value content is acceptable (Base64-protected)
        let value = serde_json::json!("any string including eval()");
        assert!(sanitize_python_variable("valid_var", &value).is_ok());
    }

    #[test]
    fn test_sanitize_python_variable_invalid_name() {
        let value = serde_json::json!("value doesn't matter");
        assert!(sanitize_python_variable("invalid-var", &value).is_err());
        assert!(sanitize_python_variable("123var", &value).is_err());
        assert!(sanitize_python_variable("for", &value).is_err());
    }

    #[test]
    fn test_sanitize_python_variable_dangerous_value_should_pass() {
        // Dangerous values should pass because they are Base64-encoded
        let value = serde_json::json!("eval(__import__('os').system('rm -rf /'))");
        assert!(sanitize_python_variable("var", &value).is_ok());
    }

    #[test]
    fn test_sanitize_python_variable_nested_dangerous_value_should_pass() {
        // Nested dangerous values should pass (Base64-protected)
        let value = serde_json::json!({
            "nested": {
                "field": "exec(malicious)"
            }
        });
        assert!(sanitize_python_variable("var", &value).is_ok());
    }

    #[test]
    fn test_sanitize_python_variable_complex_structure() {
        let value = serde_json::json!({
            "user_name": "john_doe",
            "user_id": 123,
            "is_active": true,
            "metadata": {
                "created_at": "2024-01-01",
                "tags": ["tag1", "tag2"],
                "code_examples": ["eval(x)", "__import__('os')"]
            }
        });
        assert!(sanitize_python_variable("config", &value).is_ok());
    }

    // ========== Tests for download_script_secure ==========

    #[tokio::test]
    async fn test_download_script_secure_invalid_url() {
        let result = download_script_secure("not-a-valid-url").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_download_script_secure_non_https() {
        let result = download_script_secure("http://example.com/script.py").await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Only HTTPS URLs are allowed"));
    }

    #[tokio::test]
    async fn test_download_script_secure_https_valid_scheme() {
        // This will fail to connect but should pass the HTTPS check
        let result =
            download_script_secure("https://nonexistent-domain-12345.example/script.py").await;
        // Should fail at download stage, not at scheme validation
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(!err_msg.contains("Only HTTPS URLs are allowed"));
    }
}
