// Streaming version of RunTaskExecutor
// Separated from run.rs for better code organization

use crate::workflow::{
    definition::{
        transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
        workflow::{self, RunJobRunner, RunJobWorker},
    },
    execute::{
        context::{TaskContext, WorkflowContext, WorkflowStreamEvent},
        expression::UseExpression,
        task::{
            NamedTimeouts, StreamTaskExecutorTrait,
            run::{alias, resolve_run_task_timeout_sec},
        },
    },
};
use anyhow::Result;
use app::app::job::execute::{JobExecutorWrapper, WorkerForEnqueue};
use command_utils::trace::Tracing;
use futures::stream::BoxStream;
use jobworkerp_runner::jobworkerp::runner::{
    ChildDurableLookupState, ChildExecutionReceipt, ChildProducerEof, ChildSandboxObservation,
    SandboxExecResult, sandbox_exec_result,
};
use prost::Message;
use proto::jobworkerp::data::{
    JobId, QueueType, ResponseType, RunnerId, StreamingType, WorkerData,
};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::RwLock;

const SANDBOX_RUNNER_NAME: &str = "SANDBOX";
const DURABLE_LOOKUP_MAX: Duration = Duration::from_millis(250);
const DURABLE_LOOKUP_POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_DURABLE_RESULT_CANDIDATES: usize = 16;
const MAX_SANDBOX_PROTOCOL_FAILURE_BYTES: usize = 1024;

/// RunStreamTaskExecutor - Streaming version of RunTaskExecutor
///
/// This executor implements `TaskExecutorTrait` and internally uses streaming execution.
/// It enqueues jobs with `streaming=true` and `broadcast_results=true`, allowing:
/// - Intermediate results to be accessed via `JobResultService/ListenStream`
/// - Final results to be collected and stored in `TaskContext.raw_output`
pub struct RunStreamTaskExecutor {
    workflow_context: Arc<RwLock<WorkflowContext>>,
    default_task_timeout: Duration,
    named_timeouts: Arc<NamedTimeouts>,
    task: workflow::RunTask,
    job_executor_wrapper: Arc<JobExecutorWrapper>,
    metadata: Arc<HashMap<String, String>>,
}

impl UseExpression for RunStreamTaskExecutor {}
impl UseJqAndTemplateTransformer for RunStreamTaskExecutor {}
impl UseExpressionTransformer for RunStreamTaskExecutor {}
impl Tracing for RunStreamTaskExecutor {}

impl RunStreamTaskExecutor {
    pub fn new(
        workflow_context: Arc<RwLock<WorkflowContext>>,
        default_task_timeout: Duration,
        named_timeouts: Arc<NamedTimeouts>,
        job_executor_wrapper: Arc<JobExecutorWrapper>,
        task: workflow::RunTask,
        metadata: Arc<HashMap<String, String>>,
    ) -> Self {
        Self {
            workflow_context,
            default_task_timeout,
            named_timeouts,
            task,
            job_executor_wrapper,
            metadata,
        }
    }

    /// Convert workflow QueueType to proto QueueType (same as RunTaskExecutor)
    fn convert_queue_type(qt: workflow::QueueType) -> i32 {
        match qt {
            workflow::QueueType::Normal => QueueType::Normal as i32,
            workflow::QueueType::WithBackup => QueueType::WithBackup as i32,
            workflow::QueueType::DbOnly => QueueType::DbOnly as i32,
        }
    }

    /// Convert workflow ResponseType to proto ResponseType (same as RunTaskExecutor)
    fn convert_response_type(rt: workflow::ResponseType) -> i32 {
        match rt {
            workflow::ResponseType::NoResult => ResponseType::NoResult as i32,
            workflow::ResponseType::Direct => ResponseType::Direct as i32,
        }
    }

    pub(super) fn function_options_to_worker_data(
        options: Option<workflow::WorkerOptions>,
        name: &str,
    ) -> Option<WorkerData> {
        if let Some(options) = options {
            let worker_data = WorkerData {
                name: name.to_string(),
                description: String::new(),
                broadcast_results: options.broadcast_results.unwrap_or(false),
                store_failure: options.store_failure.unwrap_or(false),
                store_success: options.store_success.unwrap_or(false),
                use_static: options.use_static.unwrap_or(false),
                queue_type: options
                    .queue_type
                    .map(Self::convert_queue_type)
                    .unwrap_or(QueueType::Normal as i32),
                channel: options.channel,
                retry_policy: options.retry.map(|r| r.to_jobworkerp()),
                response_type: options
                    .response_type
                    .map(Self::convert_response_type)
                    .unwrap_or(ResponseType::Direct as i32),
                ..Default::default()
            };
            Some(worker_data)
        } else {
            None
        }
    }
}

/// Handle for a started streaming job, containing all info needed to collect results
pub struct StreamingJobHandle {
    pub job_id: JobId,
    pub runner_id: RunnerId,
    pub runner_name: String,
    pub runner_data: proto::jobworkerp::data::RunnerData,
    /// Worker name - None for Runner-based jobs (temporary worker), Some for Worker-based jobs
    pub worker_name: Option<String>,
    pub runner_spec: Option<Box<dyn jobworkerp_runner::runner::RunnerSpec + Send + Sync>>,
    pub using: Option<String>,
    pub timeout_sec: u32,
    /// `enqueue_with_worker_or_temp` creates a new job with retry ordinal 0.
    /// Any retry result is conservatively rejected unless that ordinal matches.
    retry_ordinal: u32,
    worker_id: Option<i64>,
    receipt_worker_name: String,
    receipt_method_using: Option<String>,
    settings_sha256: Vec<u8>,
    method_schema_sha256: Vec<u8>,
    arguments_sha256: Vec<u8>,
    store_success: Option<bool>,
    store_failure: Option<bool>,
    broadcast_results: Option<bool>,
    job_deadline: tokio::time::Instant,
}

#[derive(Debug, Default)]
struct SandboxStreamObservation {
    cli_exit_code: Option<i32>,
    end_received: bool,
    protocol_failure: Option<String>,
    // Pub/Sub ends at its End marker; it does not prove producer-side EOF.
    producer_eof: i32,
}

impl SandboxStreamObservation {
    fn new() -> Self {
        Self {
            producer_eof: ChildProducerEof::Unknown as i32,
            ..Self::default()
        }
    }

    fn fail(&mut self, message: String) {
        if self.protocol_failure.is_none() {
            let mut bounded = String::new();
            for character in message.chars() {
                if bounded.len() + character.len_utf8() > MAX_SANDBOX_PROTOCOL_FAILURE_BYTES {
                    break;
                }
                bounded.push(character);
            }
            self.protocol_failure = Some(bounded);
        }
    }

    fn observe_data(&mut self, data: &[u8]) {
        match SandboxExecResult::decode(data) {
            Ok(result) => match result.result {
                Some(sandbox_exec_result::Result::Output(_)) => {
                    if self.cli_exit_code.is_some() {
                        self.fail("SANDBOX output arrived after the exit result".to_string());
                    }
                }
                Some(sandbox_exec_result::Result::Exit(exit)) => {
                    if self.cli_exit_code.is_some() {
                        self.fail("SANDBOX stream contained a duplicate exit result".to_string());
                    } else {
                        self.cli_exit_code = Some(exit.exit_code);
                    }
                }
                None => {
                    self.fail("SANDBOX stream contained a result without a oneof value".to_string())
                }
            },
            Err(error) => self.fail(format!(
                "SANDBOX stream contained malformed result data: {error}"
            )),
        }
    }

    fn observe_end(&mut self, trailer: &proto::jobworkerp::data::Trailer) {
        if self.end_received {
            self.fail("SANDBOX stream contained duplicate End items".to_string());
            return;
        }
        self.end_received = true;
        match proto::stream_error::parse_stream_error(trailer) {
            proto::stream_error::StreamErrorOutcome::Missing => {}
            proto::stream_error::StreamErrorOutcome::Error(error) => self.fail(format!(
                "SANDBOX child stream failed ({}): {}",
                error.code, error.message
            )),
            proto::stream_error::StreamErrorOutcome::Malformed(error) => self.fail(format!(
                "SANDBOX child stream ended with malformed stream_error metadata: {error:?}"
            )),
        }
    }

    fn finish(&mut self) -> Result<()> {
        if !self.end_received {
            self.fail("SANDBOX child stream ended without an End item".to_string());
        }
        if self.cli_exit_code.is_none() {
            self.fail("SANDBOX child stream ended without an exit result".to_string());
        }
        match self.protocol_failure.as_deref() {
            Some(message) => Err(anyhow::anyhow!(message.to_string())),
            None => Ok(()),
        }
    }
}

struct ProcessStreamOutcome {
    output: Result<serde_json::Value>,
    sandbox_observation: Option<SandboxStreamObservation>,
}

struct DurableLookupObservation {
    state: i32,
    result_id: Option<i64>,
    result_status: Option<i32>,
    worker_id: Option<i64>,
    sandbox_observation: Option<ChildSandboxObservation>,
}

impl Default for DurableLookupObservation {
    fn default() -> Self {
        Self {
            state: ChildDurableLookupState::Unknown as i32,
            result_id: None,
            result_status: None,
            worker_id: None,
            sandbox_observation: None,
        }
    }
}

