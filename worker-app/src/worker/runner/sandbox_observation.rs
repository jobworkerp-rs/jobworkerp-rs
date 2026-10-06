use futures::{Stream, stream::BoxStream};
use jobworkerp_runner::jobworkerp::runner::{SandboxExecResult, sandbox_exec_result};
use jobworkerp_runner::runner::RunnerSpec;
use jobworkerp_runner::runner::sandbox::SandboxRunner;
use prost::Message;
use proto::jobworkerp::data::{
    Job, JobData, ResultOutputItem, RunnerData, WorkerData, WorkerId, result_output_item::Item,
};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::oneshot;

const HOST_DISPATCH_SNAPSHOT_DOMAIN: &[u8] =
    b"jobworkerp.sandbox_execution_observation.host_dispatch_snapshot.v1\0";

/// Immutable input identity captured only after the dispatch path downcasts the
/// executing instance to the built-in SANDBOX type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxObservationDispatchBinding {
    pub job_id: i64,
    pub worker_id: i64,
    pub runner_id: i64,
    pub dispatch_args_sha256: [u8; 32],
    pub worker_settings_sha256: [u8; 32],
    pub method_schema_sha256: [u8; 32],
    pub host_settings_sha256: [u8; 32],
    /// The value as stored on the dispatched job; `None` is represented by "".
    pub using: String,
    pub retry_ordinal: u32,
}

/// Capture a SERVER-side dispatch snapshot after proving the actual instance is
/// the native SANDBOX runner. The global WorkerConfig is not serialized by this
/// server version, so host_settings_sha256 hashes this canonical snapshot:
/// encoded WorkerData, encoded per-job overrides (with presence), timeout,
/// streaming type, and the resolved delivery/retry policy. It is not a claim
/// about settings consumed by the SDK, guest process, VM, or assets.
pub(super) fn capture_native_sandbox_dispatch_binding(
    job: &Job,
    worker_id: &WorkerId,
    worker_data: &WorkerData,
    runner_data: &RunnerData,
    native_runner: &SandboxRunner,
) -> Option<SandboxObservationDispatchBinding> {
    let job_id = job.id.as_ref()?;
    let data = job.data.as_ref()?;
    let runner_id = worker_data.runner_id.as_ref()?;
    if job_id.value <= 0
        || worker_id.value <= 0
        || runner_id.value <= 0
        || data.worker_id.as_ref()?.value != worker_id.value
        || runner_data.name != native_runner.name()
    {
        return None;
    }

    let selected_method = data.using.as_deref().unwrap_or(proto::DEFAULT_METHOD_NAME);
    let native_method_schema = native_runner.method_proto_map().remove(selected_method)?;
    let stored_method_schema = runner_data
        .method_proto_map
        .as_ref()?
        .schemas
        .get(selected_method)?;
    // Bind only if the runner's own spec and the server-observed RunnerData
    // schema agree for the method actually dispatched.
    if stored_method_schema != &native_method_schema {
        return None;
    }

    let resolved = app::app::job::resolve_job_params(worker_data, data.overrides.as_ref());
    Some(SandboxObservationDispatchBinding {
        job_id: job_id.value,
        worker_id: worker_id.value,
        runner_id: runner_id.value,
        dispatch_args_sha256: proto::sandbox_observation::sha256_digest(&data.args),
        worker_settings_sha256: proto::sandbox_observation::sha256_digest(
            &worker_data.runner_settings,
        ),
        // MethodSchema is hashed as its canonical prost message encoding. The
        // separate `using` field binds the selected method name in the record.
        method_schema_sha256: proto::sandbox_observation::sha256_digest(
            &native_method_schema.encode_to_vec(),
        ),
        host_settings_sha256: host_dispatch_snapshot_sha256(worker_data, data, &resolved),
        using: data.using.clone().unwrap_or_default(),
        retry_ordinal: data.retried,
    })
}

