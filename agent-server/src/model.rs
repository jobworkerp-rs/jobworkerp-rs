//! Production adapter from the chat model boundary to an LLM Worker.

use std::sync::{Arc, Mutex};

use crate::chat::{
    ChatMessage, ModelInvocation, ModelInvoker, ModelJobObserver, ModelResponse, Role, ToolCall,
};
use crate::grpc::{
    OutputChunk, OutputKind, StreamChunkObserver, ToolExecutionResult, ToolExecutor, ToolInvocation,
};
use async_trait::async_trait;
use serde_json::{Map, Value, json};

const LLM_RUN_METHOD: &str = "run";

/// Receives one validated text delta synchronously; implementations should enqueue without waiting.
pub type ModelTextDeltaCallback = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync + 'static>;

/// Invokes the `run` method on one request-selected LLM Worker using manual tool mode.
pub struct GrpcModelInvoker {
    executor: Arc<dyn ToolExecutor>,
}

impl GrpcModelInvoker {
    /// Keep the existing wiring shape; each invocation supplies its authoritative Worker ID so a
    /// restored continuation cannot fall back to constructor-time selection.
    pub fn new(_llm_worker_id: i64, executor: Arc<dyn ToolExecutor>) -> Self {
        Self { executor }
    }

    async fn invoke_inner(
        &self,
        invocation: ModelInvocation,
        observer: Option<Arc<dyn ModelJobObserver>>,
        text_delta_callback: Option<ModelTextDeltaCallback>,
    ) -> Result<ModelResponse, String> {
        if invocation.llm_worker_id <= 0 {
            return Err("LLM Worker ID must be positive".to_owned());
        }
        let arguments = to_runner_arguments(&invocation)?;
        let tool_invocation = ToolInvocation {
            worker_id: invocation.llm_worker_id,
            using: Some(LLM_RUN_METHOD.to_owned()),
            arguments,
            stream: true,
        };
        let emitted_text = Arc::new(Mutex::new(String::new()));
        let chunk_observer: Option<StreamChunkObserver> =
            text_delta_callback.as_ref().map(|callback| {
                let callback = callback.clone();
                let emitted_text = emitted_text.clone();
                Arc::new(move |chunk: &OutputChunk| {
                    let Some(text) = text_delta_from_chunk(chunk)? else {
                        return Ok(());
                    };
                    callback(&text)?;
                    emitted_text
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push_str(&text);
                    Ok(())
                }) as StreamChunkObserver
            });
        let execution = if observer.is_some() || chunk_observer.is_some() {
            self.executor
                .execute_with_observers(tool_invocation, observer, chunk_observer)
                .await
        } else {
            self.executor.execute(tool_invocation).await
        }
        .map_err(|error| format!("LLM Worker execution failed: {error:#}"))?;
        let response = response_from_execution(execution, invocation.llm_worker_id)?;
        if text_delta_callback.is_some() {
            verify_emitted_text(&response, &emitted_text)?;
        }
        Ok(response)
    }

    /// Invokes with live text deltas while preserving the ordinary final response.
    pub async fn invoke_with_text_delta(
        &self,
        invocation: ModelInvocation,
        callback: ModelTextDeltaCallback,
    ) -> Result<ModelResponse, String> {
        self.invoke_inner(invocation, None, Some(callback)).await
    }

    /// Variant used by chat orchestration when it also needs model-job lifecycle notifications.
    pub async fn invoke_with_observer_and_text_delta(
        &self,
        invocation: ModelInvocation,
        observer: Arc<dyn ModelJobObserver>,
        callback: ModelTextDeltaCallback,
    ) -> Result<ModelResponse, String> {
        self.invoke_inner(invocation, Some(observer), Some(callback))
            .await
    }
}

#[async_trait]
impl ModelInvoker for GrpcModelInvoker {
    async fn invoke(&self, invocation: ModelInvocation) -> Result<ModelResponse, String> {
        self.invoke_inner(invocation, None, None).await
    }