/// Preparation result containing everything needed for streaming
struct StreamingPreparation {
    handle: StreamingJobHandle,
    position: String,
    task_context: TaskContext,
    output_adapter: Option<ProcessOutputAdapter>,
    /// Stream subscription - subscribed immediately after job enqueue to avoid race condition
    stream: BoxStream<'static, proto::jobworkerp::data::ResultOutputItem>,
}

struct ProcessOutputAdapter {
    return_type: workflow::ProcessReturnType,
    input: serde_json::Value,
}

fn apply_output_adapter(
    output: serde_json::Value,
    adapter: Option<ProcessOutputAdapter>,
) -> Result<serde_json::Value> {
    if let Some(adapter) = adapter {
        alias::adapt_process_return_output(output, adapter.return_type, &adapter.input)
    } else {
        Ok(output)
    }
}

fn sha256_bytes(bytes: &[u8]) -> Vec<u8> {
    Sha256::digest(bytes).to_vec()
}

fn method_schema_sha256(
    runner_data: &proto::jobworkerp::data::RunnerData,
    using: Option<&str>,
) -> Vec<u8> {
    let method = using.unwrap_or("run");
    let Some(schema) = runner_data
        .method_proto_map
        .as_ref()
        .and_then(|method_map| method_map.schemas.get(method))
    else {
        return Vec::new();
    };
    sha256_bytes(&schema.encode_to_vec())
}

fn child_sandbox_observation_from_db(
    observation: &proto::jobworkerp::data::SandboxExecutionObservation,
) -> ChildSandboxObservation {
    ChildSandboxObservation {
        observation_sha256: observation.observation_sha256.clone(),
        host_settings_sha256: observation.host_settings_sha256.clone(),
        using: observation.using.clone(),
        retry_ordinal: observation.retry_ordinal,
        cli_exit_code: observation.cli_exit_code,
        end_state: observation.end_state,
        producer_state: observation.producer_state,
        anomaly_codes: observation.anomaly_codes.clone(),
        stdout_bytes: observation.stdout_bytes,
        stderr_bytes: observation.stderr_bytes,
        stdout_sha256: observation.stdout_sha256.clone(),
        stderr_sha256: observation.stderr_sha256.clone(),
        trailer_bytes: observation.trailer_bytes,
        trailer_sha256: observation.trailer_sha256.clone(),
    }
}

fn classify_durable_candidate(
    receipt: &ChildExecutionReceipt,
    handle: &StreamingJobHandle,
    result: &proto::jobworkerp::data::JobResult,
    observation: Option<proto::jobworkerp::data::SandboxExecutionObservation>,
) -> DurableLookupObservation {
    let (Some(result_id), Some(data)) = (result.id.as_ref(), result.data.as_ref()) else {
        return DurableLookupObservation::default();
    };
    let (Some(job_id), Some(status)) = (data.job_id.as_ref(),
        proto::jobworkerp::data::ResultStatus::try_from(data.status).ok()) else {
        return DurableLookupObservation::default();
    };
    let using_matches = data.using.as_deref().unwrap_or_default()
        == handle.using.as_deref().unwrap_or_default();
    let receipt_matches_handle = receipt.child_job_id == handle.job_id.value
        && receipt.worker_id == handle.worker_id
        && receipt.runner_id == Some(handle.runner_id.value)
        && receipt.runner_name == handle.runner_name
        && receipt.method_using == handle.receipt_method_using
        && receipt.settings_sha256 == handle.settings_sha256
        && receipt.method_schema_sha256 == handle.method_schema_sha256
        && receipt.arguments_sha256 == handle.arguments_sha256;
    let row_matches_prepared_child = result_id.value > 0
        && job_id.value == handle.job_id.value
        && sha256_bytes(&data.args) == handle.arguments_sha256
        && using_matches
        && (handle.worker_id.is_none() || data.worker_name == handle.receipt_worker_name)
        && handle.worker_id.is_none_or(|expected| {
            data.worker_id.as_ref().is_some_and(|actual| actual.value == expected)
        });
    if !receipt_matches_handle || !row_matches_prepared_child {
        return DurableLookupObservation::default();
    }

    // A direct RDB row can report result availability, but its flags (or the
    // current worker cache) do not prove the settings used during execution.
    let available = DurableLookupObservation {
        state: ChildDurableLookupState::Unknown as i32,
        result_id: Some(result_id.value),
        result_status: Some(data.status),
        worker_id: data.worker_id.as_ref().map(|id| id.value),
        sandbox_observation: None,
    };
    let Some(observation) = observation else {
        return available;
    };
    if observation.validate().is_err() {
        return available;
    }

    let Some(worker_id) = data.worker_id.as_ref() else {
        return available;
    };
    let end_state = match proto::jobworkerp::data::SandboxExecutionEndState::try_from(
        observation.end_state,
    ) {
        Ok(state) => state,
        Err(_) => return available,
    };
    let stream_exit_matches = match (receipt.cli_exit_code, observation.cli_exit_code) {
        (Some(stream), Some(durable)) => stream == durable,
        (Some(_), None) => false,
        (None, _) => true,
    };
    let stream_end_matches = !receipt.end_received
        || receipt.protocol_failure.is_some()
        || end_state == proto::jobworkerp::data::SandboxExecutionEndState::Normal;
    let witness_matches_prepared_child = observation.result_id == result_id.value
        && observation.job_id == handle.job_id.value
        && observation.worker_id == worker_id.value
        && handle
            .worker_id
            .is_none_or(|expected| expected == worker_id.value)
        && observation.runner_id == handle.runner_id.value
        && observation.dispatch_args_sha256 == handle.arguments_sha256
        && observation.worker_settings_sha256 == handle.settings_sha256
        && observation.method_schema_sha256 == handle.method_schema_sha256
        && observation.using == handle.using.as_deref().unwrap_or_default()
        && observation.retry_ordinal == handle.retry_ordinal
        && data.retried == handle.retry_ordinal
        && observation.stored_result_status == data.status
        && data.using.as_deref().unwrap_or_default() == observation.using
        && stream_exit_matches
        && stream_end_matches;
    if !witness_matches_prepared_child {
        return available;
    }

    DurableLookupObservation {
        state: ChildDurableLookupState::Verified as i32,
        result_id: Some(result_id.value),
        result_status: Some(status as i32),
        worker_id: Some(worker_id.value),
        sandbox_observation: Some(child_sandbox_observation_from_db(&observation)),
    }
}

async fn classify_durable_results(
    job_executor_wrapper: &Arc<JobExecutorWrapper>,
    receipt: &ChildExecutionReceipt,
    handle: &StreamingJobHandle,
    results: &[proto::jobworkerp::data::JobResult],
) -> Result<DurableLookupObservation> {
    use app::app::job_result::UseJobResultApp;

    if results.len() > MAX_DURABLE_RESULT_CANDIDATES {
        return Ok(DurableLookupObservation::default());
    }

    let job_result_app = job_executor_wrapper.job_result_app();
    let mut unverified = DurableLookupObservation::default();
    let mut verified = None;
    for listed_result in results {
        let Some(listed_id) = listed_result.id.as_ref().filter(|id| id.value > 0) else {
            continue;
        };
        // The list may be cache-enriched. Re-read the canonical row and
        // observation through explicit RDB-only APIs before trusting either.
        let Some(result) = job_result_app.find_job_result_from_db(listed_id).await? else {
            continue;
        };
        if result.id.as_ref() != Some(listed_id) {
            continue;
        }
        let observation = job_result_app
            .find_sandbox_observation_from_db(listed_id)
            .await?;
        let candidate = classify_durable_candidate(receipt, handle, &result, observation);
        if candidate.state == ChildDurableLookupState::Verified as i32 {
            if verified.replace(candidate).is_some() {
                // Multiple rows with a valid-looking witness for one dispatch
                // are ambiguous; don't select the first result arbitrarily.
                return Ok(DurableLookupObservation::default());
            }
        } else if candidate.result_id.is_some() {
            unverified = candidate;
        }
    }
    Ok(verified.unwrap_or(unverified))
}

async fn lookup_durable_sandbox_result(
    job_executor_wrapper: &Arc<JobExecutorWrapper>,
    handle: &StreamingJobHandle,
    receipt: &ChildExecutionReceipt,
) -> DurableLookupObservation {
    use app::app::job_result::UseJobResultApp;

    let now = tokio::time::Instant::now();
    if now >= handle.job_deadline {
        return DurableLookupObservation::default();
    }
    let lookup_deadline = std::cmp::min(handle.job_deadline, now + DURABLE_LOOKUP_MAX);
    let mut last_observation = DurableLookupObservation::default();
    loop {
        match tokio::time::timeout_at(
            lookup_deadline,
            job_executor_wrapper
                .job_result_app()
                .find_job_result_list_by_job_id(&handle.job_id),
        )
        .await
        {
            Ok(Ok(results)) => {
                match tokio::time::timeout_at(
                    lookup_deadline,
                    classify_durable_results(job_executor_wrapper, receipt, handle, &results),
                )
                .await
                {
                    Ok(Ok(observation))
                        if observation.state == ChildDurableLookupState::Verified as i32 =>
                    {
                        return observation;
                    }
                    Ok(Ok(observation)) => {
                        if observation.result_id.is_some() {
                            last_observation = observation;
                        }
                    }
                    Ok(Err(error)) => {
                        tracing::debug!(job_id = handle.job_id.value, %error, "SANDBOX database witness lookup failed");
                        return last_observation;
                    }
                    Err(_) => return last_observation,
                }
            }
            Ok(Err(error)) => {
                tracing::debug!(job_id = handle.job_id.value, %error, "SANDBOX durable result lookup failed");
                return last_observation;
            }
            Err(_) => return last_observation,
        }
        if tokio::time::Instant::now() >= lookup_deadline {
            return last_observation;
        }
        tokio::time::sleep_until(std::cmp::min(
            lookup_deadline,
            tokio::time::Instant::now() + DURABLE_LOOKUP_POLL_INTERVAL,
        ))
        .await;
    }
}