fn host_dispatch_snapshot_sha256(
    worker_data: &WorkerData,
    job_data: &JobData,
    resolved: &app::app::job::ResolvedJobParams,
) -> [u8; 32] {
    let mut snapshot = HOST_DISPATCH_SNAPSHOT_DOMAIN.to_vec();
    append_len_prefixed(&mut snapshot, &worker_data.encode_to_vec());
    match job_data.overrides.as_ref() {
        Some(overrides) => {
            snapshot.push(1);
            append_len_prefixed(&mut snapshot, &overrides.encode_to_vec());
        }
        None => snapshot.push(0),
    }
    snapshot.extend_from_slice(&job_data.timeout.to_be_bytes());
    snapshot.extend_from_slice(&job_data.streaming_type.to_be_bytes());
    snapshot.extend_from_slice(&resolved.response_type.to_be_bytes());
    snapshot.push(u8::from(resolved.store_success));
    snapshot.push(u8::from(resolved.store_failure));
    snapshot.push(u8::from(resolved.broadcast_results));
    match resolved.retry_policy.as_ref() {
        Some(retry_policy) => {
            snapshot.push(1);
            append_len_prefixed(&mut snapshot, &retry_policy.encode_to_vec());
        }
        None => snapshot.push(0),
    }
    proto::sandbox_observation::sha256_digest(&snapshot)
}

fn append_len_prefixed(target: &mut Vec<u8>, value: &[u8]) {
    let len = u64::try_from(value.len()).unwrap_or(u64::MAX);
    target.extend_from_slice(&len.to_be_bytes());
    target.extend_from_slice(value);
}

/// The raw stream's transport termination. A clean termination says only that
/// the native SANDBOX stream followed its protobuf/end-marker protocol; it does
/// not imply that the process exit code is successful.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawSandboxOutcome {
    CleanTermination,
    ProtocolInvalid,
    Aborted,
    Unknown,
}

/// Fixed, non-textual protocol anomaly identifiers. These fit in the bounded
/// `anomaly_flags` field below and can never contain Runner-controlled text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RawSandboxAnomaly {
    MissingEnd = 0,
    MissingExit = 1,
    DuplicateEnd = 2,
    DuplicateExit = 3,
    DataAfterEnd = 4,
    MalformedProtobuf = 5,
    ErrorEnd = 6,
    MalformedStreamError = 7,
    InvalidItem = 8,
}

/// Facts derived only from the raw native SANDBOX stream. This is intentionally
/// independent of storage and JobResult/protobuf schemas.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawSandboxFacts {
    pub outcome: RawSandboxOutcome,
    pub exit_code: Option<i32>,
    pub exit_count: u64,
    pub output_item_count: u64,
    pub output_byte_count: u64,
    /// At most 16 fixed flags; currently only the low 9 bits are used.
    pub anomaly_flags: u16,
}

impl Default for RawSandboxFacts {
    fn default() -> Self {
        Self {
            outcome: RawSandboxOutcome::Unknown,
            exit_code: None,
            exit_count: 0,
            output_item_count: 0,
            output_byte_count: 0,
            anomaly_flags: 0,
        }
    }
}

impl RawSandboxFacts {
    pub fn has_anomaly(&self, anomaly: RawSandboxAnomaly) -> bool {
        self.anomaly_flags & (1_u16 << anomaly as u8) != 0
    }
}

/// One-shot channel carrying raw source facts after EOF or source drop.
pub type RawSandboxObservationReceiver = oneshot::Receiver<RawSandboxFacts>;

/// Observe a proven-native SANDBOX stream without transforming or buffering its
/// items. The caller must establish native identity with `sandbox_runner_mut`
/// before calling this function.
pub(super) fn observe_raw_sandbox_stream(
    stream: BoxStream<'static, ResultOutputItem>,
) -> (
    BoxStream<'static, ResultOutputItem>,
    RawSandboxObservationReceiver,
) {
    let (sender, receiver) = oneshot::channel();
    let observed = RawSandboxObserver {
        stream,
        sender: Some(sender),
        facts: RawSandboxFacts::default(),
        source_completed: false,
        saw_end: false,
        invalid: false,
    };
    (Box::pin(observed), receiver)
}

struct RawSandboxObserver {
    stream: BoxStream<'static, ResultOutputItem>,
    sender: Option<oneshot::Sender<RawSandboxFacts>>,
    facts: RawSandboxFacts,
    source_completed: bool,
    saw_end: bool,
    invalid: bool,
}

impl RawSandboxObserver {
    fn mark_anomaly(&mut self, anomaly: RawSandboxAnomaly) {
        self.facts.anomaly_flags |= 1_u16 << anomaly as u8;
        self.invalid = true;
    }

