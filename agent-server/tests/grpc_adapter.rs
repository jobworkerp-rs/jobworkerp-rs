use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use agent_server::chat::{ChatMessage, ModelInvocation, ModelJobObserver, ModelOptions, Role};
use agent_server::grpc::proto::jobworkerp::{
    data::{
        JobId, JobResult, JobResultData, MethodProtoMap, MethodSchema, ResultOutput,
        ResultOutputItem, RunnerData, RunnerId, WorkerData, WorkerId, result_output_item,
    },
    service::{CreateJobResponse, JobRequest},
};
use agent_server::grpc::{
    GrpcToolExecutor, JobworkerpRpc, ResultEnqueueResponse, ResultStream, StreamChunkObserver,
    StreamEnqueueResponse, ToolExecutor, ToolInvocation,
};
use agent_server::model::GrpcModelInvoker;
use agent_server::tool_registry::WorkerSchemaResolver;
use async_trait::async_trait;
use futures::{Stream, StreamExt, stream};
use prost::Message;
use tonic::Status;

type TestStream = Pin<Box<dyn Stream<Item = Result<ResultOutputItem, Status>> + Send>>;

#[derive(Default)]
struct DeleteGate {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

struct MockRpc {
    worker: Option<WorkerData>,
    runner: Option<RunnerData>,
    direct_response: CreateJobResponse,
    stream_job_id_header: Option<Vec<u8>>,
    stream_items: Vec<Result<ResultOutputItem, Status>>,
    stream_delay: Option<(usize, Arc<tokio::sync::Notify>)>,
    pending_stream: bool,
    requests: Arc<Mutex<Vec<JobRequest>>>,
    deleted: Arc<Mutex<Vec<i64>>>,
    delete_failures_remaining: Arc<Mutex<usize>>,
    delete_gate: Option<Arc<DeleteGate>>,
}

impl MockRpc {
    fn new(schemas: Vec<(&str, bool)>, direct_response: CreateJobResponse) -> Self {
        let schemas = schemas
            .into_iter()
            .map(|(name, requires_client_stream)| {
                (
                    name.to_owned(),
                    MethodSchema {
                        args_proto: "syntax = \"proto3\"; message Args { string text = 1; }"
                            .to_owned(),
                        result_proto: "syntax = \"proto3\"; message Reply { string answer = 1; }"
                            .to_owned(),
                        require_client_stream: requires_client_stream,
                        ..Default::default()
                    },
                )
            })
            .collect::<HashMap<_, _>>();

        Self {
            worker: Some(WorkerData {
                runner_id: Some(RunnerId { value: 2 }),
                ..Default::default()
            }),
            runner: Some(RunnerData {
                method_proto_map: Some(MethodProtoMap { schemas }),
                ..Default::default()
            }),
            direct_response,
            stream_job_id_header: Some(JobId { value: 19 }.encode_to_vec()),
            stream_items: Vec::new(),
            stream_delay: None,
            pending_stream: false,
            requests: Arc::new(Mutex::new(Vec::new())),
            deleted: Arc::new(Mutex::new(Vec::new())),
            delete_failures_remaining: Arc::new(Mutex::new(0)),
            delete_gate: None,
        }
    }

    fn with_stream_items(mut self, items: Vec<Result<ResultOutputItem, Status>>) -> Self {
        self.stream_items = items;
        self
    }

    fn with_result_proto(mut self, result_proto: &str) -> Self {
        self.runner
            .as_mut()
            .unwrap()
            .method_proto_map
            .as_mut()
            .unwrap()
            .schemas
            .get_mut("run")
            .unwrap()
            .result_proto = result_proto.to_owned();
        self
    }

    fn with_args_proto(mut self, args_proto: &str) -> Self {
        self.runner
            .as_mut()
            .unwrap()
            .method_proto_map
            .as_mut()
            .unwrap()
            .schemas
            .get_mut("run")
            .unwrap()
            .args_proto = args_proto.to_owned();
        self
    }

    fn with_delay_before_stream_item(
        mut self,
        item_index: usize,
        release: Arc<tokio::sync::Notify>,
    ) -> Self {
        self.stream_delay = Some((item_index, release));
        self
    }

    fn with_stream_job_id_header(mut self, header: Option<Vec<u8>>) -> Self {
        self.stream_job_id_header = header;
        self
    }

    fn with_pending_stream(mut self) -> Self {
        self.pending_stream = true;
        self
    }

    fn with_delete_failures(mut self, count: usize) -> Self {
        self.delete_failures_remaining = Arc::new(Mutex::new(count));
        self
    }

    fn with_delete_gate(mut self, gate: Arc<DeleteGate>) -> Self {
        self.delete_gate = Some(gate);
        self
    }
}

#[async_trait]
impl JobworkerpRpc for MockRpc {
    async fn find_worker(&self, worker_id: i64) -> Result<Option<WorkerData>, Status> {
        assert_eq!(worker_id, 7);
        Ok(self.worker.clone())
    }