    async fn invoke_with_observer(
        &self,
        invocation: ModelInvocation,
        observer: Arc<dyn ModelJobObserver>,
    ) -> Result<ModelResponse, String> {
        self.invoke_inner(invocation, Some(observer), None).await
    }

    async fn invoke_with_observer_and_text_delta(
        &self,
        invocation: ModelInvocation,
        observer: Arc<dyn ModelJobObserver>,
        callback: ModelTextDeltaCallback,
    ) -> Result<ModelResponse, String> {
        GrpcModelInvoker::invoke_with_observer_and_text_delta(self, invocation, observer, callback)
            .await
    }

    async fn cancel_job(&self, job_id: &str) -> Result<(), String> {
        self.executor.cancel_job(job_id).await
    }
}

fn to_runner_arguments(invocation: &ModelInvocation) -> Result<Value, String> {
    if invocation.is_auto_calling {
        return Err(
            "automatic tool calling is not supported by the Agent Server bridge".to_owned(),
        );
    }
    if invocation.function_set_name.is_some() {
        return Err("FunctionSet selection is not supported by the Agent Server bridge".to_owned());
    }
    if invocation.history.is_empty() {
        return Err("LLM chat history must contain at least one message".to_owned());
    }

    let client_tools: Value = serde_json::from_str(&invocation.client_tools_json)
        .map_err(|error| format!("client_tools_json is invalid JSON: {error}"))?;
    if !client_tools.is_array() {
        return Err("client_tools_json must be a JSON array".to_owned());
    }

    let mut messages = Vec::with_capacity(invocation.history.len());
    for (index, message) in invocation.history.iter().enumerate() {
        messages.push(to_runner_message(&invocation.history, index, message)?);
    }

    let mut options = Map::new();
    if let Some(max_tokens) = invocation.options.max_tokens {
        options.insert("maxTokens".to_owned(), json!(max_tokens));
    }
    if let Some(temperature) = invocation.options.temperature {
        options.insert("temperature".to_owned(), json!(temperature));
    }
    if let Some(top_p) = invocation.options.top_p {
        options.insert("topP".to_owned(), json!(top_p));
    }

    let mut arguments = Map::new();
    arguments.insert(
        "functionOptions".to_owned(),
        json!({
            "useFunctionCalling": true,
            "isAutoCalling": false,
            "clientToolsJson": invocation.client_tools_json,
        }),
    );
    arguments.insert("messages".to_owned(), Value::Array(messages));
    if !options.is_empty() {
        arguments.insert("options".to_owned(), Value::Object(options));
    }
    Ok(Value::Object(arguments))
}

