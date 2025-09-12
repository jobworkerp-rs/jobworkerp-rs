#![cfg(feature = "local_llm")]

use anyhow::Result;
use app::app::function::{FunctionApp, FunctionAppImpl, UseFunctionApp};
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
    fn build_request(
        &self,
        args: &LlmChatArgs,
        _need_stream: bool,
    ) -> impl std::future::Future<Output = Result<RequestBuilder>> + Send {
        let args = args.clone();
        let function_app = self.function_app();
        async move {
            let mut builder = RequestBuilder::new();

            // Note: Streaming is implemented at higher level in mistral.rs stream_chat()

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
                            // Image support requires VisionModel implementation
                            // Ref: https://github.com/EricLBuehler/mistral.rs/blob/main/mistralrs/examples/phi4mm/main.rs
                            tracing::warn!("Image content not yet supported for mistralrs, requires VisionModel");
                            builder = builder.add_message(role, "[Image content not supported]");
                        }
                        Some(message_content::Content::ToolCalls(tool_calls)) => {
                            // Handle tool calls in assistant messages
                            if matches!(role, TextMessageRole::Assistant) {
                                // Add assistant message with tool calls - MistralRS expects empty content for tool-calling messages
                                builder = builder.add_message(role, "");
                                tracing::debug!(
                                    "Added assistant message with {} tool calls",
                                    tool_calls.calls.len()
                                );
                            } else {
                                tracing::warn!(
                                    "Tool calls in non-assistant message role: {:?}",
                                    role
                                );
                                builder = builder.add_message(role, "[Tool calls present]");
                            }
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
                    let tools =
                        Self::create_tools_from_options_static(function_opts, function_app).await?;
                    if !tools.is_empty() {
                        builder = builder.set_tools(tools);
                        builder = builder.set_tool_choice(ToolChoice::Auto);
                    }
                }
            }

            Ok(builder)
        }
    }

    fn build_completion_request(
        &self,
        args: &LlmCompletionArgs,
        _need_stream: bool,
    ) -> impl std::future::Future<Output = Result<RequestBuilder>> + Send {
        let args = args.clone();
        async move {
            let mut builder = RequestBuilder::new();

            // Note: Streaming is implemented at higher level in mistral.rs stream_chat()

            // Set the prompt as a user message
            builder = builder.add_message(TextMessageRole::User, &args.prompt);

            // Apply completion options if provided
            if let Some(opts) = &args.options {
                builder = Self::apply_completion_options(builder, opts)?;
            }

            Ok(builder)
        }
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

    /// Create tools from function options using function app (static version)
    fn create_tools_from_options_static(
        function_opts: &FunctionOptions,
        function_app: &FunctionAppImpl,
    ) -> impl std::future::Future<Output = Result<Vec<Tool>>> + Send {
        let function_opts = function_opts.clone();
        async move {
            // Get function list based on options
            let functions = if let Some(set_name) = &function_opts.function_set_name {
                tracing::debug!("Loading functions from set: {set_name}");
                match function_app.find_functions_by_set(set_name).await {
                    Ok(result) => {
                        tracing::debug!(
                            "Found {} functions in set '{set_name}': {:?}",
                            result.len(),
                            result.iter().map(|f| &f.name).collect::<Vec<_>>()
                        );
                        result
                    }
                    Err(e) => {
                        tracing::error!("Failed to find functions for set '{set_name}': {e}");
                        return Err(e);
                    }
                }
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
            let tools = Self::convert_functions_to_tools_static(&functions)?;
            tracing::debug!("Created {} tools for mistralrs", tools.len());
            Ok(tools)
        }
    }

    /// Create tools from function options using function app
    fn create_tools_from_options(
        &self,
        function_opts: &FunctionOptions,
    ) -> impl std::future::Future<Output = Result<Vec<Tool>>> + Send {
        let function_opts = function_opts.clone();
        let function_app = self.function_app();
        async move { Self::create_tools_from_options_static(&function_opts, function_app).await }
    }

    /// Convert FunctionSpecs to mistralrs Tools (static version)
    fn convert_functions_to_tools_static(functions: &[FunctionSpecs]) -> Result<Vec<Tool>> {
        tracing::debug!("Converting {} functions to tools", functions.len());
        for func in functions {
            tracing::debug!("Function: {} ({})", func.name, func.description);
        }

        // Convert to ToolInfo format first (reuse existing logic from ollama)
        let tool_infos =
            super::super::chat::conversion::ToolConverter::convert_functions_to_mcp_tools(
                functions.to_vec(),
            )?;
        tracing::debug!("Converted to {} tool infos", tool_infos.tools.len());

        // Convert ToolInfo to mistralrs Tool format
        let tools: Vec<Tool> = tool_infos
            .tools
            .iter()
            .map(|tool| {
                tracing::debug!("Creating MistralRS tool: {}", tool.name);
                Tool {
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
                }
            })
            .collect();

        tracing::debug!("Created {} MistralRS tools", tools.len());
        Ok(tools)
    }

    /// Convert FunctionSpecs to mistralrs Tools
    fn convert_functions_to_tools(&self, functions: &[FunctionSpecs]) -> Result<Vec<Tool>> {
        Self::convert_functions_to_tools_static(functions)
    }
}

// DefaultLLMRequestConverter struct removed - LLMRequestConverter is now used as mixin trait