    async fn find_runner(&self, runner_id: i64) -> Result<Option<RunnerData>, Status> {
        assert_eq!(runner_id, 2);
        Ok(self.runner.clone())
    }

    async fn enqueue(&self, request: JobRequest) -> Result<CreateJobResponse, Status> {
        self.requests.lock().unwrap().push(request);
        Ok(self.direct_response.clone())
    }

    async fn enqueue_for_stream(
        &self,
        request: JobRequest,
    ) -> Result<StreamEnqueueResponse, Status> {
        self.requests.lock().unwrap().push(request);
        let items: TestStream = if self.pending_stream {
            Box::pin(stream::pending())
        } else if let Some((delay_index, release)) = self.stream_delay.clone() {
            Box::pin(
                stream::iter(self.stream_items.clone().into_iter().enumerate()).then(
                    move |(index, item)| {
                        let release = release.clone();
                        async move {
                            if index == delay_index {
                                release.notified().await;
                            }
                            item
                        }
                    },
                ),
            )
        } else {
            Box::pin(stream::iter(self.stream_items.clone()))
        };
        Ok(StreamEnqueueResponse {
            job_id_header: self.stream_job_id_header.clone(),
            items,
        })
    }

    async fn enqueue_for_result(
        &self,
        request: JobRequest,
    ) -> Result<ResultEnqueueResponse, Status> {
        self.requests.lock().unwrap().push(request);
        let results: ResultStream = Box::pin(stream::empty::<Result<JobResult, Status>>());
        Ok(ResultEnqueueResponse {
            job_id_header: Some(JobId { value: 19 }.encode_to_vec()),
            results,
        })
    }

    async fn delete(&self, job_id: JobId) -> Result<(), Status> {
        self.deleted.lock().unwrap().push(job_id.value);
        if let Some(gate) = self.delete_gate.as_ref() {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        let mut failures_remaining = self.delete_failures_remaining.lock().unwrap();
        if *failures_remaining > 0 {
            *failures_remaining -= 1;
            return Err(Status::failed_precondition("delete failed"));
        }
        Ok(())
    }
}

#[derive(Default)]
struct RecordingObserver {
    job_ids: Mutex<Vec<i64>>,
    started: tokio::sync::Notify,
    finished: AtomicUsize,
}

#[async_trait]
impl ModelJobObserver for RecordingObserver {
    async fn job_started(&self, job_id: i64) -> Result<(), String> {
        self.job_ids.lock().unwrap().push(job_id);
        self.started.notify_one();
        Ok(())
    }

    fn job_finished(&self, _job_id: i64) {
        self.finished.fetch_add(1, Ordering::Relaxed);
    }
}

struct RejectingObserver {
    job_ids: Mutex<Vec<i64>>,
}

struct CancellingObserver {
    executor: Arc<GrpcToolExecutor>,
    finished: AtomicUsize,
}

#[async_trait]
impl ModelJobObserver for CancellingObserver {
    async fn job_started(&self, job_id: i64) -> Result<(), String> {
        self.executor.cancel_job(&job_id.to_string()).await?;
        Err("observer rejected after confirming cancellation".to_owned())
    }

    fn job_finished(&self, _job_id: i64) {
        self.finished.fetch_add(1, Ordering::Relaxed);
    }
}

#[async_trait]
impl ModelJobObserver for RejectingObserver {
    async fn job_started(&self, job_id: i64) -> Result<(), String> {
        self.job_ids.lock().unwrap().push(job_id);
        Err("observer rejected the model job".to_owned())
    }

