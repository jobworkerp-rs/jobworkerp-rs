//! Reasoning (thinking) controls shared by the genai and Ollama backends.
//!
//! Both chat and completion expose the same knobs on `LLMOptions`
//! (`extract_reasoning_content`, `reasoning_effort`, `chat_template_kwargs`), so
//! the translation into provider requests lives here to keep the backends in step.

use anyhow::{Result, bail};
use genai::adapter::AdapterKind;
use genai::chat::{ChatOptions, ReasoningEffort};
use jobworkerp_runner::jobworkerp::runner::llm::{llm_chat_args, llm_completion_args};
use ollama_rs::generation::parameters::ThinkType;
use serde_json::{Map, Value, json};

/// Reasoning-related request options, borrowed from either chat or completion `LLMOptions`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReasoningParams<'a> {
    pub extract_reasoning_content: Option<bool>,
    pub reasoning_effort: Option<&'a str>,
    pub chat_template_kwargs: Option<&'a str>,
}

macro_rules! impl_from_llm_options {
    ($options:ty) => {
        impl<'a> From<&'a $options> for ReasoningParams<'a> {
            fn from(opt: &'a $options) -> Self {
                Self {
                    extract_reasoning_content: opt.extract_reasoning_content,
                    reasoning_effort: opt.reasoning_effort.as_deref(),
                    chat_template_kwargs: opt.chat_template_kwargs.as_deref(),
                }
            }
        }
    };
}
impl_from_llm_options!(llm_chat_args::LlmOptions);
impl_from_llm_options!(llm_completion_args::LlmOptions);

impl ReasoningParams<'_> {
    /// Effective reasoning effort. `extract_reasoning_content = false` means "no thinking"
    /// so that genai behaves like Ollama, where the same flag maps to `think: false`.
    pub fn effective_reasoning_effort(&self) -> Result<Option<ReasoningEffort>> {
        if let Some(raw) = self
            .reasoning_effort
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return raw
                .to_ascii_lowercase()
                .parse::<ReasoningEffort>()
                .map(Some)
                .map_err(|_| {
                    // Bound the echoed value so oversized input does not bloat errors and logs.
                    let shown: String = raw.chars().take(32).collect();
                    anyhow::anyhow!(
                        "invalid reasoning_effort {shown:?}: expected none, minimal, low, medium, high, xhigh, max or a token budget number"
                    )
                });
        }
        Ok((self.extract_reasoning_content == Some(false)).then_some(ReasoningEffort::Zero))
    }

    /// Ollama `think` value derived from the effective reasoning effort.
    pub fn ollama_think(&self) -> Result<ThinkType> {
        Ok(match self.effective_reasoning_effort()? {
            Some(ReasoningEffort::Zero) => ThinkType::False,
            Some(ReasoningEffort::Minimal | ReasoningEffort::Low) => ThinkType::Low,
            Some(ReasoningEffort::Medium) => ThinkType::Medium,
            Some(ReasoningEffort::High | ReasoningEffort::XHigh | ReasoningEffort::Max) => {
                ThinkType::High
            }
            // Ollama has no token-budget knob; enable thinking at the model default.
            Some(ReasoningEffort::Budget(_)) => ThinkType::True,
            // Preserve the long-standing Ollama behavior where an unset flag on present
            // options meant `think: false` (proto3 getter default).
            None => ThinkType::from(self.extract_reasoning_content.unwrap_or(false)),
        })
    }

    /// Parsed `chat_template_kwargs`; rejected before any request is sent if not a JSON object.
    pub fn parsed_chat_template_kwargs(&self) -> Result<Option<Map<String, Value>>> {
        let Some(raw) = self
            .chat_template_kwargs
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return Ok(None);
        };
        match serde_json::from_str::<Value>(raw) {
            Ok(Value::Object(map)) => Ok(Some(map)),
            Ok(_) => bail!("chat_template_kwargs must be a JSON object"),
            Err(e) => bail!("chat_template_kwargs is not valid JSON: {e}"),
        }
    }

    /// Apply reasoning effort, `<think>` normalization and `chat_template_kwargs` to genai options.
    pub fn apply_to_genai(
        &self,
        chat_opts: &mut ChatOptions,
        adapter_kind: Option<AdapterKind>,
    ) -> Result<()> {
        let mut effort = self.effective_reasoning_effort()?;
        let user_kwargs = self.parsed_chat_template_kwargs()?;
        chat_opts.normalize_reasoning_content = self.extract_reasoning_content;

        let mut kwargs = Map::new();
        // genai's Omlx adapter rewrites chat_template_kwargs to `enable_thinking: true` whenever
        // reasoning_effort is present, clobbering both "no thinking" and caller-supplied kwargs.
        if adapter_kind == Some(AdapterKind::Omlx) {
            if matches!(effort, Some(ReasoningEffort::Zero)) {
                kwargs.insert("enable_thinking".to_string(), Value::Bool(false));
                effort = None;
            } else if user_kwargs.is_some() && effort.is_some() {
                tracing::warn!(
                    "reasoning_effort is ignored for the omlx adapter because chat_template_kwargs is set"
                );
                effort = None;
            }
        }
        if let Some(user_kwargs) = user_kwargs {
            kwargs.extend(user_kwargs);
        }

        chat_opts.reasoning_effort = effort;
        if !kwargs.is_empty() {
            chat_opts.extra_body = Some(json!({ "chat_template_kwargs": kwargs }));
        }
        Ok(())
    }
}

