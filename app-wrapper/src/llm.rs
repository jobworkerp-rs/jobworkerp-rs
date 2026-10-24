pub mod chat;
pub mod common;
pub mod completion;
pub mod embedding;
pub mod generic_tracing_helper;
pub mod reasoning;
pub(crate) mod schema_sanitize;
pub mod token_count;
pub mod tracing;
pub mod unified;

pub(crate) const GENAI_JSON_SPEC_NAME: &str = "structured_output";

pub trait ThinkTagHelper {
    const START_THINK_TAG: &'static str = "<think>";
    const END_THINK_TAG: &'static str = "</think>";

    fn divide_think_tag(prompt: String) -> (String, Option<String>) {
        if let Some(think_start) = prompt.find(Self::START_THINK_TAG)
            && let Some(think_end) = prompt.find(Self::END_THINK_TAG)
        {
            let think = Some(prompt[think_start + 7..think_end].trim().to_string());
            let new_prompt = prompt[..think_start].to_string() + &prompt[think_end + 8..];
            return (new_prompt.trim().to_string(), think);
        }
        (prompt.trim().to_string(), None)
    }

    /// Reasoning reported by the provider (e.g. OpenAI-compatible `reasoning_content`) takes
    /// precedence; `<think>` tags inlined in the text are the fallback for servers that do not
    /// separate it.
    fn split_text_and_reasoning(
        text: &str,
        provider_reasoning: Option<String>,
    ) -> (String, Option<String>) {
        let (text, think) = Self::divide_think_tag(text.to_string());
        (text, provider_reasoning.filter(|r| !r.is_empty()).or(think))
    }
}
