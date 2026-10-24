use std::{
    collections::HashMap,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use crate::chat::ModelJobObserver;
use crate::tool_registry::{ResolvedInputSchema, SchemaResolutionError, WorkerSchemaResolver};
use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use command_utils::protobuf::ProtobufDescriptor;
use futures::{Stream, StreamExt};
use prost::Message;
use prost_reflect::MessageDescriptor;
use proto::jobworkerp::data::{
    JobExecutionOverrides, JobId, JobResult, MethodSchema, ResponseType, ResultOutputItem,
    ResultStatus, RunnerData, WorkerData, result_output_item,
};
use serde_json::Value;
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tonic::{
    Status,
    transport::{Channel, Endpoint},
};

/// Generated service clients are separate from grpc-front's application server.
pub mod proto {
    pub mod jobworkerp {
        pub mod data {
            pub use ::proto::jobworkerp::data::*;
        }

        pub mod service {
            tonic::include_proto!("jobworkerp.service");
        }
    }
}

const JOB_ID_HEADER_NAME: &str = "x-job-id-bin";
const MAX_STREAM_CHUNKS: usize = 4096;
const STREAM_JOB_CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
const STREAM_CALLBACK_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const MAX_STREAM_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_METHOD_SCHEMA_BYTES: usize = 512 * 1024;

pub type ResultItemStream =
    Pin<Box<dyn Stream<Item = std::result::Result<ResultOutputItem, Status>> + Send + 'static>>;

/// The response from the streaming enqueue RPC, with its binary job ID header separated out.
pub struct StreamEnqueueResponse {
    pub job_id_header: Option<Vec<u8>>,
    pub items: ResultItemStream,
}

pub type ResultStream =
    Pin<Box<dyn Stream<Item = std::result::Result<JobResult, Status>> + Send + 'static>>;

/// The response from EnqueueForResult, retaining its eager ID header separately from results.
pub struct ResultEnqueueResponse {
    pub job_id_header: Option<Vec<u8>>,
    pub results: ResultStream,
}

/// Small RPC seam allowing the adapter to be tested without a jobworkerp process.
/// Enqueue responses are the only source of generated job IDs; if a response is lost after server
/// acceptance, this protocol has no reservation token or reconciliation query to recover the ID.
#[async_trait]
pub trait JobworkerpRpc: Send + Sync {
    async fn find_worker(&self, worker_id: i64) -> std::result::Result<Option<WorkerData>, Status>;
    async fn find_runner(&self, runner_id: i64) -> std::result::Result<Option<RunnerData>, Status>;
    async fn enqueue(
        &self,
        request: proto::jobworkerp::service::JobRequest,
    ) -> std::result::Result<proto::jobworkerp::service::CreateJobResponse, Status>;
    async fn enqueue_for_stream(
        &self,
        request: proto::jobworkerp::service::JobRequest,
    ) -> std::result::Result<StreamEnqueueResponse, Status>;
    async fn enqueue_for_result(
        &self,
        request: proto::jobworkerp::service::JobRequest,
    ) -> std::result::Result<ResultEnqueueResponse, Status>;
    async fn delete(&self, job_id: JobId) -> std::result::Result<(), Status>;
}

pub(crate) async fn connect_jobworkerp_rpc(
    endpoint: impl Into<String>,
) -> Result<Arc<dyn JobworkerpRpc>> {
    let endpoint = Endpoint::from_shared(endpoint.into())?;
    let channel = endpoint
        .connect()
        .await
        .context("connect to jobworkerp gRPC")?;
    Ok(Arc::new(TonicJobworkerpRpc { channel }))
}

/// An invocation requested by the chat/tool orchestrator.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolInvocation {
    pub worker_id: i64,
    /// Exact key from RunnerData.method_proto_map. `None` is accepted only for a sole `run` method.
    pub using: Option<String>,
    pub arguments: Value,
    pub stream: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputKind {
    Data,
    FinalCollected,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OutputChunk {
    pub kind: OutputKind,
    pub bytes: Vec<u8>,
    pub json: Option<Value>,
}

/// The normalized result returned to the agent loop.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolExecutionResult {
    pub worker_id: i64,
    pub using: String,
    pub job_id: i64,
    /// Present for unary enqueue; streamed output is represented in `chunks`.
    pub result: Option<Value>,
    pub raw_result: Option<Vec<u8>>,
    pub chunks: Vec<OutputChunk>,
}

/// Observes a decoded DATA chunk synchronously while a streamed RPC is being consumed.
/// Implementations must return promptly and must not block waiting for downstream delivery.
pub type StreamChunkObserver =
    Arc<dyn Fn(&OutputChunk) -> std::result::Result<(), String> + Send + Sync + 'static>;

/// Narrow execution contract consumed by the chat orchestrator and replaceable in its tests.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(&self, invocation: ToolInvocation) -> Result<ToolExecutionResult>;

    /// Executes with an optional lifecycle observer and a synchronous per-DATA callback.
    /// The default preserves compatibility for test and third-party executors, observing chunks
    /// after execution; streaming executors should override this to report each decoded DATA item
    /// before waiting for the rest of the stream.
    async fn execute_with_observers(
        &self,
        invocation: ToolInvocation,
        job_observer: Option<Arc<dyn ModelJobObserver>>,
        chunk_observer: Option<StreamChunkObserver>,
    ) -> Result<ToolExecutionResult> {
        let result = if let Some(observer) = job_observer {
            self.execute_with_observer(invocation, observer).await?
        } else {
            self.execute(invocation).await?
        };
        if let Some(observer) = chunk_observer {
            for chunk in &result.chunks {
                if chunk.kind == OutputKind::Data {
                    observer(chunk).map_err(anyhow::Error::msg)?;
                }
            }
        }
        Ok(result)
    }

    async fn execute_with_stream_observer(
        &self,
        invocation: ToolInvocation,
        observer: StreamChunkObserver,
    ) -> Result<ToolExecutionResult> {
        self.execute_with_observers(invocation, None, Some(observer))
            .await
    }

    async fn execute_with_observer(
        &self,
        invocation: ToolInvocation,
        observer: Arc<dyn ModelJobObserver>,
    ) -> Result<ToolExecutionResult> {
        let result = self.execute(invocation).await?;
        observer
            .job_started(result.job_id)
            .await
            .map_err(anyhow::Error::msg)?;
        observer.job_finished(result.job_id);
        Ok(result)
    }

    async fn cancel_job(&self, _job_id: &str) -> std::result::Result<(), String> {
        Err("tool executor does not support model job cancellation".to_owned())
    }
}

