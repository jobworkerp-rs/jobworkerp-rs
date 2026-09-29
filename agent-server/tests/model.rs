use std::sync::{Arc, Mutex};

use agent_server::chat::{
    ChatMessage, ModelInvocation, ModelInvoker, ModelJobObserver, ModelOptions, ModelResponse,
    Role, ToolCall, ToolResult,
};
use agent_server::grpc::{
    OutputChunk, OutputKind, ToolExecutionResult, ToolExecutor, ToolInvocation,
};
use agent_server::model::{GrpcModelInvoker, ModelTextDeltaCallback};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};

#[derive(Default)]
struct FakeToolExecutor {
    invocations: Mutex<Vec<ToolInvocation>>,
    result: Mutex<Option<ToolExecutionResult>>,
}

impl FakeToolExecutor {
    fn with_result(result: ToolExecutionResult) -> Self {
        Self {
            invocations: Mutex::new(Vec::new()),
            result: Mutex::new(Some(result)),
        }
    }

    fn invocations(&self) -> Vec<ToolInvocation> {
        self.invocations.lock().unwrap().clone()
    }
}

#[async_trait]
impl ToolExecutor for FakeToolExecutor {
    async fn execute(&self, invocation: ToolInvocation) -> Result<ToolExecutionResult> {
        self.invocations.lock().unwrap().push(invocation);
        self.result
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| anyhow::anyhow!("fake result was already consumed"))
    }
}

#[derive(Default)]
struct RecordingObserver {
    job_ids: Mutex<Vec<i64>>,
}

#[async_trait]
impl ModelJobObserver for RecordingObserver {
    async fn job_started(&self, job_id: i64) -> Result<(), String> {
        self.job_ids.lock().unwrap().push(job_id);
        Ok(())
    }

    fn job_finished(&self, _job_id: i64) {}
}

fn stream_result(final_result: Value) -> ToolExecutionResult {
    stream_result_for(31, final_result)
}

fn stream_result_for(worker_id: i64, final_result: Value) -> ToolExecutionResult {
    ToolExecutionResult {
        worker_id,
        using: "run".to_owned(),
        job_id: 77,
        result: None,
        raw_result: None,
        chunks: vec![
            OutputChunk {
                kind: OutputKind::Data,
                bytes: Vec::new(),
                json: Some(json!({"content": {"text": "partial"}, "done": false})),
            },
            OutputChunk {
                kind: OutputKind::FinalCollected,
                bytes: Vec::new(),
                json: Some(final_result),
            },
        ],
    }
}

fn invocation(history: Vec<ChatMessage>) -> ModelInvocation {
    selected_invocation(history, 31, ModelOptions::default())
}

fn selected_invocation(
    history: Vec<ChatMessage>,
    llm_worker_id: i64,
    options: ModelOptions,
) -> ModelInvocation {
    ModelInvocation {
        llm_worker_id,
        options,
        history,
        client_tools_json:
            r#"[{"type":"function","function":{"name":"lookup","parameters":{"type":"object"}}}]"#
                .to_owned(),
        is_auto_calling: false,
        function_set_name: None,
    }
}

fn text_message(role: Role, text: &str) -> ChatMessage {
    ChatMessage::new(role, Value::String(text.to_owned()))
}

#[tokio::test]
async fn persisted_image_url_is_rejected_before_model_job_is_enqueued() {
    let executor = Arc::new(FakeToolExecutor::with_result(stream_result_for(
        45,
        json!({"content": {"text": "unreachable"}, "done": true}),
    )));
    let model = GrpcModelInvoker::new(31, executor.clone());
    let image = ChatMessage::new(
        Role::User,
        json!({"image": {
            "contentType": "image/png",
            "source": {"url": "http://127.0.0.1/private"}
        }}),
    );
    let error = model
        .invoke(selected_invocation(
            vec![image],
            45,
            ModelOptions::default(),
        ))
        .await
        .unwrap_err();
    assert!(error.contains("URL image sources"));
    assert!(executor.invocations().is_empty());
}