    fn job_finished(&self, _job_id: i64) {}
}

fn direct_response(job_id: i64, status: i32, output: Vec<u8>) -> CreateJobResponse {
    CreateJobResponse {
        id: Some(JobId { value: job_id }),
        result: Some(JobResult {
            data: Some(JobResultData {
                status,
                output: Some(ResultOutput { items: output }),
                ..Default::default()
            }),
            ..Default::default()
        }),
    }
}

fn encoded_reply(answer: &str) -> Vec<u8> {
    let descriptor = command_utils::protobuf::ProtobufDescriptor::new(
        &"syntax = \"proto3\"; message Reply { string answer = 1; }".to_owned(),
    )
    .unwrap();
    command_utils::protobuf::ProtobufDescriptor::json_to_message(
        descriptor.get_messages().remove(0),
        &serde_json::json!({ "answer": answer }).to_string(),
        false,
    )
    .unwrap()
}

const LLM_CHAT_RESULT_PROTO: &str = "syntax = \"proto3\"; message LLMChatResult { ChatContent content = 1; bool done = 2; } message ChatContent { string text = 1; }";
const LLM_CHAT_ARGS_PROTO: &str = "syntax = \"proto3\"; message Args { FunctionOptions function_options = 1; repeated ChatMessage messages = 2; } message FunctionOptions { bool use_function_calling = 1; bool is_auto_calling = 2; string client_tools_json = 3; } message ChatMessage { string role = 1; ChatContent content = 2; } message ChatContent { string text = 1; }";

fn encoded_chat_result(text: &str, done: bool) -> Vec<u8> {
    let proto = LLM_CHAT_RESULT_PROTO.to_owned();
    let descriptor = command_utils::protobuf::ProtobufDescriptor::new(&proto).unwrap();
    command_utils::protobuf::ProtobufDescriptor::json_to_message(
        descriptor.get_messages().remove(0),
        &serde_json::json!({ "content": { "text": text }, "done": done }).to_string(),
        false,
    )
    .unwrap()
}

fn executor(rpc: MockRpc) -> GrpcToolExecutor {
    GrpcToolExecutor::with_rpc(Arc::new(rpc))
}

#[tokio::test]
async fn streaming_output_is_bounded_before_collecting_into_memory() {
    let chunk = encoded_reply("chunk");
    let rpc = MockRpc::new(
        vec![("run", false)],
        direct_response(11, 0, encoded_reply("unused")),
    )
    .with_stream_items(
        (0..5000)
            .map(|_| {
                Ok(ResultOutputItem {
                    item: Some(result_output_item::Item::Data(chunk.clone())),
                })
            })
            .collect(),
    );

    let observed = Arc::new(AtomicUsize::new(0));
    let observed_by_callback = observed.clone();
    let observer: StreamChunkObserver = Arc::new(move |_| {
        observed_by_callback.fetch_add(1, Ordering::Relaxed);
        Ok(())
    });
    let error = executor(rpc)
        .execute_with_stream_observer(
            invocation(Some("run"), true, serde_json::json!({ "text": "hello" })),
            observer,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("stream output limit"));
    assert!(observed.load(Ordering::Relaxed) < 5000);
    assert!(observed.load(Ordering::Relaxed) > 0);
}

#[tokio::test]
async fn stream_byte_limit_is_cumulative_across_data_and_final_collected_items() {
    let data_bytes = vec![0; 7 * 1024 * 1024];
    let final_bytes = vec![0; 2 * 1024 * 1024];
    let rpc = MockRpc::new(vec![("run", false)], CreateJobResponse::default())
        .with_result_proto("")
        .with_stream_items(vec![
            Ok(ResultOutputItem {
                item: Some(result_output_item::Item::Data(data_bytes)),
            }),
            Ok(ResultOutputItem {
                item: Some(result_output_item::Item::FinalCollected(final_bytes)),
            }),
            Ok(ResultOutputItem {
                item: Some(result_output_item::Item::End(Default::default())),
            }),
        ]);

    let error = executor(rpc)
        .execute(invocation(
            Some("run"),
            true,
            serde_json::json!({ "text": "hello" }),
        ))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("stream output limit"));
}

#[tokio::test]
async fn malformed_protobuf_stream_items_are_not_observed() {
    let rpc =
        MockRpc::new(vec![("run", false)], CreateJobResponse::default()).with_stream_items(vec![
            Ok(ResultOutputItem {
                item: Some(result_output_item::Item::Data(vec![0xff])),
            }),
        ]);
    let observed = Arc::new(AtomicUsize::new(0));
    let observed_by_callback = observed.clone();
    let observer: StreamChunkObserver = Arc::new(move |_| {
        observed_by_callback.fetch_add(1, Ordering::Relaxed);
        Ok(())
    });
    let error = executor(rpc)
        .execute_with_stream_observer(
            invocation(Some("run"), true, serde_json::json!({ "text": "hello" })),
            observer,
        )
        .await
        .unwrap_err();

    assert!(format!("{error:#}").contains("decode worker result protobuf"));
    assert_eq!(observed.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn llm_text_deltas_arrive_before_delayed_final_and_final_text_is_not_duplicated() {
    let release_final = Arc::new(tokio::sync::Notify::new());
    let rpc = MockRpc::new(vec![("run", false)], CreateJobResponse::default())
        .with_args_proto(LLM_CHAT_ARGS_PROTO)
        .with_result_proto(LLM_CHAT_RESULT_PROTO)
        .with_stream_items(vec![
            Ok(ResultOutputItem {
                item: Some(result_output_item::Item::Data(encoded_chat_result(
                    "Hello ", false,
                ))),
            }),
            Ok(ResultOutputItem {
                item: Some(result_output_item::Item::Data(encoded_chat_result(
                    "world", false,
                ))),
            }),
            // Some providers include a done=true Data item before the collected terminal result.
            Ok(ResultOutputItem {
                item: Some(result_output_item::Item::Data(encoded_chat_result(
                    "Hello world",
                    true,
                ))),
            }),
            Ok(ResultOutputItem {
                item: Some(result_output_item::Item::FinalCollected(
                    encoded_chat_result("Hello world", true),
                )),
            }),
            Ok(ResultOutputItem {
                item: Some(result_output_item::Item::End(Default::default())),
            }),
        ])
        .with_delay_before_stream_item(2, release_final.clone());
    let executor: Arc<dyn ToolExecutor> = Arc::new(GrpcToolExecutor::with_rpc(Arc::new(rpc)));
    let model = Arc::new(GrpcModelInvoker::new(7, executor));
    let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel();
    let text_callback = Arc::new(move |text: &str| {
        delta_tx
            .send(text.to_owned())
            .map_err(|_| "test delta receiver closed".to_owned())
    });
    let invocation = ModelInvocation {
        llm_worker_id: 7,
        options: ModelOptions::default(),
        history: vec![ChatMessage::new(Role::User, serde_json::json!("Say hello"))],
        client_tools_json: "[]".to_owned(),
        is_auto_calling: false,
        function_set_name: None,
    };
    let task = tokio::spawn(async move {
        model
            .invoke_with_text_delta(invocation, text_callback)
            .await
    });

    let first = tokio::time::timeout(std::time::Duration::from_secs(1), delta_rx.recv())
        .await
        .expect("first delta should arrive before the final item");
    let Some(first) = first else {
        panic!("delta stream closed early: {:?}", task.await);
    };
    let second = tokio::time::timeout(std::time::Duration::from_secs(1), delta_rx.recv())
        .await
        .expect("second delta should arrive before the final item");
    let Some(second) = second else {
        panic!("delta stream closed early: {:?}", task.await);
    };
    assert_eq!([first.as_str(), second.as_str()], ["Hello ", "world"]);
    assert!(
        !task.is_finished(),
        "the final event is deliberately blocked"
    );

    release_final.notify_one();
    let response = tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .expect("the stream should finish after releasing the final event")
        .unwrap()
        .unwrap();
    assert_eq!(response.content, serde_json::json!("Hello world"));
    assert!(
        delta_rx.try_recv().is_err(),
        "final collected text is not emitted again"
    );
}

#[tokio::test]
async fn resolves_worker_method_input_schema_for_the_registry() {
    let rpc = MockRpc::new(
        vec![("run", false)],
        direct_response(11, 0, encoded_reply("unused")),
    );
    let resolver = executor(rpc);
    let resolved = resolver.resolve_input_schema(7, "run").await.unwrap();
    assert_eq!(resolved.schema["type"], "object");
    assert_eq!(resolved.schema["properties"]["text"]["type"], "string");
    assert!(!resolved.revision.is_empty());
    assert!(resolver.resolve_input_schema(7, "missing").await.is_err());
}

#[tokio::test]
async fn oversized_runner_schemas_are_rejected_before_enqueue() {
    for oversized_field in ["args", "result"] {
        let mut rpc = MockRpc::new(
            vec![("run", false)],
            direct_response(11, 0, encoded_reply("unused")),
        );
        let requests = rpc.requests.clone();
        let schema = rpc
            .runner
            .as_mut()
            .unwrap()
            .method_proto_map
            .as_mut()
            .unwrap()
            .schemas
            .get_mut("run")
            .unwrap();
        if oversized_field == "args" {
            schema.args_proto = "x".repeat(512 * 1024 + 1);
        } else {
            schema.result_proto = "x".repeat(512 * 1024 + 1);
        }
        let error = executor(rpc)
            .execute(invocation(
                Some("run"),
                false,
                serde_json::json!({ "text": "hi" }),
            ))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("schema size limit"));
        assert!(requests.lock().unwrap().is_empty());
    }
}

fn invocation(using: Option<&str>, stream: bool, arguments: serde_json::Value) -> ToolInvocation {
    ToolInvocation {
        worker_id: 7,
        using: using.map(ToOwned::to_owned),
        arguments,
        stream,
    }
}

#[tokio::test]
async fn invokes_worker_by_id_and_decodes_direct_result() {
    let rpc = MockRpc::new(
        vec![("run", false)],
        direct_response(11, 0, encoded_reply("done")),
    );
    let requests = rpc.requests.clone();
    let executor = executor(rpc);

    let result = executor
        .execute(invocation(
            None,
            false,
            serde_json::json!({ "text": "hello" }),
        ))
        .await
        .unwrap();

    assert_eq!(result.job_id, 11);
    assert_eq!(result.using, "run");
    assert_eq!(result.result, Some(serde_json::json!({ "answer": "done" })));
    let request = requests.lock().unwrap();
    let request = request.first().unwrap();
    assert_eq!(request.using.as_deref(), Some("run"));
    assert_eq!(
        request
            .overrides
            .as_ref()
            .and_then(|overrides| overrides.response_type),
        Some(agent_server::grpc::proto::jobworkerp::data::ResponseType::Direct as i32)
    );
    assert_eq!(
        request.worker,
        Some(
            agent_server::grpc::proto::jobworkerp::service::job_request::Worker::WorkerId(
                WorkerId { value: 7 }
            )
        )
    );

    let args_proto = "syntax = \"proto3\"; message Args { string text = 1; }".to_owned();
    let args_descriptor = command_utils::protobuf::ProtobufDescriptor::new(&args_proto).unwrap();
    let decoded_args = command_utils::protobuf::ProtobufDescriptor::get_message_from_bytes(
        args_descriptor.get_messages().remove(0),
        &request.args,
    )
    .unwrap();
    assert_eq!(
        command_utils::protobuf::ProtobufDescriptor::message_to_json_value(&decoded_args).unwrap(),
        serde_json::json!({ "text": "hello" })
    );
}

#[tokio::test]
async fn dispatches_only_the_exact_selected_runner_method() {
    let rpc = MockRpc::new(
        vec![("run", false), ("fetch", false)],
        direct_response(14, 0, encoded_reply("fetched")),
    );
    let requests = rpc.requests.clone();
    let executor = executor(rpc);

    let result = executor
        .execute(invocation(
            Some("fetch"),
            false,
            serde_json::json!({ "text": "url" }),
        ))
        .await
        .unwrap();

    assert_eq!(result.using, "fetch");
    assert_eq!(requests.lock().unwrap()[0].using.as_deref(), Some("fetch"));
}

#[tokio::test]
async fn rejects_unknown_argument_fields_before_enqueue() {
    let rpc = MockRpc::new(
        vec![("run", false)],
        direct_response(11, 0, encoded_reply("unused")),
    );
    let executor = executor(rpc);

    let error = executor
        .execute(invocation(
            None,
            false,
            serde_json::json!({ "text": "hello", "unexpected": true }),
        ))
        .await
        .unwrap_err();

    assert!(format!("{error:#}").contains("unexpected"));
}

#[tokio::test]
async fn rejects_unregistered_method_before_enqueue() {
    let rpc = MockRpc::new(
        vec![("run", false)],
        direct_response(11, 0, encoded_reply("unused")),
    );
    let requests = rpc.requests.clone();
    let executor = executor(rpc);

    let error = executor
        .execute(invocation(
            Some("execute"),
            false,
            serde_json::json!({ "text": "hello" }),
        ))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("not registered"));
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn requires_explicit_method_when_runner_is_ambiguous() {
    let rpc = MockRpc::new(
        vec![("run", false), ("fetch", false)],
        direct_response(11, 0, encoded_reply("unused")),
    );
    let requests = rpc.requests.clone();
    let executor = executor(rpc);

    let error = executor
        .execute(invocation(
            None,
            false,
            serde_json::json!({ "text": "hello" }),
        ))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("ambiguous"));
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn rejects_single_non_default_method_without_using() {
    let executor = executor(MockRpc::new(
        vec![("fetch", false)],
        direct_response(11, 0, encoded_reply("unused")),
    ));

    let error = executor
        .execute(invocation(
            None,
            false,
            serde_json::json!({ "text": "hello" }),
        ))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("default"));
}

#[tokio::test]
async fn rejects_methods_requiring_client_streaming() {
    let rpc = MockRpc::new(
        vec![("run", true)],
        direct_response(11, 0, encoded_reply("unused")),
    );
    let requests = rpc.requests.clone();
    let executor = executor(rpc);

    let error = executor
        .execute(invocation(
            Some("run"),
            false,
            serde_json::json!({ "text": "hello" }),
        ))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("client stream"));
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn checks_direct_job_status_and_requires_result() {
    let failed_executor = executor(MockRpc::new(
        vec![("run", false)],
        direct_response(12, 2, b"runner failed".to_vec()),
    ));
    let failed = failed_executor
        .execute(invocation(
            None,
            false,
            serde_json::json!({ "text": "hello" }),
        ))
        .await
        .unwrap_err();
    assert!(failed.to_string().contains("status 2"));
    assert!(failed.to_string().contains("runner failed"));

    let no_result_executor = executor(MockRpc::new(
        vec![("run", false)],
        CreateJobResponse {
            id: Some(JobId { value: 13 }),
            result: None,
        },
    ));
    let no_result = no_result_executor
        .execute(invocation(
            None,
            false,
            serde_json::json!({ "text": "hello" }),
        ))
        .await
        .unwrap_err();
    assert!(no_result.to_string().contains("without a direct result"));
}

#[tokio::test]
async fn captures_stream_job_id_and_requires_terminal_end() {
    let data = ResultOutputItem {
        item: Some(result_output_item::Item::Data(encoded_reply("chunk"))),
    };
    let end = ResultOutputItem {
        item: Some(result_output_item::Item::End(Default::default())),
    };
    let rpc = MockRpc::new(vec![("run", false)], CreateJobResponse::default())
        .with_stream_items(vec![Ok(data), Ok(end)]);
    let result = executor(rpc)
        .execute(invocation(
            Some("run"),
            true,
            serde_json::json!({ "text": "hello" }),
        ))
        .await
        .unwrap();

    assert_eq!(result.job_id, 19);
    assert_eq!(result.chunks.len(), 1);
    assert_eq!(
        result.chunks[0].json,
        Some(serde_json::json!({ "answer": "chunk" }))
    );

    let missing_end = executor(
        MockRpc::new(vec![("run", false)], CreateJobResponse::default()).with_stream_items(vec![
            Ok(ResultOutputItem {
                item: Some(result_output_item::Item::Data(encoded_reply("partial"))),
            }),
        ]),
    )
    .execute(invocation(
        Some("run"),
        true,
        serde_json::json!({ "text": "hello" }),
    ))
    .await
    .unwrap_err();
    assert!(missing_end.to_string().contains("end"));
}

#[tokio::test]
async fn preserves_terminal_stream_errors_and_rejects_missing_job_id() {
    let terminal_error = executor(
        MockRpc::new(vec![("run", false)], CreateJobResponse::default())
            .with_stream_items(vec![Err(Status::internal("stream failed"))]),
    )
    .execute(invocation(
        Some("run"),
        true,
        serde_json::json!({ "text": "hello" }),
    ))
    .await
    .unwrap_err();
    assert!(format!("{terminal_error:#}").contains("stream failed"));

    let missing_id = executor(
        MockRpc::new(vec![("run", false)], CreateJobResponse::default())
            .with_stream_job_id_header(None),
    )
    .execute(invocation(
        Some("run"),
        true,
        serde_json::json!({ "text": "hello" }),
    ))
    .await
    .unwrap_err();
    assert!(missing_id.to_string().contains("job ID"));
}

#[tokio::test]
async fn streaming_enqueue_notifies_the_eager_job_id_before_the_result_stream_finishes() {
    let rpc = MockRpc::new(vec![("run", false)], CreateJobResponse::default())
        .with_pending_stream()
        .with_delete_failures(1);
    let deleted = rpc.deleted.clone();
    let executor = Arc::new(GrpcToolExecutor::with_rpc(Arc::new(rpc)));
    let observer = Arc::new(RecordingObserver::default());
    let started = observer.started.notified();
    let task = {
        let executor = executor.clone();
        let observer = observer.clone();
        tokio::spawn(async move {
            executor
                .execute_with_observer(
                    invocation(Some("run"), true, serde_json::json!({"text": "hello"})),
                    observer,
                )
                .await
        })
    };

    tokio::time::timeout(std::time::Duration::from_secs(1), started)
        .await
        .expect("job ID should be observed while the stream remains pending");
    assert_eq!(observer.job_ids.lock().unwrap().as_slice(), [19]);

    assert!(executor.cancel_job("18").await.is_err());
    assert!(executor.cancel_job("19").await.is_err());
    executor.cancel_job("19").await.unwrap();
    assert!(executor.cancel_job("19").await.is_err());
    assert_eq!(deleted.lock().unwrap().as_slice(), [19, 19]);
    let error = task.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("cancelled"));
}

