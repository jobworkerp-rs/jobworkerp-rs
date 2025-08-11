use super::super::tracing::ollama_helper::OllamaTracingHelper;
use super::conversion::ToolConverter;
use crate::llm::tracing::LLMTracingHelper;
use crate::llm::ThinkTagHelper;
use anyhow::{anyhow, Result};
use app::app::function::{FunctionApp, FunctionAppImpl};
use futures::stream::BoxStream;
use futures::StreamExt;
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content;
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::OllamaRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::{self, LlmChatArgs, LlmChatResult};
use net_utils::trace::impls::GenericOtelClient;
use net_utils::trace::otel_span::GenAIOtelClient;
use ollama_rs::generation::chat::ChatMessageResponse;
use ollama_rs::generation::parameters::{FormatType, JsonStructure};
use ollama_rs::generation::tools::ToolInfo;
use ollama_rs::{
    generation::chat::{request::ChatMessageRequest, ChatMessage, MessageRole},
    models::ModelOptions,
    Ollama,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
// use tokio::sync::Mutex;

#[derive(Debug, Clone)]
pub struct OllamaChatService {
    pub function_app: Arc<FunctionAppImpl>,
    pub ollama: Arc<Ollama>,
    pub model: String,
    pub system_prompt: Option<String>,
    pub otel_client: Option<Arc<GenericOtelClient>>,
}

impl OllamaTracingHelper for OllamaChatService {}

impl crate::llm::tracing::LLMTracingHelper for OllamaChatService {
    fn get_otel_client(&self) -> Option<&Arc<GenericOtelClient>> {
        self.otel_client.as_ref()
    }

    fn get_provider_name(&self) -> &str {
        "ollama"
    }

    fn get_default_model(&self) -> String {
        self.model.clone()
    }
}

impl super::super::generic_tracing_helper::GenericLLMTracingHelper for OllamaChatService {
    fn get_otel_client(&self) -> Option<&Arc<GenericOtelClient>> {
        self.otel_client.as_ref()
    }

    fn convert_messages_to_input(
        &self,
        messages: &[impl super::super::generic_tracing_helper::LLMMessage],
    ) -> serde_json::Value {
        use super::super::tracing::ollama_helper::OllamaTracingHelper;
        let ollama_messages: Vec<ChatMessage> = messages
            .iter()
            .map(|m| ChatMessage {
                role: match m.get_role() {
                    "user" => MessageRole::User,
                    "assistant" => MessageRole::Assistant,
                    "system" => MessageRole::System,
                    "tool" => MessageRole::Tool,
                    unknown_role => {
                        tracing::warn!(
                            "Unknown role string '{}', defaulting to User",
                            unknown_role
                        );
                        MessageRole::User
                    }
                },
                content: m.get_content().to_string(),
                tool_calls: vec![],
                images: Some(vec![]),
                thinking: None,
            })
            .collect();
        Self::convert_messages_to_input_ollama(&ollama_messages)
    }

    fn get_provider_name(&self) -> &str {
        "ollama"
    }
}

impl ThinkTagHelper for OllamaChatService {}

// TODO set from job.timeout
const DEFAULT_TIMEOUT_SEC: u32 = 300; // Default timeout for Ollama chat requests in seconds

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
        })
    }

    pub fn with_otel_client(mut self, client: Arc<GenericOtelClient>) -> Self {
        self.otel_client = Some(client);
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
            ChatRole::System => MessageRole::System,
            ChatRole::User => MessageRole::User,
            ChatRole::Assistant => MessageRole::Assistant,
            ChatRole::Tool => MessageRole::Tool,
            _ => {
                tracing::warn!("Unknown ChatRole {:?}, defaulting to User", role);
                MessageRole::User
            }
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
                    thinking: None, // TODO String? bool? args.options.and_then(|o| o.extract_reasoning_content),
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
        let metadata = Arc::new(metadata);
        let options = Self::create_chat_options(&args);
        let model = args.model.clone().unwrap_or_else(|| self.model.clone());
        let mut messages = Self::convert_messages(&args);

        // Add system prompt if exists
        if let Some(system_prompt) = self.system_prompt.clone() {
            messages.retain(|m| m.role != MessageRole::System);
            messages.insert(0, ChatMessage::new(MessageRole::System, system_prompt));
        }

        let tools = Arc::new(self.function_list(&args).await?);
        let messages = Arc::new(Mutex::new(messages));

        // Use internal method with tool call support
        let res = Self::request_chat_internal_with_tracing(
            Arc::new(self.clone()),
            model,
            options,
            messages,
            tools,
            Some(cx.clone()),
            metadata.clone(),
            args.json_schema,
        )
        .await?;

        // Convert response to LlmChatResult
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

        Ok(chat_result)
    }

    #[allow(clippy::too_many_arguments)]
    async fn request_chat_internal_with_tracing(
        self: Arc<Self>,
        model: String,
        options: ModelOptions,
        messages: Arc<Mutex<Vec<ChatMessage>>>,
        tools: Arc<Vec<ToolInfo>>,
        parent_context: Option<opentelemetry::Context>,
        metadata: Arc<HashMap<String, String>>,
        json_schema: Option<String>,
    ) -> Result<ChatMessageResponse> {
        let mut req = ChatMessageRequest::new(model.clone(), messages.lock().await.clone());
        req = req.options(options.clone());

        if let Some(schema_str) = json_schema {
            match serde_json::from_str(&schema_str) {
                Ok(schema) => {
                    let format =
                        FormatType::StructuredJson(Box::new(JsonStructure::new_for_schema(schema)));
                    req = req.format(format);
                    tracing::debug!("Applied JSON schema format: {}", schema_str);
                }
                Err(e) => {
                    tracing::warn!("Invalid JSON schema, ignoring format: {}", e);
                }
            }
        }

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
                .map_err(|e| JobWorkerError::OtherError(format!("Chat API error: {e}")))
        };

        // Execute chat API call and get both result and context
        let (res, current_context) = if let Some(otel_client) = self.get_otel_client() {
            // Create span attributes for chat API call
            let span_attributes = self
                .create_chat_span_from_request(
                    &model,
                    messages.clone(),
                    &options,
                    &tools,
                    &metadata,
                )
                .await;

            // Use provided parent_context or current context as parent for the span
            let parent_ctx = parent_context.unwrap_or_else(opentelemetry::Context::current);

            // Execute chat API call with tracing span
            let result = otel_client
                .with_span_result(span_attributes, Some(parent_ctx.clone()), chat_api_action)
                .await?;
            let context = parent_ctx;

            (result, context)
        } else {
            let result = chat_api_action.await?;
            // Use provided parent_context or current context
            let context = parent_context.unwrap_or_else(opentelemetry::Context::current);
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
            let result = Box::pin(self.request_chat_internal_with_tracing(
                model,
                options,
                messages,
                tools,
                Some(updated_context),
                metadata,
                None, // json_schema is not used in recursive calls to avoid conflicts
            ))
            .await;
            result
        }
    }

    async fn process_tool_calls_with_tracing(
        &self,
        messages: Arc<Mutex<Vec<ChatMessage>>>,
        tool_calls: &[ollama_rs::generation::tools::ToolCall],
        parent_context: Option<opentelemetry::Context>,
        metadata: Arc<HashMap<String, String>>,
    ) -> Result<opentelemetry::Context> {
        if parent_context.is_none() && self.get_otel_client().is_some() {
            tracing::warn!("No parent context provided for tool calls, using current context");
        }
        let mut current_context = parent_context.unwrap_or_else(opentelemetry::Context::current);

        for call in tool_calls.iter() {
            tracing::debug!("Tool call: {:?}", call.function);
            tracing::debug!("Tool arguments: {:?}", call.function.arguments);
            tracing::debug!(
                "Tool arguments as object: {:?}",
                call.function.arguments.as_object()
            );

            // Clone necessary data to avoid lifetime issues
            let function_name = call.function.name.clone();
            let arguments = call.function.arguments.clone();
            let function_app = self.function_app.clone();

            let metadata_clone = metadata.clone();
            let tool_action = async move {
                // Handle empty or null arguments by providing an empty object
                let arguments_obj = arguments.as_object().cloned().unwrap_or_else(|| {
                    tracing::debug!("Tool call has null arguments, using empty object");
                    serde_json::Map::new()
                });

                // Execute tool and convert any error to a string result for LLM to handle
                let result = function_app
                    .call_function_for_llm(
                        metadata_clone,
                        &function_name,
                        Some(arguments_obj),
                        DEFAULT_TIMEOUT_SEC,
                    )
                    .await;

                let tool_result = match result {
                    Ok(success_result) => success_result,
                    Err(error) => {
                        // Return error as tool result for LLM to process
                        serde_json::Value::String(format!(
                            "Error executing tool '{function_name}': {error}"
                        ))
                    }
                };

                Ok(tool_result) // Always return Ok so processing continues
            };

            // Execute individual tool call as child span and get updated context
            let tool_attributes = self.create_tool_call_span_from_call(call, &metadata);

            // Execute tool call with response tracing and get both result and updated context
            let (tool_result, updated_context) = OllamaTracingHelper::with_tool_response_tracing(
                self,
                &metadata,
                current_context,
                tool_attributes,
                call,
                tool_action,
            )
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
        metadata: Arc<HashMap<String, String>>,
    ) -> Result<()> {
        for call in tool_calls {
            tracing::debug!("Tool call: {:?}", call.function);
            tracing::debug!("Tool arguments: {:?}", call.function.arguments);
            tracing::debug!(
                "Tool arguments as object: {:?}",
                call.function.arguments.as_object()
            );

            // Handle empty or null arguments by providing an empty object
            let arguments_obj = call
                .function
                .arguments
                .as_object()
                .cloned()
                .unwrap_or_else(|| {
                    tracing::debug!("Tool call has null arguments, using empty object");
                    serde_json::Map::new()
                });

            // Execute tool and convert any error to a string result for LLM to handle
            let result = self
                .function_app
                .call_function_for_llm(
                    metadata.clone(),
                    call.function.name.as_str(),
                    Some(arguments_obj),
                    DEFAULT_TIMEOUT_SEC,
                )
                .await;

            let tool_result = match result {
                Ok(success_result) => {
                    tracing::debug!("Tool execution succeeded: {}", &success_result);
                    success_result
                }
                Err(error) => {
                    tracing::info!(
                        "Tool execution failed for: {}, error: {}",
                        call.function.name,
                        error
                    );
                    // Return error as tool result for LLM to process
                    serde_json::Value::String(format!(
                        "Error executing tool '{}': {}",
                        call.function.name, error
                    ))
                }
            };

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