#[tokio::test]
async fn request_uses_manual_run_and_preserves_roles_images_and_tool_results() {
    let result = json!({"content": {"text": "completed"}, "done": true});
    let executor = Arc::new(FakeToolExecutor::with_result(stream_result_for(45, result)));
    let model = GrpcModelInvoker::new(31, executor.clone());
    let call = ToolCall {
        call_id: "call-original".to_owned(),
        name: "lookup".to_owned(),
        arguments: json!({"term": "copper"}),
    };
    let tool_result = ToolResult {
        call_id: call.call_id.clone(),
        name: call.name.clone(),
        content: json!({"matches": 3}),
        is_error: true,
    };
    let assistant = ChatMessage {
        role: Role::Assistant,
        content: Value::Null,
        tool_calls: vec![call],
        tool_results: Vec::new(),
        tool_execution_requests: None,
    };
    let tool_message = ChatMessage {
        role: Role::Tool,
        content: tool_result.content.clone(),
        tool_calls: Vec::new(),
        tool_results: vec![tool_result],
        tool_execution_requests: None,
    };
    let image_message = ChatMessage::new(
        Role::User,
        json!({
            "image": {
                "contentType": "image/png",
                "source": {"base64": "aGVsbG8="}
            }
        }),
    );
    let tools_json = invocation(Vec::new()).client_tools_json;
    let response = model
        .invoke(selected_invocation(
            vec![
                text_message(Role::System, "system instruction"),
                text_message(Role::User, "inspect this"),
                image_message,
                assistant,
                tool_message,
            ],
            45,
            ModelOptions {
                temperature: Some(0.35),
                top_p: Some(0.85),
                max_tokens: Some(240),
            },
        ))
        .await
        .unwrap();

    assert_eq!(
        response,
        ModelResponse {
            content: json!("completed"),
            tool_calls: Vec::new(),
        }
    );
    let requests = executor.invocations();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.worker_id, 45);
    assert_eq!(request.using.as_deref(), Some("run"));
    assert!(request.stream);
    assert_eq!(
        request.arguments["functionOptions"]["useFunctionCalling"],
        true
    );
    assert_eq!(request.arguments["functionOptions"]["isAutoCalling"], false);
    assert_eq!(request.arguments["options"]["temperature"], json!(0.35f32));
    assert_eq!(request.arguments["options"]["topP"], json!(0.85f32));
    assert_eq!(request.arguments["options"]["maxTokens"], 240);
    assert_eq!(
        request.arguments["functionOptions"]["clientToolsJson"].as_str(),
        Some(tools_json.as_str())
    );
    assert!(
        request.arguments["functionOptions"]
            .get("functionSetName")
            .is_none()
    );
    assert!(
        request.arguments["functionOptions"]
            .get("autoSelectFunctionSet")
            .is_none()
    );

    let messages = request.arguments["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "SYSTEM");
    assert_eq!(messages[0]["content"]["text"], "system instruction");
    assert_eq!(messages[1]["role"], "USER");
    assert_eq!(messages[1]["content"]["text"], "inspect this");
    assert_eq!(messages[2]["content"]["image"]["contentType"], "image/png");
    assert_eq!(
        messages[2]["content"]["image"]["source"]["base64"],
        "aGVsbG8="
    );
    assert_eq!(messages[3]["role"], "ASSISTANT");
    assert_eq!(
        messages[3]["content"]["toolCalls"]["calls"][0]["callId"],
        "call-original"
    );
    assert_eq!(
        messages[3]["content"]["toolCalls"]["calls"][0]["fnName"],
        "lookup"
    );
    assert_eq!(
        messages[3]["content"]["toolCalls"]["calls"][0]["fnArguments"],
        r#"{"term":"copper"}"#
    );
    assert_eq!(messages[4]["role"], "TOOL");
    assert_eq!(
        messages[4]["content"]["toolResults"]["results"][0]["callId"],
        "call-original"
    );
    assert_eq!(
        messages[4]["content"]["toolResults"]["results"][0]["fnName"],
        "lookup"
    );
    assert_eq!(
        messages[4]["content"]["toolResults"]["results"][0]["content"],
        r#"{"matches":3}"#
    );
    assert_eq!(
        messages[4]["content"]["toolResults"]["results"][0]["isError"],
        true
    );
}

#[tokio::test]
async fn model_invoker_forwards_the_observer_to_its_tool_executor() {
    let executor = Arc::new(FakeToolExecutor::with_result(stream_result(json!({
        "content": {"text": "completed"},
        "done": true
    }))));
    let model = GrpcModelInvoker::new(31, executor);
    let observer = Arc::new(RecordingObserver::default());

    let response = model
        .invoke_with_observer(
            invocation(vec![text_message(Role::User, "hello")]),
            observer.clone(),
        )
        .await
        .unwrap();

    assert_eq!(response.content, json!("completed"));
    assert_eq!(observer.job_ids.lock().unwrap().as_slice(), [77]);
}