pub struct GrpcToolExecutor {
    rpc: Arc<dyn JobworkerpRpc>,
    stream_jobs: Mutex<HashMap<i64, Arc<OwnedStreamJob>>>,
}

struct OwnedStreamJob {
    cancellation: Arc<Notify>,
    delete_lock: AsyncMutex<()>,
}

impl GrpcToolExecutor {
    pub fn with_rpc(rpc: Arc<dyn JobworkerpRpc>) -> Self {
        Self {
            rpc,
            stream_jobs: Mutex::new(HashMap::new()),
        }
    }

    pub async fn connect(endpoint: impl Into<String>) -> Result<Self> {
        Ok(Self::with_rpc(connect_jobworkerp_rpc(endpoint).await?))
    }
}

#[async_trait]
impl ToolExecutor for GrpcToolExecutor {
    async fn execute(&self, invocation: ToolInvocation) -> Result<ToolExecutionResult> {
        self.execute_inner(invocation, None, None).await
    }

    async fn execute_with_observer(
        &self,
        invocation: ToolInvocation,
        observer: Arc<dyn ModelJobObserver>,
    ) -> Result<ToolExecutionResult> {
        self.execute_inner(invocation, Some(observer), None).await
    }

    async fn execute_with_observers(
        &self,
        invocation: ToolInvocation,
        job_observer: Option<Arc<dyn ModelJobObserver>>,
        chunk_observer: Option<StreamChunkObserver>,
    ) -> Result<ToolExecutionResult> {
        self.execute_inner(invocation, job_observer, chunk_observer)
            .await
    }

    async fn cancel_job(&self, job_id: &str) -> std::result::Result<(), String> {
        let parsed_job_id = job_id
            .parse::<i64>()
            .map_err(|_| "model job ID is malformed".to_owned())?;
        if parsed_job_id <= 0 || parsed_job_id.to_string() != job_id {
            return Err("model job ID is malformed".to_owned());
        }
        let job_id = parsed_job_id;
        let owned_job = self
            .stream_jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&job_id)
            .cloned()
            .ok_or_else(|| "model job ID is not owned by this tool executor".to_owned())?;