#[tokio::test]
async fn observer_start_failure_attempts_delete_and_retains_uncertain_ownership_for_retry() {
    let rpc = MockRpc::new(vec![("run", false)], CreateJobResponse::default())
        .with_pending_stream()
        .with_delete_failures(1);
    let deleted = rpc.deleted.clone();
    let executor = executor(rpc);
    let observer = Arc::new(RejectingObserver {
        job_ids: Mutex::new(Vec::new()),
    });

    let error = executor
        .execute_with_observer(
            invocation(Some("run"), true, serde_json::json!({"text": "hello"})),
            observer.clone(),
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("observer rejected"));
    assert_eq!(observer.job_ids.lock().unwrap().as_slice(), [19]);
    assert_eq!(deleted.lock().unwrap().as_slice(), [19]);
    executor.cancel_job("19").await.unwrap();
    assert_eq!(deleted.lock().unwrap().as_slice(), [19, 19]);
    assert!(executor.cancel_job("19").await.is_err());
}

#[tokio::test]
async fn chunk_callback_failure_keeps_job_cancellable_when_stream_errors() {
    let rpc =
        MockRpc::new(vec![("run", false)], CreateJobResponse::default()).with_stream_items(vec![
            Ok(ResultOutputItem {
                item: Some(result_output_item::Item::Data(encoded_reply("partial"))),
            }),
            Err(Status::internal("stream failed after callback")),
        ]);
    let deleted = rpc.deleted.clone();
    let executor = executor(rpc);
    let observer = Arc::new(RecordingObserver::default());
    let callback: StreamChunkObserver = Arc::new(|_| Err("consumer rejected chunk".to_owned()));

    let error = executor
        .execute_with_observers(
            invocation(Some("run"), true, serde_json::json!({"text": "hello"})),
            Some(observer.clone()),
            Some(callback),
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("consumer rejected chunk"));
    assert_eq!(observer.job_ids.lock().unwrap().as_slice(), [19]);
    assert!(deleted.lock().unwrap().is_empty());
    executor.cancel_job("19").await.unwrap();
    assert_eq!(deleted.lock().unwrap().as_slice(), [19]);
}

