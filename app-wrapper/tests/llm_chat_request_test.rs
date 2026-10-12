//! LLM chat request-shape and pre-stream cancellation tests against a local mock
//! server (no external LLM required).
//!
//! GenAI tests use the `omlx::` namespace because that adapter needs no API key and
//! shares the OpenAI request builder (including `extra_body` merging) with other
//! OpenAI-compatible adapters.

use anyhow::Result;
use app::module::test::create_hybrid_test_app;
use futures::StreamExt;
use jobworkerp_runner::jobworkerp::runner::llm::LlmChatArgs;
use jobworkerp_runner::jobworkerp::runner::llm::LlmRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
    ChatMessage, ChatRole, LlmOptions, MessageContent, message_content,
};
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::{
    GenaiRunnerSettings, OllamaRunnerSettings, Settings,
};
use jobworkerp_runner::runner::RunnerTrait;
use jobworkerp_runner::runner::cancellation_helper::CancelMonitoringHelper;
use jobworkerp_runner::runner::test_common::mock::MockCancellationManager;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::{Instant, timeout};
use tokio_util::sync::CancellationToken;

const MODEL: &str = "omlx::test-model";

#[derive(Clone, Copy)]
enum Reply {
    Json,
    Sse,
    /// Accept the request but never answer, as a server busy with prefill would.
    Hang,
}

enum Event {
    Request(Value),
    Closed(Instant),
}

struct MockServer {
    base_url: String,
    events: mpsc::UnboundedReceiver<Event>,
}

impl MockServer {
    async fn start(reply: Reply) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base_url = format!("http://{}/v1/", listener.local_addr()?);
        let (tx, events) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let tx = tx.clone();
                tokio::spawn(async move {
                    let _ = handle(socket, reply, tx).await;
                });
            }
        });
        Ok(Self { base_url, events })
    }

    async fn next_request(&mut self) -> Value {
        loop {
            match timeout(Duration::from_secs(5), self.events.recv()).await {
                Ok(Some(Event::Request(body))) => return body,
                Ok(Some(Event::Closed(_))) => continue,
                Ok(None) => panic!("mock server channel closed before a request arrived"),
                Err(_) => panic!("timed out waiting for a request"),
            }
        }
    }

    async fn closed_at(&mut self) -> Option<Instant> {
        loop {
            match timeout(Duration::from_secs(5), self.events.recv()).await {
                Ok(Some(Event::Closed(at))) => return Some(at),
                Ok(Some(Event::Request(_))) => continue,
                _ => return None,
            }
        }
    }

    fn assert_no_request(&mut self) {
        assert!(
            !matches!(self.events.try_recv(), Ok(Event::Request(_))),
            "request must not be sent"
        );
    }
}

async fn handle(
    mut socket: TcpStream,
    reply: Reply,
    tx: mpsc::UnboundedSender<Event>,
) -> Result<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let n = socket.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let headers = String::from_utf8_lossy(&buf[..header_end]).to_ascii_lowercase();
    let content_length = headers
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while buf.len() < header_end + content_length {
        let n = socket.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body: Value = serde_json::from_slice(&buf[header_end..header_end + content_length])?;
    let _ = tx.send(Event::Request(body));

    match reply {
        Reply::Json => {
            let payload = r#"{"id":"x","object":"chat.completion","created":0,"model":"test-model","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
            socket.write_all(resp.as_bytes()).await?;
        }
        Reply::Sse => {
            let events = concat!(
                "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n"
            );
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{events}"
            );
            socket.write_all(resp.as_bytes()).await?;
        }
        Reply::Hang => {
            // Block until the client drops the connection.
            while socket.read(&mut chunk).await.unwrap_or(0) > 0 {}
        }
    }
    socket.shutdown().await.ok();
    let _ = tx.send(Event::Closed(Instant::now()));
    Ok(())
}

fn chat_args(options: LlmOptions) -> LlmChatArgs {
    LlmChatArgs {
        messages: vec![ChatMessage {
            role: ChatRole::User as i32,
            content: Some(MessageContent {
                content: Some(message_content::Content::Text("hi".to_string())),
            }),
        }],
        options: Some(options),
        ..Default::default()
    }
}

