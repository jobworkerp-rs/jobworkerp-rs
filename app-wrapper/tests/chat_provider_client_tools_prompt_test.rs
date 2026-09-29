use anyhow::{Context, Result, ensure};
use app::app::function::function_set::FunctionSetApp;
use app::module::{AppModule, test::create_hybrid_test_app};
use app_wrapper::llm::chat::{genai::GenaiChatService, ollama::OllamaChatService};
use futures::StreamExt;
use infra_utils::infra::test::TEST_RUNTIME;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::{
    Content as ArgsContent, ToolExecutionRequest, ToolExecutionRequests, ToolResult, ToolResults,
};
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
    ChatMessage, ChatRole, FunctionOptions, MessageContent, message_content,
};
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::{
    GenaiRunnerSettings, OllamaRunnerSettings,
};
use jobworkerp_runner::jobworkerp::runner::llm::{LlmChatArgs, LlmChatResult};
use prost::Message;
use proto::jobworkerp::data::RunnerId;
use proto::jobworkerp::data::result_output_item;
use proto::jobworkerp::function::data::{FunctionId, FunctionSetData, FunctionUsing, function_id};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

const GENAI_MODEL: &str = "openai::gpt-4o-mini";
const OLLAMA_MODEL: &str = "client-tools-contract-test";
const CLIENT_TOOLS_JSON: &str = r#"[
  {"type":"function","function":{"name":"client_lookup","description":"Look up client-owned data","parameters":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}}}
]"#;

#[derive(Clone)]
enum MockReply {
    GenaiText,
    GenaiToolCall,
    GenaiStreamText,
    GenaiStreamToolCall,
    GenaiStreamPrefixedToolCall,
    OllamaText,
    OllamaToolCall,
    OllamaStreamText,
    OllamaStreamToolCall,
    OllamaStreamPrefixedToolCall,
}

impl MockReply {
    fn response(&self) -> (&'static str, String) {
        match self {
            Self::GenaiText => (
                "application/json",
                json!({
                    "id": "chatcmpl-contract-test",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "gpt-4o-mini",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "complete"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                })
                .to_string(),
            ),
            Self::GenaiToolCall => (
                "application/json",
                genai_tool_call_response().to_string(),
            ),
            Self::GenaiStreamText => (
                "text/event-stream",
                concat!(
                    "data: {\"id\":\"chatcmpl-contract-test\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"complete\"},\"finish_reason\":null}]}\n\n",
                    "data: {\"id\":\"chatcmpl-contract-test\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                    "data: [DONE]\n\n"
                )
                .to_string(),
            ),
            Self::GenaiStreamToolCall => (
                "text/event-stream",
                genai_stream_tool_call_response("client_lookup"),
            ),
            Self::GenaiStreamPrefixedToolCall => (
                "text/event-stream",
                genai_stream_tool_call_response("select_toolset_client_owned"),
            ),
            Self::OllamaText => (
                "application/json",
                ollama_response(false).to_string(),
            ),
            Self::OllamaToolCall => (
                "application/json",
                ollama_response(true).to_string(),
            ),
            Self::OllamaStreamText => (
                "application/x-ndjson",
                format!("{}\n", ollama_response(false)),
            ),
            Self::OllamaStreamToolCall => (
                "application/x-ndjson",
                format!("{}\n", ollama_response(true)),
            ),
            Self::OllamaStreamPrefixedToolCall => (
                "application/x-ndjson",
                format!(
                    "{}\n",
                    ollama_response_with_tool_name(Some("select_toolset_client_owned"))
                ),
            ),
        }
    }
}

