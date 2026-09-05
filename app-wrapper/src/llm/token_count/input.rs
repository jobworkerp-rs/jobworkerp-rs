//! Validation and provider-neutral rendering for token-count inputs.

use super::{TokenCountOutcomeResult, capability_unavailable, invalid};
use jobworkerp_runner::jobworkerp::runner::llm::{
    LlmCompletionArgs,
    llm_chat_args::{ChatMessage, ChatRole, message_content::Content},
    llm_runner_settings::GenaiRunnerSettings,
};
use serde_json::{Value, json};

const MAX_TEXT_COUNT: usize = 256;
const MAX_TEXT_UTF8_BYTES: usize = 1_000_000;
const MAX_TOTAL_TEXT_UTF8_BYTES: usize = 4_000_000;

pub(super) fn validate_texts(texts: &[String]) -> TokenCountOutcomeResult<()> {
    if texts.len() > MAX_TEXT_COUNT {
        return Err(invalid("text count exceeds the request limit"));
    }
    let mut total = 0usize;
    for text in texts {
        if text.len() > MAX_TEXT_UTF8_BYTES {
            return Err(invalid("a text exceeds the UTF-8 byte limit"));
        }
        total = total
            .checked_add(text.len())
            .ok_or_else(|| invalid("total text size overflows"))?;
        if total > MAX_TOTAL_TEXT_UTF8_BYTES {
            return Err(invalid("total text size exceeds the UTF-8 byte limit"));
        }
    }
    Ok(())
}

pub(super) fn validate_max_tokens(max_tokens: Option<i32>) -> TokenCountOutcomeResult<()> {
    if matches!(max_tokens, Some(value) if value <= 0) {
        Err(invalid("max_tokens must be greater than zero"))
    } else {
        Ok(())
    }
}

pub(super) fn text_messages(messages: &[ChatMessage]) -> TokenCountOutcomeResult<Vec<Value>> {
    messages
        .iter()
        .map(|message| {
            let role = match message.role() {
                ChatRole::System => "system",
                ChatRole::User => "user",
                ChatRole::Assistant => "assistant",
                ChatRole::Tool => "tool",
                ChatRole::Unspecified => return Err(invalid("chat message role is required")),
            };
            match message
                .content
                .as_ref()
                .and_then(|content| content.content.as_ref())
            {
                Some(Content::Text(text)) => Ok(json!({"role": role, "content": text})),
                // Do not silently omit content that changes provider tokenization.
                _ => Err(capability_unavailable()),
            }
        })
        .collect()
}

pub(super) fn chat_messages(
    args: &jobworkerp_runner::jobworkerp::runner::llm::LlmChatArgs,
    settings: &GenaiRunnerSettings,
) -> TokenCountOutcomeResult<Vec<Value>> {
    // A parsed schema and resolved tools are both part of the provider request.
    // Until their provider-specific count envelopes are shared with the
    // generation adapter, returning a guessed count is unsafe.
    if args
        .json_schema
        .as_deref()
        .is_some_and(|schema| serde_json::from_str::<Value>(schema).is_ok())
        || args.function_options.as_ref().is_some_and(|options| {
            options.use_function_calling
                || options
                    .client_tools_json
                    .as_deref()
                    .is_some_and(|tools| !tools.is_empty())
        })
    {
        return Err(capability_unavailable());
    }
    let mut messages = text_messages(&args.messages)?;
    if let Some(system) = settings.system_prompt.as_ref() {
        messages.retain(|message| message.get("role") != Some(&Value::String("system".into())));
        messages.insert(0, json!({"role": "system", "content": system}));
    }
    Ok(messages)
}

pub(super) fn completion_messages(
    args: &LlmCompletionArgs,
    settings: &GenaiRunnerSettings,
) -> Vec<Value> {
    let mut messages = Vec::new();
    if let Some(system) = args
        .system_prompt
        .as_ref()
        .or(settings.system_prompt.as_ref())
    {
        messages.push(json!({"role": "system", "content": system}));
    }
    messages.push(json!({"role": "user", "content": args.prompt}));
    messages
}

#[cfg(test)]
mod tests {
    use super::*;
    use jobworkerp_runner::jobworkerp::runner::llm::{
        llm_chat_args::MessageContent,
        token_count_result::{Error, Outcome, TokenCountErrorCode},
    };

    fn outcome(result: jobworkerp_runner::jobworkerp::runner::llm::TokenCountResult) -> Outcome {
        result.outcome.expect("outcome")
    }

    #[test]
    fn validates_text_limits() {
        let error = validate_texts(&["x".repeat(MAX_TEXT_UTF8_BYTES + 1)]).unwrap_err();
        assert!(
            matches!(outcome(error), Outcome::Error(Error { code, .. }) if code == TokenCountErrorCode::InvalidTokenCountArgument as i32)
        );
    }

    #[test]
    fn rejects_non_positive_max_tokens() {
        assert!(validate_max_tokens(Some(0)).is_err());
        assert!(validate_max_tokens(Some(-1)).is_err());
        assert!(validate_max_tokens(Some(1)).is_ok());
    }

    #[test]
    fn rejects_non_text_chat_content() {
        let error = text_messages(&[ChatMessage {
            role: ChatRole::User as i32,
            content: Some(MessageContent::default()),
        }])
        .unwrap_err();
        assert!(
            matches!(outcome(error), Outcome::Error(Error { code, .. }) if code == TokenCountErrorCode::ProviderCapabilityUnavailable as i32)
        );
    }
}
