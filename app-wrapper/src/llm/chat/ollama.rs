use super::conversion::ToolConverter;
use super::tracing_helper::OllamaTracingHelper;
use crate::llm::ThinkTagHelper;
use anyhow::{anyhow, Result};
use app::app::function::{FunctionApp, FunctionAppImpl};
use futures::stream::BoxStream;
use futures::StreamExt;
use infra_utils::infra::trace::impls::GenericOtelClient;
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
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
pub struct OllamaChatService {
    pub function_app: Arc<FunctionAppImpl>,
    pub ollama: Arc<Ollama>,
    pub model: String,
    pub system_prompt: Option<String>,
    pub otel_client: Option<Arc<GenericOtelClient>>,
    pub session_id: Option<String>,
    pub user_id: Option<String>,
}

impl OllamaTracingHelper for OllamaChatService {
    fn get_session_id(&self) -> &str {
        if let Some(s) = self.session_id.as_ref() {
            s
        } else {
            "default-session"
        }
    }
    fn get_user_id(&self) -> Option<&String> {
        self.user_id.as_ref()
    }
    fn get_otel_client(&self) -> Option<&Arc<GenericOtelClient>> {
        self.otel_client.as_ref()
    }
}

impl ThinkTagHelper for OllamaChatService {}

impl OllamaChatService {
    pub fn new(function_app: Arc<FunctionAppImpl>, settings: OllamaRunnerSettings) -> Result<Self> {
        let ollama = Arc::new(Ollama::try_new(
            settings
                .base_url
                .unwrap_or_else(|| "http://localhost:11434".to_string()),
        )?);

        Ok(Self {
            function_app,
            ollama,
            model: settings.model,
            system_prompt: settings.system_prompt,
            otel_client: Some(Arc::new(GenericOtelClient::new("ollama.chat_service"))),
            session_id: None,
            user_id: None,
        })
    }