fn to_runner_message(
    history: &[ChatMessage],
    index: usize,
    message: &ChatMessage,
) -> Result<Value, String> {
    if message.tool_execution_requests.is_some() {
        return Err(format!(
            "history message {index} contains unsupported tool_execution_requests"
        ));
    }
    if contains_execution_directive(&message.content)
        || message
            .tool_calls
            .iter()
            .any(|call| contains_execution_directive(&call.arguments))
        || message
            .tool_results
            .iter()
            .any(|result| contains_execution_directive(&result.content))
    {
        return Err(format!(
            "history message {index} contains an unsupported tool execution directive"
        ));
    }

    let role = match message.role {
        Role::System => "SYSTEM",
        Role::User => "USER",
        Role::Assistant => "ASSISTANT",
        Role::Tool => "TOOL",
    };

    let content = match message.role {
        Role::System => {
            if !message.tool_calls.is_empty() || !message.tool_results.is_empty() {
                return Err(format!("system message {index} cannot contain tool data"));
            }
            to_text_content(&message.content, index)?
        }
        Role::User => {
            if !message.tool_calls.is_empty() || !message.tool_results.is_empty() {
                return Err(format!("user message {index} cannot contain tool data"));
            }
            to_text_or_image_content(&message.content, index)?
        }
        Role::Assistant => {
            if !message.tool_results.is_empty() {
                return Err(format!(
                    "assistant message {index} cannot contain tool results"
                ));
            }
            if message.tool_calls.is_empty() {
                to_text_or_image_content(&message.content, index)?
            } else {
                if !message.content.is_null() {
                    return Err(format!(
                        "assistant message {index} cannot combine tool calls with separate content"
                    ));
                }
                let mut seen_call_ids = std::collections::HashSet::new();
                let mut calls = Vec::with_capacity(message.tool_calls.len());
                for call in &message.tool_calls {
                    if !seen_call_ids.insert(call.call_id.as_str()) {
                        return Err(format!(
                            "assistant message {index} contains duplicate call_id `{}`",
                            call.call_id
                        ));
                    }
                    calls.push(to_runner_tool_call(call, index)?);
                }
                Some(json!({"toolCalls": {"calls": calls}}))
            }
        }
        Role::Tool => {
            if !message.tool_calls.is_empty() {
                return Err(format!("tool message {index} cannot contain tool calls"));
            }
            if message.tool_results.is_empty() {
                return Err(format!("tool message {index} has no tool_results"));
            }
            if !message.content.is_null()
                && (message.tool_results.len() != 1
                    || message.content != message.tool_results[0].content)
            {
                return Err(format!(
                    "tool message {index} has content that cannot be represented alongside its tool_results"
                ));
            }
            validate_results_follow_assistant(history, index, message)?;
            let results = message
                .tool_results
                .iter()
                .map(|result| {
                    if result.call_id.is_empty() {
                        return Err(format!(
                            "tool message {index} contains a tool result with an empty call_id"
                        ));
                    }
                    let content = tool_result_content(&result.content).map_err(|error| {
                        format!("tool message {index} result content is not JSON: {error}")
                    })?;
                    Ok(json!({
                        "callId": result.call_id,
                        "fnName": result.name,
                        "content": content,
                        "isError": result.is_error,
                    }))
                })
                .collect::<Result<Vec<_>, String>>()?;
            Some(json!({"toolResults": {"results": results}}))
        }
    };

    let mut runner_message = Map::new();
    runner_message.insert("role".to_owned(), Value::String(role.to_owned()));
    if let Some(content) = content {
        runner_message.insert("content".to_owned(), content);
    }
    Ok(Value::Object(runner_message))
}

fn tool_result_content(content: &Value) -> Result<String, serde_json::Error> {
    match content {
        Value::String(text) => Ok(text.clone()),
        _ => serde_json::to_string(content),
    }
}

fn validate_results_follow_assistant(
    history: &[ChatMessage],
    index: usize,
    message: &ChatMessage,
) -> Result<(), String> {
    let preceding = history[..index]
        .iter()
        .rev()
        .find(|candidate| candidate.role != Role::Tool)
        .filter(|candidate| candidate.role == Role::Assistant)
        .ok_or_else(|| {
            format!(
                "tool message {index} must follow an assistant message containing matching tool calls"
            )
        })?;
    let mut seen = std::collections::HashSet::new();
    for result in &message.tool_results {
        if !seen.insert(result.call_id.as_str()) {
            return Err(format!(
                "tool message {index} contains duplicate result call_id `{}`",
                result.call_id
            ));
        }
        let Some(call) = preceding
            .tool_calls
            .iter()
            .find(|call| call.call_id == result.call_id)
        else {
            return Err(format!(
                "tool message {index} result call_id `{}` does not match the preceding assistant tool calls",
                result.call_id
            ));
        };
        if !result.name.is_empty() && result.name != call.name {
            return Err(format!(
                "tool message {index} result name does not match call_id `{}`",
                result.call_id
            ));
        }
    }
    Ok(())
}