fn make_sandbox_child_receipt(
    workflow_execution_id: &str,
    task_position: &str,
    handle: &StreamingJobHandle,
    observation: &SandboxStreamObservation,
    durable_lookup: DurableLookupObservation,
) -> ChildExecutionReceipt {
    ChildExecutionReceipt {
        workflow_execution_id: workflow_execution_id.to_string(),
        task_position: task_position.to_string(),
        child_job_id: handle.job_id.value,
        worker_id: handle.worker_id.or(durable_lookup.worker_id),
        worker_name: handle.receipt_worker_name.clone(),
        runner_id: Some(handle.runner_id.value),
        runner_name: handle.runner_name.clone(),
        method_using: handle.receipt_method_using.clone(),
        settings_sha256: handle.settings_sha256.clone(),
        method_schema_sha256: handle.method_schema_sha256.clone(),
        arguments_sha256: handle.arguments_sha256.clone(),
        timeout_sec: (handle.timeout_sec > 0).then_some(handle.timeout_sec),
        cli_exit_code: observation.cli_exit_code,
        end_received: observation.end_received,
        protocol_failure: observation.protocol_failure.clone(),
        producer_eof: observation.producer_eof,
        child_job_result_id: durable_lookup.result_id,
        child_job_result_status: durable_lookup.result_status,
        sandbox_observation: durable_lookup.sandbox_observation,
        durable_lookup_state: durable_lookup.state,
        store_success: handle.store_success,
        store_failure: handle.store_failure,
        broadcast_results: handle.broadcast_results,
    }
}

fn validate_child_stream_end(
    runner_name: &str,
    trailer: Option<&proto::jobworkerp::data::Trailer>,
) -> Result<()> {
    if runner_name != "SANDBOX" {
        return Ok(());
    }

    let trailer =
        trailer.ok_or_else(|| anyhow::anyhow!("SANDBOX child stream ended without an End item"))?;
    match proto::stream_error::parse_stream_error(trailer) {
        proto::stream_error::StreamErrorOutcome::Missing => Ok(()),
        proto::stream_error::StreamErrorOutcome::Error(error) => Err(anyhow::anyhow!(
            "SANDBOX child stream failed ({}): {}",
            error.code,
            error.message
        )),
        proto::stream_error::StreamErrorOutcome::Malformed(error) => Err(anyhow::anyhow!(
            "SANDBOX child stream ended with malformed stream_error metadata: {error:?}"
        )),
    }
}

/// Streaming execution must collect a runner result to forward stream items, so
/// `await: false` (fire-and-forget) is incompatible with `useStreaming: true`.
/// Reject it uniformly across every run.* instance with one positioned error.
async fn reject_await_false_for_streaming(
    await_completion: bool,
    task_context: &TaskContext,
) -> Result<(), Box<workflow::Error>> {
    if await_completion {
        return Ok(());
    }
    let pos = task_context.position.read().await.as_error_instance();
    Err(workflow::errors::ErrorFactory::new().bad_argument(
        "run.await=false is not supported with streaming execution".to_string(),
        Some(pos),
        Some("useStreaming=true requires collecting a runner result".to_string()),
    ))
}

impl StreamTaskExecutorTrait<'_> for RunStreamTaskExecutor {
    fn execute_stream(
        &self,
        cx: Arc<opentelemetry::Context>,
        task_name: Arc<String>,
        task_context: TaskContext,
    ) -> impl futures::Stream<Item = Result<WorkflowStreamEvent, Box<workflow::Error>>> + Send {
        // Clone all required values
        let workflow_context = self.workflow_context.clone();
        let job_executor_wrapper = self.job_executor_wrapper.clone();
        let task = self.task.clone();
        let base_metadata = self.metadata.clone();
        let default_timeout = self.default_task_timeout;
        let named_timeouts = self.named_timeouts.clone();

        // Create channel for streaming events
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<
            Result<WorkflowStreamEvent, Box<workflow::Error>>,
        >();

        // Spawn a task that handles the entire execution
        // This avoids the stream! macro's blocking yield behavior
        tokio::spawn(async move {
            // Run preparation phase
            let prep_result = prepare_streaming_job(
                &workflow_context,
                &job_executor_wrapper,
                &task,
                &base_metadata,
                default_timeout,
                &named_timeouts,
                &cx,
                &task_name,
                task_context,
            )
            .await;

            let prep = match prep_result {
                Ok(p) => p,
                Err(e) => {
                    let _ = event_tx.send(Err(e));
                    return;
                }
            };

            let job_id = prep.handle.job_id;
            let job_id_value = job_id.value;
            let position = prep.position.clone();
            let mut task_context = prep.task_context;
            // Stream is already subscribed in prepare_streaming_job to avoid race condition
            let stream = prep.stream;

            // Emit StreamingJobStarted immediately
            let _ = event_tx.send(Ok(WorkflowStreamEvent::streaming_job_started(
                job_id,
                &prep.handle.runner_name,
                prep.handle.worker_name.clone(),
                &position,
            )));

            // Process stream and forward events
            let processed = process_stream(
                stream,
                job_id_value,
                &prep.handle,
                &job_executor_wrapper,
                &event_tx,
            )
            .await;

            if let Some(observation) = processed.sandbox_observation.as_ref() {
                let workflow_execution_id = workflow_context.read().await.id.to_string();
                let mut receipt = make_sandbox_child_receipt(
                    &workflow_execution_id,
                    &position,
                    &prep.handle,
                    observation,
                    DurableLookupObservation::default(),
                );
                let durable_lookup =
                    lookup_durable_sandbox_result(&job_executor_wrapper, &prep.handle, &receipt)
                        .await;
                receipt.worker_id = receipt.worker_id.or(durable_lookup.worker_id);
                receipt.child_job_result_id = durable_lookup.result_id;
                receipt.child_job_result_status = durable_lookup.result_status;
                receipt.durable_lookup_state = durable_lookup.state;
                workflow_context
                    .read()
                    .await
                    .record_child_execution_receipt(receipt);
            }

            // Set output on task context
            match processed.output {
                Ok(o) => {
                    let output = match apply_output_adapter(o, prep.output_adapter) {
                        Ok(output) => output,
                        Err(e) => {
                            tracing::error!(error = ?e, job_id = %job_id_value, "Failed to adapt streaming run alias process output");
                            let _ = event_tx.send(Err(workflow::errors::ErrorFactory::new()
                                .bad_argument(
                                    "Failed to adapt run alias process output".to_string(),
                                    Some(position.clone()),
                                    Some(e.to_string()),
                                )));
                            return;
                        }
                    };
                    task_context.set_raw_output(output);
                }
                Err(e) => {
                    tracing::error!(error = ?e, job_id = %job_id_value, "Failed to process stream");
                    let _ = event_tx.send(Err(workflow::errors::ErrorFactory::new()
                        .service_unavailable(
                            "Failed to process streaming result".to_string(),
                            Some(position.clone()),
                            Some(e.to_string()),
                        )));
                    return;
                }
            }

            // Position cleanup
            task_context.remove_position().await; // "worker"/"runner"/"function"
            task_context.remove_position().await; // "run"

            // Emit StreamingJobCompleted
            let _ = event_tx.send(Ok(WorkflowStreamEvent::streaming_job_completed(
                job_id,
                None, // job_result_id
                &position,
                task_context,
            )));

            // Channel is dropped when this task ends, signaling end of stream
        });

        // Return stream from channel receiver
        tokio_stream::wrappers::UnboundedReceiverStream::new(event_rx)
    }
}

