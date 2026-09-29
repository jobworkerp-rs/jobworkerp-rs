//! Production WorkerExecutor backed by JobService.EnqueueForResult.

use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use futures::StreamExt;
use prost::Message;
use prost_reflect::MessageDescriptor;

use crate::chat::{RegisteredTool, WorkerError, WorkerExecutor, WorkerJob, WorkerWaitError};
use crate::grpc::{
    JobworkerpRpc, MAX_METHOD_SCHEMA_BYTES, MAX_STREAM_BYTES, ResultStream,
    compile_result_descriptor, decode_result, encode_arguments, proto, resolve_method,
};
use proto::jobworkerp::data::{JobExecutionOverrides, JobId, ResponseType, ResultStatus};

/// Executes only targets projected from the current ToolRegistry record.
///
/// The result streams are process-local: the EnqueueForResult stream must remain attached to the
/// instance which received it. HTTP/SSE connection lifetime is deliberately unrelated to this
/// owner, so disconnecting a client does not cancel or discard its jobs.
pub struct GrpcWorkerExecutor {
    rpc: Arc<dyn JobworkerpRpc>,
    jobs: Mutex<HashMap<i64, Arc<OwnedJob>>>,
}

struct OwnedJob {
    worker_id: i64,
    method: String,
    result_descriptor: MessageDescriptor,
    results: tokio::sync::Mutex<ResultStream>,
    pending_result: Mutex<Option<Arc<proto::jobworkerp::data::JobResult>>>,
    wait_lock: tokio::sync::Mutex<()>,
    cancel_lock: tokio::sync::Mutex<()>,
    consumed: AtomicBool,
    cancelled: AtomicBool,
}

impl GrpcWorkerExecutor {
    pub fn with_rpc(rpc: Arc<dyn JobworkerpRpc>) -> Self {
        Self {
            rpc,
            jobs: Mutex::new(HashMap::new()),
        }
    }

    pub async fn connect(endpoint: impl Into<String>) -> Result<Self> {
        Ok(Self::with_rpc(
            crate::grpc::connect_jobworkerp_rpc(endpoint).await?,
        ))
    }

    async fn start_inner(
        &self,
        tool: &RegisteredTool,
        arguments: serde_json::Value,
    ) -> std::result::Result<WorkerJob, WorkerError> {
        if tool.worker_id <= 0 {
            return Err(WorkerError::Runtime(
                "registered tool has an invalid Worker ID".to_owned(),
            ));
        }
        if tool.method.trim().is_empty() {
            return Err(WorkerError::Runtime(
                "registered tool has an empty method".to_owned(),
            ));
        }
        if tool.schema_revision.trim().is_empty() {
            return Err(WorkerError::StaleSchema);
        }

        let worker = self
            .rpc
            .find_worker(tool.worker_id)
            .await
            .context("find registered Worker")?
            .ok_or_else(|| anyhow!("registered Worker {} was not found", tool.worker_id))?;
        let runner_id = worker
            .runner_id
            .as_ref()
            .map(|id| id.value)
            .filter(|id| *id > 0)
            .ok_or_else(|| {
                anyhow!(
                    "registered Worker {} has no valid runner ID",
                    tool.worker_id
                )
            })?;
        let runner = self
            .rpc
            .find_runner(runner_id)
            .await
            .context("find registered Worker's runner")?
            .ok_or_else(|| anyhow!("runner {runner_id} was not found"))?;

        let (method, method_schema) = resolve_method(&runner, Some(&tool.method))
            .context("resolve exact registered Worker method")?;
        if method_schema.args_proto != tool.schema_revision {
            return Err(WorkerError::StaleSchema);
        }
        if method_schema.require_client_stream {
            return Err(WorkerError::Runtime(format!(
                "registered method '{method}' requires client streaming"
            )));
        }
        if method_schema.args_proto.len() > MAX_METHOD_SCHEMA_BYTES
            || method_schema.result_proto.len() > MAX_METHOD_SCHEMA_BYTES
        {
            return Err(WorkerError::Runtime(
                "Worker method schema size limit exceeded".to_owned(),
            ));
        }

        let args = encode_arguments(&method_schema, &arguments)
            .context("validate and encode Worker arguments")?;
        let result_descriptor = compile_result_descriptor(&method_schema)
            .context("compile Worker result schema")?
            .ok_or_else(|| anyhow!("Worker result schema is missing"))?;

        let request = proto::jobworkerp::service::JobRequest {
            worker: Some(proto::jobworkerp::service::job_request::Worker::WorkerId(
                proto::jobworkerp::data::WorkerId {
                    value: tool.worker_id,
                },
            )),
            args,
            using: Some(method.clone()),
            overrides: Some(JobExecutionOverrides {
                response_type: Some(ResponseType::Direct as i32),
                ..Default::default()
            }),
            ..Default::default()
        };
        let response = self
            .rpc
            .enqueue_for_result(request)
            .await
            .context("enqueue Worker for direct result")?;
        let id_bytes = response
            .job_id_header
            .ok_or_else(|| anyhow!("EnqueueForResult response is missing x-job-id-bin"))?;
        let job_id = JobId::decode(id_bytes.as_slice()).context("decode x-job-id-bin")?;
        if job_id.value <= 0 {
            return Err(WorkerError::Runtime(
                "EnqueueForResult returned an invalid job ID".to_owned(),
            ));
        }

        let owned = Arc::new(OwnedJob {
            worker_id: tool.worker_id,
            method,
            result_descriptor,
            results: tokio::sync::Mutex::new(response.results),
            pending_result: Mutex::new(None),
            wait_lock: tokio::sync::Mutex::new(()),
            cancel_lock: tokio::sync::Mutex::new(()),
            consumed: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
        });
        let mut jobs = self
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if jobs.contains_key(&job_id.value) {
            return Err(WorkerError::Runtime(
                "EnqueueForResult reused a job ID already owned by this adapter".to_owned(),
            ));
        }
        jobs.insert(job_id.value, owned);

        Ok(WorkerJob {
            job_id: job_id.value.to_string(),
        })
    }