#[tokio::test]
async fn callback_failure_drains_a_terminal_stream_without_more_callbacks() {
    let rpc =
        MockRpc::new(vec![("run", false)], CreateJobResponse::default()).with_stream_items(vec![
            Ok(ResultOutputItem {
                item: Some(result_output_item::Item::Data(encoded_reply("first"))),
            }),
            Ok(ResultOutputItem {
                item: Some(result_output_item::Item::Data(encoded_reply("later"))),
            }),
            Ok(ResultOutputItem {
                item: Some(result_output_item::Item::End(Default::default())),
            }),
        ]);
    let deleted = rpc.deleted.clone();
    let executor = executor(rpc);
    let observer = Arc::new(RecordingObserver::default());
    let callbacks = Arc::new(AtomicUsize::new(0));
    let callbacks_from_callback = callbacks.clone();
    let callback: StreamChunkObserver = Arc::new(move |_| {
        callbacks_from_callback.fetch_add(1, Ordering::Relaxed);
        Err("consumer rejected chunk".to_owned())
    });

    let error = executor
        .execute_with_observers(
            invocation(Some("run"), true, serde_json::json!({"text": "hello"})),
            Some(observer.clone()),
            Some(callback),
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("consumer rejected chunk"));
    assert!(!error.to_string().contains("ownership was retained"));
    assert_eq!(callbacks.load(Ordering::Relaxed), 1);
    assert_eq!(observer.job_ids.lock().unwrap().as_slice(), [19]);
    assert_eq!(observer.finished.load(Ordering::Relaxed), 1);
    assert!(deleted.lock().unwrap().is_empty());
    assert!(executor.cancel_job("19").await.is_err());
}