        let _delete_guard = owned_job.delete_lock.lock().await;
        if !self.is_same_owned_stream_job(job_id, &owned_job) {
            return Err("model job ID is not owned by this tool executor".to_owned());
        }
        self.rpc
            .delete(JobId { value: job_id })
            .await
            .map_err(|error| format!("cancel owned model job: {error}"))?;
        let removed = self
            .stream_jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&job_id);
        if removed.is_some_and(|current| Arc::ptr_eq(&current, &owned_job)) {
            owned_job.cancellation.notify_one();
        }
        Ok(())
    }
}

impl GrpcToolExecutor {
    fn release_stream_job(&self, job_id: i64) {
        let _ = self
            .stream_jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&job_id);
    }

    fn is_same_owned_stream_job(&self, job_id: i64, expected: &Arc<OwnedStreamJob>) -> bool {
        self.stream_jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&job_id)
            .is_some_and(|current| Arc::ptr_eq(current, expected))
    }

    async fn cleanup_rejected_stream_job(
        &self,
        job_id: i64,
        observer: &dyn ModelJobObserver,
    ) -> std::result::Result<(), String> {
        if !self
            .stream_jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&job_id)
        {
            observer.job_finished(job_id);
            return Ok(());
        }

        match tokio::time::timeout(
            STREAM_JOB_CLEANUP_TIMEOUT,
            self.cancel_job(&job_id.to_string()),
        )
        .await
        {
            Ok(Ok(())) => {
                observer.job_finished(job_id);
                Ok(())
            }
            Ok(Err(_)) if !self.is_stream_job_owned(job_id) => {
                observer.job_finished(job_id);
                Ok(())
            }
            Ok(Err(error)) => Err(format!(
                "best-effort Delete failed and job ownership was retained: {error}"
            )),
            Err(_) if !self.is_stream_job_owned(job_id) => {
                observer.job_finished(job_id);
                Ok(())
            }
            Err(_) => Err(
                "best-effort Delete exceeded its time limit; job ownership was retained".to_owned(),
            ),
        }
    }

    fn is_stream_job_owned(&self, job_id: i64) -> bool {
        self.stream_jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&job_id)
    }

    async fn execute_inner(
        &self,
        invocation: ToolInvocation,
        observer: Option<Arc<dyn ModelJobObserver>>,
        chunk_observer: Option<StreamChunkObserver>,
    ) -> Result<ToolExecutionResult> {
        let worker = self
            .rpc
            .find_worker(invocation.worker_id)
            .await
            .context("find worker by ID")?
            .ok_or_else(|| anyhow!("worker {} was not found", invocation.worker_id))?;
        let runner_id = worker
            .runner_id
            .as_ref()
            .map(|id| id.value)
            .ok_or_else(|| anyhow!("worker {} has no runner ID", invocation.worker_id))?;
        let runner = self
            .rpc
            .find_runner(runner_id)
            .await
            .context("find worker runner by ID")?
            .ok_or_else(|| anyhow!("runner {runner_id} was not found"))?;

        let (using, method_schema) = resolve_method(&runner, invocation.using.as_deref())?;
        if method_schema.require_client_stream {
            bail!(
                "method '{using}' requires client streaming, which this adapter does not support"
            );
        }

        if method_schema.args_proto.len() > MAX_METHOD_SCHEMA_BYTES
            || method_schema.result_proto.len() > MAX_METHOD_SCHEMA_BYTES
        {
            bail!("worker method schema size limit exceeded");
        }

        let args = encode_arguments(&method_schema, &invocation.arguments)?;
        let result_descriptor = compile_result_descriptor(&method_schema)?;
        let request = proto::jobworkerp::service::JobRequest {
            worker: Some(proto::jobworkerp::service::job_request::Worker::WorkerId(
                proto::jobworkerp::data::WorkerId {
                    value: invocation.worker_id,
                },
            )),
            args,
            using: Some(using.clone()),
            overrides: Some(JobExecutionOverrides {
                response_type: Some(ResponseType::Direct as i32),
                ..Default::default()
            }),
            ..Default::default()
        };

        if invocation.stream {
            // A lost enqueue response can hide a server-created job; the current protocol has no
            // reservation token or reconciliation query to recover its ID safely.
            let response = self.rpc.enqueue_for_stream(request).await?;
            let job_id = decode_stream_job_id(response.job_id_header)?;
            let owned_job = Arc::new(OwnedStreamJob {
                cancellation: Arc::new(Notify::new()),
                delete_lock: AsyncMutex::new(()),
            });
            {
                let mut stream_jobs = self
                    .stream_jobs
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if stream_jobs.contains_key(&job_id) {
                    bail!("EnqueueForStream reused a job ID already owned by this adapter");
                }
                stream_jobs.insert(job_id, owned_job.clone());
            }
            if let Some(observer) = observer.as_ref()
                && let Err(error) = observer.job_started(job_id).await
            {
                let cleanup = self
                    .cleanup_rejected_stream_job(job_id, observer.as_ref())
                    .await;
                return match cleanup {
                    Ok(()) => Err(anyhow!("model job observer rejected job {job_id}: {error}")),
                    Err(cleanup_error) => Err(anyhow!(
                        "model job observer rejected job {job_id}: {error}; {cleanup_error}; retry cancellation with job ID {job_id}"
                    )),
                };
            }
            let result = collect_stream_result(
                invocation.worker_id,
                using,
                job_id,
                response.items,
                owned_job.cancellation.clone(),
                result_descriptor.as_ref(),
                chunk_observer.as_ref(),
            )
            .await;
            match result {
                Ok(StreamCollectionOutcome::Complete(result)) => {
                    self.release_stream_job(job_id);
                    if let Some(observer) = observer {
                        observer.job_finished(job_id);
                    }
                    Ok(result)
                }
                Ok(StreamCollectionOutcome::CallbackFailed {
                    error,
                    terminal: true,
                }) => {
                    self.release_stream_job(job_id);
                    if let Some(observer) = observer {
                        observer.job_finished(job_id);
                    }
                    Err(error)
                }
                Ok(StreamCollectionOutcome::CallbackFailed {
                    error,
                    terminal: false,
                })
                | Err(error) => Err(anyhow!(
                    "{error:#}; stream result failed for model job {job_id}; job ownership was retained"
                )),
            }
        } else {
            let response = self.rpc.enqueue(request).await?;
            collect_direct_result(
                invocation.worker_id,
                using,
                response,
                result_descriptor.as_ref(),
            )
        }
    }
}

