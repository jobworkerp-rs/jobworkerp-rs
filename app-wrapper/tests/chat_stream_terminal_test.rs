use anyhow::{Context, Result, ensure};
use app::module::test::create_hybrid_test_app;
use app_wrapper::llm::chat::{LLMChatRunnerImpl, ollama::OllamaChatService};
use futures::{StreamExt, stream};
use infra_utils::infra::test::TEST_RUNTIME;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
    ChatMessage, ChatRole, MessageContent, message_content,
};
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::OllamaRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::{LlmChatArgs, LlmChatResult};
use jobworkerp_runner::runner::RunnerSpec;
use jobworkerp_runner::runner::RunnerTrait;
use jobworkerp_runner::runner::llm_chat::LLMChatRunnerSpecImpl;
use prost::Message;
use proto::jobworkerp::data::{ResultOutputItem, result_output_item};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

const MODEL: &str = "chat-stream-terminal-test";

struct MockOllama {
    base_url: String,
    task: JoinHandle<Result<()>>,
}

impl MockOllama {
    async fn finish(self) -> Result<()> {
        self.task.await.context("mock Ollama task panicked")??;
        Ok(())
    }
}

async fn start_mock_ollama(body: String) -> Result<MockOllama> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base_url = format!("http://{}", listener.local_addr()?);
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        read_request(&mut socket).await?;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await?;
        Ok(())
    });

    Ok(MockOllama { base_url, task })
}

async fn read_request(socket: &mut TcpStream) -> Result<()> {
    let mut request = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        let read = socket.read(&mut chunk).await?;
        ensure!(read > 0, "client closed before sending request");
        request.extend_from_slice(&chunk[..read]);

        if let Some(header_end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
            let content_length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or_default();
            if request.len() >= header_end + 4 + content_length {
                return Ok(());
            }
        }
        ensure!(
            request.len() <= 1_048_576,
            "test request exceeded size limit"
        );
    }
}

fn ollama_text_chunk(text: &str, done: bool) -> serde_json::Value {
    let mut chunk = serde_json::json!({
        "model": MODEL,
        "created_at": "2026-01-01T00:00:00Z",
        "message": {"role": "assistant", "content": text},
        "done": done,
    });
    if done {
        chunk["total_duration"] = serde_json::json!(0);
        chunk["load_duration"] = serde_json::json!(0);
        chunk["prompt_eval_count"] = serde_json::json!(1);
        chunk["prompt_eval_duration"] = serde_json::json!(0);
        chunk["eval_count"] = serde_json::json!(1);
        chunk["eval_duration"] = serde_json::json!(0);
    }
    chunk
}

async fn run_adapter_stream(body: String) -> Result<Vec<ResultOutputItem>> {
    let mock = start_mock_ollama(body).await?;
    let app = create_hybrid_test_app().await?;
    let ollama = OllamaChatService::new(
        app.function_app.clone(),
        app.function_set_app.clone(),
        OllamaRunnerSettings {
            model: MODEL.to_string(),
            base_url: Some(mock.base_url.clone()),
            system_prompt: None,
            pull_model: Some(false),
        },
    )?;
    let mut runner = LLMChatRunnerImpl::new(Arc::new(app));
    runner.ollama = Some(ollama);
    let args = LlmChatArgs {
        messages: vec![ChatMessage {
            role: ChatRole::User as i32,
            content: Some(MessageContent {
                content: Some(message_content::Content::Text("say hello".to_string())),
            }),
        }],
        ..Default::default()
    };
    let metadata = HashMap::from([("request-id".to_string(), "terminal-test".to_string())]);
    let output = runner
        .run_stream(&args.encode_to_vec(), metadata, None)
        .await?;
    let output = output.collect::<Vec<_>>().await;
    mock.finish().await?;
    Ok(output)
}

fn decode_data(item: &ResultOutputItem) -> LlmChatResult {
    match item.item.as_ref() {
        Some(result_output_item::Item::Data(data)) => {
            LlmChatResult::decode(data.as_slice()).expect("adapter should encode an LlmChatResult")
        }
        other => panic!("expected Data item, got {other:?}"),
    }
}

#[test]
fn ollama_terminal_chunk_is_forwarded_and_collected() -> Result<()> {
    TEST_RUNTIME.block_on(async {
    let body = format!(
        "{}\n{}\n",
        ollama_text_chunk("hello", false),
        ollama_text_chunk("", true)
    );
    let output = run_adapter_stream(body).await?;

    assert_eq!(
        output.len(),
        3,
        "text, terminal data, then End are expected"
    );
    let text_chunk = decode_data(&output[0]);
    assert!(!text_chunk.done);
    let terminal_chunk = decode_data(&output[1]);
    assert!(terminal_chunk.done);
    assert!(terminal_chunk.content.is_none());
    assert_eq!(
        terminal_chunk.usage.as_ref().unwrap().prompt_tokens,
        Some(1)
    );
    match output[2].item.as_ref() {
        Some(result_output_item::Item::End(trailer)) => assert_eq!(
            trailer.metadata.get("request-id").map(String::as_str),
            Some("terminal-test")
        ),
        other => panic!("expected End after terminal data, got {other:?}"),
    }

    let spec = LLMChatRunnerSpecImpl::new();
    let (bytes, metadata) = spec
        .collect_stream(Box::pin(stream::iter(output)), None)
        .await?;
    let collected = LlmChatResult::decode(bytes.as_slice())?;
    assert!(collected.done);
    assert_eq!(
        collected.content.unwrap().content,
        Some(jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(
            "hello".to_string()
        ))
    );
    assert_eq!(
        metadata.get("request-id").map(String::as_str),
        Some("terminal-test")
    );
    Ok(())
    })
}

#[test]
fn ollama_early_eof_without_done_is_rejected_by_collector() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let body = format!("{}\n", ollama_text_chunk("partial", false));
        let output = run_adapter_stream(body).await?;

        assert_eq!(
            output.len(),
            1,
            "early EOF must not synthesize a terminal item"
        );
        assert!(!decode_data(&output[0]).done);

        let spec = LLMChatRunnerSpecImpl::new();
        let error = spec
            .collect_stream(Box::pin(stream::iter(output)), None)
            .await
            .expect_err("collector must reject a stream without done=true");
        assert!(
            error
                .to_string()
                .contains("without a successful done=true chunk")
        );
        Ok(())
    })
}