fn genai_tool_call_response() -> Value {
    json!({
        "id": "chatcmpl-contract-test",
        "object": "chat.completion",
        "created": 1,
        "model": "gpt-4o-mini",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-client-1",
                    "type": "function",
                    "function": {
                        "name": "client_lookup",
                        "arguments": "{\"query\":\"hello\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })
}

fn genai_stream_tool_call_response(tool_name: &str) -> String {
    let tool_call_chunk = json!({
        "id": "chatcmpl-contract-test",
        "object": "chat.completion.chunk",
        "created": 1,
        "model": "gpt-4o-mini",
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "tool_calls": [{
                    "index": 0,
                    "id": "call-client-1",
                    "type": "function",
                    "function": {
                        "name": tool_name,
                        "arguments": "{\"query\":\"hello\"}"
                    }
                }]
            },
            "finish_reason": null
        }]
    });
    let end_chunk = json!({
        "id": "chatcmpl-contract-test",
        "object": "chat.completion.chunk",
        "created": 1,
        "model": "gpt-4o-mini",
        "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]
    });
    format!("data: {tool_call_chunk}\n\ndata: {end_chunk}\n\ndata: [DONE]\n\n")
}

fn ollama_response(tool_call: bool) -> Value {
    ollama_response_with_tool_name(tool_call.then_some("client_lookup"))
}

fn ollama_response_with_tool_name(tool_name: Option<&str>) -> Value {
    let message = if let Some(tool_name) = tool_name {
        json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "function": {"name": tool_name, "arguments": {"query": "hello"}}
            }]
        })
    } else {
        json!({"role": "assistant", "content": "complete"})
    };
    json!({
        "model": OLLAMA_MODEL,
        "created_at": "2026-01-01T00:00:00Z",
        "message": message,
        "done": true,
        "total_duration": 0,
        "load_duration": 0,
        "prompt_eval_count": 1,
        "prompt_eval_duration": 0,
        "eval_count": 1,
        "eval_duration": 0
    })
}

struct MockServer {
    base_url: String,
    requests: tokio::sync::mpsc::UnboundedReceiver<Value>,
    task: JoinHandle<Result<()>>,
}

impl MockServer {
    async fn next_request(&mut self) -> Result<Value> {
        self.requests
            .recv()
            .await
            .context("mock provider did not receive a request")
    }

    async fn finish(self) -> Result<()> {
        self.task.await.context("mock provider task panicked")??;
        Ok(())
    }
}

async fn start_mock_server(replies: Vec<MockReply>) -> Result<MockServer> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base_url = format!("http://{}", listener.local_addr()?);
    let (request_tx, requests) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        for reply in replies {
            let (mut socket, _) = listener.accept().await?;
            let request = read_request_body(&mut socket).await?;
            request_tx
                .send(request)
                .map_err(|_| anyhow::anyhow!("test receiver dropped"))?;
            write_response(&mut socket, reply).await?;
        }
        Ok(())
    });
    Ok(MockServer {
        base_url,
        requests,
        task,
    })
}

async fn read_request_body(socket: &mut TcpStream) -> Result<Value> {
    let mut bytes = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        if let Some(header_end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let body_start = header_end + 4;
            let headers = String::from_utf8_lossy(&bytes[..header_end]).to_ascii_lowercase();
            if let Some(length) = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
            {
                if bytes.len() >= body_start + length {
                    return Ok(serde_json::from_slice(
                        &bytes[body_start..body_start + length],
                    )?);
                }
            } else if headers.contains("transfer-encoding: chunked")
                && let Some(body) = decode_chunked_body(&bytes[body_start..])
            {
                return Ok(serde_json::from_slice(&body)?);
            }
        }
        let read = socket.read(&mut chunk).await?;
        ensure!(
            read > 0,
            "provider closed the request before the body arrived"
        );
        bytes.extend_from_slice(&chunk[..read]);
        ensure!(
            bytes.len() <= 1_048_576,
            "provider request exceeded test limit"
        );
    }
}