fn to_runner_tool_call(call: &ToolCall, index: usize) -> Result<Value, String> {
    if call.call_id.is_empty() {
        return Err(format!(
            "assistant message {index} contains a tool call with an empty call_id"
        ));
    }
    if call.name.trim().is_empty() {
        return Err(format!(
            "assistant message {index} contains a tool call with an empty function name"
        ));
    }
    let arguments = call
        .arguments
        .as_object()
        .ok_or_else(|| "assistant tool call fn_arguments must be a JSON object".to_owned())?;
    let arguments = serde_json::to_string(arguments)
        .map_err(|error| format!("assistant tool call arguments are invalid JSON: {error}"))?;
    Ok(json!({
        "callId": call.call_id,
        "fnName": call.name,
        "fnArguments": arguments,
    }))
}

fn to_text_content(content: &Value, index: usize) -> Result<Option<Value>, String> {
    match content {
        Value::Null => Ok(None),
        Value::String(text) => Ok(Some(json!({"text": text}))),
        Value::Object(object) if object.len() == 1 && object.get("text").is_some() => {
            let text = object["text"]
                .as_str()
                .ok_or_else(|| format!("history message {index} text content must be a string"))?;
            Ok(Some(json!({"text": text})))
        }
        _ => Err(format!(
            "history message {index} content is not supported as text"
        )),
    }
}

fn to_text_or_image_content(content: &Value, index: usize) -> Result<Option<Value>, String> {
    if let Some(image) = normalize_image_content(content)? {
        return Ok(Some(json!({"image": image})));
    }
    to_text_content(content, index)
}

fn normalize_image_content(content: &Value) -> Result<Option<Value>, String> {
    let Some(object) = content.as_object() else {
        return Ok(None);
    };
    let Some(image) = object.get("image") else {
        return Ok(None);
    };
    if object.len() != 1 {
        return Err("image content cannot include additional content fields".to_owned());
    }
    let image = image
        .as_object()
        .ok_or_else(|| "image content must be a JSON object".to_owned())?;
    if image.len() != 2 || !image.contains_key("contentType") || !image.contains_key("source") {
        return Err("image content requires only contentType and source".to_owned());
    }
    let content_type = image["contentType"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "image contentType must be a non-empty string".to_owned())?;
    let source = image["source"]
        .as_object()
        .ok_or_else(|| "image source must be a JSON object".to_owned())?;
    if source.len() != 1 {
        return Err("image source must contain exactly one URL or base64 value".to_owned());
    }
    let source = if source.contains_key("url") {
        return Err("URL image sources are not supported by the Agent Server".to_owned());
    } else if let Some(base64) = source.get("base64") {
        let base64 = base64
            .as_str()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "image source base64 must be a non-empty string".to_owned())?;
        json!({"base64": base64})
    } else {
        return Err("image source must contain a URL or base64 value".to_owned());
    };
    Ok(Some(json!({"contentType": content_type, "source": source})))
}

fn contains_execution_directive(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            object
                .keys()
                .any(|key| key == "tool_execution_requests" || key == "toolExecutionRequests")
                || object.values().any(contains_execution_directive)
        }
        Value::Array(values) => values.iter().any(contains_execution_directive),
        _ => false,
    }
}

fn text_delta_from_chunk(chunk: &OutputChunk) -> Result<Option<String>, String> {
    if chunk.kind != OutputKind::Data {
        return Ok(None);
    }
    let data = chunk
        .json
        .as_ref()
        .ok_or_else(|| "LLM Worker DATA chunk did not decode to JSON".to_owned())?;
    let parsed = parse_chat_result(data, false)?;
    if parsed.done || !parsed.response.tool_calls.is_empty() {
        return Ok(None);
    }
    Ok(parsed
        .response
        .content
        .as_str()
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned))
}

fn verify_emitted_text(
    response: &ModelResponse,
    emitted_text: &Mutex<String>,
) -> Result<(), String> {
    let emitted_text = emitted_text
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // A provider may only send a FinalCollected result. In that case there is nothing to compare.
    if emitted_text.is_empty() {
        return Ok(());
    }
    // Live text before a manual tool call is ephemeral commentary, not assistant history content.
    if !response.tool_calls.is_empty() {
        return Ok(());
    }
    let final_text = response.content.as_str().ok_or_else(|| {
        "streamed LLM text deltas do not match the FinalCollected text".to_owned()
    })?;
    if !final_text.starts_with(emitted_text.as_str()) {
        return Err("streamed LLM text deltas do not match the FinalCollected text".to_owned());
    }
    Ok(())
}