    fn owned_job(&self, job_id: i64) -> Option<Arc<OwnedJob>> {
        self.jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&job_id)
            .cloned()
    }

    fn remove_if_same(&self, job_id: i64, expected: &Arc<OwnedJob>) {
        let mut jobs = self
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if jobs
            .get(&job_id)
            .is_some_and(|current| Arc::ptr_eq(current, expected))
        {
            jobs.remove(&job_id);
        }
    }
}

#[async_trait]
impl WorkerExecutor for GrpcWorkerExecutor {
    async fn start(
        &self,
        tool: &RegisteredTool,
        arguments: serde_json::Value,
    ) -> std::result::Result<WorkerJob, WorkerError> {
        self.start_inner(tool, arguments).await
    }

    async fn wait(&self, job_id: &str) -> std::result::Result<serde_json::Value, WorkerError> {
        self.wait_with_ownership(job_id)
            .await
            .map_err(|error| match error {
                WorkerWaitError::Terminal(error) | WorkerWaitError::Uncertain(error) => error,
            })
    }

    async fn wait_with_ownership(
        &self,
        job_id: &str,
    ) -> std::result::Result<serde_json::Value, WorkerWaitError> {
        let job_id = parse_job_id(job_id).map_err(WorkerWaitError::Uncertain)?;
        let owned = self.owned_job(job_id).ok_or_else(|| {
            WorkerWaitError::Uncertain(WorkerError(
                "job ID is not owned by this adapter".to_owned(),
            ))
        })?;
        let _wait_guard = owned.wait_lock.lock().await;
        if owned.consumed.load(Ordering::Acquire) {
            return Err(WorkerWaitError::Terminal(WorkerError(
                "job result was already consumed".to_owned(),
            )));
        }

        let mut stream = owned.results.lock().await;
        let result = receive_result(job_id, &owned, &mut stream).await;
        match result {
            Ok(content) => {
                owned.consumed.store(true, Ordering::Release);
                drop(stream);
                drop(_wait_guard);
                self.remove_if_same(job_id, &owned);
                Ok(content)
            }
            Err(error) if error.definitive => {
                owned.consumed.store(true, Ordering::Release);
                drop(stream);
                drop(_wait_guard);
                self.remove_if_same(job_id, &owned);
                Err(WorkerWaitError::Terminal(WorkerError(format!(
                    "{:#}",
                    error.error
                ))))
            }
            Err(error) => Err(WorkerWaitError::Uncertain(WorkerError(format!(
                "{:#}",
                error.error
            )))),
        }
    }