#[tokio::test]
async fn callback_failure_with_missing_end_retains_cancellable_ownership() {
    let rpc =
        MockRpc::new(vec![("run", false)], CreateJobResponse::default()).with_stream_items(vec![
            Ok(ResultOutputItem {
                item: Some(result_output_item::Item::Data(encoded_reply("first"))),
            }),
            Ok(ResultOutputItem {
                item: Some(result_output_item::Item::Data(encoded_reply("later"))),
            }),
        ]);
    let deleted = rpc.deleted.clone();
    let executor = executor(rpc);
    let observer = Arc::new(RecordingObserver::default());
    let callbacks = Arc::new(AtomicUsize::new(0));
    let callbacks_from_callback = callbacks.clone();
    let callback: StreamChunkObserver = Arc::new(move |_| {
        callbacks_from_callback.fetch_add(1, Ordering::Relaxed);
        Err("consumer rejected chunk".to_owned())
    });

    let error = executor
        .execute_with_observers(
            invocation(Some("run"), true, serde_json::json!({"text": "hello"})),
            Some(observer.clone()),
            Some(callback),
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("consumer rejected chunk"));
    assert!(error.to_string().contains("ownership was retained"));
    assert_eq!(callbacks.load(Ordering::Relaxed), 1);
    assert_eq!(observer.finished.load(Ordering::Relaxed), 0);
    assert!(deleted.lock().unwrap().is_empty());
    executor.cancel_job("19").await.unwrap();
    assert_eq!(deleted.lock().unwrap().as_slice(), [19]);
}

