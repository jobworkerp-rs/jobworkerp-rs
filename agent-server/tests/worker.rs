use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_server::{chat, tool_registry};
use async_trait::async_trait;
use futures::{StreamExt, channel::mpsc, stream};
use prost::Message;
use tonic::Status;

#[path = "../src/grpc.rs"]
#[allow(dead_code)]
mod grpc;
#[path = "../src/worker.rs"]
#[allow(dead_code)]
mod worker;

use agent_server::chat::{RegisteredTool, WorkerExecutor};
use grpc::proto::jobworkerp::{
    data::{
        JobId, JobResult, JobResultData, MethodProtoMap, MethodSchema, ResponseType, ResultOutput,
        ResultStatus, RunnerData, RunnerId, WorkerData, WorkerId,
    },
    service::{CreateJobResponse, JobRequest, job_request},
};
use grpc::{JobworkerpRpc, ResultEnqueueResponse, ResultStream};
use worker::GrpcWorkerExecutor;

const INPUT_PROTO: &str = "syntax = \"proto3\"; message Args { string text = 1; }";

#[derive(Default)]
struct Calls {
    requests: Vec<JobRequest>,
    deleted: Vec<i64>,
}

struct FakeRpc {
    runner: RunnerData,
    job_id: i64,
    results: Option<Vec<JobResult>>,
    controlled_results: Option<ControlledResults>,
    calls: Arc<Mutex<Calls>>,
}

type ControlledReceiver = mpsc::UnboundedReceiver<Result<JobResult, Status>>;
type ControlledResults = Arc<Mutex<Option<(ControlledReceiver, tokio::sync::oneshot::Sender<()>)>>>;

impl FakeRpc {
    fn new(results: Option<Vec<JobResult>>) -> Self {
        let schemas = HashMap::from([(
            "run".to_owned(),
            MethodSchema {
                args_proto: INPUT_PROTO.to_owned(),
                result_proto: "syntax = \"proto3\"; message Reply { string answer = 1; }"
                    .to_owned(),
                ..Default::default()
            },
        )]);
        Self {
            runner: RunnerData {
                method_proto_map: Some(MethodProtoMap { schemas }),
                ..Default::default()
            },
            job_id: 42,
            results,
            controlled_results: None,
            calls: Arc::new(Mutex::new(Calls::default())),
        }
    }

    fn with_controlled_results(
        mut self,
    ) -> (
        Self,
        mpsc::UnboundedSender<Result<JobResult, Status>>,
        tokio::sync::oneshot::Receiver<()>,
    ) {
        let (sender, receiver) = mpsc::unbounded();
        let (observed_sender, observed_receiver) = tokio::sync::oneshot::channel();
        self.controlled_results = Some(Arc::new(Mutex::new(Some((receiver, observed_sender)))));
        (self, sender, observed_receiver)
    }
}

#[async_trait]
impl JobworkerpRpc for FakeRpc {
    async fn find_worker(&self, worker_id: i64) -> Result<Option<WorkerData>, Status> {
        assert_eq!(worker_id, 7);
        Ok(Some(WorkerData {
            runner_id: Some(RunnerId { value: 2 }),
            ..Default::default()
        }))
    }

    async fn find_runner(&self, runner_id: i64) -> Result<Option<RunnerData>, Status> {
        assert_eq!(runner_id, 2);
        Ok(Some(self.runner.clone()))
    }

    async fn enqueue(&self, _request: JobRequest) -> Result<CreateJobResponse, Status> {
        Err(Status::unimplemented("unary enqueue is not used"))
    }

    async fn enqueue_for_stream(
        &self,
        _request: JobRequest,
    ) -> Result<grpc::StreamEnqueueResponse, Status> {
        Err(Status::unimplemented("stream enqueue is not used"))
    }