fn decode_chunked_body(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut cursor = 0;
    let mut body = Vec::new();
    loop {
        let line_end = bytes[cursor..].windows(2).position(|w| w == b"\r\n")? + cursor;
        let size = usize::from_str_radix(
            std::str::from_utf8(&bytes[cursor..line_end])
                .ok()?
                .split(';')
                .next()?,
            16,
        )
        .ok()?;
        cursor = line_end + 2;
        if size == 0 {
            return Some(body);
        }
        let end = cursor.checked_add(size)?;
        if bytes.len() < end + 2 {
            return None;
        }
        body.extend_from_slice(&bytes[cursor..end]);
        cursor = end + 2;
    }
}

async fn write_response(socket: &mut TcpStream, reply: MockReply) -> Result<()> {
    let (content_type, body) = reply.response();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(response.as_bytes()).await?;
    Ok(())
}

async fn genai_service(app: &AppModule, base_url: &str) -> Result<GenaiChatService> {
    if std::env::var_os("OPENAI_API_KEY").is_none() {
        unsafe { std::env::set_var("OPENAI_API_KEY", "contract-test") };
    }
    GenaiChatService::new(
        app.function_app.clone(),
        app.function_set_app.clone(),
        GenaiRunnerSettings {
            model: GENAI_MODEL.to_string(),
            base_url: Some(base_url.to_string()),
            system_prompt: None,
        },
    )
    .await
}

fn ollama_service(app: &AppModule, base_url: &str) -> Result<OllamaChatService> {
    OllamaChatService::new(
        app.function_app.clone(),
        app.function_set_app.clone(),
        OllamaRunnerSettings {
            model: OLLAMA_MODEL.to_string(),
            base_url: Some(base_url.to_string()),
            system_prompt: None,
            pull_model: Some(false),
        },
    )
}

fn system_args(request_prompts: &[&str]) -> LlmChatArgs {
    let mut messages: Vec<_> = request_prompts
        .iter()
        .map(|prompt| ChatMessage {
            role: ChatRole::System as i32,
            content: Some(MessageContent {
                content: Some(ArgsContent::Text((*prompt).to_string())),
            }),
        })
        .collect();
    messages.push(ChatMessage {
        role: ChatRole::User as i32,
        content: Some(MessageContent {
            content: Some(ArgsContent::Text("Say complete".to_string())),
        }),
    });
    LlmChatArgs {
        messages,
        ..Default::default()
    }
}

fn system_messages(request: &Value) -> Vec<String> {
    request["messages"]
        .as_array()
        .expect("chat request should include messages")
        .iter()
        .filter(|message| message["role"] == "system")
        .map(|message| {
            message["content"]
                .as_str()
                .expect("system message should contain text")
                .to_string()
        })
        .collect()
}

async fn run_genai_stream(
    service: &GenaiChatService,
    args: LlmChatArgs,
) -> Result<Vec<LlmChatResult>> {
    let mut stream = service
        .request_chat_stream(args, HashMap::new(), None)
        .await?;
    let mut results = Vec::new();
    while let Some(item) = stream.next().await {
        if let Some(result_output_item::Item::Data(bytes)) = item.item {
            results.push(LlmChatResult::decode(bytes.as_slice())?);
        }
    }
    Ok(results)
}

async fn run_ollama_stream(
    service: Arc<OllamaChatService>,
    args: LlmChatArgs,
) -> Result<Vec<LlmChatResult>> {
    let mut stream = service
        .request_stream_chat(args, HashMap::new(), None)
        .await?;
    let mut results = Vec::new();
    while let Some(result) = stream.next().await {
        results.push(result);
    }
    Ok(results)
}

fn assert_tool_definition(request: &Value) -> Result<()> {
    assert_tool_definition_named(request, "client_lookup")
}

fn assert_tool_definition_named(request: &Value, tool_name: &str) -> Result<()> {
    let tools = request["tools"]
        .as_array()
        .context("provider request did not include client tools")?;
    ensure!(
        tools.len() == 1,
        "expected exactly one client tool, got {tools:?}"
    );
    ensure!(
        tools[0]["function"]["name"] == tool_name,
        "client tool name was not forwarded: {tools:?}"
    );
    ensure!(
        tools[0]["function"]["parameters"]["properties"]["query"]["type"] == "string",
        "client schema was not forwarded: {tools:?}"
    );
    Ok(())
}