#[async_trait]
impl WorkerSchemaResolver for GrpcToolExecutor {
    async fn resolve_input_schema(
        &self,
        worker_id: i64,
        using: &str,
    ) -> std::result::Result<ResolvedInputSchema, SchemaResolutionError> {
        let worker = self
            .rpc
            .find_worker(worker_id)
            .await
            .map_err(|error| SchemaResolutionError::Unavailable(error.to_string()))?
            .ok_or(SchemaResolutionError::WorkerNotFound)?;
        let runner_id = worker
            .runner_id
            .ok_or(SchemaResolutionError::WorkerNotFound)?
            .value;
        let runner = self
            .rpc
            .find_runner(runner_id)
            .await
            .map_err(|error| SchemaResolutionError::Unavailable(error.to_string()))?
            .ok_or(SchemaResolutionError::WorkerNotFound)?;
        let (_, method) = resolve_method(&runner, Some(using))
            .map_err(|_| SchemaResolutionError::MethodNotFound)?;
        if method.require_client_stream {
            return Err(SchemaResolutionError::MethodNotFound);
        }
        if method.args_proto.is_empty()
            || method.args_proto.len() > MAX_METHOD_SCHEMA_BYTES
            || method.result_proto.len() > MAX_METHOD_SCHEMA_BYTES
        {
            return Err(SchemaResolutionError::SchemaNotFound);
        }
        let descriptor = ProtobufDescriptor::new(&method.args_proto)
            .map_err(|error| SchemaResolutionError::Unavailable(error.to_string()))?;
        let message = descriptor
            .get_messages()
            .into_iter()
            .next()
            .ok_or(SchemaResolutionError::SchemaNotFound)?;
        let schema = ProtobufDescriptor::message_descriptor_to_json_schema(&message);
        Ok(ResolvedInputSchema {
            schema,
            revision: method.args_proto,
        })
    }
}