    pub fn with_otel_client(mut self, client: Arc<GenericOtelClient>) -> Self {
        self.otel_client = Some(client);
        self
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn with_user_id(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = Some(user_id.into());
        self
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

    pub async fn request_chat(
        &self,
        args: LlmChatArgs,
        cx: opentelemetry::Context,
        metadata: HashMap<String, String>,
    ) -> Result<LlmChatResult> {
        let options = Self::create_chat_options(&args);
        let model = args.model.clone().unwrap_or_else(|| self.model.clone());
        let mut messages = Self::convert_messages(&args);

        // Add system prompt if exists
        if let Some(system_prompt) = self.system_prompt.clone() {
            messages.retain(|m| m.role != MessageRole::System);
            messages.insert(0, ChatMessage::new(MessageRole::System, system_prompt));
        }

        let tools = Arc::new(self.function_list(&args).await?);

        // Use the instance methods for tracing-enabled internal call
        let res = Self::request_chat_internal_with_tracing(
            Arc::new(self.clone()),
            model.clone(),
            options,
            Arc::new(Mutex::new(messages)),
            tools.clone(),
            Some(cx), // No parent context for initial call
            metadata,
        )
        .await?;

        let text = res.message.content.clone();
        let (prompt, think) = Self::divide_think_tag(text);

        let chat_result = LlmChatResult {
            content: Some(llm::llm_chat_result::MessageContent {
                content: Some(message_content::Content::Text(prompt)),
            }),
            reasoning_content: think,
            done: true,
            usage: None,
        };

        // Record usage if available
        if let Some(final_data) = &res.final_data {
            let content = chat_result
                .content
                .as_ref()
                .and_then(|c| c.content.as_ref())
                .and_then(|content| match content {
                    message_content::Content::Text(text) => Some(text.as_str()),
                    _ => None,
                });
            let _ = self.trace_usage("ollama.usage", final_data, content).await;
        }

        Ok(chat_result)
    }
    async fn request_chat_internal_with_tracing(
        self: Arc<Self>,
        model: String,
        options: ModelOptions,
        messages: Arc<Mutex<Vec<ChatMessage>>>,
        tools: Arc<Vec<ToolInfo>>,
        parent_context: Option<opentelemetry::Context>,
        metadata: HashMap<String, String>,
    ) -> Result<ChatMessageResponse> {
        let mut req = ChatMessageRequest::new(model.clone(), messages.lock().await.clone());
        req = req.options(options.clone());
        if tools.is_empty() {
            tracing::debug!("No tools found");
        } else {
            tracing::debug!("Tools found: {:#?}", &tools);
            req = req.tools((*tools).clone());
        }

        let ollama_clone = self.ollama.clone();
        // closure of Execute chat API call
        let chat_api_action = async move {
            ollama_clone
                .send_chat_messages(req)
                .await
                .map_err(|e| JobWorkerError::OtherError(format!("Chat API error: {}", e)))
        };

        // Execute chat API call and get both result and context
        let (res, current_context) = if let Some(_otel_client) = self.get_otel_client() {
            // Create span attributes for chat API call
            let span_attributes = self
                .create_chat_span_from_request(&model, messages.clone(), &options, &tools)
                .await;

            // Execute chat API call with response tracing, getting both result and context
            self.with_chat_response_tracing(
                parent_context.clone(),
                span_attributes,
                chat_api_action,
            )
            .await?
        } else {
            let result = chat_api_action.await?;
            let context = opentelemetry::Context::current();
            (result, context)
        };

        tracing::debug!("Ollama chat response: {:#?}", &res);

        if res.message.tool_calls.is_empty() {
            tracing::debug!("No tool calls in response");
            Ok(res)
        } else {
            tracing::debug!("Tool calls in response: {:#?}", &res.message.tool_calls);

            // Process tool calls with hierarchical tracing (child spans)
            let tool_calls = res.message.tool_calls.clone();

            // Process tool calls and get updated context for each tool call
            let mut updated_context = current_context;
            if self.get_otel_client().is_some() {
                // Process tool calls with tracing using current context as parent
                updated_context = self
                    .process_tool_calls_with_tracing(
                        messages.clone(),
                        &tool_calls,
                        Some(updated_context),
                        metadata.clone(),
                    )
                    .await?;
            } else {
                self.process_tool_calls_without_tracing(
                    messages.clone(),
                    &tool_calls,
                    metadata.clone(),
                )
                .await?;
                // Keep current context unchanged when not tracing
                // updated_context = current_context;
            }

            // Recursive call with updated context from tool execution
            Box::pin(self.request_chat_internal_with_tracing(
                model,
                options,
                messages,
                tools,
                Some(updated_context),
                metadata,
            ))
            .await
        }
    }

    async fn process_tool_calls_with_tracing(
        &self,
        messages: Arc<Mutex<Vec<ChatMessage>>>,
        tool_calls: &[ollama_rs::generation::tools::ToolCall],
        parent_context: Option<opentelemetry::Context>,
        metadata: HashMap<String, String>,
    ) -> Result<opentelemetry::Context> {
        if parent_context.is_none() && self.get_otel_client().is_some() {
            tracing::warn!("No parent context provided for tool calls, using current context");
        }
        let mut current_context = parent_context.unwrap_or_else(opentelemetry::Context::current);

        for call in tool_calls {
            tracing::debug!("Tool call: {:?}", call.function);

            // Clone necessary data to avoid lifetime issues
            let function_name = call.function.name.clone();
            let arguments = call.function.arguments.clone();
            let function_app = self.function_app.clone();

            let metadata_clone = metadata.clone();
            let tool_action = async move {
                function_app
                    .call_function(
                        metadata_clone,
                        &function_name,
                        arguments.as_object().cloned(),
                    )
                    .await
                    .map_err(|e| JobWorkerError::OtherError(format!("Tool execution error: {}", e)))
            };

            // Execute individual tool call as child span and get updated context
            let tool_attributes = self.create_tool_call_span_from_call(call);

            // Execute tool call with response tracing and get both result and updated context
            let (tool_result, updated_context) = self
                .with_tool_response_tracing(current_context, tool_attributes, call, tool_action)
                .await?;

            tracing::debug!("Tool response: {}", &tool_result);
            messages
                .lock()
                .await
                .push(ChatMessage::tool(tool_result.to_string()));

            // Update context for next tool call
            current_context = updated_context;
        }
        Ok(current_context)
    }

    async fn process_tool_calls_without_tracing(
        &self,
        messages: Arc<Mutex<Vec<ChatMessage>>>,
        tool_calls: &[ollama_rs::generation::tools::ToolCall],
        metadata: HashMap<String, String>,
    ) -> Result<()> {
        for call in tool_calls {
            tracing::debug!("Tool call: {:?}", call.function);

            let tool_result = self
                .function_app
                .call_function(
                    metadata.clone(),
                    call.function.name.as_str(),
                    call.function.arguments.as_object().cloned(),
                )
                .await?;

            tracing::debug!("Tool response: {}", &tool_result);
            messages
                .lock()
                .await
                .push(ChatMessage::tool(tool_result.to_string()));
        }
        Ok(())
    }

    pub async fn request_stream_chat(
        &self,
        args: LlmChatArgs,
    ) -> Result<BoxStream<'static, LlmChatResult>> {
        let options = Self::create_chat_options(&args);
        let model_name = args.model.clone().unwrap_or_else(|| self.model.clone());
        let messages = Self::convert_messages(&args);

        let mut req = ChatMessageRequest::new(model_name.clone(), messages.clone());
        req = req.options(options);

        if let Some(system_prompt) = self.system_prompt.clone() {
            req = req.template(system_prompt);
        }

        // Clone the ollama instance to avoid borrowing self
        let ollama = self.ollama.clone();
        let stream = ollama
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