fn client_args() -> LlmChatArgs {
    client_args_with_tool_json(CLIENT_TOOLS_JSON)
}

fn client_args_with_tool_name(tool_name: &str) -> LlmChatArgs {
    let tool_json = json!([{
        "type": "function",
        "function": {
            "name": tool_name,
            "description": "Client-owned tool for contract testing",
            "parameters": {
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"]
            }
        }
    }])
    .to_string();
    client_args_with_tool_json(&tool_json)
}

fn client_args_with_tool_json(tool_json: &str) -> LlmChatArgs {
    let mut args = system_args(&[]);
    args.function_options = Some(FunctionOptions {
        client_tools_json: Some(tool_json.to_string()),
        is_auto_calling: Some(true),
        ..Default::default()
    });
    args
}

fn client_tool_execution_request_args() -> LlmChatArgs {
    let mut args = client_args();
    args.messages.push(ChatMessage {
        role: ChatRole::Assistant as i32,
        content: Some(MessageContent {
            content: Some(ArgsContent::ToolCalls(message_content::ToolCalls {
                calls: vec![message_content::ToolCall {
                    call_id: "client-call-1".to_string(),
                    fn_name: "client_lookup".to_string(),
                    fn_arguments: r#"{"query":"hello"}"#.to_string(),
                }],
            })),
        }),
    });
    args.messages.push(ChatMessage {
        role: ChatRole::Tool as i32,
        content: Some(MessageContent {
            content: Some(ArgsContent::ToolExecutionRequests(ToolExecutionRequests {
                requests: vec![ToolExecutionRequest {
                    call_id: "client-call-1".to_string(),
                    fn_name: "client_lookup".to_string(),
                    fn_arguments: r#"{"query":"hello"}"#.to_string(),
                }],
            })),
        }),
    });
    args
}

fn continuation_args(
    mut args: LlmChatArgs,
    call_id: &str,
    fn_name: &str,
    fn_arguments: &str,
) -> LlmChatArgs {
    args.messages.push(ChatMessage {
        role: ChatRole::Assistant as i32,
        content: Some(MessageContent {
            content: Some(ArgsContent::ToolCalls(message_content::ToolCalls {
                calls: vec![message_content::ToolCall {
                    call_id: call_id.to_string(),
                    fn_name: fn_name.to_string(),
                    fn_arguments: fn_arguments.to_string(),
                }],
            })),
        }),
    });
    args.messages.push(ChatMessage {
        role: ChatRole::Tool as i32,
        content: Some(MessageContent {
            content: Some(ArgsContent::ToolResults(ToolResults {
                results: vec![ToolResult {
                    call_id: call_id.to_string(),
                    fn_name: fn_name.to_string(),
                    content: "client supplied result".to_string(),
                    is_error: false,
                }],
            })),
        }),
    });
    args
}

fn assert_tool_result_in_history(request: &Value) -> Result<()> {
    let messages = request["messages"].as_array().context("missing messages")?;
    ensure!(
        messages.iter().any(|message| {
            message["role"] == "tool"
                && (message["content"] == "client supplied result"
                    || message["content"]
                        .as_str()
                        .is_some_and(|s| s.contains("client supplied result")))
        }),
        "client tool result was not included in provider history: {messages:?}"
    );
    Ok(())
}

fn client_tool_conflicts() -> Vec<FunctionOptions> {
    vec![
        FunctionOptions {
            client_tools_json: Some(CLIENT_TOOLS_JSON.to_string()),
            function_set_name: Some("server-set".to_string()),
            ..Default::default()
        },
        FunctionOptions {
            client_tools_json: Some(CLIENT_TOOLS_JSON.to_string()),
            auto_select_function_set: Some(true),
            ..Default::default()
        },
        FunctionOptions {
            client_tools_json: Some(CLIENT_TOOLS_JSON.to_string()),
            use_runners_as_function: Some(false),
            ..Default::default()
        },
        FunctionOptions {
            client_tools_json: Some(CLIENT_TOOLS_JSON.to_string()),
            use_workers_as_function: Some(false),
            ..Default::default()
        },
    ]
}