    async fn enqueue_for_result(
        &self,
        request: JobRequest,
    ) -> Result<ResultEnqueueResponse, Status> {
        self.calls.lock().unwrap().requests.push(request);
        let controlled = self
            .controlled_results
            .as_ref()
            .and_then(|results| results.lock().unwrap().take());
        let result_stream: ResultStream = match controlled {
            Some((receiver, observed)) => Box::pin(stream::unfold(
                (receiver, Some(observed)),
                |(mut receiver, mut observed)| async move {
                    let result = receiver.next().await?;
                    if let Some(observed) = observed.take() {
                        let _ = observed.send(());
                    }
                    Some((result, (receiver, observed)))
                },
            )),
            None => match self.results.clone() {
                Some(results) => Box::pin(stream::iter(results.into_iter().map(Ok))),
                None => Box::pin(stream::pending()),
            },
        };
        Ok(ResultEnqueueResponse {
            job_id_header: Some(JobId { value: self.job_id }.encode_to_vec()),
            results: result_stream,
        })
    }

    async fn delete(&self, job_id: JobId) -> Result<(), Status> {
        self.calls.lock().unwrap().deleted.push(job_id.value);
        Ok(())
    }
}

fn success_result(job_id: i64, answer: &str) -> JobResult {
    JobResult {
        data: Some(JobResultData {
            job_id: Some(JobId { value: job_id }),
            worker_id: Some(WorkerId { value: 7 }),
            using: Some("run".to_owned()),
            status: ResultStatus::Success as i32,
            output: Some(ResultOutput {
                items: encoded_reply(answer),
            }),
            ..Default::default()
        }),
        ..Default::default()
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

fn tool() -> RegisteredTool {
    RegisteredTool {
        name: "lookup".to_owned(),
        description: "Look up a value".to_owned(),
        worker_id: 7,
        method: "run".to_owned(),
        requires_approval: false,
        schema_revision: INPUT_PROTO.to_owned(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
            "additionalProperties": false
        }),
    }
}

fn executor(rpc: &FakeRpc) -> GrpcWorkerExecutor {
    GrpcWorkerExecutor::with_rpc(Arc::new(FakeRpc {
        runner: rpc.runner.clone(),
        job_id: rpc.job_id,
        results: rpc.results.clone(),
        controlled_results: rpc.controlled_results.clone(),
        calls: rpc.calls.clone(),
    }))
}

#[tokio::test]
async fn start_returns_the_eager_job_id_without_waiting_for_the_result() {
    let rpc = FakeRpc::new(None);
    let executor = executor(&rpc);

    let job = tokio::time::timeout(
        Duration::from_secs(1),
        executor.start(&tool(), serde_json::json!({ "text": "hello" })),
    )
    .await
    .expect("start must not wait for the result stream")
    .unwrap();

    assert_eq!(job.job_id, "42");
    let calls = rpc.calls.lock().unwrap();
    let request = calls.requests.first().expect("enqueue request");
    assert_eq!(request.using.as_deref(), Some("run"));
    assert_eq!(
        request.worker,
        Some(job_request::Worker::WorkerId(WorkerId { value: 7 }))
    );
    assert_eq!(
        request
            .overrides
            .as_ref()
            .and_then(|overrides| overrides.response_type),
        Some(ResponseType::Direct as i32)
    );
}

#[tokio::test]
async fn wait_decodes_one_success_result_and_rejects_a_second_wait() {
    let rpc = FakeRpc::new(Some(vec![success_result(42, "done")]));
    let executor = executor(&rpc);
    let job = executor
        .start(&tool(), serde_json::json!({ "text": "hello" }))
        .await
        .unwrap();

    assert_eq!(
        executor.wait(&job.job_id).await.unwrap(),
        serde_json::json!({ "answer": "done" })
    );
    assert!(executor.wait(&job.job_id).await.is_err());
}

#[tokio::test]
async fn failed_job_status_is_not_returned_as_successful_output() {
    let mut failed = success_result(42, "runner failed");
    let data = failed.data.as_mut().unwrap();
    data.status = ResultStatus::FatalError as i32;
    data.output.as_mut().unwrap().items = b"runner failed".to_vec();
    let rpc = FakeRpc::new(Some(vec![failed]));
    let executor = executor(&rpc);
    let job = executor
        .start(&tool(), serde_json::json!({ "text": "hello" }))
        .await
        .unwrap();

    let error = executor.wait(&job.job_id).await.unwrap_err();
    assert!(error.to_string().contains("status"));
    assert!(error.to_string().contains("runner failed"));
    assert!(executor.cancel(&job.job_id).await.is_err());
    assert!(rpc.calls.lock().unwrap().deleted.is_empty());
}

#[tokio::test]
async fn cancellation_deletes_only_a_job_owned_by_this_executor() {
    let rpc = FakeRpc::new(None);
    let executor = executor(&rpc);
    let job = executor
        .start(&tool(), serde_json::json!({ "text": "hello" }))
        .await
        .unwrap();

    assert!(executor.cancel("999").await.is_err());
    assert!(rpc.calls.lock().unwrap().deleted.is_empty());

    executor.cancel(&job.job_id).await.unwrap();
    assert_eq!(rpc.calls.lock().unwrap().deleted, vec![42]);
}

#[tokio::test]
async fn invalid_proto_arguments_are_rejected_before_enqueue() {
    let rpc = FakeRpc::new(None);
    let executor = executor(&rpc);

    let error = executor
        .start(
            &tool(),
            serde_json::json!({ "text": "hello", "unexpected": true }),
        )
        .await
        .unwrap_err();

    assert!(matches!(&error, chat::WorkerError::Runtime(_)));
    assert!(error.to_string().contains("unexpected"));
    assert!(rpc.calls.lock().unwrap().requests.is_empty());
}

#[tokio::test]
async fn changed_method_schema_revision_is_rejected_before_enqueue() {
    let mut rpc = FakeRpc::new(None);
    rpc.runner
        .method_proto_map
        .as_mut()
        .unwrap()
        .schemas
        .get_mut("run")
        .unwrap()
        .args_proto = "syntax = \"proto3\"; message Args { string text = 2; }".to_owned();
    let executor = executor(&rpc);

    let error = executor
        .start(&tool(), serde_json::json!({ "text": "hello" }))
        .await
        .unwrap_err();

    assert!(matches!(&error, chat::WorkerError::StaleSchema));
    assert!(error.to_string().contains("schema revision"));
    assert!(rpc.calls.lock().unwrap().requests.is_empty());
}

#[tokio::test]
async fn oversized_changed_args_schema_is_stale_before_size_validation_or_enqueue() {
    let mut rpc = FakeRpc::new(None);
    rpc.runner
        .method_proto_map
        .as_mut()
        .unwrap()
        .schemas
        .get_mut("run")
        .unwrap()
        .args_proto = "x".repeat(512 * 1024 + 1);
    let executor = executor(&rpc);

    let error = executor
        .start(&tool(), serde_json::json!({ "text": "hello" }))
        .await
        .unwrap_err();

    assert!(matches!(error, chat::WorkerError::StaleSchema));
    assert!(rpc.calls.lock().unwrap().requests.is_empty());
}

#[tokio::test]
async fn changed_args_schema_and_client_stream_flag_is_stale_before_enqueue() {
    let mut rpc = FakeRpc::new(None);
    let schema = rpc
        .runner
        .method_proto_map
        .as_mut()
        .unwrap()
        .schemas
        .get_mut("run")
        .unwrap();
    schema.args_proto = "syntax = \"proto3\"; message Args { string text = 2; }".to_owned();
    schema.require_client_stream = true;
    let executor = executor(&rpc);

    let error = executor
        .start(&tool(), serde_json::json!({ "text": "hello" }))
        .await
        .unwrap_err();

    assert!(matches!(error, chat::WorkerError::StaleSchema));
    assert!(rpc.calls.lock().unwrap().requests.is_empty());
}

#[tokio::test]
async fn missing_method_schema_revision_is_rejected_before_enqueue() {
    let rpc = FakeRpc::new(None);
    let executor = executor(&rpc);
    let mut missing_revision = tool();
    missing_revision.schema_revision.clear();

    let error = executor
        .start(&missing_revision, serde_json::json!({ "text": "hello" }))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("schema revision"));
    assert!(rpc.calls.lock().unwrap().requests.is_empty());
}

