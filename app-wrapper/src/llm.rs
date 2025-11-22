pub mod chat;
pub mod completion;
pub mod generic_tracing_helper;

pub mod tracing;

pub trait ThinkTagHelper {
    const START_THINK_TAG: &'static str = "<think>";
    const END_THINK_TAG: &'static str = "</think>";

    fn divide_think_tag(prompt: String) -> (String, Option<String>) {
        if let Some(think_start) = prompt.find(Self::START_THINK_TAG) {
            if let Some(think_end) = prompt.find(Self::END_THINK_TAG) {
                let think = Some(prompt[think_start + 7..think_end].trim().to_string());
                let new_prompt = prompt[..think_start].to_string() + &prompt[think_end + 8..];
                return (new_prompt.trim().to_string(), think);
            }
        }
        (prompt.trim().to_string(), None)
    }
}