fn assert_pending_tool(result: &LlmChatResult) -> Result<()> {
    assert_pending_tool_named(result, "client_lookup")
}

fn assert_pending_tool_named(result: &LlmChatResult, tool_name: &str) -> Result<()> {
    ensure!(!result.done, "client tool call should remain pending");
    ensure!(
        result.requires_tool_execution == Some(true),
        "client tool call should request client execution"
    );
    let pending = result
        .pending_tool_calls
        .as_ref()
        .context("client tool call should be returned as pending")?;
    ensure!(pending.calls.len() == 1, "expected one pending call");
    ensure!(pending.calls[0].fn_name == tool_name, "wrong pending tool");
    Ok(())
}

async fn create_legacy_function_set(app: &AppModule) -> Result<String> {
    let name = format!("legacy-client-tools-{}", uuid::Uuid::new_v4());
    app.function_set_app
        .create_function_set(&FunctionSetData {
            name: name.clone(),
            description: "Legacy FunctionSet compatibility test".to_string(),
            category: 0,
            targets: vec![FunctionUsing {
                function_id: Some(FunctionId {
                    id: Some(function_id::Id::RunnerId(RunnerId { value: 1 })),
                }),
                using: None,
            }],
        })
        .await?;
    Ok(name)
}

#[test]
fn genai_system_prompt_contract_covers_streaming_and_non_streaming() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let replies = vec![MockReply::GenaiText; 4]
            .into_iter()
            .chain(vec![MockReply::GenaiStreamText; 4])
            .collect();
        let mut server = start_mock_server(replies).await?;
        let app = create_hybrid_test_app().await?;
        let mut service = genai_service(&app, &server.base_url).await?;
        let cases = [
            (
                None,
                vec!["request one", "request two"],
                vec!["request one", "request two"],
            ),
            (Some("worker prompt"), vec![], vec!["worker prompt"]),
            (
                Some("worker prompt"),
                vec!["request prompt"],
                vec!["worker prompt\nrequest prompt"],
            ),
            (None, vec![], vec![]),
        ];

        for streaming in [false, true] {
            for (configured, request, expected) in &cases {
                service.system_prompt = configured.map(str::to_string);
                let args = system_args(request);
                if streaming {
                    let results = run_genai_stream(&service, args).await?;
                    ensure!(
                        results.iter().any(|result| result.done),
                        "stream did not finish"
                    );
                } else {
                    let result = service
                        .request_chat(args, opentelemetry::Context::current(), HashMap::new())
                        .await?;
                    ensure!(result.done, "non-stream response did not finish");
                }
                let captured = server.next_request().await?;
                ensure!(
                    system_messages(&captured) == *expected,
                    "unexpected GenAI system messages: {captured}"
                );
            }
        }
        server.finish().await
    })
}

#[test]
fn ollama_system_prompt_contract_covers_streaming_and_non_streaming() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let replies = vec![MockReply::OllamaText; 4]
            .into_iter()
            .chain(vec![MockReply::OllamaStreamText; 4])
            .collect();
        let mut server = start_mock_server(replies).await?;
        let app = create_hybrid_test_app().await?;
        let mut service = ollama_service(&app, &server.base_url)?;
        let cases = [
            (
                None,
                vec!["request one", "request two"],
                vec!["request one", "request two"],
            ),
            (Some("worker prompt"), vec![], vec!["worker prompt"]),
            (
                Some("worker prompt"),
                vec!["request prompt"],
                vec!["worker prompt\nrequest prompt"],
            ),
            (None, vec![], vec![]),
        ];

        for streaming in [false, true] {
            for (configured, request, expected) in &cases {
                service.system_prompt = configured.map(str::to_string);
                let args = system_args(request);
                if streaming {
                    let results = run_ollama_stream(Arc::new(service.clone()), args).await?;
                    ensure!(
                        results.iter().any(|result| result.done),
                        "stream did not finish"
                    );
                } else {
                    let result = service
                        .request_chat(args, opentelemetry::Context::current(), HashMap::new())
                        .await?;
                    ensure!(result.done, "non-stream response did not finish");
                }
                let captured = server.next_request().await?;
                ensure!(
                    system_messages(&captured) == *expected,
                    "unexpected Ollama system messages: {captured}"
                );
                if streaming {
                    ensure!(
                        captured.get("template").is_none(),
                        "Ollama template must not override system messages"
                    );
                }
            }
        }
        server.finish().await
    })
}