async fn genai_runner(
    model: String,
    base_url: &str,
    cancel_token: CancellationToken,
) -> Result<app_wrapper::llm::chat::LLMChatRunnerImpl> {
    runner_with(
        Settings::Genai(GenaiRunnerSettings {
            model,
            system_prompt: None,
            base_url: Some(base_url.to_string()),
        }),
        cancel_token,
    )
    .await
}

async fn runner_for(
    base_url: &str,
    cancel_token: CancellationToken,
) -> Result<app_wrapper::llm::chat::LLMChatRunnerImpl> {
    genai_runner(MODEL.to_string(), base_url, cancel_token).await
}

async fn runner_with(
    settings: Settings,
    cancel_token: CancellationToken,
) -> Result<app_wrapper::llm::chat::LLMChatRunnerImpl> {
    let app_module = Arc::new(create_hybrid_test_app().await?);
    let cancel_helper = CancelMonitoringHelper::new(Box::new(
        MockCancellationManager::new_with_token(cancel_token),
    ));
    let mut runner = app_wrapper::llm::chat::LLMChatRunnerImpl::new_with_cancel_monitoring(
        app_module,
        cancel_helper,
    );
    let settings = LlmRunnerSettings {
        settings: Some(settings),
        embedding_chunking: None,
    };
    runner
        .load(prost::Message::encode_to_vec(&settings))
        .await?;
    Ok(runner)
}

#[tokio::test]
async fn extract_false_disables_thinking_via_chat_template_kwargs() -> Result<()> {
    let mut server = MockServer::start(Reply::Json).await?;
    let mut runner = runner_for(&server.base_url, CancellationToken::new()).await?;
    let args = chat_args(LlmOptions {
        extract_reasoning_content: Some(false),
        ..Default::default()
    });

    let (res, _) = runner
        .run(&prost::Message::encode_to_vec(&args), HashMap::new(), None)
        .await;
    res?;

    let body = server.next_request().await;
    assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
    assert!(body.get("reasoning_effort").is_none(), "{body}");
    Ok(())
}