    async fn cancel(&self, job_id: &str) -> std::result::Result<(), WorkerError> {
        let job_id = parse_job_id(job_id)?;
        let owned = self
            .owned_job(job_id)
            .ok_or_else(|| WorkerError("job ID is not owned by this adapter".to_owned()))?;
        let _cancel_guard = owned.cancel_lock.lock().await;
        if owned.cancelled.load(Ordering::Acquire) {
            return Err(WorkerError("job was already cancelled".to_owned()));
        }
        if owned.consumed.load(Ordering::Acquire) {
            return Err(WorkerError("job result was already consumed".to_owned()));
        }

        self.rpc
            .delete(JobId { value: job_id })
            .await
            .context("cancel owned Worker job")
            .map_err(|error| WorkerError(format!("{error:#}")))?;
        owned.cancelled.store(true, Ordering::Release);
        self.remove_if_same(job_id, &owned);
        Ok(())
    }
}

fn parse_job_id(job_id: &str) -> std::result::Result<i64, WorkerError> {
    let parsed = job_id
        .parse::<i64>()
        .map_err(|_| WorkerError("job ID is malformed".to_owned()))?;
    if parsed <= 0 || parsed.to_string() != job_id {
        return Err(WorkerError("job ID is malformed".to_owned()));
    }
    Ok(parsed)
}

async fn receive_result(
    job_id: i64,
    owned: &OwnedJob,
    stream: &mut ResultStream,
) -> std::result::Result<serde_json::Value, ReceiveResultError> {
    let cached = owned
        .pending_result
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let result = match cached {
        Some(result) => result,
        None => {
            let result = stream
                .next()
                .await
                .ok_or_else(|| anyhow!("job {job_id} result stream ended without a JobResult"))?
                .context("receive terminal Worker JobResult")?;
            let result = Arc::new(result);
            *owned
                .pending_result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result.clone());
            result
        }
    };
    match stream.next().await {
        None => {}
        Some(Ok(_)) => {
            return Err(ReceiveResultError::uncertain(anyhow!(
                "job {job_id} result stream contained more than one terminal JobResult"
            )));
        }
        Some(Err(error)) => {
            return Err(ReceiveResultError::uncertain(
                anyhow!(error).context("receive end of Worker result stream"),
            ));
        }
    }

    let data = result
        .data
        .as_ref()
        .ok_or_else(|| anyhow!("job {job_id} JobResult is missing result data"))?;
    if data.job_id.as_ref().map(|id| id.value) != Some(job_id) {
        return Err(anyhow!("job {job_id} JobResult callback has mismatched job metadata").into());
    }
    if data.worker_id.as_ref().map(|id| id.value) != Some(owned.worker_id) {
        return Err(
            anyhow!("job {job_id} JobResult callback has mismatched Worker metadata").into(),
        );
    }
    if data.using.as_deref() != Some(owned.method.as_str()) {
        return Err(
            anyhow!("job {job_id} JobResult callback has mismatched method metadata").into(),
        );
    }

    decode_terminal_result(job_id, owned, data, result.encoded_len())
        .map_err(ReceiveResultError::definitive)
}

fn decode_terminal_result(
    job_id: i64,
    owned: &OwnedJob,
    data: &proto::jobworkerp::data::JobResultData,
    encoded_len: usize,
) -> Result<serde_json::Value> {
    if encoded_len > MAX_STREAM_BYTES {
        bail!("job {job_id} result stream payload limit exceeded");
    }
    if data.status != ResultStatus::Success as i32 {
        let diagnostic = data
            .output
            .as_ref()
            .map(|output| String::from_utf8_lossy(&output.items).into_owned())
            .unwrap_or_default();
        bail!(
            "job {job_id} failed with status {}: {diagnostic}",
            data.status
        );
    }
    let output = data
        .output
        .as_ref()
        .ok_or_else(|| anyhow!("job {job_id} successful result is missing output"))?;
    if output.items.len() > MAX_STREAM_BYTES {
        bail!("job {job_id} result output limit exceeded");
    }
    decode_result(Some(&owned.result_descriptor), &output.items)
        .context("decode terminal Worker result")?
        .ok_or_else(|| anyhow!("job {job_id} result schema produced no JSON value"))
}

struct ReceiveResultError {
    error: anyhow::Error,
    definitive: bool,
}

impl ReceiveResultError {
    fn uncertain(error: anyhow::Error) -> Self {
        Self {
            error,
            definitive: false,
        }
    }

    fn definitive(error: anyhow::Error) -> Self {
        Self {
            error,
            definitive: true,
        }
    }
}

impl From<anyhow::Error> for ReceiveResultError {
    fn from(error: anyhow::Error) -> Self {
        Self::uncertain(error)
    }
}