/// Resolve only an exact registered method. Implicit dispatch is intentionally limited to `run`.
pub fn resolve_method(runner: &RunnerData, using: Option<&str>) -> Result<(String, MethodSchema)> {
    let schemas = &runner
        .method_proto_map
        .as_ref()
        .ok_or_else(|| anyhow!("runner is missing method_proto_map"))?
        .schemas;
    if schemas.is_empty() {
        bail!("runner method_proto_map has no methods");
    }

    let method_name = match using {
        Some("") => bail!("using method must not be empty"),
        Some(name) => {
            if schemas.contains_key(name) {
                name
            } else if name == ::proto::DEFAULT_METHOD_NAME {
                bail!("default method '{name}' does not match any registered runner method");
            } else {
                bail!("using method '{name}' is not registered on the runner");
            }
        }
        None if schemas.len() > 1 => {
            bail!("runner method selection is ambiguous; an exact using method is required")
        }
        None if schemas.contains_key(::proto::DEFAULT_METHOD_NAME) => ::proto::DEFAULT_METHOD_NAME,
        None => {
            let only_method = schemas.keys().next().expect("nonempty method map");
            bail!(
                "using is required because the sole method '{only_method}' is not the default method"
            )
        }
    };

    let method_schema = schemas
        .get(method_name)
        .cloned()
        .ok_or_else(|| anyhow!("using method '{method_name}' disappeared from method map"))?;
    Ok((method_name.to_owned(), method_schema))
}

pub(crate) fn encode_arguments(schema: &MethodSchema, arguments: &Value) -> Result<Vec<u8>> {
    if schema.args_proto.trim().is_empty() {
        bail!("method arguments schema args_proto is missing");
    }
    let descriptor =
        ProtobufDescriptor::new(&schema.args_proto).context("compile method args_proto")?;
    let message = descriptor
        .get_messages()
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("method args_proto defines no protobuf message"))?;
    let json = serde_json::to_string(arguments).context("serialize tool arguments as JSON")?;
    ProtobufDescriptor::json_to_message(message, &json, false)
        .context("convert tool arguments to protobuf bytes")
}

pub(crate) fn compile_result_descriptor(
    schema: &MethodSchema,
) -> Result<Option<MessageDescriptor>> {
    if schema.result_proto.trim().is_empty() {
        return Ok(None);
    }
    let descriptor =
        ProtobufDescriptor::new(&schema.result_proto).context("compile method result_proto")?;
    descriptor
        .get_messages()
        .into_iter()
        .next()
        .map(Some)
        .ok_or_else(|| anyhow!("method result_proto defines no protobuf message"))
}

pub(crate) fn decode_result(
    descriptor: Option<&MessageDescriptor>,
    bytes: &[u8],
) -> Result<Option<Value>> {
    let Some(descriptor) = descriptor else {
        return Ok(None);
    };
    let message = ProtobufDescriptor::get_message_from_bytes(descriptor.clone(), bytes)
        .context("decode worker result protobuf")?;
    ProtobufDescriptor::message_to_json_value(&message)
        .context("serialize worker result as JSON")
        .map(Some)
}

fn collect_direct_result(
    worker_id: i64,
    using: String,
    response: proto::jobworkerp::service::CreateJobResponse,
    result_descriptor: Option<&MessageDescriptor>,
) -> Result<ToolExecutionResult> {
    let job_id = response
        .id
        .ok_or_else(|| anyhow!("enqueue response is missing job ID"))?
        .value;
    let result = response
        .result
        .ok_or_else(|| anyhow!("job {job_id} completed without a direct result"))?;
    let data = result
        .data
        .ok_or_else(|| anyhow!("job {job_id} result is missing result data"))?;
    let output = data.output.map(|output| output.items);
    if data.status != ResultStatus::Success as i32 {
        let detail = output
            .as_deref()
            .map(String::from_utf8_lossy)
            .unwrap_or_default();
        bail!("job {job_id} failed with status {}: {detail}", data.status);
    }
    let output =
        output.ok_or_else(|| anyhow!("job {job_id} successful result is missing output bytes"))?;
    let decoded = decode_result(result_descriptor, &output)?;
    Ok(ToolExecutionResult {
        worker_id,
        using,
        job_id,
        result: decoded,
        raw_result: Some(output),
        chunks: Vec::new(),
    })
}

enum StreamCollectionOutcome {
    Complete(ToolExecutionResult),
    CallbackFailed {
        error: anyhow::Error,
        terminal: bool,
    },
}

fn collect_stream_chunk(
    bytes: Vec<u8>,
    kind: OutputKind,
    result_descriptor: Option<&MessageDescriptor>,
    chunk_count: &mut usize,
    total_bytes: &mut usize,
) -> Result<OutputChunk> {
    if *chunk_count >= MAX_STREAM_CHUNKS
        || bytes.len() > MAX_STREAM_BYTES.saturating_sub(*total_bytes)
    {
        bail!("worker stream output limit exceeded");
    }
    let json = decode_result(result_descriptor, &bytes)?;
    *chunk_count += 1;
    *total_bytes += bytes.len();
    Ok(OutputChunk { kind, bytes, json })
}