/// Prepare streaming job: evaluate expressions, start job, return handle
#[allow(clippy::too_many_arguments)]
async fn prepare_streaming_job(
    workflow_context: &Arc<RwLock<WorkflowContext>>,
    job_executor_wrapper: &Arc<JobExecutorWrapper>,
    task: &workflow::RunTask,
    base_metadata: &Arc<HashMap<String, String>>,
    default_timeout: Duration,
    named_timeouts: &NamedTimeouts,
    cx: &Arc<opentelemetry::Context>,
    task_name: &Arc<String>,
    task_context: TaskContext,
) -> Result<StreamingPreparation, Box<workflow::Error>> {
    let workflow::RunTask {
        metadata: task_metadata,
        timeout,
        run,
        ..
    } = task;

    // === 1. Timeout setting ===
    let timeout_sec =
        resolve_run_task_timeout_sec(timeout.as_ref(), named_timeouts, default_timeout)?;

    // === 2. Metadata merge ===
    let mut metadata = (**base_metadata).clone();
    for (k, v) in task_metadata {
        if !metadata.contains_key(k)
            && let Some(v) = v.as_str()
        {
            metadata.insert(k.clone(), v.to_string());
        }
    }
    RunStreamTaskExecutor::inject_metadata_from_context(&mut metadata, cx);
    let metadata = Arc::new(metadata);

    // === 3. Position setup ===
    task_context.add_position_name("run".to_string()).await;

    // === 4. Expression evaluation ===
    let expression = match RunStreamTaskExecutor::expression(
        &*workflow_context.read().await,
        Arc::new(task_context.clone()),
    )
    .await
    {
        Ok(e) => e,
        Err(mut e) => {
            let pos = task_context.position.read().await;
            e.position(&pos);
            return Err(e);
        }
    };

    // === 5. Start job based on configuration ===
    let (handle, output_adapter) = match run {
        // Worker configuration
        workflow::RunTaskConfiguration::Worker(workflow::RunWorker {
            await_,
            worker:
                RunJobWorker {
                    arguments,
                    name: worker_name,
                    using,
                },
        }) => {
            task_context.add_position_name("worker".to_string()).await;
            reject_await_false_for_streaming(*await_, &task_context).await?;

            let args = match RunStreamTaskExecutor::transform_map(
                task_context.input.clone(),
                arguments.clone(),
                &expression,
            ) {
                Ok(args) => args,
                Err(mut e) => {
                    task_context
                        .position
                        .write()
                        .await
                        .push("arguments".to_string());
                    e.position(&*task_context.position.read().await);
                    return Err(e);
                }
            };

            // Capture position before await to avoid blocking_read() in async context
            let pos_for_err = task_context.position.read().await.as_error_instance();
            (
                start_worker_streaming_job_static(
                    job_executor_wrapper,
                    metadata.clone(),
                    worker_name,
                    args,
                    timeout_sec,
                    using.clone(),
                )
                .await
                .map_err(|e| {
                    tracing::error!(error = ?e, position = %pos_for_err, "Failed to start streaming job by jobworkerp (function)");
                    workflow::errors::ErrorFactory::new().service_unavailable(
                        "Failed to start streaming job by jobworkerp".to_string(),
                        Some(pos_for_err),
                        Some(e.to_string()),
                    )
                })?,
                None,
            )
        }

        // Function(WorkerFunction) configuration
        workflow::RunTaskConfiguration::Function(workflow::RunFunction {
            await_,
            function:
                workflow::RunJobFunction::WorkerFunction {
                    arguments,
                    using,
                    worker_name,
                },
        }) => {
            task_context.add_position_name("function".to_string()).await;
            reject_await_false_for_streaming(*await_, &task_context).await?;

            let args = match RunStreamTaskExecutor::transform_map(
                task_context.input.clone(),
                arguments.clone(),
                &expression,
            ) {
                Ok(args) => args,
                Err(mut e) => {
                    task_context
                        .position
                        .write()
                        .await
                        .push("arguments".to_string());
                    e.position(&*task_context.position.read().await);
                    return Err(e);
                }
            };

            // Capture position before await to avoid blocking_read() in async context
            let pos_for_err = task_context.position.read().await.as_error_instance();
            (
                start_worker_streaming_job_static(
                    job_executor_wrapper,
                    metadata.clone(),
                    worker_name,
                    args,
                    timeout_sec,
                    using.clone(),
                )
                .await
                .map_err(|e| {
                    tracing::error!(error = ?e, position = %pos_for_err, "Failed to start streaming job by jobworkerp (worker function)");
                    workflow::errors::ErrorFactory::new().service_unavailable(
                        "Failed to start streaming job by jobworkerp".to_string(),
                        Some(pos_for_err),
                        Some(e.to_string()),
                    )
                })?,
                None,
            )
        }

        // Runner configuration
        workflow::RunTaskConfiguration::Runner(workflow::RunRunner {
            await_,
            runner:
                RunJobRunner {
                    arguments,
                    name: runner_name,
                    options,
                    settings,
                    using,
                },
        }) => {
            task_context.add_position_name("runner".to_string()).await;
            reject_await_false_for_streaming(*await_, &task_context).await?;

            let args = match RunStreamTaskExecutor::transform_map(
                task_context.input.clone(),
                arguments.clone(),
                &expression,
            ) {
                Ok(args) => args,
                Err(mut e) => {
                    task_context
                        .position
                        .write()
                        .await
                        .push("arguments".to_string());
                    e.position(&*task_context.position.read().await);
                    return Err(e);
                }
            };

            let transformed_settings = match RunStreamTaskExecutor::transform_map(
                task_context.input.clone(),
                settings.clone(),
                &expression,
            ) {
                Ok(s) => s,
                Err(mut e) => {
                    task_context
                        .position
                        .write()
                        .await
                        .push("settings".to_string());
                    e.position(&*task_context.position.read().await);
                    return Err(e);
                }
            };

            // Capture position before await to avoid blocking_read() in async context
            let pos_for_err = task_context.position.read().await.as_error_instance();
            (
                start_runner_streaming_job_static(
                    job_executor_wrapper,
                    metadata.clone(),
                    timeout_sec,
                    runner_name,
                    Some(transformed_settings),
                    options.clone(),
                    args,
                    task_name,
                    using.clone(),
                )
                .await
                .map_err(|e| {
                    tracing::error!(error = ?e, position = %pos_for_err, "Failed to start streaming runner job by jobworkerp");
                    workflow::errors::ErrorFactory::new().service_unavailable(
                        "Failed to start streaming runner job by jobworkerp".to_string(),
                        Some(pos_for_err),
                        Some(e.to_string()),
                    )
                })?,
                None,
            )
        }

        // Function(RunnerFunction) configuration
        workflow::RunTaskConfiguration::Function(workflow::RunFunction {
            await_,
            function:
                workflow::RunJobFunction::RunnerFunction {
                    arguments,
                    options,
                    runner_name,
                    settings,
                    using,
                },
        }) => {
            task_context.add_position_name("function".to_string()).await;
            reject_await_false_for_streaming(*await_, &task_context).await?;

            let args = match RunStreamTaskExecutor::transform_map(
                task_context.input.clone(),
                arguments.clone(),
                &expression,
            ) {
                Ok(args) => args,
                Err(mut e) => {
                    task_context
                        .position
                        .write()
                        .await
                        .push("arguments".to_string());
                    e.position(&*task_context.position.read().await);
                    return Err(e);
                }
            };

            let transformed_settings = match RunStreamTaskExecutor::transform_map(
                task_context.input.clone(),
                settings.clone(),
                &expression,
            ) {
                Ok(s) => s,
                Err(mut e) => {
                    task_context
                        .position
                        .write()
                        .await
                        .push("settings".to_string());
                    e.position(&*task_context.position.read().await);
                    return Err(e);
                }
            };

            // Capture position before await to avoid blocking_read() in async context
            let pos_for_err = task_context.position.read().await.as_error_instance();
            (
                start_runner_streaming_job_static(
                    job_executor_wrapper,
                    metadata.clone(),
                    timeout_sec,
                    runner_name,
                    Some(transformed_settings),
                    options.clone(),
                    args,
                    task_name,
                    using.clone(),
                )
                .await
                .map_err(|e| {
                    tracing::error!(error = ?e, position = %pos_for_err, "Failed to start streaming runner function job by jobworkerp");
                    workflow::errors::ErrorFactory::new().service_unavailable(
                        "Failed to start streaming runner job by jobworkerp".to_string(),
                        Some(pos_for_err),
                        Some(e.to_string()),
                    )
                })?,
                None,
            )
        }

        // Open Workflow-style/jobworkerp extension aliases backed by
        // existing jobworkerp runners.
        workflow::RunTaskConfiguration::Shell(_)
        | workflow::RunTaskConfiguration::Container(_)
        | workflow::RunTaskConfiguration::Workflow(_) => {
            let normalized =
                alias::resolve_run_alias::<RunStreamTaskExecutor>(run, &task_context, &expression)
                    .await?;
            reject_await_false_for_streaming(normalized.await_completion, &task_context).await?;
            let output_adapter = if normalized.produces_process_result() {
                Some(ProcessOutputAdapter {
                    return_type: normalized.return_type,
                    input: task_context.input.as_ref().clone(),
                })
            } else {
                None
            };

            let pos_for_err = task_context.position.read().await.as_error_instance();
            let handle = start_runner_streaming_job_static(
                job_executor_wrapper,
                metadata.clone(),
                timeout_sec,
                normalized.runner_name,
                None,
                None,
                normalized.arguments,
                task_name,
                normalized.using,
            )
            .await
            .map_err(|e| {
                tracing::error!(error = ?e, position = %pos_for_err, "Failed to start streaming run alias by jobworkerp");
                workflow::errors::ErrorFactory::new().service_unavailable(
                    "Failed to start streaming run alias by jobworkerp".to_string(),
                            Some(pos_for_err),
                            Some(e.to_string()),
                        )
            })?;
            (handle, output_adapter)
        }

        // run.script has no streaming output (the PYTHON_COMMAND runner does not
        // implement run_stream), so it cannot run under useStreaming: true.
        // run.script still works inside a streaming workflow when the task itself
        // sets useStreaming: false (the default), which routes it to the
        // non-streaming executor.
        workflow::RunTaskConfiguration::Script(_) => {
            let pos = task_context.position.read().await.as_error_instance();
            return Err(workflow::errors::ErrorFactory::new().not_implemented(
                "run.script does not support useStreaming: true".to_string(),
                Some(pos),
                Some("Set useStreaming: false (the default) on the script task.".to_string()),
            ));
        }
    };

    // Capture position for both success and error cases
    let pos_for_err = task_context.position.read().await.as_error_instance();
    let position = task_context.position.read().await.as_json_pointer();

    // Subscribe to stream IMMEDIATELY after job enqueue to avoid race condition
    // This must happen before worker completes and publishes stream data
    use infra::infra::job_result::pubsub::JobResultSubscriber;
    let timeout_ms = Some(timeout_sec as u64 * 1000);

    // Try redis repository first (Scalable mode), then channel repository (Standalone mode)
    let stream_result =
        if let Some(pubsub_repo) = job_executor_wrapper.redis_job_result_pubsub_repository() {
            pubsub_repo
                .subscribe_result_stream(&handle.job_id, timeout_ms)
                .await
        } else if let Some(pubsub_repo) = job_executor_wrapper.chan_job_result_pubsub_repository() {
            pubsub_repo
                .subscribe_result_stream(&handle.job_id, timeout_ms)
                .await
        } else {
            Err(anyhow::anyhow!(
                "No pubsub repository available for streaming"
            ))
        };
    let stream = match stream_result {
        Ok(stream) => stream,
        Err(error) => {
            tracing::error!(error = ?error, position = %pos_for_err, "Failed to subscribe to result stream");
            if handle.runner_name == SANDBOX_RUNNER_NAME {
                let mut observation = SandboxStreamObservation::new();
                observation.fail(format!(
                    "Failed to subscribe to child result stream: {error}"
                ));
                let workflow_execution_id = workflow_context.read().await.id.to_string();
                let receipt = make_sandbox_child_receipt(
                    &workflow_execution_id,
                    &position,
                    &handle,
                    &observation,
                    DurableLookupObservation::default(),
                );
                workflow_context
                    .read()
                    .await
                    .record_child_execution_receipt(receipt);
            }
            return Err(workflow::errors::ErrorFactory::new().service_unavailable(
                "Failed to subscribe to result stream".to_string(),
                Some(pos_for_err),
                Some(error.to_string()),
            ));
        }
    };

    Ok(StreamingPreparation {
        handle,
        position,
        task_context,
        output_adapter,
        stream,
    })
}