fn response_from_execution(
    execution: ToolExecutionResult,
    expected_worker_id: i64,
) -> Result<ModelResponse, String> {
    if execution.worker_id != expected_worker_id {
        return Err(format!(
            "LLM Worker result came from worker {}, expected {expected_worker_id}",
            execution.worker_id
        ));
    }
    if execution.using != LLM_RUN_METHOD {
        return Err(format!(
            "LLM Worker result used method `{}`, expected `{LLM_RUN_METHOD}`",
            execution.using
        ));
    }
    if execution.job_id <= 0 {
        return Err("LLM Worker result has an invalid job ID".to_owned());
    }
    // ModelResponse has no job/cancellation metadata slot; the backend must own that lifecycle.

    if let Some(result) = execution.result {
        if !execution.chunks.is_empty() {
            return Err("LLM Worker returned both a direct result and stream chunks".to_owned());
        }
        return parse_chat_result(&result, true).map(|parsed| parsed.response);
    }
    if execution.raw_result.is_some() {
        return Err("LLM Worker returned raw result bytes without decoded JSON".to_owned());
    }

    let mut final_result = None;
    let mut saw_final_collected = false;
    for (index, chunk) in execution.chunks.iter().enumerate() {
        let data = chunk
            .json
            .as_ref()
            .ok_or_else(|| format!("LLM Worker stream chunk {index} did not decode to JSON"))?;
        match &chunk.kind {
            OutputKind::Data => {
                if saw_final_collected {
                    return Err(format!(
                        "LLM Worker stream contained a DATA chunk after FinalCollected at chunk {index}"
                    ));
                }
                parse_chat_result(data, false).map_err(|error| {
                    format!("LLM Worker DATA chunk {index} is invalid: {error}")
                })?;
            }
            OutputKind::FinalCollected => {
                if saw_final_collected {
                    return Err(
                        "LLM Worker stream contained more than one FinalCollected chunk".to_owned(),
                    );
                }
                saw_final_collected = true;
                final_result = Some(parse_chat_result(data, true).map_err(|error| {
                    format!("LLM Worker FinalCollected result is invalid: {error}")
                })?);
            }
        }
    }
    final_result
        .map(|result| result.response)
        .ok_or_else(|| "LLM Worker stream ended without a FinalCollected result".to_owned())
}

struct ParsedChatResult {
    response: ModelResponse,
    done: bool,
}

enum ResultContent {
    Text(String),
    Image(Value),
    ToolCalls(Vec<ToolCall>),
}

