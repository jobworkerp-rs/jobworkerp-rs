use crate::mistral_runner::{
    mistral_chat_args::{message_content, ChatRole, LlmOptions},
    MistralChatArgs,
};
use anyhow::Result;
use mistralrs::{RequestBuilder, TextMessageRole};

pub struct RequestBuilderUtils;

impl RequestBuilderUtils {
    pub fn build_request(args: &MistralChatArgs) -> Result<RequestBuilder> {
        let mut builder = RequestBuilder::new();

        // Process messages
        for msg in &args.messages {
            let role = match ChatRole::try_from(msg.role)? {
                ChatRole::User => TextMessageRole::User,
                ChatRole::Assistant => TextMessageRole::Assistant,
                ChatRole::System => TextMessageRole::System,
                ChatRole::Tool => TextMessageRole::Tool,
                ChatRole::Unspecified => TextMessageRole::User, // Default fallback
            };

            // Handle message content
            match &msg.content {
                Some(content) => match &content.content {
                    Some(message_content::Content::Text(text)) => {
                        builder = builder.add_message(role, text);
                    }
                    Some(message_content::Content::Image(_image)) => {
                        tracing::warn!("Image content not yet supported for mistralrs");
                        builder = builder.add_message(role, "[Image content not supported]");
                    }
                    Some(message_content::Content::ToolCalls(tool_calls)) => {
                        if matches!(role, TextMessageRole::Assistant) {
                            builder = builder.add_message(role, "");
                            tracing::debug!(
                                "Added assistant message with {} tool calls",
                                tool_calls.calls.len()
                            );
                        } else {
                            tracing::warn!("Tool calls in non-assistant message role: {:?}", role);
                            builder = builder.add_message(role, "[Tool calls present]");
                        }
                    }
                    None => {
                        builder = builder.add_message(role, "");
                    }
                },
                None => {
                    builder = builder.add_message(role, "");
                }
            }
        }

        // Apply LLM options if provided
        if let Some(opts) = &args.options {
            builder = Self::apply_chat_options(builder, opts)?;
        }

        // Handle function calling options - currently not supported in plugin without FunctionApp
        if let Some(function_opts) = &args.function_options {
            if function_opts.use_function_calling {
                tracing::warn!("Function calling is not yet supported in MistralPlugin");
            }
        }

        Ok(builder)
    }

    fn apply_chat_options(
        mut builder: RequestBuilder,
        opts: &LlmOptions,
    ) -> Result<RequestBuilder> {
        // Set maximum token count
        if let Some(max_tokens) = opts.max_tokens {
            builder = builder.set_sampler_max_len(max_tokens as usize);
        }

        // Set temperature
        if let Some(temperature) = opts.temperature {
            builder = builder.set_sampler_temperature(temperature.into());
        }

        // Set top-p (nucleus sampling)
        if let Some(top_p) = opts.top_p {
            builder = builder.set_sampler_topp(top_p.into());
        }

        // Set repeat penalty (mapped to frequency penalty in MistralRS)
        if let Some(repeat_penalty) = opts.repeat_penalty {
            builder = builder.set_sampler_frequency_penalty(repeat_penalty);
        }

        // Handle unsupported parameters with warnings
        if opts.repeat_last_n.is_some() {
            tracing::warn!("repeat_last_n is not supported by MistralRS RequestBuilder, ignoring");
        }
        if opts.seed.is_some() {
            tracing::warn!("seed is not supported by MistralRS RequestBuilder, ignoring");
        }

        Ok(builder)
    }
}