#[tokio::test]
async fn callback_failure_with_a_stalled_drain_is_bounded_and_cancellable() {
    let release = Arc::new(tokio::sync::Notify::new());
    let rpc = MockRpc::new(vec![("run", false)], CreateJobResponse::default())
        .with_stream_items(vec![
            Ok(ResultOutputItem {
                item: Some(result_output_item::Item::Data(encoded_reply("first"))),
            }),
            Ok(ResultOutputItem {
                item: Some(result_output_item::Item::End(Default::default())),
            }),
        ])
        .with_delay_before_stream_item(1, release);
    let deleted = rpc.deleted.clone();
    let executor = Arc::new(executor(rpc));
    let observer = Arc::new(RecordingObserver::default());
    let callback_failed = Arc::new(tokio::sync::Notify::new());
    let callback_failed_from_callback = callback_failed.clone();
    let callback: StreamChunkObserver = Arc::new(move |_| {
        callback_failed_from_callback.notify_one();
        Err("consumer rejected chunk".to_owned())
    });
    let execution = {
        let executor = executor.clone();
        let observer = observer.clone();
        tokio::spawn(async move {
            executor
                .execute_with_observers(
                    invocation(Some("run"), true, serde_json::json!({"text": "hello"})),
                    Some(observer),
                    Some(callback),
                )
                .await
        })
    };

    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        callback_failed.notified(),
    )
    .await
    .expect("the callback should fail before the stream stalls");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        !execution.is_finished(),
        "the executor should keep draining instead of returning immediately"
    );
    let error = tokio::time::timeout(std::time::Duration::from_secs(4), execution)
        .await
        .expect("a stalled post-callback drain must have a deadline")
        .unwrap()
        .unwrap_err();

    assert!(error.to_string().contains("consumer rejected chunk"));
    assert!(error.to_string().contains("ownership was retained"));
    assert_eq!(observer.finished.load(Ordering::Relaxed), 0);
    assert!(deleted.lock().unwrap().is_empty());
    executor.cancel_job("19").await.unwrap();
    assert_eq!(deleted.lock().unwrap().as_slice(), [19]);
}