async fn collect_stream_result(
    worker_id: i64,
    using: String,
    job_id: i64,
    mut stream: ResultItemStream,
    cancellation: Arc<Notify>,
    result_descriptor: Option<&MessageDescriptor>,
    observer: Option<&StreamChunkObserver>,
) -> Result<StreamCollectionOutcome> {
    let mut chunks = Vec::new();
    let mut total_bytes = 0usize;
    let mut chunk_count = 0usize;
    let mut saw_end = false;
    let mut callback_error: Option<anyhow::Error> = None;
    let mut drain_deadline = None;

    loop {
        let item = if let Some(deadline) = drain_deadline {
            tokio::select! {
                _ = cancellation.notified() => {
                    return Ok(StreamCollectionOutcome::CallbackFailed {
                        error: callback_error.take().expect("drain deadline requires callback error"),
                        terminal: false,
                    });
                }
                item = tokio::time::timeout_at(deadline, stream.next()) => match item {
                    Ok(item) => item,
                    Err(_) => {
                        return Ok(StreamCollectionOutcome::CallbackFailed {
                            error: callback_error.take().expect("drain deadline requires callback error"),
                            terminal: false,
                        });
                    }
                },
            }
        } else {
            tokio::select! {
                _ = cancellation.notified() => bail!("job {job_id} was cancelled"),
                item = stream.next() => item,
            }
        };
        let Some(item) = item else {
            break;
        };
        let item = match item.context("receive streaming worker result") {
            Ok(item) => item,
            Err(error) => {
                return stream_collection_error(callback_error.take(), error);
            }
        };
        if saw_end {
            return stream_collection_error(
                callback_error.take(),
                anyhow!("worker result stream contained an item after its terminal end marker"),
            );
        }
        match item.item {
            Some(result_output_item::Item::Data(bytes)) => {
                let chunk = match collect_stream_chunk(
                    bytes,
                    OutputKind::Data,
                    result_descriptor,
                    &mut chunk_count,
                    &mut total_bytes,
                ) {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        return stream_collection_error(callback_error.take(), error);
                    }
                };
                if callback_error.is_none() {
                    if let Some(observer) = observer {
                        if let Err(error) = observer(&chunk) {
                            callback_error = Some(anyhow!(
                                "stream chunk observer rejected DATA chunk: {error}"
                            ));
                            drain_deadline =
                                Some(tokio::time::Instant::now() + STREAM_CALLBACK_DRAIN_TIMEOUT);
                            chunks.clear();
                        } else {
                            chunks.push(chunk);
                        }
                    } else {
                        chunks.push(chunk);
                    }
                }
            }
            Some(result_output_item::Item::FinalCollected(bytes)) => {
                let chunk = match collect_stream_chunk(
                    bytes,
                    OutputKind::FinalCollected,
                    result_descriptor,
                    &mut chunk_count,
                    &mut total_bytes,
                ) {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        return stream_collection_error(callback_error.take(), error);
                    }
                };
                if callback_error.is_none() {
                    chunks.push(chunk);
                }
            }
            Some(result_output_item::Item::End(_)) => saw_end = true,
            None => {
                return stream_collection_error(
                    callback_error.take(),
                    anyhow!("worker result stream contained an item without a value"),
                );
            }
        }
    }
    if !saw_end {
        if let Some(error) = callback_error {
            return Ok(StreamCollectionOutcome::CallbackFailed {
                error,
                terminal: false,
            });
        }
        bail!("worker result stream ended without a terminal end marker");
    }
    if let Some(error) = callback_error {
        return Ok(StreamCollectionOutcome::CallbackFailed {
            error,
            terminal: true,
        });
    }

    Ok(StreamCollectionOutcome::Complete(ToolExecutionResult {
        worker_id,
        using,
        job_id,
        result: None,
        raw_result: None,
        chunks,
    }))
}

fn stream_collection_error(
    callback_error: Option<anyhow::Error>,
    stream_error: anyhow::Error,
) -> Result<StreamCollectionOutcome> {
    match callback_error {
        Some(error) => Ok(StreamCollectionOutcome::CallbackFailed {
            error,
            terminal: false,
        }),
        None => Err(stream_error),
    }
}