fn parse_chat_result(value: &Value, require_done: bool) -> Result<ParsedChatResult, String> {
    let result = value
        .as_object()
        .ok_or_else(|| "LLMChatResult must be a JSON object".to_owned())?;
    const RESULT_FIELDS: &[&str] = &[
        "content",
        "reasoningContent",
        "done",
        "usage",
        "pendingToolCalls",
        "requiresToolExecution",
        "toolExecutionResults",
        "toolExecutionStarted",
    ];
    if let Some(field) = result
        .keys()
        .find(|field| !RESULT_FIELDS.contains(&field.as_str()))
    {
        return Err(format!(
            "LLMChatResult contains unsupported field `{field}`"
        ));
    }

    if let Some(reasoning) = result.get("reasoningContent")
        && !reasoning.is_null()
        && !reasoning.as_str().is_some_and(str::is_empty)
    {
        return Err("LLMChatResult contains unsupported reasoningContent".to_owned());
    }
    if let Some(usage) = result.get("usage") {
        validate_usage_metadata(usage)?;
    }
    if let Some(tool_results) = result.get("toolExecutionResults")
        && !tool_results.is_null()
        && !tool_results.as_array().is_some_and(Vec::is_empty)
    {
        return Err("LLMChatResult contains unsupported toolExecutionResults".to_owned());
    }
    if result
        .get("toolExecutionStarted")
        .is_some_and(|started| !started.is_null())
    {
        return Err("LLMChatResult contains unsupported toolExecutionStarted".to_owned());
    }

    let done = result
        .get("done")
        .map(|done| {
            done.as_bool()
                .ok_or_else(|| "LLMChatResult done must be a boolean".to_owned())
        })
        .transpose()?
        .unwrap_or(false);
    if require_done && !done {
        return Err("final LLMChatResult has done=false".to_owned());
    }

    let content = result
        .get("content")
        .filter(|content| !content.is_null())
        .map(parse_result_content)
        .transpose()?;
    let pending_calls = result
        .get("pendingToolCalls")
        .filter(|pending| !pending.is_null())
        .map(parse_pending_tool_calls)
        .transpose()?;
    let requires_execution = result
        .get("requiresToolExecution")
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| "LLMChatResult requiresToolExecution must be a boolean".to_owned())
        })
        .transpose()?;

    let content_calls = match &content {
        Some(ResultContent::ToolCalls(calls)) => Some(calls),
        _ => None,
    };
    let tool_calls = match (content_calls, pending_calls.as_ref()) {
        (Some(content), Some(pending)) if content != pending => {
            return Err(
                "LLMChatResult content toolCalls disagree with pendingToolCalls".to_owned(),
            );
        }
        (Some(content), _) => content.clone(),
        (None, Some(pending)) => pending.clone(),
        (None, None) => Vec::new(),
    };
    if requires_execution == Some(true) && tool_calls.is_empty() {
        return Err(
            "LLMChatResult requires tool execution but contains no pending tool calls".to_owned(),
        );
    }
    if requires_execution == Some(false) && !tool_calls.is_empty() {
        return Err(
            "LLMChatResult has pendingToolCalls with requiresToolExecution=false".to_owned(),
        );
    }
    if pending_calls.as_ref().is_some_and(Vec::is_empty) {
        return Err("LLMChatResult contains an empty pendingToolCalls list".to_owned());
    }

    let response_content = match content {
        Some(ResultContent::Text(text)) => {
            if !tool_calls.is_empty() {
                return Err(
                    "LLMChatResult cannot combine text content and pending tool calls".to_owned(),
                );
            }
            Value::String(text)
        }
        Some(ResultContent::Image(image)) => {
            if !tool_calls.is_empty() {
                return Err(
                    "LLMChatResult cannot combine image content and pending tool calls".to_owned(),
                );
            }
            image
        }
        Some(ResultContent::ToolCalls(_)) | None => Value::Null,
    };

    // Usage is execution metadata; ModelResponse intentionally carries only conversation content.
    Ok(ParsedChatResult {
        response: ModelResponse {
            content: response_content,
            tool_calls,
        },
        done,
    })
}

fn parse_result_content(value: &Value) -> Result<ResultContent, String> {
    let content = value
        .as_object()
        .ok_or_else(|| "LLMChatResult content must be a JSON object".to_owned())?;
    if content.len() != 1 {
        return Err("LLMChatResult content must have exactly one content variant".to_owned());
    }
    if let Some(text) = content.get("text") {
        return text
            .as_str()
            .map(|text| ResultContent::Text(text.to_owned()))
            .ok_or_else(|| "LLMChatResult text content must be a string".to_owned());
    }
    if let Some(image) = content.get("image") {
        let normalized = normalize_image_content(&json!({"image": image}))?
            .ok_or_else(|| "LLMChatResult image content is missing".to_owned())?;
        return Ok(ResultContent::Image(json!({"image": normalized})));
    }
    if let Some(tool_calls) = content.get("toolCalls") {
        return parse_tool_calls_container(tool_calls).map(ResultContent::ToolCalls);
    }
    Err("LLMChatResult contains unsupported content variant".to_owned())
}