#[test]
fn genai_client_tools_are_manual_continuable_exclusive_and_keep_legacy_path() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = create_hybrid_test_app().await?;
        let conflict_service = genai_service(&app, "http://127.0.0.1:1").await?;
        for options in client_tool_conflicts() {
            let args = LlmChatArgs {
                function_options: Some(options.clone()),
                ..client_args()
            };
            let error = conflict_service
                .request_chat(args, opentelemetry::Context::current(), HashMap::new())
                .await
                .expect_err("client and server-driven tools must be mutually exclusive");
            ensure!(
                error.to_string().contains("mutually exclusive"),
                "wrong conflict error: {error}"
            );

            let stream_error = conflict_service
                .request_chat_stream(
                    LlmChatArgs {
                        function_options: Some(options),
                        ..client_args()
                    },
                    HashMap::new(),
                    None,
                )
                .await
                .err()
                .context("streaming client/server tool conflict should be rejected")?;
            ensure!(
                stream_error.to_string().contains("mutually exclusive"),
                "wrong stream conflict error: {stream_error}"
            );
        }

        let execution_args = client_tool_execution_request_args();
        let execution_error = conflict_service
            .request_chat(
                execution_args.clone(),
                opentelemetry::Context::current(),
                HashMap::new(),
            )
            .await
            .err()
            .context("client ToolExecutionRequests should be rejected")?;
        ensure!(
            execution_error.to_string().contains("ToolResults"),
            "wrong client-tool execution error: {execution_error}"
        );
        let execution_stream_error = conflict_service
            .request_chat_stream(execution_args, HashMap::new(), None)
            .await
            .err()
            .context("streaming client ToolExecutionRequests should be rejected")?;
        ensure!(
            execution_stream_error.to_string().contains("ToolResults"),
            "wrong streaming client-tool execution error: {execution_stream_error}"
        );

        let mut server = start_mock_server(vec![
            MockReply::GenaiToolCall,
            MockReply::GenaiText,
            MockReply::GenaiStreamToolCall,
            MockReply::GenaiStreamText,
            MockReply::GenaiStreamPrefixedToolCall,
            MockReply::GenaiText,
        ])
        .await?;
        let service = genai_service(&app, &server.base_url).await?;

        let original_args = client_args();
        let pending_result = service
            .request_chat(
                original_args.clone(),
                opentelemetry::Context::current(),
                HashMap::new(),
            )
            .await?;
        assert_pending_tool(&pending_result)?;
        let pending_call = pending_result.pending_tool_calls.unwrap().calls.remove(0);
        let first_request = server.next_request().await?;
        assert_tool_definition(&first_request)?;

        let continuation = continuation_args(
            original_args,
            &pending_call.call_id,
            &pending_call.fn_name,
            &pending_call.fn_arguments,
        );
        let continued = service
            .request_chat(
                continuation,
                opentelemetry::Context::current(),
                HashMap::new(),
            )
            .await?;
        ensure!(continued.done, "tool-result continuation should complete");
        let second_request = server.next_request().await?;
        assert_tool_definition(&second_request)?;
        assert_tool_result_in_history(&second_request)?;

        let stream_results = run_genai_stream(&service, client_args()).await?;
        let streamed_pending = stream_results
            .iter()
            .find(|result| result.pending_tool_calls.is_some())
            .context("stream response should surface pending client tool calls")?;
        assert_pending_tool(streamed_pending)?;
        let streamed_call = streamed_pending
            .pending_tool_calls
            .as_ref()
            .expect("pending calls checked above")
            .calls[0]
            .clone();
        let stream_request = server.next_request().await?;
        assert_tool_definition(&stream_request)?;

        let streamed_continuation = run_genai_stream(
            &service,
            continuation_args(
                client_args(),
                &streamed_call.call_id,
                &streamed_call.fn_name,
                &streamed_call.fn_arguments,
            ),
        )
        .await?;
        ensure!(
            streamed_continuation.iter().any(|result| result.done),
            "streaming tool-result continuation should complete"
        );
        let streamed_continuation_request = server.next_request().await?;
        assert_tool_definition(&streamed_continuation_request)?;
        assert_tool_result_in_history(&streamed_continuation_request)?;

        let prefixed_name = "select_toolset_client_owned";
        let prefixed_results =
            run_genai_stream(&service, client_args_with_tool_name(prefixed_name)).await?;
        let prefixed_pending = prefixed_results
            .iter()
            .find(|result| result.pending_tool_calls.is_some())
            .context("client-owned selector-prefixed tool should remain pending")?;
        assert_pending_tool_named(prefixed_pending, prefixed_name)?;
        let prefixed_request = server.next_request().await?;
        assert_tool_definition_named(&prefixed_request, prefixed_name)?;

        let set_name = create_legacy_function_set(&app).await?;
        let legacy_result = service
            .request_chat(
                LlmChatArgs {
                    function_options: Some(FunctionOptions {
                        use_function_calling: true,
                        function_set_name: Some(set_name),
                        ..Default::default()
                    }),
                    ..system_args(&[])
                },
                opentelemetry::Context::current(),
                HashMap::new(),
            )
            .await?;
        ensure!(
            legacy_result.done,
            "legacy FunctionSet request should complete"
        );
        let legacy_request = server.next_request().await?;
        ensure!(
            legacy_request["tools"]
                .as_array()
                .is_some_and(|tools| !tools.is_empty()),
            "FunctionSet tools should remain available when client_tools_json is unset"
        );
        server.finish().await
    })
}

