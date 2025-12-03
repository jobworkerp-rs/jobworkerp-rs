//! Request conversion for MistralRS plugin
//!
//! Simplified version that doesn't depend on FunctionApp
//! Tool definitions are fetched via gRPC instead

use anyhow::Result;
use jobworkerp_runner::jobworkerp::runner::llm::{
    llm_chat_args::{message_content, ChatRole, LlmOptions},
    llm_completion_args::LlmOptions as CompletionLlmOptions,
    LlmChatArgs, LlmCompletionArgs,
};
use mistralrs::{Function, RequestBuilder, TextMessageRole, Tool, ToolChoice, ToolType};
use std::collections::HashMap;

// Import FunctionSpecs from plugin's generated gRPC types
use crate::grpc::generated::jobworkerp::function::data::FunctionSpecs;

/// Request converter for MistralRS
pub struct RequestConverter;

impl RequestConverter {
    /// Build a chat request from LlmChatArgs
    pub fn build_request(args: &LlmChatArgs, tools: Vec<Tool>) -> Result<RequestBuilder> {
        let mut builder = RequestBuilder::new();

        // Process messages
        for msg in &args.messages {
            let role = match ChatRole::try_from(msg.role)? {
                ChatRole::User => TextMessageRole::User,
                ChatRole::Assistant => TextMessageRole::Assistant,
                ChatRole::System => TextMessageRole::System,
                ChatRole::Tool => TextMessageRole::Tool,
                ChatRole::Unspecified => TextMessageRole::User,
            };

            match &msg.content {
                Some(content) => match &content.content {
                    Some(message_content::Content::Text(text)) => {
                        builder = builder.add_message(role, text);
                    }
                    Some(message_content::Content::Image(_image)) => {
                        tracing::warn!(
                            "Image content not yet supported for mistralrs, requires VisionModel"
                        );
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

        // Set tools if any
        if !tools.is_empty() {
            builder = builder.set_tools(tools);
            builder = builder.set_tool_choice(ToolChoice::Auto);
        }

        Ok(builder)
    }

    /// Build a completion request from LlmCompletionArgs
    pub fn build_completion_request(args: &LlmCompletionArgs) -> Result<RequestBuilder> {
        let mut builder = RequestBuilder::new();

        builder = builder.add_message(TextMessageRole::User, &args.prompt);

        if let Some(opts) = &args.options {
            builder = Self::apply_completion_options(builder, opts)?;
        }

        Ok(builder)
    }

    fn apply_chat_options(
        mut builder: RequestBuilder,
        opts: &LlmOptions,
    ) -> Result<RequestBuilder> {
        if let Some(max_tokens) = opts.max_tokens {
            builder = builder.set_sampler_max_len(max_tokens as usize);
        }

        if let Some(temperature) = opts.temperature {
            builder = builder.set_sampler_temperature(temperature.into());
        }

        if let Some(top_p) = opts.top_p {
            builder = builder.set_sampler_topp(top_p.into());
        }

        if let Some(repeat_penalty) = opts.repeat_penalty {
            builder = builder.set_sampler_frequency_penalty(repeat_penalty);
        }

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
        if let Some(max_tokens) = opts.max_tokens {
            builder = builder.set_sampler_max_len(max_tokens as usize);
        }

        if let Some(temperature) = opts.temperature {
            builder = builder.set_sampler_temperature(temperature.into());
        }

        if let Some(top_p) = opts.top_p {
            builder = builder.set_sampler_topp(top_p.into());
        }

        if let Some(repeat_penalty) = opts.repeat_penalty {
            builder = builder.set_sampler_frequency_penalty(repeat_penalty);
        }

        if opts.repeat_last_n.is_some() {
            tracing::warn!("repeat_last_n is not supported by MistralRS RequestBuilder, ignoring");
        }
        if opts.seed.is_some() {
            tracing::warn!("seed is not supported by MistralRS RequestBuilder, ignoring");
        }

        Ok(builder)
    }

    /// Convert FunctionSpecs to MistralRS Tools
    pub fn convert_function_specs_to_tools(specs: &[FunctionSpecs]) -> Result<Vec<Tool>> {
        // Default method name (same as proto::DEFAULT_METHOD_NAME)
        const DEFAULT_METHOD_NAME: &str = "run";

        let mut tools = Vec::new();

        for spec in specs {
            if let Some(methods) = &spec.methods {
                for (method_name, method_schema) in &methods.schemas {
                    // Parse arguments schema JSON
                    let parameters: Option<HashMap<String, serde_json::Value>> =
                        if !method_schema.arguments_schema.is_empty() {
                            serde_json::from_str(&method_schema.arguments_schema).ok()
                        } else {
                            None
                        };

                    let tool_name = if methods.schemas.len() == 1
                        && (method_name == "run" || method_name == DEFAULT_METHOD_NAME)
                    {
                        spec.name.clone()
                    } else {
                        format!("{}___{}", spec.name, method_name)
                    };

                    let tool = Tool {
                        tp: ToolType::Function,
                        function: Function {
                            name: tool_name,
                            description: method_schema.description.clone(),
                            parameters,
                        },
                    };
                    tools.push(tool);
                }
            }
        }

        tracing::debug!(
            "Converted {} FunctionSpecs to {} tools",
            specs.len(),
            tools.len()
        );
        Ok(tools)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grpc::generated::jobworkerp::function::data::{MethodSchema, MethodSchemaMap};
    use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
        ChatMessage, MessageContent as ChatMessageContent,
    };

    fn create_text_message(role: ChatRole, text: &str) -> ChatMessage {
        ChatMessage {
            role: role as i32,
            content: Some(ChatMessageContent {
                content: Some(message_content::Content::Text(text.to_string())),
            }),
        }
    }

    #[test]
    fn test_build_request_simple_chat() {
        let args = LlmChatArgs {
            model: None,
            messages: vec![
                create_text_message(ChatRole::System, "You are a helpful assistant."),
                create_text_message(ChatRole::User, "Hello!"),
            ],
            options: None,
            function_options: None,
            json_schema: None,
        };

        let result = RequestConverter::build_request(&args, vec![]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_build_request_with_options() {
        let args = LlmChatArgs {
            model: None,
            messages: vec![create_text_message(ChatRole::User, "Test")],
            options: Some(LlmOptions {
                max_tokens: Some(1024),
                temperature: Some(0.7),
                top_p: Some(0.9),
                repeat_penalty: Some(1.1),
                repeat_last_n: None,
                seed: None,
                extract_reasoning_content: None,
            }),
            function_options: None,
            json_schema: None,
        };

        let result = RequestConverter::build_request(&args, vec![]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_build_request_empty_messages() {
        let args = LlmChatArgs {
            model: None,
            messages: vec![],
            options: None,
            function_options: None,
            json_schema: None,
        };

        let result = RequestConverter::build_request(&args, vec![]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_build_completion_request() {
        let args = LlmCompletionArgs {
            model: None,
            system_prompt: None,
            prompt: "Once upon a time".to_string(),
            context: None,
            options: Some(CompletionLlmOptions {
                max_tokens: Some(512),
                temperature: Some(0.8),
                top_p: None,
                repeat_penalty: None,
                repeat_last_n: None,
                seed: None,
                extract_reasoning_content: None,
            }),
            function_options: None,
            json_schema: None,
        };

        let result = RequestConverter::build_completion_request(&args);
        assert!(result.is_ok());
    }

    #[test]
    fn test_convert_function_specs_single_method() {
        let mut schemas = std::collections::HashMap::new();
        schemas.insert(
            "run".to_string(),
            MethodSchema {
                arguments_schema: r#"{"type": "object", "properties": {"input": {"type": "string"}}}"#.to_string(),
                description: Some("Execute the function".to_string()),
                ..Default::default()
            },
        );

        let specs = vec![FunctionSpecs {
            name: "my_tool".to_string(),
            methods: Some(MethodSchemaMap { schemas }),
            ..Default::default()
        }];

        let result = RequestConverter::convert_function_specs_to_tools(&specs);
        assert!(result.is_ok());

        let tools = result.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].function.name, "my_tool");
        assert_eq!(
            tools[0].function.description,
            Some("Execute the function".to_string())
        );
    }

    #[test]
    fn test_convert_function_specs_multiple_methods() {
        let mut schemas = std::collections::HashMap::new();
        schemas.insert(
            "create".to_string(),
            MethodSchema {
                arguments_schema: "{}".to_string(),
                description: Some("Create item".to_string()),
                ..Default::default()
            },
        );
        schemas.insert(
            "delete".to_string(),
            MethodSchema {
                arguments_schema: "{}".to_string(),
                description: Some("Delete item".to_string()),
                ..Default::default()
            },
        );

        let specs = vec![FunctionSpecs {
            name: "item_manager".to_string(),
            methods: Some(MethodSchemaMap { schemas }),
            ..Default::default()
        }];

        let result = RequestConverter::convert_function_specs_to_tools(&specs);
        assert!(result.is_ok());

        let tools = result.unwrap();
        assert_eq!(tools.len(), 2);

        let tool_names: Vec<&str> = tools.iter().map(|t| t.function.name.as_str()).collect();
        assert!(tool_names.contains(&"item_manager___create"));
        assert!(tool_names.contains(&"item_manager___delete"));
    }

    #[test]
    fn test_convert_function_specs_empty() {
        let specs: Vec<FunctionSpecs> = vec![];
        let result = RequestConverter::convert_function_specs_to_tools(&specs);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_convert_function_specs_no_methods() {
        let specs = vec![FunctionSpecs {
            name: "empty_tool".to_string(),
            methods: None,
            ..Default::default()
        }];

        let result = RequestConverter::convert_function_specs_to_tools(&specs);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }
}