/// Process stream: forward events to channel and collect output
async fn process_stream(
    mut stream: BoxStream<'static, proto::jobworkerp::data::ResultOutputItem>,
    job_id: i64,
    handle: &StreamingJobHandle,
    job_executor_wrapper: &Arc<JobExecutorWrapper>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<
        Result<WorkflowStreamEvent, Box<workflow::Error>>,
    >,
) -> ProcessStreamOutcome {
    use app::app::job::execute::UseJobExecutor;
    use futures::StreamExt;
    use proto::jobworkerp::data::result_output_item::Item;

    let mut final_collected_bytes: Option<Vec<u8>> = None;
    let mut all_data_chunks: Vec<Vec<u8>> = Vec::new();
    let mut end_trailer = None;
    let mut sandbox_observation =
        (handle.runner_data.name == SANDBOX_RUNNER_NAME).then(SandboxStreamObservation::new);

    // Use timeout for each stream item to prevent hanging indefinitely
    let item_timeout = std::time::Duration::from_secs(handle.timeout_sec as u64);

    loop {
        match tokio::time::timeout(item_timeout, stream.next()).await {
            Ok(Some(item)) => {
                match item.item {
                    Some(Item::Data(data)) => {
                        // Forward to UI for real-time display
                        tracing::trace!(
                            "process_stream: received Data chunk for job {}, size={}",
                            job_id,
                            data.len()
                        );
                        let _ = event_tx.send(Ok(WorkflowStreamEvent::streaming_data(
                            JobId { value: job_id },
                            data.clone(),
                        )));
                        if let Some(observation) = sandbox_observation.as_mut() {
                            observation.observe_data(&data);
                        }
                        // Collect all chunks for later aggregation
                        all_data_chunks.push(data);
                    }
                    Some(Item::End(trailer)) => {
                        tracing::debug!("Stream ended for job {}", job_id);
                        if let Some(observation) = sandbox_observation.as_mut() {
                            observation.observe_end(&trailer);
                        }
                        end_trailer = Some(trailer);
                        break;
                    }
                    Some(Item::FinalCollected(data)) => {
                        // Use FinalCollected as the authoritative task output
                        if let Some(observation) = sandbox_observation.as_mut()
                            && observation.cli_exit_code.is_none()
                        {
                            observation.observe_data(&data);
                        }
                        final_collected_bytes = Some(data);
                    }
                    None => {}
                }
            }
            Ok(None) => {
                // Stream ended without End message
                tracing::debug!("Stream closed without End message for job {}", job_id);
                break;
            }
            Err(_) => {
                // Timeout waiting for next item
                tracing::warn!(
                    "Timeout waiting for stream item for job {} ({}s), ending stream processing",
                    job_id,
                    handle.timeout_sec
                );
                break;
            }
        }
    }

    let validation = if let Some(observation) = sandbox_observation.as_mut() {
        observation.finish()
    } else {
        validate_child_stream_end(&handle.runner_data.name, end_trailer.as_ref())
    };
    if let Err(error) = validation {
        return ProcessStreamOutcome {
            output: Err(error),
            sandbox_observation,
        };
    }

    // Prefer FinalCollected (properly aggregated by runner)
    // Otherwise, use runner_spec.collect_stream to properly aggregate Data chunks
    let output_bytes = if let Some(bytes) = final_collected_bytes {
        tracing::debug!("Using FinalCollected for job {}", job_id);
        bytes
    } else if !all_data_chunks.is_empty() {
        // No FinalCollected - use runner_spec to aggregate chunks
        if let Some(runner_spec) = handle.runner_spec.as_ref() {
            tracing::debug!(
                "Using runner_spec.collect_stream for job {} ({} chunks)",
                job_id,
                all_data_chunks.len()
            );
            // Create a stream from collected chunks
            let chunk_stream = futures::stream::iter(all_data_chunks.into_iter().map(|data| {
                proto::jobworkerp::data::ResultOutputItem {
                    item: Some(Item::Data(data)),
                }
            }))
            .chain(futures::stream::once(async {
                proto::jobworkerp::data::ResultOutputItem {
                    item: Some(Item::End(proto::jobworkerp::data::Trailer::default())),
                }
            }));
            match runner_spec
                .collect_stream(Box::pin(chunk_stream), handle.using.as_deref())
                .await
            {
                Ok((bytes, _metadata)) => bytes,
                Err(e) => {
                    tracing::warn!("Failed to collect stream for job {}: {:?}", job_id, e);
                    Vec::new()
                }
            }
        } else {
            // No runner_spec available - concatenate all chunks as fallback
            // This may not produce semantically correct output for all runner types,
            // but preserves all data rather than losing chunks
            tracing::warn!(
                "No runner_spec available for job {}, concatenating {} chunks as fallback",
                job_id,
                all_data_chunks.len()
            );
            all_data_chunks.concat()
        }
    } else {
        tracing::warn!(
            "No FinalCollected or Data received for job {}, using empty output",
            job_id
        );
        Vec::new()
    };

    tracing::debug!(
        "Stream collected for job {}: {} bytes",
        job_id,
        output_bytes.len()
    );

    // Transform collected bytes to JSON output
    let output = job_executor_wrapper
        .transform_raw_output(
            &handle.runner_id,
            &handle.runner_data,
            output_bytes.as_slice(),
            handle.using.as_deref(),
        )
        .await;
    ProcessStreamOutcome {
        output,
        sandbox_observation,
    }
}

