//! Conversion helpers for the registered LLM streaming profile.

use crate::error::AgUiError;
use crate::events::EventEncoder;
use crate::events::{
    AgUiEvent,
    llm::{extract_text_from_protobuf_result, extract_tool_calls_from_protobuf_result},
};
use crate::session::{EventStore, Session, SessionManager, SessionState};
use crate::types::MessageId;
use crate::types::{RunId, ThreadId};
use crate::types::{
    input::RunAgentInput,
    message::{ContentPart, Message, MessageContent, Role},
};
use anyhow::{Result, bail};
use app::app::job::JobApp;
use futures::{Stream, StreamExt, stream::BoxStream};
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::{
    Content, ToolCall as ProtoToolCall, ToolCalls, ToolResult, ToolResults,
};
use jobworkerp_runner::jobworkerp::runner::llm::{
    LlmChatArgs, LlmChatResult,
    llm_chat_args::{
        ChatMessage, ChatRole, FunctionOptions, MessageContent as ProtoContent, MessageParts,
        chat_message::Body,
    },
};
use prost::Message as ProstMessage;
use proto::jobworkerp::data::JobId;
use proto::jobworkerp::data::{ResultOutputItem, result_output_item};
use std::collections::HashSet;
use std::{collections::HashMap, sync::Arc};
use tokio::{sync::RwLock, task::JoinHandle};
use tokio_util::sync::CancellationToken;

/// Enqueue one registered chat worker and expose its early job ID with AG-UI events.
pub async fn enqueue_direct_run(
    app_module: &app::module::AppModule,
    worker_name: &str,
    input: &RunAgentInput,
    timeout_sec: u32,
) -> Result<(
    proto::jobworkerp::data::JobId,
    futures::stream::BoxStream<'static, AgUiEvent>,
)> {
    let args = build_chat_args(input)?;
    let args_bytes = args.encode_to_vec();
    let worker = crate::job_queue::resolve_live_stream_worker(app_module, worker_name).await?;
    let (job_id, result_future) =
        crate::job_queue::enqueue_live_stream(app_module, worker, "chat", args_bytes, timeout_sec)
            .await?;
    let (result_future, stream, result_wait_abort) =
        result_future.into_parts_with_result_wait_abort_handle();
    let stream = stream
        .ok_or_else(|| anyhow::anyhow!("streaming chat worker returned no response stream"))?;
    let message_id = MessageId::random();
    let events = direct_events_with_job_result(
        convert_direct_stream(stream, message_id.clone(), input.run_id.clone()).boxed(),
        result_future,
        input.run_id.clone(),
        message_id,
    )
    .boxed();
    // Keep the result subscriber alive for as long as the event producer consumes output.
    let events = futures::stream::unfold(
        (events, result_wait_abort),
        |(mut events, _abort)| async move { events.next().await.map(|event| (event, (events, _abort))) },
    );
    Ok((job_id, events.boxed()))
}

fn direct_events_with_job_result(
    mut events: BoxStream<'static, AgUiEvent>,
    result_future: futures::future::BoxFuture<
        'static,
        anyhow::Result<Option<proto::jobworkerp::data::JobResult>>,
    >,
    run_id: String,
    message_id: MessageId,
) -> impl Stream<Item = AgUiEvent> + Send + 'static {
    async_stream::stream! {
        tokio::pin!(result_future);
        let mut result_checked = false;
        let mut text_open = false;
        loop {
            tokio::select! {
                event = events.next() => match event {
                    Some(event) => {
                        match &event {
                            AgUiEvent::TextMessageStart { .. } => text_open = true,
                            AgUiEvent::TextMessageEnd { .. } => text_open = false,
                            AgUiEvent::RunError { .. } => {
                                if text_open {
                                    yield AgUiEvent::text_message_end(message_id.clone());
                                }
                                yield event;
                                break;
                            }
                            _ => {}
                        }
                        yield event;
                    }
                    None => break,
                },
                result = &mut result_future, if !result_checked => {
                    result_checked = true;
                    let check = validate_job_result(result);
                    if let Err(error) = check {
                        if text_open {
                            yield AgUiEvent::text_message_end(message_id.clone());
                        }
                        yield AgUiEvent::run_error(run_id.clone(), error.to_string(), Some("LLM_JOB_FAILED".into()), None);
                        break;
                    }
                }
            }
        }
        if !result_checked {
            let check = validate_job_result(result_future.await);
            if let Err(error) = check {
                if text_open {
                    yield AgUiEvent::text_message_end(message_id.clone());
                }
                yield AgUiEvent::run_error(run_id, error.to_string(), Some("LLM_JOB_FAILED".into()), None);
            }
        }
    }
}

fn validate_job_result(
    result: anyhow::Result<Option<proto::jobworkerp::data::JobResult>>,
) -> anyhow::Result<()> {
    match result? {
        Some(result) => app::app::job_result::ensure_successful_job_result(&result).map(|_| ()),
        None => Err(anyhow::anyhow!(
            "streaming chat worker completed without a job result"
        )),
    }
}

/// Direct runs whose event producer is still running, keyed by run ID.
///
/// The producer is the only writer of a run's terminal event and session state, so
/// cancellation goes through it instead of writing a terminal event from outside.
#[derive(Clone, Default)]
pub struct DirectRuns(Arc<RwLock<HashMap<String, DirectRun>>>);

