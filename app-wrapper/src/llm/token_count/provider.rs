//! Provider-specific token-count endpoints and payloads.

use super::{TokenCountOutcomeResult, capability_unavailable};
use anyhow::{Context, Result, anyhow};
use genai::ServiceTarget;
use genai::adapter::AdapterKind;
use genai::resolver::AuthData;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CountProtocol {
    OpenAiChat,
    OpenAiResponses,
    Anthropic,
    Gemini,
    Unsupported,
}

pub(super) struct ResolvedTarget {
    pub(super) protocol: CountProtocol,
    pub(super) base_url: url::Url,
    pub(super) model: String,
    pub(super) request_model: String,
    auth: AuthData,
    pub(super) custom_endpoint: bool,
}

pub(super) fn resolved_target(
    target: ServiceTarget,
    model: String,
    custom_endpoint: bool,
) -> Result<ResolvedTarget> {
    let protocol = match target.model.adapter_kind {
        AdapterKind::OpenAI => CountProtocol::OpenAiChat,
        AdapterKind::OpenAIResp => CountProtocol::OpenAiResponses,
        AdapterKind::Anthropic => CountProtocol::Anthropic,
        AdapterKind::Gemini => CountProtocol::Gemini,
        _ => CountProtocol::Unsupported,
    };
    let base_url = target
        .endpoint
        .base_url()
        .parse::<url::Url>()
        .context("resolved GenAI endpoint is not a URL")?;
    let (_, request_model) = target.model.model_name.namespace_and_name();
    Ok(ResolvedTarget {
        protocol,
        base_url,
        model,
        request_model: request_model.to_string(),
        auth: target.auth,
        custom_endpoint,
    })
}

fn append_path(base: &url::Url, segments: &[&str]) -> Result<url::Url> {
    let mut url = base.clone();
    url.set_fragment(None);
    let mut path = url
        .path_segments_mut()
        .map_err(|_| anyhow!("endpoint URL cannot accept path segments"))?;
    path.pop_if_empty();
    for segment in segments {
        path.push(segment);
    }
    drop(path);
    Ok(url)
}

pub(super) fn vllm_tokenize_url(base: &url::Url) -> Result<url::Url> {
    let mut url = base.clone();
    url.set_fragment(None);
    let mut path = url
        .path_segments_mut()
        .map_err(|_| anyhow!("endpoint URL cannot accept path segments"))?;
    path.pop_if_empty();
    path.pop();
    path.push("tokenize");
    drop(path);
    Ok(url)
}

pub(super) fn count_url(
    target: &ResolvedTarget,
    raw_tokenize: bool,
) -> Result<TokenCountOutcomeResult<url::Url>> {
    if raw_tokenize {
        return Ok(Ok(vllm_tokenize_url(&target.base_url)?));
    }
    let mut url = match target.protocol {
        CountProtocol::OpenAiResponses => {
            append_path(&target.base_url, &["responses", "input_tokens"])?
        }
        CountProtocol::Anthropic => append_path(&target.base_url, &["messages", "count_tokens"])?,
        CountProtocol::Gemini => append_path(&target.base_url, &["models", &target.request_model])?,
        _ => return Ok(Err(capability_unavailable())),
    };
    if target.protocol == CountProtocol::Gemini {
        let path = url.path().trim_end_matches('/');
        url.set_path(&format!("{path}:countTokens"));
    }
    Ok(Ok(url))
}

fn credential_request(
    client: &reqwest::Client,
    target: &ResolvedTarget,
    url: url::Url,
) -> Result<reqwest::RequestBuilder> {
    let key = match &target.auth {
        AuthData::None => String::new(),
        auth => auth
            .single_key_value()
            .context("GenAI credential is unavailable")?,
    };
    let request = client.post(url);
    Ok(match target.protocol {
        CountProtocol::OpenAiChat | CountProtocol::OpenAiResponses => request.bearer_auth(key),
        CountProtocol::Anthropic => request
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01"),
        CountProtocol::Gemini => request.header("x-goog-api-key", key),
        CountProtocol::Unsupported => request,
    })
}

fn provider_reports_missing_model(body: &Value) -> bool {
    body.pointer("/error/code")
        .and_then(Value::as_str)
        .is_some_and(|code| code == "model_not_found" || code == "deployment_not_found")
}