    fn observe_item(&mut self, item: &ResultOutputItem) {
        match item.item.as_ref() {
            Some(Item::Data(_)) if self.saw_end => {
                self.mark_anomaly(RawSandboxAnomaly::DataAfterEnd);
            }
            Some(Item::End(_)) if self.saw_end => {
                self.mark_anomaly(RawSandboxAnomaly::DuplicateEnd);
            }
            Some(Item::End(trailer)) => {
                self.saw_end = true;
                match proto::stream_error::parse_stream_error(trailer) {
                    proto::stream_error::StreamErrorOutcome::Missing => {}
                    proto::stream_error::StreamErrorOutcome::Error(_) => {
                        self.mark_anomaly(RawSandboxAnomaly::ErrorEnd);
                    }
                    proto::stream_error::StreamErrorOutcome::Malformed(_) => {
                        self.mark_anomaly(RawSandboxAnomaly::MalformedStreamError);
                    }
                }
            }
            Some(Item::Data(bytes)) => match SandboxExecResult::decode(bytes.as_slice()) {
                Ok(SandboxExecResult {
                    result: Some(sandbox_exec_result::Result::Exit(exit)),
                }) => {
                    self.facts.exit_count = self.facts.exit_count.saturating_add(1);
                    if self.facts.exit_count == 1 {
                        self.facts.exit_code = Some(exit.exit_code);
                    } else {
                        self.mark_anomaly(RawSandboxAnomaly::DuplicateExit);
                    }
                }
                Ok(SandboxExecResult {
                    result: Some(sandbox_exec_result::Result::Output(output)),
                }) => {
                    self.facts.output_item_count = self.facts.output_item_count.saturating_add(1);
                    self.facts.output_byte_count = self
                        .facts
                        .output_byte_count
                        .saturating_add(u64::try_from(output.data.len()).unwrap_or(u64::MAX));
                }
                Ok(SandboxExecResult { result: None }) | Err(_) => {
                    self.mark_anomaly(RawSandboxAnomaly::MalformedProtobuf);
                }
            },
            Some(Item::FinalCollected(_)) => self.mark_anomaly(RawSandboxAnomaly::InvalidItem),
            None => self.mark_anomaly(RawSandboxAnomaly::InvalidItem),
        }
    }

    fn finish_source(&mut self) {
        if !self.saw_end {
            self.mark_anomaly(RawSandboxAnomaly::MissingEnd);
        }
        if self.facts.exit_count == 0 {
            self.mark_anomaly(RawSandboxAnomaly::MissingExit);
        }
        self.facts.outcome = if self.invalid {
            RawSandboxOutcome::ProtocolInvalid
        } else {
            RawSandboxOutcome::CleanTermination
        };
        self.send_facts();
    }

    fn send_facts(&mut self) {
        if let Some(sender) = self.sender.take() {
            let facts = std::mem::take(&mut self.facts);
            let _ = sender.send(facts);
        }
    }
}

