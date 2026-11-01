use crate::llm::reasoning::ReasoningParams;
use crate::llm::result_stream::{PROVIDER_STREAM_INTERRUPTED, llm_error_end};
use anyhow::Result;
use command_utils::trace::impls::GenericOtelClient;
use futures::StreamExt;
use futures::stream::BoxStream;
use genai::Client;
use genai::chat::{
    ChatMessage, ChatOptions, ChatRequest, ChatResponseFormat, ChatStreamEvent, JsonSpec,
    MessageContent as GenaiMessageContent,
};
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::jobworkerp::runner::llm::llm_completion_result::message_content;
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::GenaiRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::{
    LlmCompletionArgs, LlmCompletionResult, llm_completion_result,
};
use proto::jobworkerp::data::{ResultOutputItem, StreamErrorCode, result_output_item};
use std::collections::HashMap;
use std::sync::Arc;

use command_utils::trace::attr::{OtelSpanAttributes, OtelSpanBuilder, OtelSpanType};

use super::super::generic_tracing_helper::{
    GenericLLMTracingHelper, LLMMessage, StreamingTraceUpdate,
};
use super::super::tracing::genai_helper::GenaiCompletionTracingHelper;

/// Build a generation span for a genai-backed text completion request.
fn build_genai_completion_span(
    model: &str,
    prompt: Option<String>,
    metadata: &HashMap<String, String>,
) -> OtelSpanAttributes {
    let mut builder = OtelSpanBuilder::new("genai.completion.completions")
        .span_type(OtelSpanType::Generation)
        .model(model.to_string())
        .system("genai")
        .operation_name("text_completion")
        .input(serde_json::json!(prompt.unwrap_or_default()))
        .openinference_span_kind("LLM");
    if let Some(sid) = metadata.get("session_id").cloned() {
        builder = builder.session_id(sid);
    }
    if let Some(uid) = metadata.get("user_id").cloned() {
        builder = builder.user_id(uid);
    }
    builder.build()
}

pub struct GenaiLLMConfig {
    pub model_name: String,
    pub endpoint_url: Option<String>,
}
#[derive(Clone)]
pub struct GenaiCompletionService {
    pub client: Client,
    pub model: String,
    pub system_prompt: Option<String>,
    pub otel_client: Option<Arc<GenericOtelClient>>,
    // Cached at construction so per-request schema sanitisation does not
    // re-classify the (immutable) model name. None means the model name did
    // not match any known adapter; sanitize_schema_for_adapter treats that
    // as "no fixups".
    adapter_kind: Option<genai::adapter::AdapterKind>,
}
impl GenaiCompletionService {
    pub async fn new(settings: GenaiRunnerSettings) -> Result<Self> {
        let target_resolver = crate::llm::common::fixed_model_genai_service_target_resolver(
            settings.model.clone(),
            settings.base_url.clone(),
        );
        tracing::debug!("=== Genai LLM: target_resolver: {:?}", &target_resolver,);
        // -- Build the new client with this adapter_config
        let client = crate::llm::common::build_genai_client(Some(target_resolver))?;
        let adapter_kind = genai::adapter::AdapterKind::from_model(&settings.model)
            .map_err(|e| {
                tracing::warn!(
                    "AdapterKind::from_model({}) failed; provider-specific schema fixups will be skipped: {e}",
                    settings.model
                );
                e
            })
            .ok();
        Ok(Self {
            client,
            model: settings.model,
            system_prompt: settings.system_prompt,
            otel_client: Some(Arc::new(GenericOtelClient::new("genai.completion_service"))),
            adapter_kind,
        })
    }
    pub(super) fn build_options(&self, args: &LlmCompletionArgs) -> Result<Option<ChatOptions>> {
        Self::build_options_with_adapter(args, self.adapter_kind)
    }