#[derive(Clone)]
struct DirectRun {
    job_id: JobId,
    cancel: CancellationToken,
    /// Fires once the producer has finished, normally after storing the terminal event and
    /// session state, and the run is deregistered.
    settled: CancellationToken,
}

impl DirectRuns {
    pub async fn is_running(&self, run_id: &str) -> bool {
        self.0.read().await.contains_key(run_id)
    }

    /// Start producing the session's run events and keep the run registered until the
    /// producer finishes, so a concurrent cancel always finds the run and waits for the
    /// same settlement.
    pub async fn start<SM, S, E>(
        &self,
        job_id: JobId,
        session: &Session,
        events: E,
        event_store: Arc<S>,
        encoder: Arc<EventEncoder>,
        session_manager: Arc<SM>,
    ) -> BoxStream<'static, (u64, AgUiEvent)>
    where
        SM: SessionManager + 'static,
        S: EventStore + 'static,
        E: Stream<Item = AgUiEvent> + Send + 'static,
    {
        let key = session.run_id.to_string();
        let session_id = session.session_id.clone();
        let cancel = CancellationToken::new();
        let settled = CancellationToken::new();
        // Registered before the producer starts, so the watcher below always deregisters it.
        self.0.write().await.insert(
            key.clone(),
            DirectRun {
                job_id,
                cancel: cancel.clone(),
                settled: settled.clone(),
            },
        );
        let (output, producer) = spawn_event_producer(
            session.run_id.clone(),
            session.thread_id.clone(),
            events,
            event_store,
            encoder,
            cancel,
            move |state| async move {
                session_manager.set_session_state(&session_id, state).await;
            },
        );
        let registry = self.clone();
        // Deregisters even if the producer panics, so cancellers never wait forever.
        tokio::spawn(async move {
            if let Err(error) = producer.await {
                tracing::error!("direct run producer for {key} failed: {error}");
            }
            registry.0.write().await.remove(&key);
            settled.cancel();
        });
        output
    }

    /// Cancel a registered run's job and wait until its producer stores the terminal event.
    /// Returns false when no direct run is registered under `run_id`.
    pub async fn cancel(&self, job_app: &dyn JobApp, run_id: &str) -> crate::error::Result<bool> {
        let Some(run) = self.0.read().await.get(run_id).cloned() else {
            return Ok(false);
        };
        // A job that may still run must not be reported as cancelled.
        job_app.delete_job(&run.job_id).await.map_err(|e| {
            AgUiError::Internal(anyhow::anyhow!("Failed to cancel direct LLM job: {e}"))
        })?;
        run.cancel.cancel();
        run.settled.cancelled().await;
        Ok(true)
    }
}

/// Live events buffered for a subscriber before delivery waits for it to read.
const LIVE_EVENT_BUFFER: usize = 64;

enum ProducerEnd {
    /// The source emitted the run's terminal event itself.
    Terminal(SessionState),
    SourceEnded,
    Cancelled,
}

/// Persist and publish a run independently from its HTTP subscriber.
/// Dropping the returned receiver only stops live delivery; the producer keeps storing events
/// until the source ends or `cancel` fires, then reports the run's final state.
fn spawn_event_producer<S, E, C, Fut>(
    run_id: RunId,
    thread_id: ThreadId,
    events: E,
    event_store: Arc<S>,
    encoder: Arc<EventEncoder>,
    cancel: CancellationToken,
    on_terminal: C,
) -> (BoxStream<'static, (u64, AgUiEvent)>, JoinHandle<()>)
where
    S: EventStore + 'static,
    E: Stream<Item = AgUiEvent> + Send + 'static,
    C: FnOnce(SessionState) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let run_id_string = run_id.to_string();
    let thread_id_string = thread_id.to_string();
    let (sender, receiver) = tokio::sync::mpsc::channel(LIVE_EVENT_BUFFER);
    let producer = tokio::spawn(async move {
        let publish = |event| {
            persist_and_publish(
                event_store.as_ref(),
                &encoder,
                &run_id_string,
                &sender,
                &cancel,
                event,
            )
        };
        publish(AgUiEvent::run_started(
            run_id_string.clone(),
            thread_id_string.clone(),
        ))
        .await;
        let mut events = Box::pin(events);
        let mut open_text: Option<String> = None;
        let end = loop {
            let event = tokio::select! {
                biased;
                _ = cancel.cancelled() => break ProducerEnd::Cancelled,
                event = events.next() => event,
            };
            let Some(event) = event else {
                break ProducerEnd::SourceEnded;
            };
            let terminal = match &event {
                AgUiEvent::TextMessageStart { message_id, .. } => {
                    open_text = Some(message_id.clone());
                    None
                }
                AgUiEvent::TextMessageEnd { .. } => {
                    open_text = None;
                    None
                }
                AgUiEvent::RunFinished { .. } => Some(SessionState::Completed),
                AgUiEvent::RunError { .. } => Some(SessionState::Error),
                _ => None,
            };
            publish(event).await;
            if let Some(state) = terminal {
                break ProducerEnd::Terminal(state);
            }
        };
        // Stop consuming the job's output first, so nothing can follow the terminal event.
        drop(events);
        let state = match end {
            ProducerEnd::Terminal(state) => state,
            ProducerEnd::SourceEnded => {
                publish(AgUiEvent::run_finished_success(
                    run_id_string.clone(),
                    thread_id_string,
                    None,
                ))
                .await;
                SessionState::Completed
            }
            ProducerEnd::Cancelled => {
                if let Some(message_id) = open_text {
                    publish(AgUiEvent::text_message_end(message_id)).await;
                }
                publish(AgUiEvent::run_cancelled(run_id_string.clone())).await;
                SessionState::Cancelled
            }
        };
        on_terminal(state).await;
    });
    (
        Box::pin(tokio_stream::wrappers::ReceiverStream::new(receiver)),
        producer,
    )
}