#[tokio::test]
async fn unregistered_method_is_rejected_before_enqueue() {
    let rpc = FakeRpc::new(None);
    let executor = executor(&rpc);
    let mut unregistered = tool();
    unregistered.method = "execute".to_owned();

    let error = executor
        .start(&unregistered, serde_json::json!({ "text": "hello" }))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("not registered"));
    assert!(rpc.calls.lock().unwrap().requests.is_empty());
}

#[tokio::test]
async fn oversized_result_schema_is_rejected_before_enqueue() {
    let mut rpc = FakeRpc::new(None);
    rpc.runner
        .method_proto_map
        .as_mut()
        .unwrap()
        .schemas
        .get_mut("run")
        .unwrap()
        .result_proto = "x".repeat(512 * 1024 + 1);
    let executor = executor(&rpc);

    let error = executor
        .start(&tool(), serde_json::json!({ "text": "hello" }))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("schema size limit"));
    assert!(rpc.calls.lock().unwrap().requests.is_empty());
}

#[tokio::test]
async fn result_stream_must_match_its_job_and_worker_metadata() {
    let rpc = FakeRpc::new(Some(vec![success_result(41, "wrong job")]));
    let executor = executor(&rpc);
    let job = executor
        .start(&tool(), serde_json::json!({ "text": "hello" }))
        .await
        .unwrap();

    assert!(executor.wait(&job.job_id).await.is_err());
}