fn validate_usage_metadata(value: &Value) -> Result<(), String> {
    if value.is_null() {
        return Ok(());
    }
    let usage = value
        .as_object()
        .ok_or_else(|| "LLMChatResult usage metadata must be an object".to_owned())?;
    const USAGE_FIELDS: &[&str] = &[
        "model",
        "promptTokens",
        "completionTokens",
        "totalPromptTimeSec",
        "totalCompletionTimeSec",
    ];
    if let Some(field) = usage
        .keys()
        .find(|field| !USAGE_FIELDS.contains(&field.as_str()))
    {
        return Err(format!(
            "LLMChatResult usage contains unsupported field `{field}`"
        ));
    }
    if usage.get("model").is_some_and(|model| !model.is_string())
        || ["promptTokens", "completionTokens"]
            .iter()
            .any(|key| usage.get(*key).is_some_and(|value| !value.is_u64()))
        || ["totalPromptTimeSec", "totalCompletionTimeSec"]
            .iter()
            .any(|key| usage.get(*key).is_some_and(|value| !value.is_number()))
    {
        return Err("LLMChatResult usage metadata has invalid field types".to_owned());
    }
    Ok(())
}

fn parse_pending_tool_calls(value: &Value) -> Result<Vec<ToolCall>, String> {
    parse_tool_call_envelope(value, "pendingToolCalls")
}

fn parse_tool_calls_container(value: &Value) -> Result<Vec<ToolCall>, String> {
    parse_tool_call_envelope(value, "toolCalls")
}

fn parse_tool_call_envelope(value: &Value, field: &str) -> Result<Vec<ToolCall>, String> {
    let envelope = value
        .as_object()
        .ok_or_else(|| format!("LLMChatResult {field} must be an object"))?;
    if envelope.len() != 1 || !envelope.contains_key("calls") {
        return Err(format!("LLMChatResult {field} must contain only calls"));
    }
    parse_tool_call_array(&envelope["calls"])
}

fn parse_tool_call_array(value: &Value) -> Result<Vec<ToolCall>, String> {
    let calls = value
        .as_array()
        .ok_or_else(|| "LLMChatResult tool call calls must be an array".to_owned())?;
    if calls.is_empty() {
        return Err("LLMChatResult tool call list must not be empty".to_owned());
    }
    let mut seen = std::collections::HashSet::new();
    calls
        .iter()
        .enumerate()
        .map(|(index, call)| {
            let call = call
                .as_object()
                .ok_or_else(|| format!("LLMChatResult tool call {index} must be an object"))?;
            if call.len() != 3
                || !call.contains_key("callId")
                || !call.contains_key("fnName")
                || !call.contains_key("fnArguments")
            {
                return Err(format!(
                    "LLMChatResult tool call {index} requires only callId, fnName, and fnArguments"
                ));
            }
            let call_id = call["callId"]
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    format!("LLMChatResult tool call {index} callId must be non-empty")
                })?;
            if !seen.insert(call_id) {
                return Err(format!(
                    "LLMChatResult contains duplicate tool call ID `{call_id}`"
                ));
            }
            let name = call["fnName"]
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    format!("LLMChatResult tool call {index} fnName must be non-empty")
                })?;
            let arguments_text = call["fnArguments"].as_str().ok_or_else(|| {
                format!("LLMChatResult tool call {index} fnArguments must be a JSON string")
            })?;
            let arguments: Value = serde_json::from_str(arguments_text).map_err(|error| {
                format!("LLMChatResult tool call {index} fnArguments is invalid JSON: {error}")
            })?;
            if !arguments.is_object() {
                return Err(format!(
                    "LLMChatResult tool call {index} fnArguments must decode to a JSON object"
                ));
            }
            Ok(ToolCall {
                call_id: call_id.to_owned(),
                name: name.to_owned(),
                arguments,
            })
        })
        .collect()
}