pub(super) async fn send_count(
    client: &reqwest::Client,
    target: &ResolvedTarget,
    url: url::Url,
    body: Value,
    raw_tokenize: bool,
) -> Result<TokenCountOutcomeResult<(u64, Option<u64>)>> {
    let response = credential_request(client, target, url)?
        .json(&body)
        .send()
        .await
        .context("token-count provider request failed")?;
    let status = response.status();
    let value = response
        .json::<Value>()
        .await
        .context("token-count provider returned an invalid JSON response")?;
    if !status.is_success() {
        if matches!(status.as_u16(), 404 | 405 | 501) && !provider_reports_missing_model(&value) {
            return Ok(Err(capability_unavailable()));
        }
        return Err(anyhow!("token-count provider returned HTTP {status}"));
    }
    if raw_tokenize {
        let count = value
            .get("count")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("tokenize response has no valid count"))?;
        let tokens = value
            .get("tokens")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("tokenize response has no token array"))?;
        if tokens.len() as u64 != count {
            return Err(anyhow!(
                "tokenize response count does not match token array length"
            ));
        }
        return Ok(Ok((
            count,
            value.get("max_model_len").and_then(Value::as_u64),
        )));
    }
    let field = match target.protocol {
        CountProtocol::OpenAiResponses | CountProtocol::Anthropic => "input_tokens",
        CountProtocol::Gemini => "totalTokens",
        _ => return Err(anyhow!("unsupported count protocol")),
    };
    let count = value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("token-count response has no valid count"))?;
    Ok(Ok((count, None)))
}

pub(super) fn text_body(target: &ResolvedTarget, text: &str) -> Value {
    json!({"model": target.request_model, "prompt": text, "add_special_tokens": false})
}

pub(super) fn request_body(
    target: &ResolvedTarget,
    messages: Vec<Value>,
    raw_tokenize: bool,
) -> TokenCountOutcomeResult<Value> {
    if raw_tokenize {
        return Ok(json!({"model": target.request_model, "messages": messages}));
    }
    match target.protocol {
        CountProtocol::OpenAiResponses => {
            Ok(json!({"model": target.request_model, "input": messages}))
        }
        CountProtocol::Anthropic => {
            let mut system = None;
            let mut converted = Vec::new();
            for message in messages {
                let role = message
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let content = message.get("content").cloned().unwrap_or(Value::Null);
                if role == "system" {
                    system = Some(content);
                } else if matches!(role, "user" | "assistant") {
                    converted.push(json!({"role": role, "content": content}));
                } else {
                    return Err(capability_unavailable());
                }
            }
            let mut body = json!({"model": target.request_model, "messages": converted});
            if let Some(system) = system {
                body["system"] = system;
            }
            Ok(body)
        }
        CountProtocol::Gemini => {
            let mut system_parts = Vec::new();
            let mut contents = Vec::new();
            for message in messages {
                let role = message
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let text = message
                    .get("content")
                    .and_then(Value::as_str)
                    .ok_or_else(capability_unavailable)?;
                if role == "system" {
                    system_parts.push(text.to_string());
                    continue;
                }
                let role = if role == "assistant" {
                    "model"
                } else if role == "user" {
                    "user"
                } else {
                    return Err(capability_unavailable());
                };
                contents.push(json!({"role": role, "parts": [{"text": text}]}));
            }
            let mut generate_content_request = json!({"contents": contents});
            // genai's Gemini adapter concatenates all system messages with a
            // newline and places the result in systemInstruction.
            if !system_parts.is_empty() {
                generate_content_request["systemInstruction"] = json!({
                    "parts": [{"text": system_parts.join("\n")}]
                });
            }
            Ok(json!({"generateContentRequest": generate_content_request}))
        }
        _ => Err(capability_unavailable()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_vllm_sibling_url_without_losing_mount_prefix() {
        let base = "http://example.test/proxy/openai/v1/".parse().unwrap();
        assert_eq!(
            vllm_tokenize_url(&base).unwrap().as_str(),
            "http://example.test/proxy/openai/tokenize"
        );
    }

    #[test]
    fn recognizes_only_structured_missing_model_codes() {
        assert!(provider_reports_missing_model(
            &json!({"error": {"code": "model_not_found"}})
        ));
        assert!(!provider_reports_missing_model(
            &json!({"error": {"message": "model_not_found"}})
        ));
    }

    #[test]
    fn gemini_count_body_matches_generation_system_instruction() {
        let target = ResolvedTarget {
            protocol: CountProtocol::Gemini,
            base_url: "https://generativelanguage.googleapis.com/v1beta/"
                .parse()
                .unwrap(),
            model: "gemini::gemini-test".into(),
            request_model: "gemini-test".into(),
            auth: AuthData::None,
            custom_endpoint: false,
        };
        let body = request_body(
            &target,
            vec![
                json!({"role": "system", "content": "first instruction"}),
                json!({"role": "user", "content": "hello"}),
                json!({"role": "system", "content": "second instruction"}),
            ],
            false,
        )
        .expect("Gemini count supports system instructions");
        assert_eq!(
            body.pointer("/generateContentRequest/systemInstruction/parts/0/text")
                .and_then(Value::as_str),
            Some("first instruction\nsecond instruction")
        );
        assert_eq!(
            body["generateContentRequest"]["contents"],
            json!([{"role": "user", "parts": [{"text": "hello"}]}])
        );
    }
}