/// Ollama `think` value for optional `LLMOptions`; absent options leave `think` unset.
pub fn ollama_think_for<'a, O>(options: Option<&'a O>) -> Result<Option<ThinkType>>
where
    ReasoningParams<'a>: From<&'a O>,
{
    let Some(params) = options.map(ReasoningParams::from) else {
        return Ok(None);
    };
    if params
        .chat_template_kwargs
        .is_some_and(|s| !s.trim().is_empty())
    {
        tracing::warn!("chat_template_kwargs is not supported by the Ollama runner; ignoring");
    }
    params.ollama_think().map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params<'a>(
        extract: Option<bool>,
        effort: Option<&'a str>,
        kwargs: Option<&'a str>,
    ) -> ReasoningParams<'a> {
        ReasoningParams {
            extract_reasoning_content: extract,
            reasoning_effort: effort,
            chat_template_kwargs: kwargs,
        }
    }

    fn effort_name(p: ReasoningParams<'_>) -> Option<String> {
        p.effective_reasoning_effort()
            .unwrap()
            .map(|e| e.to_string())
    }

    #[test]
    fn effort_unset_when_nothing_specified() {
        assert_eq!(effort_name(params(None, None, None)), None);
    }

    #[test]
    fn extract_false_means_no_thinking() {
        assert_eq!(
            effort_name(params(Some(false), None, None)),
            Some("zero".into())
        );
    }

    #[test]
    fn extract_true_leaves_effort_to_provider_default() {
        assert_eq!(effort_name(params(Some(true), None, None)), None);
    }

    #[test]
    fn explicit_effort_wins_over_extract_flag() {
        assert_eq!(
            effort_name(params(Some(false), Some("high"), None)),
            Some("high".into())
        );
        assert_eq!(
            effort_name(params(Some(true), Some("none"), None)),
            Some("zero".into())
        );
    }

    #[test]
    fn effort_accepts_case_and_whitespace_variations() {
        assert_eq!(
            effort_name(params(None, Some(" Medium "), None)),
            Some("medium".into())
        );
    }

    #[test]
    fn invalid_effort_error_truncates_echoed_value() {
        let long = "x".repeat(1000);
        let err = params(None, Some(&long), None)
            .effective_reasoning_effort()
            .unwrap_err()
            .to_string();
        assert!(err.len() < 200, "{err}");
    }

    #[test]
    fn numeric_effort_is_token_budget() {
        let effort = params(None, Some("2048"), None)
            .effective_reasoning_effort()
            .unwrap();
        assert!(matches!(effort, Some(ReasoningEffort::Budget(2048))));
    }

    #[test]
    fn empty_effort_is_treated_as_unset() {
        assert_eq!(
            effort_name(params(Some(false), Some("  "), None)),
            Some("zero".into())
        );
        assert_eq!(effort_name(params(None, Some(""), None)), None);
    }

    #[test]
    fn invalid_effort_is_rejected() {
        let err = params(None, Some("extreme"), None)
            .effective_reasoning_effort()
            .unwrap_err();
        assert!(err.to_string().contains("reasoning_effort"), "{err}");
    }

    #[test]
    fn ollama_think_mapping() {
        let cases: &[(Option<bool>, Option<&str>, ThinkType)] = &[
            (None, None, ThinkType::False),
            (Some(true), None, ThinkType::True),
            (Some(false), None, ThinkType::False),
            (None, Some("none"), ThinkType::False),
            (None, Some("zero"), ThinkType::False),
            (None, Some("minimal"), ThinkType::Low),
            (None, Some("low"), ThinkType::Low),
            (None, Some("medium"), ThinkType::Medium),
            (None, Some("high"), ThinkType::High),
            (None, Some("xhigh"), ThinkType::High),
            (None, Some("max"), ThinkType::High),
            (None, Some("1024"), ThinkType::True),
            (Some(true), Some("none"), ThinkType::False),
        ];
        for (extract, effort, expected) in cases {
            assert_eq!(
                &params(*extract, *effort, None).ollama_think().unwrap(),
                expected,
                "extract={extract:?} effort={effort:?}"
            );
        }
    }

    #[test]
    fn ollama_think_propagates_invalid_effort() {
        assert!(params(None, Some("bogus"), None).ollama_think().is_err());
    }

    #[test]
    fn kwargs_unset_or_blank_is_none() {
        assert!(
            params(None, None, None)
                .parsed_chat_template_kwargs()
                .unwrap()
                .is_none()
        );
        assert!(
            params(None, None, Some(" "))
                .parsed_chat_template_kwargs()
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn kwargs_object_is_parsed() {
        let map = params(None, None, Some(r#"{"enable_thinking":false}"#))
            .parsed_chat_template_kwargs()
            .unwrap()
            .unwrap();
        assert_eq!(map.get("enable_thinking"), Some(&json!(false)));
    }

    #[test]
    fn kwargs_non_object_or_invalid_json_is_rejected() {
        for raw in ["{not json", "[1,2]", "\"text\"", "null", "true"] {
            let err = params(None, None, Some(raw))
                .parsed_chat_template_kwargs()
                .unwrap_err();
            assert!(
                err.to_string().contains("chat_template_kwargs"),
                "{raw}: {err}"
            );
        }
    }

    fn apply(p: ReasoningParams<'_>, kind: Option<AdapterKind>) -> ChatOptions {
        let mut opts = ChatOptions::default();
        p.apply_to_genai(&mut opts, kind).unwrap();
        opts
    }

    #[test]
    fn genai_nothing_specified_keeps_defaults() {
        let opts = apply(params(None, None, None), Some(AdapterKind::OpenAI));
        assert!(opts.reasoning_effort.is_none());
        assert!(opts.normalize_reasoning_content.is_none());
        assert!(opts.extra_body.is_none());
    }

    #[test]
    fn genai_extract_false_sets_zero_effort() {
        let opts = apply(params(Some(false), None, None), Some(AdapterKind::OpenAI));
        assert!(matches!(opts.reasoning_effort, Some(ReasoningEffort::Zero)));
        assert_eq!(opts.normalize_reasoning_content, Some(false));
        assert!(opts.extra_body.is_none());
    }

    #[test]
    fn genai_extract_true_only_normalizes() {
        let opts = apply(params(Some(true), None, None), Some(AdapterKind::OpenAI));
        assert!(opts.reasoning_effort.is_none());
        assert_eq!(opts.normalize_reasoning_content, Some(true));
    }

    #[test]
    fn genai_kwargs_go_to_extra_body() {
        let opts = apply(
            params(None, Some("low"), Some(r#"{"enable_thinking":true,"x":1}"#)),
            Some(AdapterKind::OpenAI),
        );
        assert!(matches!(opts.reasoning_effort, Some(ReasoningEffort::Low)));
        assert_eq!(
            opts.extra_body,
            Some(json!({"chat_template_kwargs": {"enable_thinking": true, "x": 1}}))
        );
    }

    #[test]
    fn genai_omlx_zero_effort_disables_thinking_via_template_kwargs() {
        // genai's Omlx adapter turns any reasoning_effort into enable_thinking=true,
        // so "no thinking" has to be expressed through chat_template_kwargs instead.
        let opts = apply(params(Some(false), None, None), Some(AdapterKind::Omlx));
        assert!(opts.reasoning_effort.is_none());
        assert_eq!(
            opts.extra_body,
            Some(json!({"chat_template_kwargs": {"enable_thinking": false}}))
        );
    }

    #[test]
    fn genai_omlx_user_kwargs_override_injected_default() {
        let opts = apply(
            params(
                None,
                Some("none"),
                Some(r#"{"enable_thinking":true,"k":"v"}"#),
            ),
            Some(AdapterKind::Omlx),
        );
        assert!(opts.reasoning_effort.is_none());
        assert_eq!(
            opts.extra_body,
            Some(json!({"chat_template_kwargs": {"enable_thinking": true, "k": "v"}}))
        );
    }

    #[test]
    fn genai_omlx_keeps_non_zero_effort_without_user_kwargs() {
        let opts = apply(params(None, Some("high"), None), Some(AdapterKind::Omlx));
        assert!(matches!(opts.reasoning_effort, Some(ReasoningEffort::High)));
        assert!(opts.extra_body.is_none());
    }

    #[test]
    fn genai_omlx_user_kwargs_suppress_effort_to_avoid_overwrite() {
        let opts = apply(
            params(None, Some("high"), Some(r#"{"enable_thinking":true}"#)),
            Some(AdapterKind::Omlx),
        );
        assert!(opts.reasoning_effort.is_none());
        assert_eq!(
            opts.extra_body,
            Some(json!({"chat_template_kwargs": {"enable_thinking": true}}))
        );
    }

    #[test]
    fn genai_invalid_inputs_fail_before_request() {
        let mut opts = ChatOptions::default();
        assert!(
            params(None, Some("bogus"), None)
                .apply_to_genai(&mut opts, Some(AdapterKind::OpenAI))
                .is_err()
        );
        assert!(
            params(None, None, Some("[]"))
                .apply_to_genai(&mut opts, Some(AdapterKind::OpenAI))
                .is_err()
        );
    }

    #[test]
    fn ollama_think_for_absent_options_leaves_think_unset() {
        let none: Option<&llm_chat_args::LlmOptions> = None;
        assert_eq!(ollama_think_for(none).unwrap(), None);
    }

    #[test]
    fn ollama_think_for_ignores_kwargs_but_honors_effort() {
        let opts = llm_completion_args::LlmOptions {
            reasoning_effort: Some("medium".to_string()),
            chat_template_kwargs: Some(r#"{"enable_thinking":false}"#.to_string()),
            ..Default::default()
        };
        assert_eq!(
            ollama_think_for(Some(&opts)).unwrap(),
            Some(ThinkType::Medium)
        );
    }

    #[test]
    fn ollama_think_for_rejects_invalid_effort() {
        let opts = llm_chat_args::LlmOptions {
            reasoning_effort: Some("bogus".to_string()),
            ..Default::default()
        };
        assert!(ollama_think_for(Some(&opts)).is_err());
    }
}