    pub(super) fn build_options_with_adapter(
        args: &LlmCompletionArgs,
        adapter_kind: Option<genai::adapter::AdapterKind>,
    ) -> Result<Option<ChatOptions>> {
        // Explicit opt-in to StreamEnd captures (genai 0.6+). request_chat_stream()
        // reads end.captured_content / captured_reasoning_content / captured_usage;
        // without these flags the values are no longer guaranteed to be populated.
        // capture_tool_calls is intentionally omitted: the completion path never
        // negotiates tool calls (see chat::genai for the tool-calling counterpart).
        let mut chat_opts = ChatOptions {
            capture_content: Some(true),
            capture_reasoning_content: Some(true),
            capture_usage: Some(true),
            ..ChatOptions::default()
        };

        if let Some(opt) = args.options.as_ref() {
            chat_opts.temperature = opt.temperature.map(|v| v as f64);
            chat_opts.max_tokens = opt.max_tokens.map(|v| v as u32);
            chat_opts.top_p = opt.top_p.map(|v| v as f64);
            ReasoningParams::from(opt).apply_to_genai(&mut chat_opts, adapter_kind)?;
        }

        if let Some(schema_str) = args.json_schema.as_deref() {
            match serde_json::from_str::<serde_json::Value>(schema_str) {
                Ok(mut schema) => {
                    // See schema_sanitize for the Gemini-specific stripping.
                    // Ollama path lives in a separate file and accepts the full
                    // draft-2020-12 keyword set, so it is intentionally not routed here.
                    crate::llm::schema_sanitize::sanitize_schema_for_adapter(
                        &mut schema,
                        adapter_kind,
                    );
                    chat_opts.response_format = Some(ChatResponseFormat::JsonSpec(JsonSpec::new(
                        crate::llm::GENAI_JSON_SPEC_NAME,
                        schema,
                    )));
                    tracing::debug!("Applied JSON schema to GenAI completion: {}", schema_str);
                }
                Err(e) => {
                    // Mirror Ollama behavior: warn and ignore on parse failure.
                    tracing::warn!("Invalid JSON schema for GenAI completion, ignoring: {}", e);
                }
            }
        }

        Ok(Some(chat_opts))
    }
    fn messages(&self, args: LlmCompletionArgs) -> Vec<ChatMessage> {
        let mut messages = Vec::new();
        if let Some(s) = args.system_prompt.or_else(|| self.system_prompt.clone()) {
            messages.push(ChatMessage::system(s))
        }
        messages.push(ChatMessage::user(args.prompt));
        messages
    }
    pub fn with_otel_client(mut self, client: Arc<GenericOtelClient>) -> Self {
        self.otel_client = Some(client);
        self
    }

    pub async fn request_chat(
        &self,
        args: LlmCompletionArgs,
        cx: opentelemetry::Context,
        metadata: HashMap<String, String>,
    ) -> Result<LlmCompletionResult> {
        let metadata = Arc::new(metadata);
        let options = self.build_options(&args)?;
        let messages = self.messages(args);
        let chat_req = ChatRequest::new(messages);

        let res = Self::request_completion_internal_with_tracing(
            Arc::new(self.clone()),
            chat_req,
            options,
            Some(cx.clone()),
            metadata.clone(),
        )
        .await?;

        let completion_result = LlmCompletionResult {
            content: if let Some(text) = res.first_text() {
                Some(llm_completion_result::MessageContent {
                    content: Some(message_content::Content::Text(text.to_string())),
                })
            } else {
                tracing::error!("No text content found in response: {:#?}", &res.content);
                None
            },
            reasoning_content: res.reasoning_content,
            done: true,
            usage: Some(llm_completion_result::Usage {
                model: res.model_iden.model_name.to_string(),
                prompt_tokens: res.usage.prompt_tokens.map(|v| v as u32),
                completion_tokens: res.usage.completion_tokens.map(|v| v as u32),
                ..Default::default()
            }),
            ..Default::default()
        };

        // Record usage if available
        let usage = &res.usage;
        if true {
            let content = completion_result
                .content
                .as_ref()
                .and_then(|c| c.content.as_ref())
                .map(|content| match content {
                    message_content::Content::Text(text) => text.as_str(),
                    // _ => None,
                });
            let _ = GenericLLMTracingHelper::trace_usage(
                self,
                &metadata,
                cx.clone(),
                "genai.completion.usage",
                usage,
                content,
            )
            .await;
        }

        Ok(completion_result)
    }