#[tokio::test]
async fn pending_tool_calls_are_normalized_from_final_collected_result() {
    let pending_call = json!({
        "callId": "pending-1",
        "fnName": "lookup",
        "fnArguments": r#"{"term":"tin"}"#
    });
    let result = json!({
        "content": {"toolCalls": {"calls": [pending_call.clone()]}},
        "pendingToolCalls": {"calls": [pending_call]},
        "requiresToolExecution": true,
        "done": true
    });
    let executor = Arc::new(FakeToolExecutor::with_result(stream_result(result)));
    let model = GrpcModelInvoker::new(31, executor);

    let response = model
        .invoke(invocation(vec![text_message(Role::User, "search")]))
        .await
        .unwrap();

    assert_eq!(response.content, Value::Null);
    assert_eq!(
        response.tool_calls,
        vec![ToolCall {
            call_id: "pending-1".to_owned(),
            name: "lookup".to_owned(),
            arguments: json!({"term": "tin"}),
        }]
    );
}

#[tokio::test]
async fn streams_only_interim_text_and_checks_it_against_the_collected_final() {
    let execution = ToolExecutionResult {
        worker_id: 31,
        using: "run".to_owned(),
        job_id: 79,
        result: None,
        raw_result: None,
        chunks: vec![
            OutputChunk {
                kind: OutputKind::Data,
                bytes: Vec::new(),
                json: Some(json!({"content": {"text": "Hello "}, "done": false})),
            },
            OutputChunk {
                kind: OutputKind::Data,
                bytes: Vec::new(),
                json: Some(json!({"content": {"text": "world"}, "done": false})),
            },
            OutputChunk {
                kind: OutputKind::FinalCollected,
                bytes: Vec::new(),
                json: Some(json!({"content": {"text": "Hello world"}, "done": true})),
            },
        ],
    };
    let executor = Arc::new(FakeToolExecutor::with_result(execution));
    let model = GrpcModelInvoker::new(31, executor);
    let observed = Arc::new(Mutex::new(Vec::new()));
    let observed_by_callback = observed.clone();
    let callback: ModelTextDeltaCallback = Arc::new(move |text| {
        observed_by_callback.lock().unwrap().push(text.to_owned());
        Ok(())
    });

    let response = model
        .invoke_with_text_delta(
            invocation(vec![text_message(Role::User, "say hello")]),
            callback,
        )
        .await
        .unwrap();

    assert_eq!(response.content, json!("Hello world"));
    assert_eq!(observed.lock().unwrap().as_slice(), ["Hello ", "world"]);
}

#[tokio::test]
async fn accepts_legacy_terminal_text_as_a_suffix_of_emitted_deltas() {
    let execution = ToolExecutionResult {
        worker_id: 31,
        using: "run".to_owned(),
        job_id: 85,
        result: None,
        raw_result: None,
        chunks: vec![
            OutputChunk {
                kind: OutputKind::Data,
                bytes: Vec::new(),
                json: Some(json!({"content": {"text": "☕"}, "done": false})),
            },
            OutputChunk {
                kind: OutputKind::Data,
                bytes: Vec::new(),
                // Legacy runners can append an unequal terminal delta with done=true.
                json: Some(json!({"content": {"text": "☕ is ready"}, "done": true})),
            },
            OutputChunk {
                kind: OutputKind::FinalCollected,
                bytes: Vec::new(),
                json: Some(json!({"content": {"text": "☕ is ready"}, "done": true})),
            },
        ],
    };
    let executor = Arc::new(FakeToolExecutor::with_result(execution));
    let model = GrpcModelInvoker::new(31, executor);
    let observed = Arc::new(Mutex::new(Vec::new()));
    let observed_by_callback = observed.clone();
    let callback: ModelTextDeltaCallback = Arc::new(move |text| {
        observed_by_callback.lock().unwrap().push(text.to_owned());
        Ok(())
    });

    let response = model
        .invoke_with_text_delta(
            invocation(vec![text_message(Role::User, "say hello")]),
            callback,
        )
        .await
        .unwrap();

    assert_eq!(response.content, json!("☕ is ready"));
    assert_eq!(observed.lock().unwrap().as_slice(), ["☕"]);
}