/// Start a worker streaming job and return handle with job_id
///
/// For streaming jobs, we need to return immediately after enqueue to subscribe
/// to stream before any data is published. Therefore, even if the worker has
/// response_type=Direct, we override it to NoResult for enqueue to avoid blocking.
async fn start_worker_streaming_job_static(
    job_executor_wrapper: &Arc<JobExecutorWrapper>,
    metadata: Arc<HashMap<String, String>>,
    worker_name: &str,
    args: serde_json::Value,
    timeout_sec: u32,
    using: Option<String>,
) -> Result<StreamingJobHandle> {
    use app::app::job::execute::{UseJobExecutor, UseRunnerSpecFactory};
    use app::app::runner::UseRunnerApp;
    use app::app::worker::UseWorkerApp;
    use infra::infra::runner::rows::RunnerWithSchema;
    use proto::jobworkerp::data::Worker;

    // Find worker by name
    let worker = job_executor_wrapper
        .worker_app()
        .find_by_name(worker_name)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to find worker '{}': {:#?}", worker_name, e))?
        .ok_or_else(|| anyhow::anyhow!("Worker '{}' not found", worker_name))?;

    let (wid, worker_data) = match worker {
        Worker {
            id: Some(wid),
            data: Some(worker_data),
        } => (wid, worker_data),
        _ => {
            return Err(anyhow::anyhow!(
                "Worker '{}' has no id or data",
                worker_name
            ));
        }
    };

    // Find runner for this worker
    let runner = job_executor_wrapper
        .runner_app()
        .find_runner(
            worker_data
                .runner_id
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Worker '{}' has no runner_id", worker_name))?,
        )
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to find runner for worker '{}': {:#?}",
                worker_name,
                e
            )
        })?;

    let (rid, rdata) = match runner {
        Some(RunnerWithSchema {
            id: Some(rid),
            data: Some(rdata),
            ..
        }) => (rid, rdata),
        _ => {
            return Err(anyhow::anyhow!(
                "Runner for worker '{}' not found",
                worker_name
            ));
        }
    };

    // Transform job args
    let job_args = job_executor_wrapper
        .transform_job_args(&rid, &rdata, &args, using.as_deref())
        .await?;
    let receipt_worker_id = Some(wid.value);
    let receipt_worker_name = worker_data.name.clone();
    let is_sandbox_receipt = rdata.name == SANDBOX_RUNNER_NAME;
    let settings_sha256 = if is_sandbox_receipt {
        sha256_bytes(&worker_data.runner_settings)
    } else {
        Vec::new()
    };
    let arguments_sha256 = if is_sandbox_receipt {
        sha256_bytes(&job_args)
    } else {
        Vec::new()
    };
    let receipt_method_using =
        is_sandbox_receipt.then(|| using.clone().unwrap_or_else(|| "run".to_string()));
    let method_schema_sha256 = if is_sandbox_receipt {
        method_schema_sha256(&rdata, receipt_method_using.as_deref())
    } else {
        Vec::new()
    };
    let store_success = Some(worker_data.store_success);
    let store_failure = Some(worker_data.store_failure);
    let broadcast_results = Some(worker_data.broadcast_results);
    let job_deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_sec as u64);

    // For streaming jobs, use the existing worker (with worker_id) to preserve pooling.
    // This is critical for heavy resources like local LLMs where use_static=true enables
    // runner instance reuse without re-initialization.
    //
    // StreamingType::Internal is used here because:
    // 1. We want the runner to use run_stream() internally for streaming output
    // 2. The app layer returns immediately (even for Direct response_type workers)
    //    allowing us to subscribe to the stream before data is published
    // 3. The final result is collected via collect_stream() and sent as FinalCollected
    // 4. Workflow steps receive a single aggregated result, not raw stream chunks
    let (job_id, _job_result, _stream) = job_executor_wrapper
        .enqueue_with_worker_or_temp(
            metadata,
            WorkerForEnqueue::existing(wid, worker_data),
            job_args,
            None, // uniq_key
            timeout_sec,
            StreamingType::Internal,
            using.clone(),
            None, // streaming always collects a result, so no NoResult override
        )
        .await?;

    // Get RunnerSpec for stream collection
    let runner_spec = job_executor_wrapper
        .runner_spec_factory()
        .create_runner_spec_by_name(&rdata.name, false)
        .await;

    Ok(StreamingJobHandle {
        job_id,
        runner_id: rid,
        runner_name: rdata.name.clone(),
        runner_data: rdata,
        worker_name: Some(worker_name.to_string()), // Worker-based job has real worker
        runner_spec,
        using,
        timeout_sec,
        retry_ordinal: 0,
        worker_id: receipt_worker_id,
        receipt_worker_name,
        receipt_method_using,
        settings_sha256,
        method_schema_sha256,
        arguments_sha256,
        store_success,
        store_failure,
        broadcast_results,
        job_deadline,
    })
}