/// Store an event, then deliver it live. Delivery waits for a slow subscriber only until
/// the run is cancelled; after that it is best effort, since the event store already holds
/// every event for reconnecting clients.
async fn persist_and_publish<S: EventStore>(
    event_store: &S,
    encoder: &EventEncoder,
    run_id: &str,
    sender: &tokio::sync::mpsc::Sender<(u64, AgUiEvent)>,
    cancel: &CancellationToken,
    event: AgUiEvent,
) {
    let Ok((event_id, _)) = encoder.encode(&event) else {
        return;
    };
    event_store
        .store_event(run_id, event_id, event.clone())
        .await;
    tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            let _ = sender.try_send((event_id, event));
        }
        permit = sender.reserve() => {
            if let Ok(permit) = permit {
                permit.send((event_id, event));
            }
        }
    }
}

/// Convert a registered LLM worker's output stream to client-visible AG-UI events.
/// FinalCollected is a snapshot and is intentionally ignored to avoid duplicating deltas.
pub fn convert_direct_stream(
    mut stream: BoxStream<'static, ResultOutputItem>,
    message_id: MessageId,
    run_id: String,
) -> impl Stream<Item = AgUiEvent> + Send + 'static {
    async_stream::stream! {
        let mut emitted_text_start = false;
        let mut emitted_text_end = false;
        let mut emitted_calls = HashSet::new();
        let mut saw_done = false;
        let mut emitted_error = false;
        let mut emitted_text = String::new();
        while let Some(item) = stream.next().await {
            match item.item {
                Some(result_output_item::Item::Data(bytes)) => {
                    let result = match LlmChatResult::decode(bytes.as_slice()) {
                        Ok(result) => result,
                        Err(error) => {
                            emitted_error = true;
                            if emitted_text_start && !emitted_text_end {
                                emitted_text_end = true;
                                yield AgUiEvent::text_message_end(message_id.clone());
                            }
                            yield AgUiEvent::run_error(run_id.clone(), format!("invalid LLM stream item: {error}"), Some("INVALID_STREAM_ITEM".into()), None);
                            break;
                        }
                    };
                    if let Some(text) = extract_text_from_protobuf_result(&result) {
                        let delta = if result.done {
                            if text == emitted_text {
                                String::new()
                            } else if text.starts_with(&emitted_text) {
                                text[emitted_text.len()..].to_string()
                            } else {
                                emitted_error = true;
                                if emitted_text_start && !emitted_text_end {
                                    emitted_text_end = true;
                                    yield AgUiEvent::text_message_end(message_id.clone());
                                }
                                yield AgUiEvent::run_error(run_id.clone(), "terminal LLM text snapshot does not match streamed deltas", Some("INCONSISTENT_TERMINAL_SNAPSHOT".into()), None);
                                break;
                            }
                        } else {
                            text
                        };
                        if !delta.is_empty() {
                            if !emitted_text_start {
                                emitted_text_start = true;
                                yield AgUiEvent::text_message_start(message_id.clone(), Role::Assistant);
                            }
                            emitted_text.push_str(&delta);
                            yield AgUiEvent::text_message_content(message_id.clone(), delta);
                        }
                    }
                    if let Some(calls) = extract_tool_calls_from_protobuf_result(&result) {
                        for call in calls.tool_calls {
                            if emitted_calls.insert(call.call_id.clone()) {
                                yield AgUiEvent::tool_call_start(call.call_id.clone(), call.fn_name, Some(message_id.to_string()));
                                yield AgUiEvent::tool_call_args(call.call_id.clone(), call.fn_arguments);
                                yield AgUiEvent::tool_call_end(call.call_id);
                            }
                        }
                    }
                    saw_done |= result.done;
                }
                Some(result_output_item::Item::End(trailer)) => {
                    if !emitted_text_end && emitted_text_start {
                        emitted_text_end = true;
                        yield AgUiEvent::text_message_end(message_id.clone());
                    }
                    if !saw_done {
                        emitted_error = true;
                        yield AgUiEvent::run_error(run_id.clone(), "LLM stream ended without a successful completion", Some("INCOMPLETE_STREAM".into()), None);
                    } else if let Some(error) = proto::stream_error::end_failure(&trailer) {
                        emitted_error = true;
                        yield AgUiEvent::run_error(run_id.clone(), error.message, Some(proto::stream_error::code_to_json_str(error.code).to_string()), Some(serde_json::json!({"origin":error.origin})));
                    }
                    break;
                }
                Some(result_output_item::Item::FinalCollected(_)) | None => {}
            }
        }
        if !emitted_text_end && emitted_text_start {
            yield AgUiEvent::text_message_end(message_id);
        }
        if !saw_done && !emitted_error {
            yield AgUiEvent::run_error(run_id, "LLM stream ended without a successful completion", Some("INCOMPLETE_STREAM".into()), None);
        }
    }
}