    async fn request_completion_internal_with_tracing(
        self: Arc<Self>,
        chat_req: ChatRequest,
        options: Option<ChatOptions>,
        parent_context: Option<opentelemetry::Context>,
        metadata: Arc<HashMap<String, String>>,
    ) -> Result<genai::chat::ChatResponse> {
        let model = self.model.clone();
        let model_for_span = model.clone();
        let chat_req_for_span = chat_req.clone();
        let options_for_span = options.clone();

        tracing::debug!(
            "Genai LLM: model: {}, Chat request: {:?}, options: {:?}",
            &model,
            &chat_req,
            &options,
        );

        let client_clone = self.client.clone();
        let completion_api_action = async move {
            client_clone
                .exec_chat(&model, chat_req, options.as_ref())
                .await
                .map_err(|e| JobWorkerError::OtherError(format!("Completion API error: {e}")))
        };

        // Execute completion API call and get both result and context
        let (res, _current_context) = if let Some(_otel_client) = self.get_otel_client() {
            let span_attributes = self
                .create_completion_span_from_request(
                    &model_for_span,
                    &chat_req_for_span,
                    &options_for_span,
                    &metadata,
                )
                .await;

            // Execute completion API call with response tracing
            self.with_completion_response_tracing(
                &metadata,
                parent_context.clone(),
                span_attributes,
                completion_api_action,
            )
            .await?
        } else {
            let result = completion_api_action.await?;
            let context = opentelemetry::Context::current();
            (result, context)
        };

        Ok(res)
    }
    pub async fn request_chat_stream(
        &self,
        args: LlmCompletionArgs,
        metadata: HashMap<String, String>,
        parent_context: Option<opentelemetry::Context>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        let options = self.build_options(&args)?;
        let span_attributes = if GenericLLMTracingHelper::get_otel_client(self).is_some() {
            Some(build_genai_completion_span(
                &self.model,
                Some(args.prompt.clone()),
                &metadata,
            ))
        } else {
            None
        };
        let messages = self.messages(args);
        let chat_req = ChatRequest::new(messages);
        tracing::debug!(
            "Genai LLM(stream): model: {}, Chat request: {:?}, options: {:?}",
            &self.model,
            &chat_req,
            &options
        );
        let res = self
            .client
            .exec_chat_stream(&self.model, chat_req, options.as_ref())
            .await
            .map_err(|e| anyhow::anyhow!("Failed to request generation: {:#?}", e))?;

        let model_name = res.model_iden.model_name.to_string();
        let stream = completion_result_stream(res.stream, model_name, metadata);

        let traced = if let Some(span_attrs) = span_attributes {
            use prost::Message as _;
            let mut accumulated = String::with_capacity(4096);
            GenericLLMTracingHelper::with_streaming_response_tracing(
                self,
                parent_context,
                span_attrs,
                stream,
                move |item: &ResultOutputItem| {
                    let bytes = match item.item.as_ref() {
                        Some(result_output_item::Item::Data(b)) => b,
                        _ => return StreamingTraceUpdate::None,
                    };
                    let parsed = match LlmCompletionResult::decode(bytes.as_slice()) {
                        Ok(p) => p,
                        Err(_) => return StreamingTraceUpdate::None,
                    };
                    // The `done=true` chunk carries `End.captured_content`
                    // (the full text already aggregated by GenAI). Adding it
                    // to `accumulated`, which already holds the per-chunk
                    // concatenation, would duplicate the body in the trace.
                    // Skip accumulation on the terminal chunk and use the
                    // captured full text only as a fallback when no per-chunk
                    // text was streamed.
                    let chunk_text =
                        parsed
                            .content
                            .as_ref()
                            .and_then(|c| match c.content.as_ref() {
                                Some(message_content::Content::Text(text)) => Some(text.as_str()),
                                _ => None,
                            });
                    if !parsed.done {
                        if let Some(text) = chunk_text {
                            accumulated.push_str(text);
                        }
                        return StreamingTraceUpdate::None;
                    }
                    let usage = parsed.usage.as_ref().and_then(|u| {
                        super::super::generic_tracing_helper::streaming_usage_map(
                            u.prompt_tokens,
                            u.completion_tokens,
                        )
                    });
                    let final_text = if accumulated.is_empty() {
                        chunk_text.unwrap_or("").to_string()
                    } else {
                        std::mem::take(&mut accumulated)
                    };
                    StreamingTraceUpdate::Final {
                        output: serde_json::json!(final_text),
                        usage,
                    }
                },
            )
        } else {
            stream
        };

        Ok(traced)
    }
}