impl Stream for RawSandboxObserver {
    type Item = ResultOutputItem;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.as_mut().get_mut();
        if this.source_completed {
            return Poll::Ready(None);
        }
        match this.stream.as_mut().poll_next(cx) {
            Poll::Ready(Some(item)) => {
                this.observe_item(&item);
                Poll::Ready(Some(item))
            }
            Poll::Ready(None) => {
                this.source_completed = true;
                this.finish_source();
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for RawSandboxObserver {
    fn drop(&mut self) {
        if !self.source_completed {
            self.facts.outcome = RawSandboxOutcome::Aborted;
            self.send_facts();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{StreamExt, stream};
    use jobworkerp_runner::jobworkerp::runner::{
        SandboxExecExit, SandboxExecOutput, SandboxExecResult, sandbox_exec_result,
    };
    use jobworkerp_runner::runner::RunnerSpec;
    use jobworkerp_runner::runner::sandbox::SandboxRunner;
    use prost::Message;
    use proto::jobworkerp::data::{Job, RunnerData, WorkerData, WorkerId};
    use proto::jobworkerp::data::{ResultOutputItem, Trailer, result_output_item::Item};
    use std::collections::HashMap;
    use std::task::Poll;

    fn dispatch_inputs() -> (Job, WorkerId, WorkerData, RunnerData, SandboxRunner) {
        let worker_id = WorkerId { value: 73 };
        let worker = WorkerData {
            runner_id: Some(proto::jobworkerp::data::RunnerId { value: 83 }),
            runner_settings: b"native settings".to_vec(),
            name: "sandbox-worker".to_owned(),
            store_success: true,
            store_failure: true,
            ..Default::default()
        };
        let native_runner = SandboxRunner::new();
        let runner = RunnerData {
            name: native_runner.name(),
            method_proto_map: Some(proto::jobworkerp::data::MethodProtoMap {
                schemas: native_runner.method_proto_map(),
            }),
            ..Default::default()
        };
        let job = Job {
            id: Some(proto::jobworkerp::data::JobId { value: 97 }),
            data: Some(proto::jobworkerp::data::JobData {
                worker_id: Some(worker_id),
                args: b"actual dispatched args".to_vec(),
                timeout: 1234,
                retried: 2,
                using: None,
                overrides: Some(proto::jobworkerp::data::JobExecutionOverrides {
                    store_success: Some(false),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        (job, worker_id, worker, runner, native_runner)
    }

    fn output_item(payload: &[u8]) -> ResultOutputItem {
        ResultOutputItem {
            item: Some(Item::Data(
                SandboxExecResult {
                    result: Some(sandbox_exec_result::Result::Output(SandboxExecOutput {
                        stream: "stdout".to_string(),
                        data: payload.to_vec(),
                    })),
                }
                .encode_to_vec(),
            )),
        }
    }

    fn exit_item(exit_code: i32) -> ResultOutputItem {
        ResultOutputItem {
            item: Some(Item::Data(
                SandboxExecResult {
                    result: Some(sandbox_exec_result::Result::Exit(SandboxExecExit {
                        exit_code,
                        ..Default::default()
                    })),
                }
                .encode_to_vec(),
            )),
        }
    }

    fn end_item() -> ResultOutputItem {
        ResultOutputItem {
            item: Some(Item::End(Trailer::default())),
        }
    }

    fn observe<I>(
        items: I,
    ) -> (
        BoxStream<'static, ResultOutputItem>,
        RawSandboxObservationReceiver,
    )
    where
        I: IntoIterator<Item = ResultOutputItem> + Send + 'static,
        I::IntoIter: Send,
    {
        observe_raw_sandbox_stream(Box::pin(stream::iter(items)))
    }

    #[tokio::test]
    async fn passes_through_exact_items_and_records_nonzero_exit_as_clean_transport() {
        let items = vec![output_item(b"private stdout"), exit_item(7), end_item()];
        let expected = items.clone();
        let (mut observed, receiver) = observe(items);
        let mut actual = Vec::new();
        while let Some(item) = observed.next().await {
            actual.push(item);
        }

        assert_eq!(actual, expected);
        let facts = receiver.await.expect("raw EOF reports facts");
        assert_eq!(facts.outcome, RawSandboxOutcome::CleanTermination);
        assert_eq!(facts.exit_code, Some(7));
        assert_eq!(facts.exit_count, 1);
        assert_eq!(facts.output_item_count, 1);
        assert_eq!(facts.output_byte_count, b"private stdout".len() as u64);
        assert_eq!(facts.anomaly_flags, 0);
        assert!(!format!("{facts:?}").contains("private stdout"));
    }

    #[tokio::test]
    async fn missing_exit_or_end_is_protocol_invalid() {
        let (mut observed, receiver) = observe([end_item()]);
        while observed.next().await.is_some() {}
        let facts = receiver.await.unwrap();
        assert_eq!(facts.outcome, RawSandboxOutcome::ProtocolInvalid);
        assert!(facts.has_anomaly(RawSandboxAnomaly::MissingExit));

        let (mut observed, receiver) = observe([exit_item(0)]);
        while observed.next().await.is_some() {}
        let facts = receiver.await.unwrap();
        assert_eq!(facts.outcome, RawSandboxOutcome::ProtocolInvalid);
        assert!(facts.has_anomaly(RawSandboxAnomaly::MissingEnd));
    }

    #[tokio::test]
    async fn duplicate_exits_and_data_after_end_are_protocol_invalid() {
        let (mut observed, receiver) =
            observe([exit_item(0), exit_item(7), end_item(), output_item(b"late")]);
        while observed.next().await.is_some() {}

        let facts = receiver.await.unwrap();
        assert_eq!(facts.outcome, RawSandboxOutcome::ProtocolInvalid);
        assert_eq!(facts.exit_count, 2);
        assert!(facts.has_anomaly(RawSandboxAnomaly::DuplicateExit));
        assert!(facts.has_anomaly(RawSandboxAnomaly::DataAfterEnd));
    }

    #[tokio::test]
    async fn malformed_protobuf_and_missing_oneof_are_protocol_invalid() {
        for malformed in [
            ResultOutputItem {
                item: Some(Item::Data(vec![0x0a, 0x80])),
            },
            ResultOutputItem {
                item: Some(Item::Data(SandboxExecResult::default().encode_to_vec())),
            },
        ] {
            let (mut observed, receiver) = observe([malformed, end_item()]);
            while observed.next().await.is_some() {}
            let facts = receiver.await.unwrap();
            assert_eq!(facts.outcome, RawSandboxOutcome::ProtocolInvalid);
            assert!(facts.has_anomaly(RawSandboxAnomaly::MalformedProtobuf));
        }
    }

    #[tokio::test]
    async fn error_end_and_malformed_stream_error_are_never_clean() {
        let error_end = ResultOutputItem {
            item: Some(Item::End(
                proto::stream_error::build_stream_error_trailer(
                    HashMap::new(),
                    "EXECUTION_FAILED",
                    "private trailer message",
                    "SANDBOX",
                )
                .unwrap(),
            )),
        };
        let (mut observed, receiver) = observe([exit_item(0), error_end]);
        while observed.next().await.is_some() {}
        let facts = receiver.await.unwrap();
        assert_eq!(facts.outcome, RawSandboxOutcome::ProtocolInvalid);
        assert!(facts.has_anomaly(RawSandboxAnomaly::ErrorEnd));

        let malformed_error = ResultOutputItem {
            item: Some(Item::End(Trailer {
                metadata: HashMap::from([(
                    proto::stream_error::STREAM_ERROR_METADATA_KEY.to_string(),
                    "private malformed text".to_string(),
                )]),
            })),
        };
        let (mut observed, receiver) = observe([exit_item(0), malformed_error]);
        while observed.next().await.is_some() {}
        let facts = receiver.await.unwrap();
        assert_eq!(facts.outcome, RawSandboxOutcome::ProtocolInvalid);
        assert!(facts.has_anomaly(RawSandboxAnomaly::MalformedStreamError));
        assert!(!format!("{facts:?}").contains("private malformed text"));
    }

    #[tokio::test]
    async fn duplicate_end_is_protocol_invalid_but_each_item_is_forwarded() {
        let items = vec![exit_item(0), end_item(), end_item()];
        let expected = items.clone();
        let (mut observed, receiver) = observe(items);
        let mut actual = Vec::new();
        while let Some(item) = observed.next().await {
            actual.push(item);
        }
        assert_eq!(actual, expected);
        let facts = receiver.await.unwrap();
        assert_eq!(facts.outcome, RawSandboxOutcome::ProtocolInvalid);
        assert!(facts.has_anomaly(RawSandboxAnomaly::DuplicateEnd));
    }

    #[tokio::test]
    async fn end_is_yielded_immediately_and_drop_never_reports_clean_eof() {
        let (sender, source) = tokio::sync::mpsc::unbounded_channel();
        let (mut observed, mut receiver) = observe_raw_sandbox_stream(Box::pin(
            tokio_stream::wrappers::UnboundedReceiverStream::new(source),
        ));
        sender.send(end_item()).unwrap();

        assert_eq!(observed.next().await, Some(end_item()));
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));

        drop(observed);
        drop(sender);
        let facts = receiver.await.unwrap();
        assert_eq!(facts.outcome, RawSandboxOutcome::Aborted);
    }

    #[tokio::test]
    async fn dropping_the_receiver_does_not_panic_or_hold_the_stream() {
        let source: BoxStream<'static, ResultOutputItem> =
            Box::pin(stream::iter([exit_item(0), end_item()]));
        let (mut observed, receiver) = observe_raw_sandbox_stream(source);
        drop(receiver);

        while observed.next().await.is_some() {}
    }

    #[tokio::test]
    async fn outer_idle_timeout_drop_is_aborted_not_clean() {
        let source: BoxStream<'static, ResultOutputItem> = Box::pin(stream::pending());
        let (observed, receiver) = observe_raw_sandbox_stream(source);
        let timeout_item = end_item();
        let mut outer = Box::pin(
            super::super::stream_guard::IdleTimeoutStream::with_async_timeout_item(
                observed,
                std::time::Duration::from_millis(1),
                || async {},
                Some(timeout_item.clone()),
            ),
        );

        assert_eq!(outer.next().await, Some(timeout_item));
        let facts = receiver.await.unwrap();
        assert_eq!(facts.outcome, RawSandboxOutcome::Aborted);
    }

    #[tokio::test]
    async fn observer_never_delays_a_pending_source_poll() {
        let source: BoxStream<'static, ResultOutputItem> = Box::pin(stream::pending());
        let (observed, _receiver) = observe_raw_sandbox_stream(source);
        let mut observed = Box::pin(observed);
        let poll = futures::poll! { observed.as_mut().next() };
        assert!(matches!(poll, Poll::Pending));
    }

    #[test]
    fn dispatch_binding_hashes_the_actual_args_settings_and_selected_native_method_schema() {
        let (job, worker_id, worker, runner, native_runner) = dispatch_inputs();
        let binding = capture_native_sandbox_dispatch_binding(
            &job,
            &worker_id,
            &worker,
            &runner,
            &native_runner,
        )
        .expect("typed native SANDBOX and matching stored schema are required");

        let data = job.data.as_ref().unwrap();
        let schema = native_runner.method_proto_map()[proto::DEFAULT_METHOD_NAME].clone();
        assert_eq!(binding.job_id, 97);
        assert_eq!(binding.worker_id, 73);
        assert_eq!(binding.runner_id, 83);
        assert_eq!(
            binding.dispatch_args_sha256,
            proto::sandbox_observation::sha256_digest(&data.args)
        );
        assert_eq!(
            binding.worker_settings_sha256,
            proto::sandbox_observation::sha256_digest(&worker.runner_settings)
        );
        assert_eq!(
            binding.method_schema_sha256,
            proto::sandbox_observation::sha256_digest(&schema.encode_to_vec())
        );
        assert_eq!(binding.using, "");
        assert_eq!(binding.retry_ordinal, 2);
        assert_ne!(binding.host_settings_sha256, [0; 32]);
    }

    #[test]
    fn dispatch_binding_rejects_mismatched_ids_or_a_nonmatching_runner_schema() {
        let (job, worker_id, worker, runner, native_runner) = dispatch_inputs();
        let wrong_worker_id = WorkerId {
            value: worker_id.value + 1,
        };
        assert!(
            capture_native_sandbox_dispatch_binding(
                &job,
                &wrong_worker_id,
                &worker,
                &runner,
                &native_runner,
            )
            .is_none()
        );

        let mut wrong_schema = runner.clone();
        wrong_schema
            .method_proto_map
            .as_mut()
            .unwrap()
            .schemas
            .get_mut(proto::DEFAULT_METHOD_NAME)
            .unwrap()
            .args_proto = "a different dispatch schema".to_owned();
        assert!(
            capture_native_sandbox_dispatch_binding(
                &job,
                &worker_id,
                &worker,
                &wrong_schema,
                &native_runner,
            )
            .is_none()
        );

        let mut missing_runner_id = worker;
        missing_runner_id.runner_id = None;
        assert!(
            capture_native_sandbox_dispatch_binding(
                &job,
                &worker_id,
                &missing_runner_id,
                &runner,
                &native_runner,
            )
            .is_none()
        );
    }

    #[test]
    fn dispatch_binding_uses_the_selected_client_method_and_resolved_job_overrides() {
        let (mut job, worker_id, worker, runner, native_runner) = dispatch_inputs();
        let default_binding = capture_native_sandbox_dispatch_binding(
            &job,
            &worker_id,
            &worker,
            &runner,
            &native_runner,
        )
        .unwrap();

        job.data.as_mut().unwrap().using = Some("run_with_client".to_owned());
        let client_binding = capture_native_sandbox_dispatch_binding(
            &job,
            &worker_id,
            &worker,
            &runner,
            &native_runner,
        )
        .unwrap();
        let client_schema = native_runner.method_proto_map()["run_with_client"].clone();
        assert_eq!(client_binding.using, "run_with_client");
        assert_eq!(
            client_binding.method_schema_sha256,
            proto::sandbox_observation::sha256_digest(&client_schema.encode_to_vec())
        );
        assert_ne!(
            client_binding.method_schema_sha256,
            default_binding.method_schema_sha256
        );

        job.data
            .as_mut()
            .unwrap()
            .overrides
            .as_mut()
            .unwrap()
            .broadcast_results = Some(true);
        let overridden_binding = capture_native_sandbox_dispatch_binding(
            &job,
            &worker_id,
            &worker,
            &runner,
            &native_runner,
        )
        .unwrap();
        assert_ne!(
            overridden_binding.host_settings_sha256,
            client_binding.host_settings_sha256
        );
    }
}