#[tokio::test]
async fn user_chat_template_kwargs_are_sent_on_stream() -> Result<()> {
    let mut server = MockServer::start(Reply::Sse).await?;
    let mut runner = runner_for(&server.base_url, CancellationToken::new()).await?;
    let args = chat_args(LlmOptions {
        chat_template_kwargs: Some(r#"{"enable_thinking": false, "custom": 1}"#.to_string()),
        ..Default::default()
    });

    let stream = runner
        .run_stream(&prost::Message::encode_to_vec(&args), HashMap::new(), None)
        .await?;
    let items: Vec<_> = stream.collect().await;
    assert!(!items.is_empty());

    let body = server.next_request().await;
    assert_eq!(body["stream"], true);
    assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
    assert_eq!(body["chat_template_kwargs"]["custom"], 1);
    Ok(())
}

#[tokio::test]
async fn unspecified_options_send_no_reasoning_fields() -> Result<()> {
    let mut server = MockServer::start(Reply::Json).await?;
    let mut runner = runner_for(&server.base_url, CancellationToken::new()).await?;
    let args = chat_args(LlmOptions::default());

    let (res, _) = runner
        .run(&prost::Message::encode_to_vec(&args), HashMap::new(), None)
        .await;
    res?;

    let body = server.next_request().await;
    assert!(body.get("chat_template_kwargs").is_none(), "{body}");
    assert!(body.get("reasoning_effort").is_none(), "{body}");
    Ok(())
}

#[tokio::test]
async fn invalid_chat_template_kwargs_fail_before_request() -> Result<()> {
    let mut server = MockServer::start(Reply::Json).await?;
    let mut runner = runner_for(&server.base_url, CancellationToken::new()).await?;
    let args = chat_args(LlmOptions {
        chat_template_kwargs: Some("[\"not an object\"]".to_string()),
        ..Default::default()
    });
    let encoded = prost::Message::encode_to_vec(&args);

    let (res, _) = runner.run(&encoded, HashMap::new(), None).await;
    let err = res.expect_err("non-object kwargs must be rejected");
    assert!(err.to_string().contains("chat_template_kwargs"), "{err:#}");

    let stream_res = runner.run_stream(&encoded, HashMap::new(), None).await;
    assert!(stream_res.is_err(), "stream must be rejected as well");

    tokio::time::sleep(Duration::from_millis(100)).await;
    server.assert_no_request();
    Ok(())
}

#[tokio::test]
async fn invalid_reasoning_effort_fails_before_request() -> Result<()> {
    let mut server = MockServer::start(Reply::Json).await?;
    let mut runner = runner_for(&server.base_url, CancellationToken::new()).await?;
    let args = chat_args(LlmOptions {
        reasoning_effort: Some("extreme".to_string()),
        ..Default::default()
    });

    let (res, _) = runner
        .run(&prost::Message::encode_to_vec(&args), HashMap::new(), None)
        .await;
    let err = res.expect_err("invalid effort must be rejected");
    assert!(err.to_string().contains("reasoning_effort"), "{err:#}");

    tokio::time::sleep(Duration::from_millis(100)).await;
    server.assert_no_request();
    Ok(())
}

/// Drive `run_stream` against a server that never answers, cancel, and verify that the
/// call returns and the upstream connection is closed promptly.
async fn assert_cancel_before_stream_start(
    mut server: MockServer,
    mut runner: app_wrapper::llm::chat::LLMChatRunnerImpl,
    cancel_token: CancellationToken,
) -> Result<()> {
    let args = chat_args(LlmOptions::default());
    let encoded = prost::Message::encode_to_vec(&args);

    let task = tokio::spawn(async move {
        // Drain the stream so the request is actually driven to completion.
        let stream = runner.run_stream(&encoded, HashMap::new(), None).await?;
        stream.collect::<Vec<_>>().await;
        anyhow::Ok(())
    });

    server.next_request().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let cancelled_at = Instant::now();
    cancel_token.cancel();

    let finished = timeout(Duration::from_secs(2), task).await;
    assert!(
        finished.is_ok(),
        "run_stream must return promptly after cancellation"
    );

    let closed_at = server
        .closed_at()
        .await
        .expect("upstream connection must be closed after cancellation");
    assert!(
        closed_at.duration_since(cancelled_at) < Duration::from_secs(2),
        "connection closed too late: {:?}",
        closed_at.duration_since(cancelled_at)
    );
    Ok(())
}

#[tokio::test]
async fn genai_cancel_while_waiting_for_stream_start_closes_connection() -> Result<()> {
    let server = MockServer::start(Reply::Hang).await?;
    let cancel_token = CancellationToken::new();
    let runner = runner_for(&server.base_url, cancel_token.clone()).await?;
    assert_cancel_before_stream_start(server, runner, cancel_token).await
}

#[tokio::test]
async fn ollama_cancel_while_waiting_for_stream_start_closes_connection() -> Result<()> {
    let server = MockServer::start(Reply::Hang).await?;
    let cancel_token = CancellationToken::new();
    let base_url = server.base_url.trim_end_matches("v1/").to_string();
    let runner = runner_with(
        Settings::Ollama(OllamaRunnerSettings {
            model: "test-model".to_string(),
            base_url: Some(base_url),
            ..Default::default()
        }),
        cancel_token.clone(),
    )
    .await?;
    assert_cancel_before_stream_start(server, runner, cancel_token).await
}

// ---------------------------------------------------------------------------
// Real-server checks against an omlx (OpenAI-compatible) endpoint.
//
// Run with, e.g.:
//   OMLX_TEST_BASE_URL=http://<host>:11434/v1/ OMLX_TEST_MODEL=<model> \
//   OMLX_API_KEY=<key> OPENAI_API_KEY=<key> \
//   cargo test -p app-wrapper --test llm_chat_request_test omlx_real -- --ignored --test-threads=1 --nocapture
// ---------------------------------------------------------------------------

mod omlx_real {
    use super::*;
    use jobworkerp_runner::jobworkerp::runner::llm::LlmChatResult;
    use prost::Message;
    use proto::jobworkerp::data::result_output_item;

    struct Target {
        base_url: String,
        model: String,
    }

    fn target() -> Option<Target> {
        let base_url = std::env::var("OMLX_TEST_BASE_URL").ok()?;
        let model = std::env::var("OMLX_TEST_MODEL").ok()?;
        Some(Target { base_url, model })
    }

    fn prompt_args(prompt: &str, options: LlmOptions) -> LlmChatArgs {
        let mut args = chat_args(LlmOptions {
            max_tokens: Some(400),
            ..options
        });
        args.messages[0].content = Some(MessageContent {
            content: Some(message_content::Content::Text(prompt.to_string())),
        });
        args
    }

    fn text_of(result: &LlmChatResult) -> String {
        match result.content.as_ref().and_then(|c| c.content.as_ref()) {
            Some(jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(t)) => t.clone(),
            _ => String::new(),
        }
    }

    /// Returns (content, reasoning) of a non-streaming chat.
    async fn run_chat(
        runner: &mut app_wrapper::llm::chat::LLMChatRunnerImpl,
        args: &LlmChatArgs,
    ) -> Result<(String, String)> {
        let (res, _) = runner
            .run(&args.encode_to_vec(), HashMap::new(), None)
            .await;
        let result = LlmChatResult::decode(res?.as_slice())?;
        Ok((
            text_of(&result),
            result.reasoning_content.unwrap_or_default(),
        ))
    }

    /// Returns (content, reasoning) accumulated from a streaming chat.
    async fn run_chat_stream(
        runner: &mut app_wrapper::llm::chat::LLMChatRunnerImpl,
        args: &LlmChatArgs,
    ) -> Result<(String, String)> {
        let mut stream = runner
            .run_stream(&args.encode_to_vec(), HashMap::new(), None)
            .await?;
        let (mut content, mut reasoning) = (String::new(), String::new());
        while let Some(item) = stream.next().await {
            if let Some(result_output_item::Item::Data(bytes)) = item.item {
                let r = LlmChatResult::decode(bytes.as_slice())?;
                // The terminal (done) chunk carries the full aggregated result.
                if r.done {
                    let final_text = text_of(&r);
                    if !final_text.is_empty() {
                        content = final_text;
                    }
                    if let Some(final_reasoning) = r.reasoning_content.filter(|s| !s.is_empty()) {
                        reasoning = final_reasoning;
                    }
                } else {
                    content.push_str(&text_of(&r));
                    reasoning.push_str(r.reasoning_content.as_deref().unwrap_or_default());
                }
            }
        }
        Ok((content, reasoning))
    }

    const PROMPT: &str = "What is 1+1? Answer with a single number.";

    #[tokio::test]
    #[ignore = "requires a real omlx server (OMLX_TEST_BASE_URL / OMLX_TEST_MODEL)"]
    async fn omlx_real_default_keeps_reasoning() -> Result<()> {
        let Some(t) = target() else { return Ok(()) };
        let mut runner = genai_runner(
            format!("omlx::{}", t.model),
            &t.base_url,
            CancellationToken::new(),
        )
        .await?;
        let (content, reasoning) =
            run_chat(&mut runner, &prompt_args(PROMPT, LlmOptions::default())).await?;
        println!(
            "default: content={content:?} reasoning_chars={}",
            reasoning.len()
        );
        assert!(
            !reasoning.is_empty(),
            "thinking model should reason by default"
        );

        let (content, reasoning) =
            run_chat_stream(&mut runner, &prompt_args(PROMPT, LlmOptions::default())).await?;
        println!(
            "default stream: content={content:?} reasoning_chars={}",
            reasoning.len()
        );
        assert!(
            !reasoning.is_empty(),
            "thinking model should reason by default (stream)"
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires a real omlx server (OMLX_TEST_BASE_URL / OMLX_TEST_MODEL)"]
    async fn omlx_real_extract_false_suppresses_reasoning() -> Result<()> {
        let Some(t) = target() else { return Ok(()) };
        let mut runner = genai_runner(
            format!("omlx::{}", t.model),
            &t.base_url,
            CancellationToken::new(),
        )
        .await?;
        let args = prompt_args(
            PROMPT,
            LlmOptions {
                extract_reasoning_content: Some(false),
                ..Default::default()
            },
        );
        let started = Instant::now();
        let (content, reasoning) = run_chat(&mut runner, &args).await?;
        println!(
            "extract=false: content={content:?} reasoning_chars={} elapsed={:?}",
            reasoning.len(),
            started.elapsed()
        );
        assert!(
            reasoning.is_empty(),
            "reasoning must be suppressed: {reasoning:?}"
        );
        assert!(content.contains('2'), "{content:?}");

        let started = Instant::now();
        let (content, reasoning) = run_chat_stream(&mut runner, &args).await?;
        println!(
            "extract=false stream: content={content:?} reasoning_chars={} elapsed={:?}",
            reasoning.len(),
            started.elapsed()
        );
        assert!(
            reasoning.is_empty(),
            "stream reasoning must be suppressed: {reasoning:?}"
        );
        assert!(content.contains('2'), "{content:?}");
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires a real omlx server (OMLX_TEST_BASE_URL / OMLX_TEST_MODEL)"]
    async fn omlx_real_reasoning_effort_none_suppresses_reasoning() -> Result<()> {
        let Some(t) = target() else { return Ok(()) };
        let mut runner = genai_runner(
            format!("omlx::{}", t.model),
            &t.base_url,
            CancellationToken::new(),
        )
        .await?;
        let args = prompt_args(
            PROMPT,
            LlmOptions {
                reasoning_effort: Some("none".to_string()),
                ..Default::default()
            },
        );
        let (content, reasoning) = run_chat(&mut runner, &args).await?;
        println!(
            "effort=none: content={content:?} reasoning_chars={}",
            reasoning.len()
        );
        assert!(reasoning.is_empty(), "{reasoning:?}");
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires a real omlx server and OPENAI_API_KEY for the openai:: adapter"]
    async fn omlx_real_openai_adapter_with_chat_template_kwargs() -> Result<()> {
        let Some(t) = target() else { return Ok(()) };
        let mut runner = genai_runner(
            format!("openai::{}", t.model),
            &t.base_url,
            CancellationToken::new(),
        )
        .await?;
        let args = prompt_args(
            PROMPT,
            LlmOptions {
                chat_template_kwargs: Some(r#"{"enable_thinking": false}"#.to_string()),
                ..Default::default()
            },
        );
        let (content, reasoning) = run_chat(&mut runner, &args).await?;
        println!(
            "openai+kwargs: content={content:?} reasoning_chars={}",
            reasoning.len()
        );
        assert!(reasoning.is_empty(), "{reasoning:?}");

        let (content, reasoning) = run_chat_stream(&mut runner, &args).await?;
        println!(
            "openai+kwargs stream: content={content:?} reasoning_chars={}",
            reasoning.len()
        );
        assert!(reasoning.is_empty(), "{reasoning:?}");
        Ok(())
    }

    /// Abort a streaming job while it is generating, then a short request must be served
    /// within 1s. (omlx cannot interrupt an in-flight prefill, so aborting during prefill of a
    /// long prompt is bounded by the server, not by the runner.)
    #[tokio::test]
    #[ignore = "requires a real omlx server (OMLX_TEST_BASE_URL / OMLX_TEST_MODEL)"]
    async fn omlx_real_abort_generating_stream_frees_server() -> Result<()> {
        let Some(t) = target() else { return Ok(()) };
        let model = format!("omlx::{}", t.model);
        let cancel_token = CancellationToken::new();
        let mut long_runner =
            genai_runner(model.clone(), &t.base_url, cancel_token.clone()).await?;
        let mut short_runner = genai_runner(model, &t.base_url, CancellationToken::new()).await?;

        let long_args = prompt_args(
            "Write 300 words about castles.",
            LlmOptions {
                extract_reasoning_content: Some(false),
                max_tokens: Some(1000),
                ..Default::default()
            },
        );
        let long_task =
            tokio::spawn(async move { run_chat_stream(&mut long_runner, &long_args).await });

        tokio::time::sleep(Duration::from_millis(800)).await;
        cancel_token.cancel();
        let cancelled_at = Instant::now();
        let long_res = timeout(Duration::from_secs(2), long_task).await;
        println!(
            "stream finished after cancel: {:?} in {:?}",
            long_res
                .as_ref()
                .map(|r| r.as_ref().map(|x| x.as_ref().map(|(c, _)| c.len()))),
            cancelled_at.elapsed()
        );
        assert!(long_res.is_ok(), "cancelled stream must finish promptly");

        let short_args = prompt_args(
            PROMPT,
            LlmOptions {
                extract_reasoning_content: Some(false),
                max_tokens: Some(16),
                ..Default::default()
            },
        );
        let started = Instant::now();
        let (content, _) = run_chat(&mut short_runner, &short_args).await?;
        let elapsed = started.elapsed();
        println!("short request after abort: content={content:?} elapsed={elapsed:?}");
        assert!(
            elapsed < Duration::from_secs(1),
            "short request took {elapsed:?}"
        );
        Ok(())
    }
}