/// Build runner chat arguments while keeping client tools in manual execution mode.
pub fn build_chat_args(input: &RunAgentInput) -> Result<LlmChatArgs> {
    let mut messages =
        Vec::with_capacity(input.messages.len() + usize::from(input.context.is_some()));
    if let Some(context) = &input.context
        && !context.is_empty()
    {
        let context_text = context
            .iter()
            .map(|item| format!("{}: {}", item.description, item.value))
            .collect::<Vec<_>>()
            .join("\n");
        messages.push(ChatMessage {
            role: ChatRole::User as i32,
            body: Some(Body::Content(ProtoContent {
                content: Some(Content::Text(context_text)),
            })),
        });
    }

    let mut call_names = HashMap::new();
    for message in &input.messages {
        if let Some(calls) = &message.tool_calls {
            for call in calls {
                call_names.insert(call.id.clone(), call.function.name.clone());
            }
        }
    }
    for message in &input.messages {
        if matches!(message.role, Role::Activity | Role::Reasoning) {
            continue;
        }
        messages.push(convert_message(message, &call_names)?);
    }

    let client_tools_json = input.tools.as_ref().filter(|tools| !tools.is_empty()).map(|tools| {
        serde_json::to_string(&tools.iter().map(|tool| serde_json::json!({
            "type": "function",
            "function": {
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters.clone().unwrap_or_else(|| serde_json::json!({"type":"object","properties":{}})),
            }
        })).collect::<Vec<_>>())
    }).transpose()?;

    let function_options = client_tools_json.map(|client_tools_json| FunctionOptions {
        use_function_calling: true,
        function_set_name: None,
        use_runners_as_function: None,
        use_workers_as_function: None,
        is_auto_calling: Some(false),
        auto_select_function_set: Some(false),
        client_tools_json: Some(client_tools_json),
        tool_choice: None,
        parallel_tool_calls: Some(true),
    });
    let args = LlmChatArgs {
        messages,
        function_options,
        ..Default::default()
    };
    app_wrapper::llm::chat::conversion::ToolConverter::validate_chat_args(&args)?;
    Ok(args)
}

/// Text segments of a message. Registered chat workers take text only, so media parts
/// and activity content are rejected rather than silently dropped.
fn text_parts(content: &MessageContent) -> Result<Vec<&str>> {
    match content {
        MessageContent::Text(text) => Ok(vec![text.as_str()]),
        MessageContent::Parts(parts) => parts
            .iter()
            .map(|part| match part {
                ContentPart::Text { text, .. } => Ok(text.as_str()),
                ContentPart::Image { .. }
                | ContentPart::Audio { .. }
                | ContentPart::Video { .. }
                | ContentPart::Document { .. } => {
                    bail!("unsupported AG-UI content part for the registered LLM worker")
                }
            })
            .collect(),
        MessageContent::Object(_) => {
            bail!("activity content cannot be sent to the registered LLM worker")
        }
    }
}

fn convert_message(message: &Message, call_names: &HashMap<String, String>) -> Result<ChatMessage> {
    let role = match message.role {
        Role::System | Role::Developer => ChatRole::System,
        Role::User => ChatRole::User,
        Role::Assistant => ChatRole::Assistant,
        Role::Tool => ChatRole::Tool,
        Role::Activity | Role::Reasoning => {
            bail!("activity and reasoning messages must be filtered before model conversion")
        }
    };
    let mut parts = Vec::new();
    if let Some(content) = &message.content {
        if message.role == Role::Tool {
            let call_id = message
                .tool_call_id
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("tool message is missing toolCallId"))?;
            let fn_name = call_names.get(call_id).cloned().unwrap_or_default();
            let output = text_parts(content)?.join("\n");
            // AG-UI sends the failure alongside the output so a partial result survives it.
            let content = match &message.error {
                Some(error) if output.is_empty() => error.clone(),
                Some(error) => format!("{output}\n\nerror: {error}"),
                None => output,
            };
            parts.push(ProtoContent {
                content: Some(Content::ToolResults(ToolResults {
                    results: vec![ToolResult {
                        call_id: call_id.clone(),
                        fn_name,
                        content,
                        is_error: message.error.is_some(),
                    }],
                })),
            });
        } else {
            parts.extend(text_parts(content)?.into_iter().map(|text| ProtoContent {
                content: Some(Content::Text(text.to_string())),
            }));
        }
    }
    if let Some(calls) = &message.tool_calls {
        let calls = calls
            .iter()
            .map(|call| ProtoToolCall {
                call_id: call.id.clone(),
                fn_name: call.function.name.clone(),
                fn_arguments: call.function.arguments.clone(),
            })
            .collect();
        parts.push(ProtoContent {
            content: Some(Content::ToolCalls(ToolCalls { calls })),
        });
    }
    // A lone part keeps the legacy content body; tool results are only accepted there.
    let body = match parts.len() {
        0 => None,
        1 => parts.pop().map(Body::Content),
        _ => Some(Body::Parts(MessageParts { items: parts })),
    };
    Ok(ChatMessage {
        role: role as i32,
        body,
    })
}

