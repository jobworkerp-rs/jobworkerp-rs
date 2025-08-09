use anyhow::Result;
use app::app::function::{FunctionApp, UseFunctionApp};
use jobworkerp_runner::jobworkerp::runner::llm::{
    llm_chat_args::{message_content, ChatRole, FunctionOptions, LlmOptions},
    llm_completion_args::LlmOptions as CompletionLlmOptions,
    LlmChatArgs, LlmCompletionArgs,
};
use mistralrs::{Function, RequestBuilder, TextMessageRole, Tool, ToolChoice, ToolType};
use proto::jobworkerp::function::data::FunctionSpecs;
use std::collections::HashMap;
// Arc import removed as it's not used

/// Trait for converting protocol buffer messages to LLM request objects
pub trait LLMRequestConverter: UseFunctionApp {

    async fn build_request(
        &self,
        args: &LlmChatArgs,
        _need_stream: bool,
    ) -> Result<RequestBuilder> {
        let mut builder = RequestBuilder::new();

        // Set stream mode based on need_stream parameter
        // TODO: Add proper stream support to RequestBuilder if available

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
                        // For tool messages, we need to handle tool call ID from metadata if available
                        if matches!(role, TextMessageRole::Tool) {
                            // Tool messages should contain the result of a tool call
                            // The format should include the call_id for proper conversation flow
                            builder =
                                builder.add_tool_message(text.clone(), "tool_call_id".to_string());
                        } else {
                            builder = builder.add_message(role, text);
                        }
                    }
                    Some(message_content::Content::Image(_image)) => {
                        // TODO: Add support for image content
                        tracing::warn!("Image content not yet supported for mistralrs");
                        builder = builder.add_message(role, "[Image content not supported]");
                    }
                    Some(message_content::Content::ToolCalls(_tool_calls)) => {
                        // Tool calls are typically handled at the assistant message level
                        // For now, just add an empty message - proper tool call handling
                        // should be done when creating the request with tools
                        tracing::warn!("Tool calls in message content not fully supported yet in mistralrs converter");
                        builder = builder.add_message(role, "[Tool calls present]");
                    }
                    // Note: ToolResult variant doesn't exist in current proto definition
                    // Some(message_content::Content::ToolResult(_tool_result)) => {
                    //     tracing::warn!("Tool results not yet supported for mistralrs");
                    //     builder = builder.add_message(role, "[Tool result not supported]");
                    // }
                    None => {
                        // Handle empty content
                        builder = builder.add_message(role, "");
                    }
                },
                None => {
                    // Handle empty content
                    builder = builder.add_message(role, "");
                }
            }
        }

        // Apply LLM options if provided
        if let Some(opts) = &args.options {
            builder = Self::apply_chat_options(builder, opts)?;
        }

        // Handle function calling options
        if let Some(function_opts) = &args.function_options {
            if function_opts.use_function_calling {
                let tools = self.create_tools_from_options(function_opts).await?;
                if !tools.is_empty() {
                    builder = builder.set_tools(tools);
                    builder = builder.set_tool_choice(ToolChoice::Auto);
                }
            }
        }

        Ok(builder)
    }

    async fn build_completion_request(
        &self,
        args: &LlmCompletionArgs,
        _need_stream: bool,
    ) -> Result<RequestBuilder> {
        let mut builder = RequestBuilder::new();

        // Set stream mode based on need_stream parameter
        // TODO: Add proper stream support to RequestBuilder if available

        // Set the prompt as a user message
        builder = builder.add_message(TextMessageRole::User, &args.prompt);

        // Apply completion options if provided
        if let Some(opts) = &args.options {
            builder = Self::apply_completion_options(builder, opts)?;
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

        // Set repeat penalty
        if let Some(repeat_penalty) = opts.repeat_penalty {
            builder = builder.set_sampler_frequency_penalty(repeat_penalty);
        }

        // TODO: Add support for other options like seed, repeat_last_n, etc.
        // These may need to be mapped to appropriate mistralrs parameters

        Ok(builder)
    }

    fn apply_completion_options(
        mut builder: RequestBuilder,
        opts: &CompletionLlmOptions,
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

        // Set repeat penalty
        if let Some(repeat_penalty) = opts.repeat_penalty {
            builder = builder.set_sampler_frequency_penalty(repeat_penalty);
        }

        // TODO: Add support for other options like seed, repeat_last_n, etc.

        Ok(builder)
    }

    /// Create tools from function options using function app
    async fn create_tools_from_options(
        &self,
        function_opts: &FunctionOptions,
    ) -> Result<Vec<Tool>> {
        let function_app = self.function_app();
        // Get function list based on options
        let functions = if let Some(set_name) = &function_opts.function_set_name {
            tracing::debug!("Loading functions from set: {}", set_name);
            function_app.find_functions_by_set(set_name).await?
        } else {
            tracing::debug!(
                "Loading functions: runners={:?}, workers={:?}",
                function_opts.use_runners_as_function,
                function_opts.use_workers_as_function
            );
            function_app
                .find_functions(
                    !function_opts.use_runners_as_function.unwrap_or(false),
                    !function_opts.use_workers_as_function.unwrap_or(false),
                )
                .await?
        };

        // Convert FunctionSpecs to mistralrs Tools
        let tools = self.convert_functions_to_tools(&functions)?;
        tracing::debug!("Created {} tools for mistralrs", tools.len());
        Ok(tools)
    }

    /// Convert FunctionSpecs to mistralrs Tools
    fn convert_functions_to_tools(&self, functions: &[FunctionSpecs]) -> Result<Vec<Tool>> {
        // Convert to ToolInfo format first (reuse existing logic from ollama)
        let tool_infos =
            super::super::chat::conversion::ToolConverter::convert_functions_to_mcp_tools(
                functions.to_vec(),
            )?;

        // Convert ToolInfo to mistralrs Tool format
        let tools = tool_infos
            .tools
            .iter()
            .map(|tool| Tool {
                tp: ToolType::Function,
                function: Function {
                    name: tool.name.to_string(),
                    description: tool.description.clone().map(|d| d.to_string()),
                    parameters: Some(HashMap::from_iter(
                        tool.input_schema
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone())),
                    )),
                },
            })
            .collect();

        Ok(tools)
    }
}

// DefaultLLMRequestConverter struct removed - LLMRequestConverter is now used as mixin trait