#[tokio::test]
async fn dropping_a_waiter_keeps_the_job_stream_for_a_later_wait() {
    let (rpc, sender, result_observed) = FakeRpc::new(None).with_controlled_results();
    let rpc_calls = rpc.calls.clone();
    let executor = Arc::new(executor(&rpc));
    let job = executor
        .start(&tool(), serde_json::json!({ "text": "hello" }))
        .await
        .unwrap();

    let waiter = {
        let executor = executor.clone();
        let job_id = job.job_id.clone();
        tokio::spawn(async move { executor.wait(&job_id).await })
    };
    sender
        .unbounded_send(Ok(success_result(42, "resumed")))
        .unwrap();
    result_observed.await.unwrap();
    waiter.abort();
    let _ = waiter.await;
    sender.close_channel();

    assert_eq!(
        executor.wait(&job.job_id).await.unwrap(),
        serde_json::json!({ "answer": "resumed" })
    );
    assert!(rpc_calls.lock().unwrap().deleted.is_empty());
}

#[tokio::test]
async fn result_stream_payload_is_bounded() {
    let mut oversized = success_result(42, "unused");
    oversized
        .data
        .as_mut()
        .unwrap()
        .output
        .as_mut()
        .unwrap()
        .items = vec![0; 8 * 1024 * 1024 + 1];
    let rpc = FakeRpc::new(Some(vec![oversized]));
    let executor = executor(&rpc);
    let job = executor
        .start(&tool(), serde_json::json!({ "text": "hello" }))
        .await
        .unwrap();

    let error = executor.wait(&job.job_id).await.unwrap_err();
    assert!(error.to_string().contains("limit"));
}

#[tokio::test]
async fn stream_with_no_final_result_fails_closed() {
    let rpc = FakeRpc::new(Some(Vec::new()));
    let executor = executor(&rpc);
    let job = executor
        .start(&tool(), serde_json::json!({ "text": "hello" }))
        .await
        .unwrap();

    assert!(executor.wait(&job.job_id).await.is_err());
    executor.cancel(&job.job_id).await.unwrap();
    assert_eq!(rpc.calls.lock().unwrap().deleted, vec![42]);
}

#[tokio::test]
async fn result_stream_transport_error_keeps_job_owned_for_delete_retry() {
    let (rpc, sender, result_observed) = FakeRpc::new(None).with_controlled_results();
    let calls = rpc.calls.clone();
    let executor = Arc::new(executor(&rpc));
    let job = executor
        .start(&tool(), serde_json::json!({ "text": "hello" }))
        .await
        .unwrap();
    let waiter = {
        let executor = executor.clone();
        let job_id = job.job_id.clone();
        tokio::spawn(async move { executor.wait(&job_id).await })
    };

    sender
        .unbounded_send(Err(Status::unavailable("result stream transport failed")))
        .unwrap();
    result_observed.await.unwrap();
    let error = waiter.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("transport failed"));

    executor.cancel(&job.job_id).await.unwrap();
    assert_eq!(calls.lock().unwrap().deleted, vec![42]);
}
