use std::sync::Arc;

use super::conversion::ToolConverter;
use anyhow::{anyhow, Result};
use app::app::function::{FunctionApp, FunctionAppImpl};
use futures::stream::BoxStream;
use futures::StreamExt;
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content;
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::OllamaRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::{self, LlmChatArgs, LlmChatResult};
use ollama_rs::generation::chat::ChatMessageResponse;
use ollama_rs::generation::tools::ToolInfo;
use ollama_rs::{
    generation::chat::{request::ChatMessageRequest, ChatMessage, MessageRole},
    models::ModelOptions,
    Ollama,
};

pub struct OllamaChatService {
    pub function_app: Arc<FunctionAppImpl>,
    pub ollama: Ollama,
    pub model: String,
    pub system_prompt: Option<String>,
}

impl OllamaChatService {
    pub fn new(function_app: Arc<FunctionAppImpl>, settings: OllamaRunnerSettings) -> Result<Self> {
        let ollama = Ollama::try_new(
            settings
                .base_url
                .unwrap_or_else(|| "http://localhost:11434".to_string()),
        )?;
        Ok(Self {
            function_app,
            ollama,
            model: settings.model,
            system_prompt: settings.system_prompt,
        })
    }

    fn create_chat_options(args: &LlmChatArgs) -> ModelOptions {
        let mut options = ModelOptions::default();
        if let Some(opts) = args.options.as_ref() {
            if let Some(max_tokens) = opts.max_tokens {
                options = options.num_predict(max_tokens);
            } else {
                options = options.num_predict(-2);
            }
            if let Some(temperature) = opts.temperature {
                options = options.temperature(temperature);
            }
            if let Some(top_p) = opts.top_p {
                options = options.top_p(top_p);
            }
            if let Some(repeat_penalty) = opts.repeat_penalty {
                options = options.repeat_penalty(repeat_penalty);
            }
            if let Some(repeat_last_n) = opts.repeat_last_n {
                options = options.repeat_last_n(repeat_last_n);
            }
            if let Some(seed) = opts.seed {
                options = options.seed(seed);
            }
        }
        options
    }

    fn role_to_enum(role: ChatRole) -> MessageRole {
        match role {
            ChatRole::RoleSystem => MessageRole::System,
            ChatRole::RoleUser => MessageRole::User,
            ChatRole::RoleAssistant => MessageRole::Assistant,
            ChatRole::RoleTool => MessageRole::Tool,
            _ => MessageRole::User,
        }
    }

    async fn function_list(&self, args: &LlmChatArgs) -> Result<Vec<ToolInfo>> {
        if let Some(function_options) = &args.function_options {
            if function_options.use_function_calling {
                let list_future =
                    if let Some(set_name) = function_options.function_set_name.as_ref() {
                        tracing::debug!("Use functions by set: {}", set_name);
                        self.function_app.find_functions_by_set(set_name)
                    } else {
                        tracing::debug!(
                            "Use all functions from {}",
                            if function_options.use_runners_as_function()
                                && function_options.use_workers_as_function()
                            {
                                "all"
                            } else if function_options.use_workers_as_function() {
                                "workers"
                            } else if function_options.use_runners_as_function() {
                                "runners"
                            } else {
                                "none"
                            }
                        );
                        self.function_app.find_functions(
                            !function_options.use_runners_as_function(),
                            !function_options.use_workers_as_function(),
                        )
                    };
                match list_future.await {
                    Ok(functions) => {
                        tracing::debug!("Functions found: {}", &functions.len());
                        let converted =
                            ToolConverter::convert_functions_to_ollama_tools(functions.clone());
                        tracing::debug!(
                            "Converted functions: {:?}",
                            &converted
                                .iter()
                                .map(|f| f.function.name.as_str())
                                .collect::<Vec<&str>>()
                        );
                        Ok(converted)
                    }
                    Err(e) => {
                        tracing::error!("Error finding functions: {}", e);
                        Ok(vec![])
                    }
                }
            } else {
                Ok(vec![])
            }
        } else {
            Ok(vec![])
        }
    }