fn decode_stream_job_id(header: Option<Vec<u8>>) -> Result<i64> {
    let header =
        header.ok_or_else(|| anyhow!("stream enqueue response is missing x-job-id-bin job ID"))?;
    let job_id = JobId::decode(header.as_slice())
        .context("decode x-job-id-bin job ID")?
        .value;
    if job_id <= 0 {
        bail!("stream enqueue returned an invalid job ID");
    }
    Ok(job_id)
}

struct TonicJobworkerpRpc {
    channel: Channel,
}

#[async_trait]
impl JobworkerpRpc for TonicJobworkerpRpc {
    async fn find_worker(&self, worker_id: i64) -> std::result::Result<Option<WorkerData>, Status> {
        use proto::jobworkerp::service::worker_service_client::WorkerServiceClient;

        let response = WorkerServiceClient::new(self.channel.clone())
            .find(proto::jobworkerp::data::WorkerId { value: worker_id })
            .await?
            .into_inner();
        let Some(worker) = response.data else {
            return Ok(None);
        };
        if worker.id.as_ref().map(|id| id.value) != Some(worker_id) {
            return Err(Status::data_loss(
                "WorkerService.Find returned a different worker ID",
            ));
        }
        Ok(worker.data)
    }

    async fn find_runner(&self, runner_id: i64) -> std::result::Result<Option<RunnerData>, Status> {
        use proto::jobworkerp::service::runner_service_client::RunnerServiceClient;

        let response = RunnerServiceClient::new(self.channel.clone())
            .find(proto::jobworkerp::data::RunnerId { value: runner_id })
            .await?
            .into_inner();
        let Some(runner) = response.data else {
            return Ok(None);
        };
        if runner.id.as_ref().map(|id| id.value) != Some(runner_id) {
            return Err(Status::data_loss(
                "RunnerService.Find returned a different runner ID",
            ));
        }
        Ok(runner.data)
    }

    async fn enqueue(
        &self,
        request: proto::jobworkerp::service::JobRequest,
    ) -> std::result::Result<proto::jobworkerp::service::CreateJobResponse, Status> {
        use proto::jobworkerp::service::job_service_client::JobServiceClient;

        JobServiceClient::new(self.channel.clone())
            .enqueue(request)
            .await
            .map(tonic::Response::into_inner)
    }

    async fn enqueue_for_stream(
        &self,
        request: proto::jobworkerp::service::JobRequest,
    ) -> std::result::Result<StreamEnqueueResponse, Status> {
        use proto::jobworkerp::service::job_service_client::JobServiceClient;

        let response = JobServiceClient::new(self.channel.clone())
            .enqueue_for_stream(request)
            .await?;
        let job_id_header = binary_job_id_header(response.metadata())?;
        Ok(StreamEnqueueResponse {
            job_id_header,
            items: Box::pin(response.into_inner()),
        })
    }

    async fn enqueue_for_result(
        &self,
        request: proto::jobworkerp::service::JobRequest,
    ) -> std::result::Result<ResultEnqueueResponse, Status> {
        use proto::jobworkerp::service::job_service_client::JobServiceClient;

        let response = JobServiceClient::new(self.channel.clone())
            .enqueue_for_result(request)
            .await?;
        let job_id_header = binary_job_id_header(response.metadata())?;
        Ok(ResultEnqueueResponse {
            job_id_header,
            results: Box::pin(response.into_inner()),
        })
    }

    async fn delete(&self, job_id: JobId) -> std::result::Result<(), Status> {
        use proto::jobworkerp::service::job_service_client::JobServiceClient;

        let response = JobServiceClient::new(self.channel.clone())
            .delete(job_id)
            .await?
            .into_inner();
        if !response.is_success {
            return Err(Status::failed_precondition(
                "jobworkerp did not cancel the job",
            ));
        }
        Ok(())
    }
}

fn binary_job_id_header(
    metadata: &tonic::metadata::MetadataMap,
) -> std::result::Result<Option<Vec<u8>>, Status> {
    metadata
        .get_bin(JOB_ID_HEADER_NAME)
        .map(|value| {
            value
                .to_bytes()
                .map(|bytes| bytes.to_vec())
                .map_err(|error| {
                    Status::internal(format!("invalid x-job-id-bin metadata: {error}"))
                })
        })
        .transpose()
}