/// Start a runner streaming job and return handle with job_id
/// Uses NoResult response_type to avoid waiting for job completion, allowing
/// stream subscription immediately after enqueue.
#[allow(clippy::too_many_arguments)]
async fn start_runner_streaming_job_static(
    job_executor_wrapper: &Arc<JobExecutorWrapper>,
    metadata: Arc<HashMap<String, String>>,
    timeout_sec: u32,
    runner_name: &str,
    settings: Option<serde_json::Value>,
    options: Option<workflow::WorkerOptions>,
    args: serde_json::Value,
    task_name: &str,
    using: Option<String>,
) -> Result<StreamingJobHandle> {
    use app::app::job::execute::{UseJobExecutor, UseRunnerSpecFactory};
    use app::app::runner::UseRunnerApp;
    use infra::infra::runner::rows::RunnerWithSchema;

    let runner = job_executor_wrapper
        .runner_app()
        .find_runner_by_name(runner_name)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Runner '{}' not found", runner_name))?;

    let (rid, rdata) = match &runner {
        RunnerWithSchema {
            id: Some(rid),
            data: Some(rdata),
            ..
        } => (*rid, rdata.clone()),
        _ => {
            return Err(anyhow::anyhow!(
                "Runner '{}' has no id or data",
                runner_name
            ));
        }
    };

    // Setup runner settings
    let runner_settings = job_executor_wrapper
        .setup_runner_and_settings(&runner, settings)
        .await?;

    // Create temporary worker for streaming
    // Use NoResult response_type to avoid waiting for job completion
    let worker_name = format!("{}_streaming_{}", task_name, runner_name);
    let mut worker_data =
        RunStreamTaskExecutor::function_options_to_worker_data(options, &worker_name).unwrap_or(
            WorkerData {
                name: worker_name.clone(),
                description: String::new(),
                runner_id: None,
                runner_settings: vec![],
                periodic_interval: 0,
                channel: None,
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::NoResult as i32,
                store_success: true,
                store_failure: true,
                use_static: false,
                retry_policy: None,
                broadcast_results: true,
            },
        );
    worker_data.runner_id = Some(rid);
    worker_data.runner_settings = runner_settings;
    // Override response_type and store settings for streaming
    worker_data.response_type = ResponseType::NoResult as i32;
    worker_data.store_success = true;
    worker_data.store_failure = true;
    worker_data.broadcast_results = true;

    // Keep the legacy enqueue path unchanged for every runner other than the
    // one whose child execution evidence is being observed in this phase.
    if rdata.name != SANDBOX_RUNNER_NAME {
        let receipt_worker_name = worker_data.name.clone();
        let settings_sha256 = Vec::new();
        let method_schema_sha256 = Vec::new();
        let store_success = Some(worker_data.store_success);
        let store_failure = Some(worker_data.store_failure);
        let broadcast_results = Some(worker_data.broadcast_results);
        let job_deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_sec as u64);
        let (job_id, _job_result, _stream) = job_executor_wrapper
            .setup_worker_and_enqueue_with_json_full_output(
                metadata,
                runner_name,
                worker_data,
                args,
                None, // uniq_key
                timeout_sec,
                StreamingType::Internal,
                using.clone(),
            )
            .await?;
        let runner_spec = job_executor_wrapper
            .runner_spec_factory()
            .create_runner_spec_by_name(&rdata.name, false)
            .await;
        return Ok(StreamingJobHandle {
            job_id,
            runner_id: rid,
            runner_name: rdata.name.clone(),
            runner_data: rdata,
            worker_name: None,
            runner_spec,
            using,
            timeout_sec,
            retry_ordinal: 0,
            worker_id: None,
            receipt_worker_name,
            receipt_method_using: None,
            settings_sha256,
            method_schema_sha256,
            arguments_sha256: Vec::new(),
            store_success,
            store_failure,
            broadcast_results,
            job_deadline,
        });
    }

    // Resolve the exact transformed bytes before enqueue so the receipt hashes
    // the same arguments that are persisted in JobData.
    let job_args = job_executor_wrapper
        .transform_job_args(&rid, &rdata, &args, using.as_deref())
        .await?;
    let (enqueue_worker, receipt_worker_id, actual_worker_data) = if worker_data.use_static {
        let worker = job_executor_wrapper
            .find_or_create_worker(worker_data.clone())
            .await?;
        let (worker_id, actual_data) = match worker {
            proto::jobworkerp::data::Worker {
                id: Some(worker_id),
                data: Some(actual_data),
            } => (worker_id, actual_data),
            _ => return Err(anyhow::anyhow!("SANDBOX worker has no id or data")),
        };
        (
            WorkerForEnqueue::existing(worker_id, actual_data.clone()),
            Some(worker_id.value),
            actual_data,
        )
    } else {
        (
            WorkerForEnqueue::Temp(worker_data.clone()),
            None,
            worker_data,
        )
    };
    let receipt_worker_name = actual_worker_data.name.clone();
    let settings_sha256 = sha256_bytes(&actual_worker_data.runner_settings);
    let arguments_sha256 = sha256_bytes(&job_args);
    let receipt_method_using = if rdata.name == SANDBOX_RUNNER_NAME {
        Some(using.clone().unwrap_or_else(|| "run".to_string()))
    } else {
        using.clone()
    };
    let method_schema_sha256 = method_schema_sha256(&rdata, receipt_method_using.as_deref());
    let store_success = Some(actual_worker_data.store_success);
    let store_failure = Some(actual_worker_data.store_failure);
    let broadcast_results = Some(actual_worker_data.broadcast_results);
    let job_deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_sec as u64);

    // Enqueue with streaming - returns immediately (NoResult)
    let (job_id, _job_result, _stream) = job_executor_wrapper
        .enqueue_with_worker_or_temp(
            metadata,
            enqueue_worker,
            job_args,
            None, // uniq_key
            timeout_sec,
            StreamingType::Internal,
            using.clone(),
            None,
        )
        .await?;

    // Get RunnerSpec for stream collection
    let runner_spec = job_executor_wrapper
        .runner_spec_factory()
        .create_runner_spec_by_name(&rdata.name, false)
        .await;

    Ok(StreamingJobHandle {
        job_id,
        runner_id: rid,
        runner_name: rdata.name.clone(),
        runner_data: rdata,
        worker_name: None, // Runner-based job uses temporary worker
        runner_spec,
        using,
        timeout_sec,
        retry_ordinal: 0,
        worker_id: receipt_worker_id,
        receipt_worker_name,
        receipt_method_using,
        settings_sha256,
        method_schema_sha256,
        arguments_sha256,
        store_success,
        store_failure,
        broadcast_results,
        job_deadline,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use jobworkerp_runner::jobworkerp::runner::{
        ChildDurableLookupState, ChildExecutionReceipt, ChildProducerEof, SandboxExecExit,
        SandboxExecResult,
        sandbox_exec_result::{self},
    };
    use serde_json::json;

    fn encoded_sandbox_exit(exit_code: i32) -> Vec<u8> {
        SandboxExecResult {
            result: Some(sandbox_exec_result::Result::Exit(SandboxExecExit {
                exit_code,
                execution_time_ms: 5,
                sandbox_id: "sandbox-test".to_string(),
            })),
        }
        .encode_to_vec()
    }

    fn sample_sandbox_receipt() -> ChildExecutionReceipt {
        ChildExecutionReceipt {
            workflow_execution_id: "workflow-test".to_string(),
            task_position: "/task".to_string(),
            child_job_id: 77,
            worker_id: Some(88),
            worker_name: "sandbox-worker".to_string(),
            runner_id: Some(99),
            runner_name: "SANDBOX".to_string(),
            method_using: Some("run".to_string()),
            settings_sha256: vec![1; 32],
            method_schema_sha256: vec![2; 32],
            arguments_sha256: vec![3; 32],
            timeout_sec: Some(30),
            cli_exit_code: Some(7),
            end_received: true,
            protocol_failure: None,
            producer_eof: ChildProducerEof::Unknown as i32,
            child_job_result_id: None,
            durable_lookup_state: ChildDurableLookupState::Unknown as i32,
            child_job_result_status: None,
            store_success: Some(true),
            store_failure: Some(true),
            broadcast_results: Some(true),
            sandbox_observation: None,
        }
    }

    #[test]
    fn output_adapter_applies_process_return_for_streaming_aliases() {
        let output = apply_output_adapter(
            json!({"exitCode": 0, "stdout": "streamed", "stderr": ""}),
            Some(ProcessOutputAdapter {
                return_type: workflow::ProcessReturnType::Stdout,
                input: json!({"original": true}),
            }),
        )
        .unwrap();

        assert_eq!(output, json!("streamed"));
    }

    #[test]
    fn output_adapter_keeps_raw_output_when_not_alias_process() {
        let raw = json!({"exitCode": 0, "stdout": "streamed"});

        let output = apply_output_adapter(raw.clone(), None).unwrap();

        assert_eq!(output, raw);
    }

    #[test]
    fn child_receipt_hashes_use_sha256_and_bind_the_live_method_schema() {
        let expected_abc = [
            0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
            0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
            0xf2, 0x00, 0x15, 0xad,
        ];
        assert_eq!(sha256_bytes(b"abc"), expected_abc.to_vec());

        let schema_data = |result_proto: &str| proto::jobworkerp::data::RunnerData {
            method_proto_map: Some(proto::jobworkerp::data::MethodProtoMap {
                schemas: HashMap::from([(
                    "run".to_string(),
                    proto::jobworkerp::data::MethodSchema {
                        args_proto: "SandboxExecArgs".to_string(),
                        result_proto: result_proto.to_string(),
                        ..Default::default()
                    },
                )]),
            }),
            ..Default::default()
        };
        let schema_v1 = schema_data("SandboxExecResult v1");
        let canonical_schema = schema_v1
            .method_proto_map
            .as_ref()
            .unwrap()
            .schemas
            .get("run")
            .unwrap()
            .encode_to_vec();
        let first = method_schema_sha256(&schema_v1, None);
        let changed = method_schema_sha256(&schema_data("SandboxExecResult v2"), None);

        assert_eq!(first.len(), 32);
        assert_eq!(
            first,
            proto::sandbox_observation::sha256_digest(&canonical_schema).to_vec()
        );
        assert_ne!(first, changed);
        assert!(
            method_schema_sha256(&proto::jobworkerp::data::RunnerData::default(), None).is_empty()
        );
    }

    #[test]
    fn sandbox_stream_preserves_nonzero_exit_and_never_claims_producer_eof() {
        let mut observation = SandboxStreamObservation::default();
        observation.observe_data(&encoded_sandbox_exit(7));
        observation.observe_end(&proto::jobworkerp::data::Trailer::default());

        assert!(observation.finish().is_ok());
        assert_eq!(observation.cli_exit_code, Some(7));
        assert!(observation.end_received);
        assert_eq!(observation.producer_eof, ChildProducerEof::Unknown as i32);
    }

    #[test]
    fn sandbox_stream_error_end_is_a_failure_without_losing_exit_code() {
        let mut observation = SandboxStreamObservation::default();
        observation.observe_data(&encoded_sandbox_exit(7));
        let trailer = proto::stream_error::build_stream_error_trailer(
            std::collections::HashMap::new(),
            "EXECUTION_FAILED",
            "sandbox execution failed",
            "SANDBOX",
        )
        .unwrap();
        observation.observe_end(&trailer);

        let error = observation.finish().unwrap_err();

        assert!(error.to_string().contains("EXECUTION_FAILED"));
        assert_eq!(observation.cli_exit_code, Some(7));
        assert!(observation.end_received);
    }

    #[test]
    fn sandbox_stream_with_malformed_error_end_is_a_failure() {
        let mut observation = SandboxStreamObservation::default();
        observation.observe_data(&encoded_sandbox_exit(0));
        let trailer = proto::jobworkerp::data::Trailer {
            metadata: std::collections::HashMap::from([(
                proto::stream_error::STREAM_ERROR_METADATA_KEY.to_string(),
                "not-json".to_string(),
            )]),
        };
        observation.observe_end(&trailer);

        let error = observation.finish().unwrap_err();

        assert!(error.to_string().contains("malformed"));
    }

    #[test]
    fn sandbox_stream_without_end_is_a_failure_even_after_exit() {
        let mut observation = SandboxStreamObservation::default();
        observation.observe_data(&encoded_sandbox_exit(0));

        let error = observation.finish().unwrap_err();

        assert!(error.to_string().contains("without an End"));
    }

    #[test]
    fn sandbox_stream_without_exit_is_a_protocol_failure() {
        let mut observation = SandboxStreamObservation::default();
        observation.observe_end(&proto::jobworkerp::data::Trailer::default());

        let error = observation.finish().unwrap_err();

        assert!(error.to_string().contains("without an exit"));
        assert_eq!(observation.cli_exit_code, None);
    }

    #[test]
    fn sandbox_stream_rejects_a_second_end_marker_when_the_observer_sees_it() {
        let trailer = proto::jobworkerp::data::Trailer::default();
        let mut observation = SandboxStreamObservation::default();
        observation.observe_data(&encoded_sandbox_exit(0));
        observation.observe_end(&trailer);
        observation.observe_end(&trailer);

        assert!(
            observation
                .finish()
                .unwrap_err()
                .to_string()
                .contains("duplicate End")
        );
        assert_eq!(observation.producer_eof, ChildProducerEof::Unknown as i32);
    }

    #[test]
    fn sandbox_stream_rejects_malformed_data_and_duplicate_exit_items() {
        let mut malformed = SandboxStreamObservation::default();
        malformed.observe_data(b"not-a-protobuf-message");
        malformed.observe_end(&proto::jobworkerp::data::Trailer::default());
        assert!(
            malformed
                .finish()
                .unwrap_err()
                .to_string()
                .contains("malformed")
        );

        let mut duplicate = SandboxStreamObservation::default();
        duplicate.observe_data(&encoded_sandbox_exit(0));
        duplicate.observe_data(&encoded_sandbox_exit(7));
        duplicate.observe_end(&proto::jobworkerp::data::Trailer::default());
        assert!(
            duplicate
                .finish()
                .unwrap_err()
                .to_string()
                .contains("duplicate exit")
        );
        assert_eq!(duplicate.cli_exit_code, Some(0));
    }

    #[test]
    fn legacy_runner_without_end_keeps_its_existing_success_behavior() {
        assert!(validate_child_stream_end("COMMAND", None).is_ok());

        let trailer = proto::stream_error::build_stream_error_trailer(
            std::collections::HashMap::new(),
            "EXECUTION_FAILED",
            "legacy runner metadata",
            "COMMAND",
        )
        .unwrap();
        assert!(validate_child_stream_end("COMMAND", Some(&trailer)).is_ok());
    }

    struct DurableFixture {
        receipt: ChildExecutionReceipt,
        handle: StreamingJobHandle,
        result: proto::jobworkerp::data::JobResult,
        observation: proto::jobworkerp::data::SandboxExecutionObservation,
    }

    fn durable_fixture(
        status: proto::jobworkerp::data::ResultStatus,
        producer: proto::jobworkerp::data::SandboxExecutionProducerState,
        cli_exit_code: i32,
    ) -> DurableFixture {
        let args = b"the exact binary sandbox args";
        let worker_settings = b"prepared sandbox worker settings";
        let method_schema = b"canonical prepared method schema";
        let mut receipt = sample_sandbox_receipt();
        receipt.cli_exit_code = Some(cli_exit_code);
        receipt.arguments_sha256 = sha256_bytes(args);
        receipt.settings_sha256 = sha256_bytes(worker_settings);
        receipt.method_schema_sha256 = sha256_bytes(method_schema);
        let handle = StreamingJobHandle {
            job_id: JobId {
                value: receipt.child_job_id,
            },
            runner_id: RunnerId {
                value: receipt.runner_id.unwrap(),
            },
            runner_name: receipt.runner_name.clone(),
            runner_data: proto::jobworkerp::data::RunnerData::default(),
            worker_name: None,
            runner_spec: None,
            using: Some("run".to_string()),
            timeout_sec: 30,
            retry_ordinal: 0,
            worker_id: receipt.worker_id,
            receipt_worker_name: receipt.worker_name.clone(),
            receipt_method_using: receipt.method_using.clone(),
            settings_sha256: receipt.settings_sha256.clone(),
            method_schema_sha256: receipt.method_schema_sha256.clone(),
            arguments_sha256: receipt.arguments_sha256.clone(),
            store_success: receipt.store_success,
            store_failure: receipt.store_failure,
            broadcast_results: receipt.broadcast_results,
            job_deadline: tokio::time::Instant::now() + Duration::from_secs(30),
        };
        let result_id = 444;
        let result = proto::jobworkerp::data::JobResult {
            id: Some(proto::jobworkerp::data::JobResultId { value: result_id }),
            data: Some(proto::jobworkerp::data::JobResultData {
                job_id: Some(handle.job_id),
                worker_id: handle
                    .worker_id
                    .map(|value| proto::jobworkerp::data::WorkerId { value }),
                worker_name: handle.receipt_worker_name.clone(),
                args: args.to_vec(),
                using: Some("run".to_string()),
                status: status as i32,
                retried: handle.retry_ordinal,
                store_success: true,
                store_failure: true,
                broadcast_results: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut observation = proto::jobworkerp::data::SandboxExecutionObservation {
            schema_version: proto::sandbox_observation::SANDBOX_OBSERVATION_SCHEMA_VERSION,
            job_id: handle.job_id.value,
            worker_id: handle.worker_id.unwrap(),
            runner_id: handle.runner_id.value,
            result_id,
            dispatch_args_sha256: handle.arguments_sha256.clone(),
            worker_settings_sha256: handle.settings_sha256.clone(),
            method_schema_sha256: handle.method_schema_sha256.clone(),
            host_settings_sha256: sha256_bytes(b"server dispatch snapshot"),
            using: "run".to_string(),
            retry_ordinal: handle.retry_ordinal,
            stored_result_status: status as i32,
            cli_exit_code: Some(cli_exit_code),
            end_state: proto::jobworkerp::data::SandboxExecutionEndState::Normal as i32,
            producer_state: producer as i32,
            anomaly_codes: Vec::new(),
            stdout_bytes: Some(12),
            stderr_bytes: Some(0),
            stdout_sha256: Some(sha256_bytes(b"sandbox stdout")),
            stderr_sha256: Some(sha256_bytes(b"")),
            trailer_bytes: None,
            trailer_sha256: None,
            observation_sha256: Vec::new(),
        };
        observation.seal().unwrap();
        DurableFixture {
            receipt,
            handle,
            result,
            observation,
        }
    }

    #[test]
    fn a_matching_result_row_and_worker_flags_without_a_durable_witness_stay_unknown() {
        let fixture = durable_fixture(
            proto::jobworkerp::data::ResultStatus::Success,
            proto::jobworkerp::data::SandboxExecutionProducerState::Clean,
            0,
        );
        assert_eq!(fixture.receipt.store_success, Some(true));
        assert_eq!(fixture.receipt.store_failure, Some(true));
        assert_eq!(fixture.receipt.broadcast_results, Some(true));

        let result = classify_durable_candidate(
            &fixture.receipt,
            &fixture.handle,
            &fixture.result,
            None,
        );

        assert_eq!(result.state, ChildDurableLookupState::Unknown as i32);
        assert_eq!(result.result_id, Some(444));
        assert_eq!(result.worker_id, Some(88));
        assert_eq!(result.sandbox_observation, None);
    }

    #[test]
    fn a_valid_child_bound_witness_is_verified_and_retains_failed_producer_facts() {
        let fixture = durable_fixture(
            proto::jobworkerp::data::ResultStatus::FatalError,
            proto::jobworkerp::data::SandboxExecutionProducerState::Aborted,
            7,
        );

        let result = classify_durable_candidate(
            &fixture.receipt,
            &fixture.handle,
            &fixture.result,
            Some(fixture.observation.clone()),
        );

        assert_eq!(result.state, ChildDurableLookupState::Verified as i32);
        assert_eq!(result.result_id, Some(444));
        assert_eq!(
            result.result_status,
            Some(proto::jobworkerp::data::ResultStatus::FatalError as i32)
        );
        let witness = result.sandbox_observation.unwrap();
        assert_eq!(witness.observation_sha256, fixture.observation.observation_sha256);
        assert_eq!(witness.host_settings_sha256, fixture.observation.host_settings_sha256);
        assert_eq!(witness.cli_exit_code, Some(7));
        assert_eq!(
            witness.producer_state,
            proto::jobworkerp::data::SandboxExecutionProducerState::Aborted as i32
        );
        assert_eq!(
            witness.end_state,
            proto::jobworkerp::data::SandboxExecutionEndState::Normal as i32
        );
    }

    #[test]
    fn a_durable_witness_cannot_verify_a_different_dispatch_or_result_row() {
        let fixture = durable_fixture(
            proto::jobworkerp::data::ResultStatus::Success,
            proto::jobworkerp::data::SandboxExecutionProducerState::Clean,
            0,
        );

        let mut changed = fixture.observation.clone();
        changed.worker_id += 1;
        changed.seal().unwrap();
        let wrong_worker = classify_durable_candidate(
            &fixture.receipt,
            &fixture.handle,
            &fixture.result,
            Some(changed),
        );
        assert_eq!(wrong_worker.state, ChildDurableLookupState::Unknown as i32);
        assert_eq!(wrong_worker.result_id, Some(444));
        assert_eq!(wrong_worker.sandbox_observation, None);

        let mut changed = fixture.observation.clone();
        changed.dispatch_args_sha256 = sha256_bytes(b"different child arguments");
        changed.seal().unwrap();
        let wrong_args = classify_durable_candidate(
            &fixture.receipt,
            &fixture.handle,
            &fixture.result,
            Some(changed),
        );
        assert_eq!(wrong_args.state, ChildDurableLookupState::Unknown as i32);
        assert_eq!(wrong_args.sandbox_observation, None);

        let mut invalid_digest = fixture.observation.clone();
        invalid_digest.observation_sha256[0] ^= 0xff;
        let invalid = classify_durable_candidate(
            &fixture.receipt,
            &fixture.handle,
            &fixture.result,
            Some(invalid_digest),
        );
        assert_eq!(invalid.state, ChildDurableLookupState::Unknown as i32);
        assert_eq!(invalid.sandbox_observation, None);
    }

    #[test]
    fn missing_result_rows_and_stream_mismatches_never_become_verified() {
        let fixture = durable_fixture(
            proto::jobworkerp::data::ResultStatus::Success,
            proto::jobworkerp::data::SandboxExecutionProducerState::Clean,
            0,
        );
        let absent = classify_durable_candidate(
            &fixture.receipt,
            &fixture.handle,
            &proto::jobworkerp::data::JobResult::default(),
            Some(fixture.observation.clone()),
        );
        assert_eq!(absent.state, ChildDurableLookupState::Unknown as i32);
        assert_eq!(absent.result_id, None);

        let mut stream_receipt = fixture.receipt.clone();
        stream_receipt.cli_exit_code = Some(7);
        let stream_mismatch = classify_durable_candidate(
            &stream_receipt,
            &fixture.handle,
            &fixture.result,
            Some(fixture.observation.clone()),
        );
        assert_eq!(stream_mismatch.state, ChildDurableLookupState::Unknown as i32);
        assert_eq!(stream_mismatch.sandbox_observation, None);

        let mut nonmatching = fixture.result.clone();
        nonmatching.data.as_mut().unwrap().retried += 1;
        let wrong_attempt = classify_durable_candidate(
            &fixture.receipt,
            &fixture.handle,
            &nonmatching,
            Some(fixture.observation.clone()),
        );
        assert_eq!(wrong_attempt.state, ChildDurableLookupState::Unknown as i32);
        assert_eq!(wrong_attempt.result_id, None);
    }

    #[tokio::test]
    async fn await_true_is_allowed_under_streaming() {
        let task_context = TaskContext::new_empty();
        reject_await_false_for_streaming(true, &task_context)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn await_false_is_rejected_under_streaming() {
        let task_context = TaskContext::new_empty();
        let err = reject_await_false_for_streaming(false, &task_context)
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("run.await=false is not supported with streaming execution")
        );
    }
}