// Implement traits for GenaiService (completion version)
impl GenericLLMTracingHelper for GenaiCompletionService {
    fn get_otel_client(&self) -> Option<&Arc<GenericOtelClient>> {
        self.otel_client.as_ref()
    }

    fn convert_messages_to_input(&self, messages: &[impl LLMMessage]) -> serde_json::Value {
        use super::super::tracing::genai_helper::GenaiTracingHelper;
        let genai_messages: Vec<ChatMessage> = messages
            .iter()
            .map(|m| ChatMessage {
                role: match m.get_role() {
                    "user" => genai::chat::ChatRole::User,
                    "assistant" => genai::chat::ChatRole::Assistant,
                    "system" => genai::chat::ChatRole::System,
                    "tool" => genai::chat::ChatRole::Tool,
                    unknown_role => {
                        tracing::warn!(
                            "Unknown role string '{}', defaulting to User",
                            unknown_role
                        );
                        genai::chat::ChatRole::User
                    }
                },
                content: GenaiMessageContent::from_text(m.get_content().to_string()),
                options: None,
            })
            .collect();
        super::super::chat::genai::GenaiChatService::convert_messages_to_input_genai(
            &genai_messages,
        )
    }

    fn get_provider_name(&self) -> &str {
        "genai"
    }
}

impl GenaiCompletionTracingHelper for GenaiCompletionService {}

/// Converts a provider event stream into result items. A provider error ends
/// the stream with a failed End; otherwise exactly one normal End follows.
fn completion_result_stream<E: std::fmt::Debug + Send + 'static>(
    events: impl futures::Stream<Item = std::result::Result<ChatStreamEvent, E>> + Send + 'static,
    model: String,
    metadata: HashMap<String, String>,
) -> BoxStream<'static, ResultOutputItem> {
    async_stream::stream! {
        futures::pin_mut!(events);
        while let Some(event_result) = events.next().await {
            match event_result {
                Ok(event) => {
                    if let Some(item) = completion_event_item(event, &model) {
                        yield item;
                    }
                }
                Err(e) => {
                    tracing::error!("Error in chat stream: {:?}", e);
                    yield llm_error_end(
                        metadata,
                        StreamErrorCode::ProviderStreamFailed,
                        PROVIDER_STREAM_INTERRUPTED,
                    );
                    return;
                }
            }
        }
        yield proto::stream_error::normal_end_item(metadata);
    }
    .boxed()
}