#[tokio::test]
async fn live_text_before_a_final_tool_call_is_ephemeral_commentary() {
    let pending_call = json!({
        "callId": "search-next",
        "fnName": "lookup",
        "fnArguments": r#"{"term":"tea"}"#
    });
    let execution = ToolExecutionResult {
        worker_id: 31,
        using: "run".to_owned(),
        job_id: 86,
        result: None,
        raw_result: None,
        chunks: vec![
            OutputChunk {
                kind: OutputKind::Data,
                bytes: Vec::new(),
                json: Some(json!({
                    "content": {"text": "Searching the catalog..."},
                    "done": false
                })),
            },
            OutputChunk {
                kind: OutputKind::FinalCollected,
                bytes: Vec::new(),
                json: Some(json!({
                    "content": {"toolCalls": {"calls": [pending_call.clone()]}},
                    "pendingToolCalls": {"calls": [pending_call]},
                    "requiresToolExecution": true,
                    "done": true
                })),
            },
        ],
    };
    let model = GrpcModelInvoker::new(31, Arc::new(FakeToolExecutor::with_result(execution)));
    let observed = Arc::new(Mutex::new(Vec::new()));
    let observed_by_callback = observed.clone();
    let callback: ModelTextDeltaCallback = Arc::new(move |text| {
        observed_by_callback.lock().unwrap().push(text.to_owned());
        Ok(())
    });

    let response = model
        .invoke_with_text_delta(
            invocation(vec![text_message(Role::User, "find tea")]),
            callback,
        )
        .await
        .unwrap();

    assert_eq!(response.content, Value::Null);
    assert_eq!(response.tool_calls[0].call_id, "search-next");
    assert_eq!(
        observed.lock().unwrap().as_slice(),
        ["Searching the catalog..."]
    );
}

#[tokio::test]
async fn text_delta_and_job_lifecycle_observers_can_be_used_together() {
    let execution = ToolExecutionResult {
        worker_id: 31,
        using: "run".to_owned(),
        job_id: 84,
        result: None,
        raw_result: None,
        chunks: vec![
            OutputChunk {
                kind: OutputKind::Data,
                bytes: Vec::new(),
                json: Some(json!({"content": {"text": "answer"}, "done": false})),
            },
            OutputChunk {
                kind: OutputKind::FinalCollected,
                bytes: Vec::new(),
                json: Some(json!({"content": {"text": "answer"}, "done": true})),
            },
        ],
    };
    let model = GrpcModelInvoker::new(31, Arc::new(FakeToolExecutor::with_result(execution)));
    let lifecycle = Arc::new(RecordingObserver::default());
    let deltas = Arc::new(Mutex::new(Vec::new()));
    let deltas_by_callback = deltas.clone();
    let callback: ModelTextDeltaCallback = Arc::new(move |text| {
        deltas_by_callback.lock().unwrap().push(text.to_owned());
        Ok(())
    });

    let response = model
        .invoke_with_observer_and_text_delta(
            invocation(vec![text_message(Role::User, "answer")]),
            lifecycle.clone(),
            callback,
        )
        .await
        .unwrap();

    assert_eq!(response.content, json!("answer"));
    assert_eq!(lifecycle.job_ids.lock().unwrap().as_slice(), [84]);
    assert_eq!(deltas.lock().unwrap().as_slice(), ["answer"]);
}

