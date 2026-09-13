//! Small helpers shared across the LLM method runners (completion/chat/
//! embedding) and the unified runner, to avoid duplicating the Ollama default
//! URL, the GenAI endpoint URL normalization, and the model-pull call.

use anyhow::{Context, Result, anyhow};
use genai::resolver::{Endpoint, ServiceTargetResolver};
use genai::{Client, ServiceTarget};
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

/// Apply a non-empty runner-level GenAI endpoint override to an already
/// resolved service target. This intentionally does not alter its model or
/// authentication data.
pub(crate) fn apply_genai_endpoint_override(
    service_target: &mut ServiceTarget,
    endpoint_url: Option<&str>,
) -> Result<()> {
    let Some(endpoint_url) = endpoint_url.filter(|url| !url.is_empty()) else {
        return Ok(());
    };
    service_target.endpoint = Endpoint::from_owned(
        normalize_genai_endpoint_url(endpoint_url).context("invalid GenAI endpoint URL")?,
    );
    Ok(())
}

/// Resolve the fixed model used by completion, chat, and token counting.
///
/// These methods intentionally use the runner settings model, matching the
/// existing GenAI generation services. Embedding has a distinct per-request
/// model-override contract and must use the preserving resolver below.
pub(crate) async fn resolve_fixed_genai_service_target(
    model_name: &str,
    endpoint_url: Option<&str>,
) -> Result<ServiceTarget> {
    let mut service_target = Client::default()
        .resolve_service_target(model_name)
        .await
        .with_context(|| {
            format!("failed to resolve GenAI service target from model={model_name}")
        })?;
    apply_genai_endpoint_override(&mut service_target, endpoint_url)?;
    Ok(service_target)
}

/// Construct the target resolver shared by GenAI completion and chat.
pub(crate) fn fixed_model_genai_service_target_resolver(
    model_name: String,
    endpoint_url: Option<String>,
) -> ServiceTargetResolver {
    ServiceTargetResolver::from_resolver_async_fn(
        move |_: ServiceTarget| -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<ServiceTarget, genai::resolver::Error>>
                    + Send,
            >,
        > {
            let model_name = model_name.clone();
            let endpoint_url = endpoint_url.clone();
            Box::pin(async move {
                resolve_fixed_genai_service_target(&model_name, endpoint_url.as_deref()).await.map_err(
                    |error| genai::resolver::Error::Custom(format!(
                        "Failed to resolve fixed GenAI service target from model={model_name}: {error:#}"
                    )),
                )
            })
        },
    )
}

/// Construct the resolver for embedding, which keeps GenAI's request model.
pub(crate) fn request_model_genai_service_target_resolver(
    endpoint_url: Option<String>,
) -> ServiceTargetResolver {
    ServiceTargetResolver::from_resolver_async_fn(
        move |mut service_target: ServiceTarget| -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<ServiceTarget, genai::resolver::Error>>
                    + Send,
            >,
        > {
            let endpoint_url = endpoint_url.clone();
            Box::pin(async move {
                apply_genai_endpoint_override(&mut service_target, endpoint_url.as_deref())
                    .map_err(|error| {
                        genai::resolver::Error::Custom(format!(
                            "Failed to apply GenAI endpoint override: {error:#}"
                        ))
                    })?;
                Ok(service_target)
            })
        },
    )
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

    #[tokio::test]
    async fn fixed_target_uses_the_settings_model_and_endpoint() {
        let target = resolve_fixed_genai_service_target(
            "openai::fixed-model",
            Some("http://example.test/custom"),
        )
        .await
        .expect("target resolves without contacting a provider");
        let (_, model) = target.model.model_name.namespace_and_name();
        assert_eq!(model, "fixed-model");
        assert_eq!(target.endpoint.base_url(), "http://example.test/custom/");
    }

    #[tokio::test]
    async fn endpoint_override_preserves_a_request_model() {
        let mut target = Client::default()
            .resolve_service_target("openai::request-model")
            .await
            .expect("target resolves without contacting a provider");
        apply_genai_endpoint_override(&mut target, Some("http://example.test/embedding"))
            .expect("endpoint override is valid");
        let (_, model) = target.model.model_name.namespace_and_name();
        assert_eq!(model, "request-model");
        assert_eq!(target.endpoint.base_url(), "http://example.test/embedding/");
    }
}