#[tokio::test]
async fn aborted_stream_future_retains_job_ownership_for_explicit_cleanup() {
    let rpc =
        MockRpc::new(vec![("run", false)], CreateJobResponse::default()).with_pending_stream();
    let deleted = rpc.deleted.clone();
    let executor = Arc::new(GrpcToolExecutor::with_rpc(Arc::new(rpc)));
    let observer = Arc::new(RecordingObserver::default());
    let started = observer.started.notified();
    let task = {
        let executor = executor.clone();
        let observer = observer.clone();
        tokio::spawn(async move {
            executor
                .execute_with_observer(
                    invocation(Some("run"), true, serde_json::json!({"text": "hello"})),
                    observer,
                )
                .await
        })
    };

    tokio::time::timeout(std::time::Duration::from_secs(1), started)
        .await
        .expect("job ID should be observed before aborting the stream future");
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());

    executor.cancel_job("19").await.unwrap();
    assert_eq!(deleted.lock().unwrap().as_slice(), [19]);
    assert!(executor.cancel_job("19").await.is_err());
}

#[tokio::test]
async fn observer_failure_does_not_repeat_a_confirmed_successful_delete() {
    let rpc =
        MockRpc::new(vec![("run", false)], CreateJobResponse::default()).with_pending_stream();
    let deleted = rpc.deleted.clone();
    let executor = Arc::new(GrpcToolExecutor::with_rpc(Arc::new(rpc)));
    let observer = Arc::new(CancellingObserver {
        executor: executor.clone(),
        finished: AtomicUsize::new(0),
    });

    let error = executor
        .execute_with_observer(
            invocation(Some("run"), true, serde_json::json!({"text": "hello"})),
            observer.clone(),
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("observer rejected"));
    assert_eq!(deleted.lock().unwrap().as_slice(), [19]);
    assert_eq!(observer.finished.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn observer_failure_cleanup_is_bounded_and_keeps_the_job_retryable() {
    let gate = Arc::new(DeleteGate::default());
    let rpc = MockRpc::new(vec![("run", false)], CreateJobResponse::default())
        .with_pending_stream()
        .with_delete_gate(gate.clone());
    let deleted = rpc.deleted.clone();
    let executor = Arc::new(GrpcToolExecutor::with_rpc(Arc::new(rpc)));
    let observer = Arc::new(RejectingObserver {
        job_ids: Mutex::new(Vec::new()),
    });
    let cleanup_started = gate.entered.notified();
    let execution = {
        let executor = executor.clone();
        let observer = observer.clone();
        tokio::spawn(async move {
            executor
                .execute_with_observer(
                    invocation(Some("run"), true, serde_json::json!({"text": "hello"})),
                    observer,
                )
                .await
        })
    };
    cleanup_started.await;

    let error = tokio::time::timeout(std::time::Duration::from_secs(5), execution)
        .await
        .expect("best-effort Delete must obey its cleanup timeout")
        .unwrap()
        .unwrap_err();

    assert!(error.to_string().contains("exceeded its time limit"));
    assert!(
        error
            .to_string()
            .contains("retry cancellation with job ID 19")
    );
    assert_eq!(deleted.lock().unwrap().as_slice(), [19]);
    gate.release.notify_one();
    executor.cancel_job("19").await.unwrap();
    assert_eq!(deleted.lock().unwrap().as_slice(), [19, 19]);
}

#[tokio::test]
async fn concurrent_cancellations_delete_an_owned_stream_job_only_once() {
    let gate = Arc::new(DeleteGate::default());
    let rpc = MockRpc::new(vec![("run", false)], CreateJobResponse::default())
        .with_pending_stream()
        .with_delete_gate(gate.clone());
    let deleted = rpc.deleted.clone();
    let executor = Arc::new(GrpcToolExecutor::with_rpc(Arc::new(rpc)));
    let observer = Arc::new(RecordingObserver::default());
    let started = observer.started.notified();
    let stream_task = {
        let executor = executor.clone();
        let observer = observer.clone();
        tokio::spawn(async move {
            executor
                .execute_with_observer(
                    invocation(Some("run"), true, serde_json::json!({"text": "hello"})),
                    observer,
                )
                .await
        })
    };
    started.await;

    let delete_started = gate.entered.notified();
    let first_cancel = {
        let executor = executor.clone();
        tokio::spawn(async move { executor.cancel_job("19").await })
    };
    delete_started.await;
    let second_cancel = {
        let executor = executor.clone();
        tokio::spawn(async move { executor.cancel_job("19").await })
    };
    tokio::task::yield_now().await;
    assert_eq!(deleted.lock().unwrap().as_slice(), [19]);

    gate.release.notify_one();
    first_cancel.await.unwrap().unwrap();
    assert!(second_cancel.await.unwrap().is_err());
    assert_eq!(deleted.lock().unwrap().as_slice(), [19]);
    assert!(
        stream_task
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
}