#[test]
fn ollama_client_tools_are_manual_continuable_exclusive_and_keep_legacy_path() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = create_hybrid_test_app().await?;
        let conflict_service = ollama_service(&app, "http://127.0.0.1:1")?;
        for options in client_tool_conflicts() {
            let args = LlmChatArgs {
                function_options: Some(options.clone()),
                ..client_args()
            };
            let error = conflict_service
                .request_chat(args, opentelemetry::Context::current(), HashMap::new())
                .await
                .expect_err("client and server-driven tools must be mutually exclusive");
            ensure!(
                error.to_string().contains("mutually exclusive"),
                "wrong conflict error: {error}"
            );

            let stream_error = Arc::new(conflict_service.clone())
                .request_stream_chat(
                    LlmChatArgs {
                        function_options: Some(options),
                        ..client_args()
                    },
                    HashMap::new(),
                    None,
                )
                .await
                .err()
                .context("streaming client/server tool conflict should be rejected")?;
            ensure!(
                stream_error.to_string().contains("mutually exclusive"),
                "wrong stream conflict error: {stream_error}"
            );
        }

        let execution_args = client_tool_execution_request_args();
        let execution_error = conflict_service
            .request_chat(
                execution_args.clone(),
                opentelemetry::Context::current(),
                HashMap::new(),
            )
            .await
            .err()
            .context("client ToolExecutionRequests should be rejected")?;
        ensure!(
            execution_error.to_string().contains("ToolResults"),
            "wrong client-tool execution error: {execution_error}"
        );
        let execution_stream_error = Arc::new(conflict_service.clone())
            .request_stream_chat(execution_args, HashMap::new(), None)
            .await
            .err()
            .context("streaming client ToolExecutionRequests should be rejected")?;
        ensure!(
            execution_stream_error.to_string().contains("ToolResults"),
            "wrong streaming client-tool execution error: {execution_stream_error}"
        );

        let mut server = start_mock_server(vec![
            MockReply::OllamaToolCall,
            MockReply::OllamaText,
            MockReply::OllamaStreamToolCall,
            MockReply::OllamaStreamText,
            MockReply::OllamaStreamPrefixedToolCall,
            MockReply::OllamaText,
        ])
        .await?;
        let service = Arc::new(ollama_service(&app, &server.base_url)?);

        let original_args = client_args();
        let pending_result = service
            .request_chat(
                original_args.clone(),
                opentelemetry::Context::current(),
                HashMap::new(),
            )
            .await?;
        assert_pending_tool(&pending_result)?;
        let pending_call = pending_result.pending_tool_calls.unwrap().calls.remove(0);
        let first_request = server.next_request().await?;
        assert_tool_definition(&first_request)?;

        let continuation = continuation_args(
            original_args,
            &pending_call.call_id,
            &pending_call.fn_name,
            &pending_call.fn_arguments,
        );
        let continued = service
            .request_chat(
                continuation,
                opentelemetry::Context::current(),
                HashMap::new(),
            )
            .await?;
        ensure!(continued.done, "tool-result continuation should complete");
        let second_request = server.next_request().await?;
        assert_tool_definition(&second_request)?;
        assert_tool_result_in_history(&second_request)?;

        let stream_results = run_ollama_stream(service.clone(), client_args()).await?;
        let streamed_pending = stream_results
            .iter()
            .find(|result| result.pending_tool_calls.is_some())
            .context("stream response should surface pending client tool calls")?;
        assert_pending_tool(streamed_pending)?;
        let streamed_call = streamed_pending
            .pending_tool_calls
            .as_ref()
            .expect("pending calls checked above")
            .calls[0]
            .clone();
        let stream_request = server.next_request().await?;
        assert_tool_definition(&stream_request)?;

        let streamed_continuation = run_ollama_stream(
            service.clone(),
            continuation_args(
                client_args(),
                &streamed_call.call_id,
                &streamed_call.fn_name,
                &streamed_call.fn_arguments,
            ),
        )
        .await?;
        ensure!(
            streamed_continuation.iter().any(|result| result.done),
            "streaming tool-result continuation should complete"
        );
        let streamed_continuation_request = server.next_request().await?;
        assert_tool_definition(&streamed_continuation_request)?;
        assert_tool_result_in_history(&streamed_continuation_request)?;

        let prefixed_name = "select_toolset_client_owned";
        let prefixed_results =
            run_ollama_stream(service.clone(), client_args_with_tool_name(prefixed_name)).await?;
        let prefixed_pending = prefixed_results
            .iter()
            .find(|result| result.pending_tool_calls.is_some())
            .context("client-owned selector-prefixed tool should remain pending")?;
        assert_pending_tool_named(prefixed_pending, prefixed_name)?;
        let prefixed_request = server.next_request().await?;
        assert_tool_definition_named(&prefixed_request, prefixed_name)?;

        let set_name = create_legacy_function_set(&app).await?;
        let legacy_result = service
            .request_chat(
                LlmChatArgs {
                    function_options: Some(FunctionOptions {
                        use_function_calling: true,
                        function_set_name: Some(set_name),
                        ..Default::default()
                    }),
                    ..system_args(&[])
                },
                opentelemetry::Context::current(),
                HashMap::new(),
            )
            .await?;
        ensure!(
            legacy_result.done,
            "legacy FunctionSet request should complete"
        );
        let legacy_request = server.next_request().await?;
        ensure!(
            legacy_request["tools"]
                .as_array()
                .is_some_and(|tools| !tools.is_empty()),
            "FunctionSet tools should remain available when client_tools_json is unset"
        );
        server.finish().await
    })
}