    fn convert_messages(args: &LlmChatArgs) -> Vec<ChatMessage> {
        args.messages
            .iter()
            .map(|m| {
                let role = Self::role_to_enum(m.role());
                let content = match &m.content {
                    Some(content) => match &content.content {
                        Some(llm::llm_chat_args::message_content::Content::Text(t)) => t.clone(),
                        _ => "".to_string(),
                    },
                    None => "".to_string(),
                };
                ChatMessage {
                    role,
                    content,
                    tool_calls: vec![],
                    images: Some(vec![]),
                }
            })
            .collect()
    }

    pub async fn request_chat(&mut self, args: LlmChatArgs) -> Result<LlmChatResult> {
        let options = Self::create_chat_options(&args);
        let model = args.model.clone().unwrap_or_else(|| self.model.clone());
        let mut messages = Self::convert_messages(&args);
        // add system prompt if it exists
        if let Some(system_prompt) = self.system_prompt.clone() {
            // remove any existing system prompt
            messages.retain(|m| m.role != MessageRole::System);
            messages.insert(0, ChatMessage::new(MessageRole::System, system_prompt));
        }
        let tools = self.function_list(&args).await?;

        let res = self
            .request_chat_internal(model, options, messages, tools)
            .await?;
        let text = res.message.content;
        let result = LlmChatResult {
            content: Some(llm::llm_chat_result::MessageContent {
                content: Some(message_content::Content::Text(text)),
            }),
            reasoning_content: None,
            done: true,
            usage: None,
        };
        Ok(result)
    }

    pub async fn request_chat_internal(
        &mut self,
        model: String,
        options: ModelOptions,
        mut messages: Vec<ChatMessage>,
        tools: Vec<ToolInfo>,
    ) -> Result<ChatMessageResponse> {
        // XXX clone for tool call
        let mut req = ChatMessageRequest::new(model.clone(), messages.clone());
        req = req.options(options.clone());
        let res = self
            .ollama
            .send_chat_messages(req)
            .await
            .map_err(|e| JobWorkerError::OtherError(format!("Chat error: {}", e)))?;
        tracing::debug!("Ollama chat response: {:#?}", &res);
        if res.message.tool_calls.is_empty() {
            tracing::debug!("No tool calls in response");
            Ok(res)
        } else {
            tracing::debug!("Tool calls in response: {:#?}", &res.message.tool_calls);
            for call in res.message.tool_calls {
                tracing::debug!("Tool call: {:?}", call.function); // TODO: Use log crate?

                let res = self
                    .function_app
                    .call_function(
                        call.function.name.as_str(),
                        call.function.arguments.as_object().cloned(),
                    )
                    .await?;
                tracing::debug!("Tool response: {}", &res);

                messages.push(ChatMessage::tool(res.to_string()))
            }
            // recurse
            Box::pin(self.request_chat_internal(model, options, messages, tools)).await
        }
    }

    pub async fn request_stream_chat(
        &mut self,
        args: LlmChatArgs,
    ) -> Result<BoxStream<'static, LlmChatResult>> {
        let options = Self::create_chat_options(&args);
        let model_name = args.model.clone().unwrap_or_else(|| self.model.clone());
        let mut req = ChatMessageRequest::new(model_name, Self::convert_messages(&args));
        req = req.options(options);
        if let Some(system_prompt) = self.system_prompt.clone() {
            req = req.template(system_prompt);
        }
        let stream = self
            .ollama
            .send_chat_messages_stream(req)
            .await
            .map_err(|e| anyhow!("Stream chat error: {}", e))?;
        let mapped = stream
            .map(|result| match result {
                Ok(chunk) => {
                    let text = chunk.message.content;
                    LlmChatResult {
                        content: Some(llm::llm_chat_result::MessageContent {
                            content: Some(message_content::Content::Text(text)),
                        }),
                        reasoning_content: None,
                        done: chunk.done,
                        usage: None,
                    }
                }
                Err(_) => {
                    tracing::error!("Error in stream chat");
                    LlmChatResult {
                        content: None,
                        reasoning_content: None,
                        done: true,
                        usage: None,
                    }
                }
            })
            .boxed();
        Ok(mapped)
    }
}
