//! Small helpers shared across the LLM method runners (completion/chat/
//! embedding) and the unified runner, to avoid duplicating the Ollama default
//! URL, the GenAI endpoint URL normalization, and the model-pull call.

use anyhow::{Result, anyhow};
use ollama_rs::Ollama;

/// Default Ollama server URL used when settings leave `base_url` unset.
pub const OLLAMA_DEFAULT_URL: &str = "http://localhost:11434";

/// Normalize a custom base URL into a GenAI endpoint string: ensure the path
/// ends with a trailing slash, defaulting an empty/root path to `/v1/`, so the
/// adapter appends request paths correctly. Shared by the completion/chat/
/// embedding GenAI service target resolvers.
pub fn normalize_genai_endpoint_url(url: &str) -> Result<String> {
    let mut u = url.parse::<url::Url>()?;
    if u.path().is_empty() || u.path() == "/" {
        u.set_path("/v1/");
    } else if !u.path().ends_with('/') {
        u.set_path(&format!("{}/", u.path()));
    }
    Ok(u.to_string())
}

/// Pull an Ollama model (blocking until present server-side). `base_url` falls
/// back to [`OLLAMA_DEFAULT_URL`] when `None`.
pub async fn pull_ollama_model(base_url: Option<&str>, model: &str) -> Result<()> {
    let client = Ollama::try_new(base_url.unwrap_or(OLLAMA_DEFAULT_URL).to_string())?;
    client
        .pull_model(model.to_string(), false)
        .await
        .map_err(|e| anyhow!("failed to pull model '{model}': {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_defaults_root_to_v1() {
        assert_eq!(
            normalize_genai_endpoint_url("http://host:8080").unwrap(),
            "http://host:8080/v1/"
        );
        assert_eq!(
            normalize_genai_endpoint_url("http://host:8080/").unwrap(),
            "http://host:8080/v1/"
        );
    }

    #[test]
    fn test_normalize_appends_trailing_slash() {
        assert_eq!(
            normalize_genai_endpoint_url("http://host/custom/path").unwrap(),
            "http://host/custom/path/"
        );
        assert_eq!(
            normalize_genai_endpoint_url("http://host/custom/path/").unwrap(),
            "http://host/custom/path/"
        );
    }

    #[test]
    fn test_normalize_rejects_invalid() {
        assert!(normalize_genai_endpoint_url("not a url").is_err());
    }
}