#[tokio::test]
async fn final_only_text_and_manual_tool_calls_do_not_emit_deltas() {
    let final_only = ToolExecutionResult {
        worker_id: 31,
        using: "run".to_owned(),
        job_id: 80,
        result: None,
        raw_result: None,
        chunks: vec![OutputChunk {
            kind: OutputKind::FinalCollected,
            bytes: Vec::new(),
            json: Some(json!({"content": {"text": "final only"}, "done": true})),
        }],
    };
    let observed = Arc::new(Mutex::new(Vec::new()));
    let observed_by_callback = observed.clone();
    let model = GrpcModelInvoker::new(31, Arc::new(FakeToolExecutor::with_result(final_only)));
    let callback: ModelTextDeltaCallback = Arc::new(move |text| {
        observed_by_callback.lock().unwrap().push(text.to_owned());
        Ok(())
    });
    let response = model
        .invoke_with_text_delta(
            invocation(vec![text_message(Role::User, "say something")]),
            callback,
        )
        .await
        .unwrap();
    assert_eq!(response.content, json!("final only"));
    assert!(observed.lock().unwrap().is_empty());

    let pending_call = json!({
        "callId": "pending-2",
        "fnName": "lookup",
        "fnArguments": r#"{"term":"zinc"}"#
    });
    let tool_result = ToolExecutionResult {
        worker_id: 31,
        using: "run".to_owned(),
        job_id: 81,
        result: None,
        raw_result: None,
        chunks: vec![
            OutputChunk {
                kind: OutputKind::Data,
                bytes: Vec::new(),
                json: Some(json!({"content": {"text": "captured but terminal"}, "done": true})),
            },
            OutputChunk {
                kind: OutputKind::FinalCollected,
                bytes: Vec::new(),
                json: Some(json!({
                    "content": {"toolCalls": {"calls": [pending_call.clone()]}},
                    "pendingToolCalls": {"calls": [pending_call]},
                    "requiresToolExecution": true,
                    "done": true
                })),
            },
        ],
    };
    let observed = Arc::new(Mutex::new(Vec::new()));
    let observed_by_callback = observed.clone();
    let model = GrpcModelInvoker::new(31, Arc::new(FakeToolExecutor::with_result(tool_result)));
    let callback: ModelTextDeltaCallback = Arc::new(move |text| {
        observed_by_callback.lock().unwrap().push(text.to_owned());
        Ok(())
    });
    let response = model
        .invoke_with_text_delta(
            invocation(vec![text_message(Role::User, "search")]),
            callback,
        )
        .await
        .unwrap();
    assert_eq!(response.content, Value::Null);
    assert_eq!(response.tool_calls[0].call_id, "pending-2");
    assert!(observed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn malformed_interim_chunks_and_final_mismatches_fail_closed() {
    let malformed = ToolExecutionResult {
        worker_id: 31,
        using: "run".to_owned(),
        job_id: 82,
        result: None,
        raw_result: None,
        chunks: vec![
            OutputChunk {
                kind: OutputKind::Data,
                bytes: Vec::new(),
                json: Some(json!({
                    "content": {"text": "must not escape"},
                    "done": false,
                    "futureField": true
                })),
            },
            OutputChunk {
                kind: OutputKind::FinalCollected,
                bytes: Vec::new(),
                json: Some(json!({"content": {"text": "must not escape"}, "done": true})),
            },
        ],
    };
    let observed = Arc::new(Mutex::new(Vec::new()));
    let observed_by_callback = observed.clone();
    let model = GrpcModelInvoker::new(31, Arc::new(FakeToolExecutor::with_result(malformed)));
    let callback: ModelTextDeltaCallback = Arc::new(move |text| {
        observed_by_callback.lock().unwrap().push(text.to_owned());
        Ok(())
    });
    let error = model
        .invoke_with_text_delta(
            invocation(vec![text_message(Role::User, "hello")]),
            callback,
        )
        .await
        .unwrap_err();
    assert!(error.contains("unsupported field `futureField`"));
    assert!(observed.lock().unwrap().is_empty());

    let mismatch = ToolExecutionResult {
        worker_id: 31,
        using: "run".to_owned(),
        job_id: 83,
        result: None,
        raw_result: None,
        chunks: vec![
            OutputChunk {
                kind: OutputKind::Data,
                bytes: Vec::new(),
                json: Some(json!({"content": {"text": "partial"}, "done": false})),
            },
            OutputChunk {
                kind: OutputKind::FinalCollected,
                bytes: Vec::new(),
                json: Some(json!({"content": {"text": "different"}, "done": true})),
            },
        ],
    };
    let model = GrpcModelInvoker::new(31, Arc::new(FakeToolExecutor::with_result(mismatch)));
    let callback: ModelTextDeltaCallback = Arc::new(|_| Ok(()));
    let error = model
        .invoke_with_text_delta(
            invocation(vec![text_message(Role::User, "hello")]),
            callback,
        )
        .await
        .unwrap_err();
    assert!(error.contains("do not match the FinalCollected text"));
}

#[tokio::test]
async fn malformed_data_after_live_text_fails_without_completing_the_model_response() {
    let execution = ToolExecutionResult {
        worker_id: 31,
        using: "run".to_owned(),
        job_id: 87,
        result: None,
        raw_result: None,
        chunks: vec![
            OutputChunk {
                kind: OutputKind::Data,
                bytes: Vec::new(),
                json: Some(json!({"content": {"text": "partial"}, "done": false})),
            },
            OutputChunk {
                kind: OutputKind::Data,
                bytes: Vec::new(),
                json: Some(json!({
                    "content": {"text": "must not complete"},
                    "done": false,
                    "futureField": true
                })),
            },
            OutputChunk {
                kind: OutputKind::FinalCollected,
                bytes: Vec::new(),
                json: Some(json!({"content": {"text": "must not complete"}, "done": true})),
            },
        ],
    };
    let model = GrpcModelInvoker::new(31, Arc::new(FakeToolExecutor::with_result(execution)));
    let observed = Arc::new(Mutex::new(Vec::new()));
    let observed_by_callback = observed.clone();
    let callback: ModelTextDeltaCallback = Arc::new(move |text| {
        observed_by_callback.lock().unwrap().push(text.to_owned());
        Ok(())
    });

    let error = model
        .invoke_with_text_delta(
            invocation(vec![text_message(Role::User, "continue")]),
            callback,
        )
        .await
        .unwrap_err();

    assert!(error.contains("unsupported field `futureField`"));
    assert_eq!(observed.lock().unwrap().as_slice(), ["partial"]);
}

#[tokio::test]
async fn invalid_execution_directives_and_tool_arguments_fail_before_enqueue() {
    let executor = Arc::new(FakeToolExecutor::with_result(stream_result(json!({
        "content": {"text": "unused"},
        "done": true
    }))));
    let model = GrpcModelInvoker::new(31, executor.clone());

    let mut directive = text_message(Role::User, "hello");
    directive.tool_execution_requests = Some(json!([]));
    let error = model.invoke(invocation(vec![directive])).await.unwrap_err();
    assert!(error.contains("tool_execution_requests"));

    let malformed_call = ChatMessage {
        role: Role::Assistant,
        content: Value::Null,
        tool_calls: vec![ToolCall {
            call_id: "bad-args".to_owned(),
            name: "lookup".to_owned(),
            arguments: json!("not an argument object"),
        }],
        tool_results: Vec::new(),
        tool_execution_requests: None,
    };
    let error = model
        .invoke(invocation(vec![malformed_call]))
        .await
        .unwrap_err();
    assert!(error.contains("arguments must be a JSON object"));

    let unsupported = ChatMessage::new(Role::User, json!({"audio": "unsupported"}));
    let error = model
        .invoke(invocation(vec![unsupported]))
        .await
        .unwrap_err();
    assert!(error.contains("content is not supported"));

    let mut function_set = invocation(vec![text_message(Role::User, "hello")]);
    function_set.function_set_name = Some("must-not-run".to_owned());
    let error = model.invoke(function_set).await.unwrap_err();
    assert!(error.contains("FunctionSet selection is not supported"));
    assert!(executor.invocations().is_empty());
}

#[tokio::test]
async fn malformed_pending_arguments_and_missing_final_stream_chunk_are_errors() {
    let malformed_pending = json!({
        "content": {"toolCalls": {"calls": [{
            "callId": "bad-pending",
            "fnName": "lookup",
            "fnArguments": "{broken"
        }]}},
        "pendingToolCalls": {"calls": [{
            "callId": "bad-pending",
            "fnName": "lookup",
            "fnArguments": "{broken"
        }]},
        "requiresToolExecution": true,
        "done": true
    });
    let executor = Arc::new(FakeToolExecutor::with_result(stream_result(
        malformed_pending,
    )));
    let model = GrpcModelInvoker::new(31, executor);
    let error = model
        .invoke(invocation(vec![text_message(Role::User, "search")]))
        .await
        .unwrap_err();
    assert!(error.contains("fnArguments"));

    let data_only = ToolExecutionResult {
        worker_id: 31,
        using: "run".to_owned(),
        job_id: 78,
        result: None,
        raw_result: None,
        chunks: vec![OutputChunk {
            kind: OutputKind::Data,
            bytes: Vec::new(),
            json: Some(json!({"content": {"text": "partial"}, "done": false})),
        }],
    };
    let executor = Arc::new(FakeToolExecutor::with_result(data_only));
    let model = GrpcModelInvoker::new(31, executor);
    let error = model
        .invoke(invocation(vec![text_message(Role::User, "say something")]))
        .await
        .unwrap_err();
    assert!(error.contains("FinalCollected"));
}