fn completion_event_item(event: ChatStreamEvent, model: &str) -> Option<ResultOutputItem> {
    let llm_result = match event {
        // Start and keep-alive pings carry no payload.
        ChatStreamEvent::Start | ChatStreamEvent::Heartbeat => return None,
        // LlmCompletionResult has no field for tool calls.
        ChatStreamEvent::ToolCallChunk(_) => return None,
        ChatStreamEvent::Chunk(chunk) => LlmCompletionResult {
            content: Some(llm_completion_result::MessageContent {
                content: Some(message_content::Content::Text(chunk.content)),
            }),
            ..Default::default()
        },
        ChatStreamEvent::ReasoningChunk(chunk) | ChatStreamEvent::ThoughtSignatureChunk(chunk) => {
            LlmCompletionResult {
                reasoning_content: Some(chunk.content),
                ..Default::default()
            }
        }
        ChatStreamEvent::End(end) => LlmCompletionResult {
            done: true,
            usage: end
                .captured_usage
                .map(|usage| llm_completion_result::Usage {
                    model: model.to_string(),
                    prompt_tokens: usage.prompt_tokens.map(|v| v as u32),
                    completion_tokens: usage.completion_tokens.map(|v| v as u32),
                    ..Default::default()
                }),
            content: end
                .captured_content
                .as_ref()
                .and_then(|c| c.first_text())
                .map(|text| llm_completion_result::MessageContent {
                    content: Some(message_content::Content::Text(text.to_string())),
                }),
            reasoning_content: end.captured_reasoning_content,
            ..Default::default()
        },
    };
    Some(ResultOutputItem {
        item: Some(result_output_item::Item::Data(
            prost::Message::encode_to_vec(&llm_result),
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn completion_items(
        events: Vec<std::result::Result<ChatStreamEvent, &'static str>>,
    ) -> Vec<ResultOutputItem> {
        completion_result_stream(futures::stream::iter(events), "m".into(), HashMap::new())
            .collect()
            .await
    }

    fn end_trailers(items: &[ResultOutputItem]) -> Vec<&proto::jobworkerp::data::Trailer> {
        items
            .iter()
            .filter_map(|item| match &item.item {
                Some(result_output_item::Item::End(trailer)) => Some(trailer),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn provider_error_ends_the_stream_once_with_a_failure() {
        let chunk = || {
            Ok(ChatStreamEvent::Chunk(genai::chat::StreamChunk {
                content: "partial".into(),
            }))
        };
        let items = completion_items(vec![chunk(), Err("connection reset"), chunk()]).await;
        let ends = end_trailers(&items);
        assert_eq!(ends.len(), 1, "exactly one End: {items:?}");
        assert!(matches!(
            items.last().unwrap().item,
            Some(result_output_item::Item::End(_))
        ));
        let error = ends[0].error.as_ref().expect("provider error is a failure");
        assert_eq!(error.code, StreamErrorCode::ProviderStreamFailed as i32);
        assert_eq!(error.origin, "LLM");

        let items = completion_items(vec![chunk()]).await;
        let ends = end_trailers(&items);
        assert_eq!(ends.len(), 1);
        assert_eq!(ends[0].error, None);
    }
    use jobworkerp_runner::jobworkerp::runner::llm::llm_completion_args::LlmOptions;

    const TEST_OPENAI_MODEL: &str = "gpt-4o-mini";
    const TEST_GEMINI_MODEL: &str = "gemini-3.1-flash-lite";

    fn build_test_options(model: &str, args: &LlmCompletionArgs) -> Option<ChatOptions> {
        let kind = genai::adapter::AdapterKind::from_model(model).ok();
        GenaiCompletionService::build_options_with_adapter(args, kind).expect("valid options")
    }

    #[test]
    fn build_options_sets_json_spec_response_format() {
        let schema = r#"{"type":"object","properties":{"x":{"type":"integer"}}}"#;
        let args = LlmCompletionArgs {
            json_schema: Some(schema.to_string()),
            ..Default::default()
        };
        let opts = build_test_options(TEST_OPENAI_MODEL, &args).expect("options must be Some");
        match opts.response_format {
            Some(ChatResponseFormat::JsonSpec(spec)) => {
                assert_eq!(spec.name, "structured_output");
                let expected: serde_json::Value = serde_json::from_str(schema).unwrap();
                assert_eq!(spec.schema, expected);
            }
            other => panic!("expected JsonSpec response_format, got {other:?}"),
        }
    }

    #[test]
    fn build_options_returns_some_when_only_json_schema() {
        let args = LlmCompletionArgs {
            options: None,
            json_schema: Some(r#"{"type":"object"}"#.to_string()),
            ..Default::default()
        };
        let opts = build_test_options(TEST_OPENAI_MODEL, &args);
        assert!(opts.is_some());
        assert!(opts.unwrap().response_format.is_some());
    }

    #[test]
    fn build_options_ignores_invalid_json_schema() {
        let args = LlmCompletionArgs {
            options: None,
            json_schema: Some("not a json".to_string()),
            ..Default::default()
        };
        // Invalid JSON schema is silently ignored, but capture flags are still set
        // for streaming, so options must still be Some.
        let opts = build_test_options(TEST_OPENAI_MODEL, &args).expect("options must be Some");
        assert!(opts.response_format.is_none());
    }

    #[test]
    fn build_options_enables_stream_captures() {
        // genai 0.6+ gates StreamEnd.captured_* values behind ChatOptions flags.
        // request_chat_stream consumes captured_content / captured_reasoning_content /
        // captured_usage, so the three flags must be Some(true). capture_tool_calls is
        // not needed on the completion path because completions never call tools.
        let args = LlmCompletionArgs::default();
        let opts = build_test_options(TEST_OPENAI_MODEL, &args).expect("options must be Some");
        assert_eq!(opts.capture_content, Some(true));
        assert_eq!(opts.capture_reasoning_content, Some(true));
        assert_eq!(opts.capture_usage, Some(true));
    }

    #[test]
    fn build_options_preserves_existing_fields() {
        let args = LlmCompletionArgs {
            options: Some(LlmOptions {
                temperature: Some(0.42),
                max_tokens: Some(128),
                top_p: Some(0.9),
                extract_reasoning_content: Some(true),
                ..Default::default()
            }),
            json_schema: Some(r#"{"type":"object"}"#.to_string()),
            ..Default::default()
        };
        let opts = build_test_options(TEST_OPENAI_MODEL, &args).expect("options must be Some");
        assert!((opts.temperature.unwrap() - 0.42_f64).abs() < 1e-6);
        assert_eq!(opts.max_tokens, Some(128));
        assert!((opts.top_p.unwrap() - 0.9_f64).abs() < 1e-6);
        assert_eq!(opts.normalize_reasoning_content, Some(true));
        assert!(matches!(
            opts.response_format,
            Some(ChatResponseFormat::JsonSpec(_))
        ));
    }

    #[test]
    fn build_options_extract_false_requests_no_reasoning() {
        let args = LlmCompletionArgs {
            options: Some(LlmOptions {
                extract_reasoning_content: Some(false),
                chat_template_kwargs: Some(r#"{"k":"v"}"#.to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let opts = build_test_options(TEST_OPENAI_MODEL, &args).expect("options must be Some");
        assert!(matches!(
            opts.reasoning_effort,
            Some(genai::chat::ReasoningEffort::Zero)
        ));
        assert_eq!(
            opts.extra_body,
            Some(serde_json::json!({"chat_template_kwargs": {"k": "v"}}))
        );
    }

    #[test]
    fn build_options_rejects_invalid_reasoning_options() {
        for options in [
            LlmOptions {
                reasoning_effort: Some("bogus".to_string()),
                ..Default::default()
            },
            LlmOptions {
                chat_template_kwargs: Some("1".to_string()),
                ..Default::default()
            },
        ] {
            let args = LlmCompletionArgs {
                options: Some(options),
                ..Default::default()
            };
            assert!(
                GenaiCompletionService::build_options_with_adapter(
                    &args,
                    Some(genai::adapter::AdapterKind::OpenAI)
                )
                .is_err()
            );
        }
    }

    #[test]
    fn build_options_sanitizes_schema_for_gemini() {
        // gemini-3.1-flash-lite rejects responseJsonSchema with maxItems; the
        // sanitizer should strip it before genai serialises the request.
        let schema = r#"{"type":"object","properties":{
            "tags":{"type":"array","items":{"type":"string"},"maxItems":5}
        }}"#;
        let args = LlmCompletionArgs {
            json_schema: Some(schema.to_string()),
            ..Default::default()
        };
        let opts = build_test_options(TEST_GEMINI_MODEL, &args).expect("options must be Some");
        match opts.response_format {
            Some(ChatResponseFormat::JsonSpec(spec)) => {
                assert!(spec.schema["properties"]["tags"].get("maxItems").is_none());
            }
            other => panic!("expected JsonSpec response_format, got {other:?}"),
        }
    }

    #[test]
    fn build_options_keeps_schema_for_openai_model() {
        let schema = r#"{"type":"array","items":{"type":"string"},"maxItems":5}"#;
        let args = LlmCompletionArgs {
            json_schema: Some(schema.to_string()),
            ..Default::default()
        };
        let opts = build_test_options(TEST_OPENAI_MODEL, &args).expect("options must be Some");
        match opts.response_format {
            Some(ChatResponseFormat::JsonSpec(spec)) => {
                assert_eq!(spec.schema["maxItems"], 5);
            }
            other => panic!("expected JsonSpec response_format, got {other:?}"),
        }
    }
}