/// Assert that a run ended once, and by its client cancelling it.
#[cfg(test)]
pub(crate) fn assert_cancelled_once(events: &[(u64, AgUiEvent)]) {
    let terminals = events
        .iter()
        .filter(|(_, event)| event.is_terminal())
        .count();
    assert_eq!(terminals, 1, "{events:?}");
    assert!(
        matches!(
            events.last(),
            Some((_, AgUiEvent::RunError { code: Some(code), .. })) if code == "CANCELLED"
        ),
        "{events:?}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventEncoder;
    use crate::session::InMemoryEventStore;
    use crate::types::{Context, MessageId, Tool, ToolCall};
    use crate::types::{RunId, ThreadId};
    use futures::StreamExt;
    use jobworkerp_runner::jobworkerp::runner::llm::{
        LlmChatResult,
        llm_chat_result::{
            MessageContent as ResultContent, message_content as result_message_content,
        },
    };
    use proto::jobworkerp::data::{ResultOutputItem, result_output_item};
    use std::sync::Arc;

    fn body(message: &ChatMessage) -> &Body {
        message.body.as_ref().expect("message body should be set")
    }

    fn single_content(message: &ChatMessage) -> &Content {
        match body(message) {
            Body::Content(content) => content.content.as_ref().expect("content should be set"),
            other => panic!("expected a single content body, got {other:?}"),
        }
    }

    fn input_with(messages: Vec<Message>) -> RunAgentInput {
        RunAgentInput {
            thread_id: "thread".into(),
            run_id: "run".into(),
            messages,
            protocol_version: None,
            parent_run_id: None,
            state: None,
            tools: None,
            context: None,
            forwarded_props: None,
            resume: None,
        }
    }

    fn assistant_calling(text: Option<&str>, arguments: serde_json::Value) -> Message {
        let mut assistant =
            Message::assistant(text.unwrap_or_default()).with_id(MessageId::new("assistant"));
        if text.is_none() {
            assistant.content = None;
        }
        assistant.tool_calls = Some(vec![ToolCall::new("call-1", "lookup", arguments)]);
        assistant
    }

    #[test]
    fn single_part_messages_keep_the_content_body() {
        let mut input = input_with(vec![
            Message::user("hello").with_id(MessageId::new("user")),
            assistant_calling(None, serde_json::json!({})),
        ]);
        input.context = Some(vec![Context::new("locale", "ja")]);

        let args = build_chat_args(&input).unwrap();

        assert_eq!(args.messages.len(), 3);
        assert_eq!(
            single_content(&args.messages[0]),
            &Content::Text("locale: ja".into())
        );
        assert_eq!(
            single_content(&args.messages[1]),
            &Content::Text("hello".into())
        );
        assert!(matches!(
            single_content(&args.messages[2]),
            Content::ToolCalls(calls) if calls.calls[0].call_id == "call-1"
        ));
    }

    fn event_types(events: &[(u64, AgUiEvent)]) -> Vec<&'static str> {
        events.iter().map(|(_, event)| event.event_type()).collect()
    }

    fn result_item(result: LlmChatResult) -> ResultOutputItem {
        ResultOutputItem {
            item: Some(result_output_item::Item::Data(result.encode_to_vec())),
        }
    }

    #[test]
    fn direct_chat_keeps_assistant_text_and_client_tool_call_and_disables_server_execution() {
        let mut input = input_with(vec![assistant_calling(
            Some("Checking."),
            serde_json::json!({"q":"x"}),
        )]);
        input.tools = Some(vec![Tool {
            name: "lookup".into(),
            description: "Look up".into(),
            parameters: None,
            metadata: None,
        }]);
        let args = build_chat_args(&input).unwrap();
        assert_eq!(args.messages.len(), 1);
        assert!(
            matches!(body(&args.messages[0]), Body::Parts(parts) if matches!(
                parts.items.as_slice(),
                [
                    ProtoContent { content: Some(Content::Text(_)) },
                    ProtoContent { content: Some(Content::ToolCalls(_)) },
                ]
            )),
            "assistant text and tool calls must share one ordered parts body"
        );
        let options = args.function_options.unwrap();
        assert_eq!(options.is_auto_calling, Some(false));
        assert!(options.function_set_name.is_none());
        assert!(options.client_tools_json.unwrap().contains("lookup"));
        input.messages[0].role = Role::Reasoning;
        assert!(build_chat_args(&input).unwrap().messages.is_empty());
    }

    #[test]
    fn tool_results_keep_the_matching_function_name() {
        let input = input_with(vec![
            assistant_calling(Some(""), serde_json::json!({})),
            Message::tool("call-1", "done").with_id(MessageId::new("result")),
        ]);
        let args = build_chat_args(&input).unwrap();
        let message_content = single_content(&args.messages[1]);
        assert!(
            matches!(message_content, Content::ToolResults(results) if results.results[0].call_id == "call-1" && results.results[0].fn_name == "lookup" && results.results[0].content == "done")
        );
    }

    fn tool_result_of(tool: Message) -> Result<ToolResult> {
        let args = build_chat_args(&input_with(vec![
            assistant_calling(None, serde_json::json!({})),
            tool,
        ]))?;
        match single_content(&args.messages[1]) {
            Content::ToolResults(results) => Ok(results.results[0].clone()),
            other => panic!("expected tool results, got {other:?}"),
        }
    }

    #[test]
    fn tool_error_is_kept_with_its_partial_result() {
        let mut tool = Message::tool("call-1", "partial rows");
        tool.error = Some("upstream timed out".into());
        let result = tool_result_of(tool).unwrap();
        assert!(result.is_error);
        assert_eq!(result.content, "partial rows\n\nerror: upstream timed out");

        let mut without_output = Message::tool("call-1", "");
        without_output.error = Some("upstream timed out".into());
        let result = tool_result_of(without_output).unwrap();
        assert!(result.is_error);
        assert_eq!(result.content, "upstream timed out");
    }

    #[test]
    fn tool_result_keeps_text_parts_and_rejects_media_parts() {
        let tool_with = |content: serde_json::Value| -> Message {
            serde_json::from_value(serde_json::json!({
                "id": "result",
                "role": "tool",
                "toolCallId": "call-1",
                "content": content,
            }))
            .expect("valid tool message")
        };
        let text_only = tool_result_of(tool_with(serde_json::json!([
            {"type": "text", "text": "first"},
            {"type": "text", "text": "second"},
        ])))
        .unwrap();
        assert!(!text_only.is_error);
        assert_eq!(text_only.content, "first\nsecond");

        let error = tool_result_of(tool_with(serde_json::json!([
            {"type": "text", "text": "chart"},
            {"type": "image", "source": {"type": "data", "value": "aGVsbG8=", "mimeType": "image/png"}},
        ])))
        .expect_err("media cannot be carried by a text tool result");
        assert!(error.to_string().contains("unsupported"), "{error}");
    }

    #[test]
    fn chat_args_survive_the_registered_job_descriptor_round_trip() {
        let input = input_with(vec![
            Message::user("find rust"),
            assistant_calling(Some("Searching."), serde_json::json!({"q":"rust"})),
            Message::tool("call-1", "found rust"),
        ]);
        let args = build_chat_args(&input).unwrap();
        let proto =
            include_str!("../../runner/protobuf/jobworkerp/runner/llm/chat_args.proto").to_owned();
        let descriptor = command_utils::protobuf::ProtobufDescriptor::new(&proto).unwrap();
        let message_descriptor = descriptor
            .get_message_by_name("jobworkerp.runner.llm.LLMChatArgs")
            .unwrap();

        let bytes = args.encode_to_vec();
        let decoded_dynamic = command_utils::protobuf::ProtobufDescriptor::get_message_from_bytes(
            message_descriptor,
            &bytes,
        )
        .unwrap();
        let decoded = LlmChatArgs::decode(decoded_dynamic.encode_to_vec().as_slice()).unwrap();
        assert_eq!(decoded, args);
    }

    #[tokio::test]
    async fn direct_stream_emits_text_and_deduplicates_completed_client_calls() {
        let text = result_item(LlmChatResult {
            content: Some(ResultContent {
                content: Some(result_message_content::Content::Text("hello".into())),
            }),
            done: false,
            ..Default::default()
        });
        let call = LlmChatResult {
            content: Some(ResultContent {
                content: Some(result_message_content::Content::ToolCalls(
                    result_message_content::ToolCalls {
                        calls: vec![result_message_content::ToolCall {
                            call_id: "c1".into(),
                            fn_name: "lookup".into(),
                            fn_arguments: "{}".into(),
                        }],
                    },
                )),
            }),
            done: true,
            ..Default::default()
        };
        let call = result_item(call);
        let duplicate_call = call.clone();
        let end = ResultOutputItem {
            item: Some(result_output_item::Item::End(Default::default())),
        };
        let source = futures::stream::iter(vec![text, call, duplicate_call, end]).boxed();
        let events = convert_direct_stream(source, MessageId::new("m1"), "r1".into())
            .collect::<Vec<_>>()
            .await;
        let types = events.iter().map(AgUiEvent::event_type).collect::<Vec<_>>();
        assert_eq!(
            types,
            vec![
                "TEXT_MESSAGE_START",
                "TEXT_MESSAGE_CONTENT",
                "TOOL_CALL_START",
                "TOOL_CALL_ARGS",
                "TOOL_CALL_END",
                "TEXT_MESSAGE_END"
            ]
        );
    }

    #[tokio::test]
    async fn direct_stream_without_done_reports_one_error() {
        let source = futures::stream::iter(vec![ResultOutputItem {
            item: Some(result_output_item::Item::End(Default::default())),
        }])
        .boxed();
        let events = convert_direct_stream(source, MessageId::new("m1"), "r1".into())
            .collect::<Vec<_>>()
            .await;
        assert_eq!(
            events
                .iter()
                .filter(|e| e.event_type() == "RUN_ERROR")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn final_text_snapshot_does_not_duplicate_previously_streamed_deltas() {
        let delta = result_item(LlmChatResult {
            content: Some(ResultContent {
                content: Some(result_message_content::Content::Text("hello".into())),
            }),
            done: false,
            ..Default::default()
        });
        let snapshot = result_item(LlmChatResult {
            content: Some(ResultContent {
                content: Some(result_message_content::Content::Text("hello".into())),
            }),
            done: true,
            ..Default::default()
        });
        let end = ResultOutputItem {
            item: Some(result_output_item::Item::End(Default::default())),
        };
        let source = futures::stream::iter(vec![delta, snapshot, end]).boxed();
        let events = convert_direct_stream(source, MessageId::new("m1"), "r1".into())
            .collect::<Vec<_>>()
            .await;
        let text = events
            .iter()
            .filter_map(|event| match event {
                AgUiEvent::TextMessageContent { delta, .. } => Some(delta.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert_eq!(text, "hello");
    }

    #[tokio::test]
    async fn inconsistent_terminal_text_snapshot_fails_the_run() {
        let delta = result_item(LlmChatResult {
            content: Some(ResultContent {
                content: Some(result_message_content::Content::Text("hello".into())),
            }),
            done: false,
            ..Default::default()
        });
        let snapshot = result_item(LlmChatResult {
            content: Some(ResultContent {
                content: Some(result_message_content::Content::Text("hullo".into())),
            }),
            done: true,
            ..Default::default()
        });
        let events = convert_direct_stream(
            futures::stream::iter(vec![delta, snapshot]).boxed(),
            MessageId::new("m1"),
            "r1".into(),
        )
        .collect::<Vec<_>>()
        .await;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type() == "RUN_ERROR")
                .count(),
            1
        );
        assert!(!events.iter().any(
            |event| matches!(event, AgUiEvent::TextMessageContent { delta, .. } if delta == "hullo")
        ));
        assert_eq!(events.last().unwrap().event_type(), "RUN_ERROR");
    }

    #[tokio::test]
    async fn event_producer_keeps_persisting_after_live_subscriber_disconnects() {
        let store = Arc::new(InMemoryEventStore::new(32, 60));
        let encoder = Arc::new(EventEncoder::new());
        let (terminal_tx, terminal_rx) = tokio::sync::oneshot::channel();
        let (receiver, _producer) = spawn_event_producer(
            RunId::new("run-background"),
            ThreadId::new("thread-background"),
            futures::stream::iter(vec![AgUiEvent::text_message_content("m1", "hello")]),
            store.clone(),
            encoder,
            CancellationToken::new(),
            move |state| async move {
                let _ = terminal_tx.send(state);
            },
        );
        drop(receiver);
        assert_eq!(terminal_rx.await.unwrap(), SessionState::Completed);
        let stored = store.get_all_events("run-background").await;
        assert_eq!(
            event_types(&stored),
            vec!["RUN_STARTED", "TEXT_MESSAGE_CONTENT", "RUN_FINISHED"]
        );
    }

    #[tokio::test]
    async fn event_producer_cleans_up_after_failure_without_emitting_success() {
        let store = Arc::new(InMemoryEventStore::new(32, 60));
        let encoder = Arc::new(EventEncoder::new());
        let (terminal_tx, terminal_rx) = tokio::sync::oneshot::channel();
        let (receiver, _producer) = spawn_event_producer(
            RunId::new("run-failed"),
            ThreadId::new("thread-failed"),
            futures::stream::iter(vec![AgUiEvent::run_error(
                "run-failed",
                "failed",
                None,
                None,
            )]),
            store.clone(),
            encoder,
            CancellationToken::new(),
            move |state| async move {
                let _ = terminal_tx.send(state);
            },
        );
        drop(receiver);
        assert_eq!(terminal_rx.await.unwrap(), SessionState::Error);
        let stored = store.get_all_events("run-failed").await;
        assert!(
            stored
                .iter()
                .any(|(_, event)| event.event_type() == "RUN_ERROR")
        );
        assert!(
            !stored
                .iter()
                .any(|(_, event)| event.event_type() == "RUN_FINISHED")
        );
    }

    #[tokio::test]
    async fn cancelled_producer_stores_one_terminal_and_stops_its_source() {
        let store = Arc::new(InMemoryEventStore::new(32, 60));
        let (terminal_tx, terminal_rx) = tokio::sync::oneshot::channel();
        let cancel = CancellationToken::new();
        let source = futures::stream::iter(vec![
            AgUiEvent::text_message_start("m1", Role::Assistant),
            AgUiEvent::text_message_content("m1", "hel"),
        ])
        .chain(futures::stream::pending());
        let (mut receiver, producer) = spawn_event_producer(
            RunId::new("run-cancelled"),
            ThreadId::new("thread-cancelled"),
            source,
            store.clone(),
            Arc::new(EventEncoder::new()),
            cancel.clone(),
            move |state| async move {
                let _ = terminal_tx.send(state);
            },
        );
        for _ in 0..3 {
            receiver.next().await.expect("events before cancellation");
        }

        cancel.cancel();
        producer.await.unwrap();

        assert_eq!(terminal_rx.await.unwrap(), SessionState::Cancelled);
        let live = receiver.collect::<Vec<_>>().await;
        assert_eq!(event_types(&live), ["TEXT_MESSAGE_END", "RUN_ERROR"]);
        let stored = store.get_all_events("run-cancelled").await;
        assert_eq!(
            event_types(&stored),
            [
                "RUN_STARTED",
                "TEXT_MESSAGE_START",
                "TEXT_MESSAGE_CONTENT",
                "TEXT_MESSAGE_END",
                "RUN_ERROR"
            ]
        );
        assert!(matches!(
            &stored.last().unwrap().1,
            AgUiEvent::RunError { code: Some(code), .. } if code == "CANCELLED"
        ));
    }

    #[tokio::test]
    async fn cancellation_does_not_wait_for_a_subscriber_that_stopped_reading() {
        let store = Arc::new(InMemoryEventStore::new(256, 60));
        let (terminal_tx, terminal_rx) = tokio::sync::oneshot::channel();
        let cancel = CancellationToken::new();
        let source = futures::stream::iter(
            (0..100).map(|index| AgUiEvent::text_message_content("m1", index.to_string())),
        )
        .chain(futures::stream::pending());
        let (_unread, producer) = spawn_event_producer(
            RunId::new("run-stalled"),
            ThreadId::new("thread-stalled"),
            source,
            store.clone(),
            Arc::new(EventEncoder::new()),
            cancel.clone(),
            move |state| async move {
                let _ = terminal_tx.send(state);
            },
        );
        // The live channel is full once RUN_STARTED and a buffer of deltas are stored.
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while store.get_all_events("run-stalled").await.len() <= LIVE_EVENT_BUFFER {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the producer fills the live channel");

        cancel.cancel();

        tokio::time::timeout(std::time::Duration::from_secs(5), producer)
            .await
            .expect("cancellation must not wait for live delivery")
            .unwrap();
        assert_eq!(terminal_rx.await.unwrap(), SessionState::Cancelled);
        assert_cancelled_once(&store.get_all_events("run-stalled").await);
    }

    /// Dropped with the producer's source: reports the drop, then blocks until released,
    /// holding the producer between cancellation and settlement.
    struct HeldDrop {
        dropping: Option<tokio::sync::oneshot::Sender<()>>,
        release: std::sync::mpsc::Receiver<()>,
    }

    impl Drop for HeldDrop {
        fn drop(&mut self) {
            if let Some(dropping) = self.dropping.take() {
                let _ = dropping.send(());
            }
            let _ = self.release.recv();
        }
    }

    #[test]
    fn a_cancel_arriving_before_settlement_waits_for_the_same_terminal_event() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            use crate::session::{InMemorySessionManager, SessionManager};

            let app = app::module::test::create_hybrid_test_app().await.unwrap();
            let sessions = Arc::new(InMemorySessionManager::new(60));
            let run_id = "run-slow-settle";
            let session = sessions
                .create_session(RunId::new(run_id), ThreadId::new("thread-slow-settle"))
                .await;
            let store = Arc::new(InMemoryEventStore::new(32, 60));
            let (dropping_tx, dropping_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let held = HeldDrop {
                dropping: Some(dropping_tx),
                release: release_rx,
            };
            let source = futures::stream::pending::<AgUiEvent>().inspect(move |_| {
                let _ = &held;
            });
            let runs = DirectRuns::default();
            let _live = runs
                .start(
                    JobId { value: 1 },
                    &session,
                    source,
                    store.clone(),
                    Arc::new(EventEncoder::new()),
                    sessions.clone(),
                )
                .await;
            let first = tokio::spawn({
                let runs = runs.clone();
                let job_app = app.job_app.clone();
                async move { runs.cancel(job_app.as_ref(), run_id).await }
            });
            dropping_rx.await.unwrap();

            assert!(
                runs.is_running(run_id).await,
                "a run stays registered until its producer settles"
            );
            let mut concurrent = std::pin::pin!(runs.cancel(app.job_app.as_ref(), run_id));
            let waiting =
                tokio::time::timeout(std::time::Duration::from_millis(300), concurrent.as_mut())
                    .await;
            assert!(
                waiting.is_err(),
                "a concurrent cancel waits for the run to settle"
            );
            release_tx.send(()).unwrap();

            assert!(concurrent.await.unwrap());
            // Settled by the time any cancel returns.
            assert_cancelled_once(&store.get_all_events(run_id).await);
            let state = sessions
                .get_session(&session.session_id)
                .await
                .unwrap()
                .state;
            assert_eq!(state, SessionState::Cancelled);
            assert!(first.await.unwrap().unwrap());
            assert!(!runs.is_running(run_id).await);
        });
    }

    #[tokio::test]
    async fn direct_output_is_forwarded_before_the_job_result_arrives() {
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        let result_future = Box::pin(async move {
            result_rx
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!("result listener dropped")))
        });
        let source = futures::stream::iter(vec![
            AgUiEvent::text_message_start("m1", Role::Assistant),
            AgUiEvent::text_message_content("m1", "first chunk"),
        ])
        .boxed();
        let mut output = Box::pin(direct_events_with_job_result(
            source,
            result_future,
            "r1".into(),
            MessageId::new("m1"),
        ));
        assert_eq!(
            output.next().await.unwrap().event_type(),
            "TEXT_MESSAGE_START"
        );
        assert_eq!(
            output.next().await.unwrap().event_type(),
            "TEXT_MESSAGE_CONTENT"
        );
        let _ = result_tx.send(Err(anyhow::anyhow!("job failed")));
        assert_eq!(
            output.next().await.unwrap().event_type(),
            "TEXT_MESSAGE_END"
        );
        assert_eq!(output.next().await.unwrap().event_type(), "RUN_ERROR");
    }
}
