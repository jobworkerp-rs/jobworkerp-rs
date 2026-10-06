use app::app::job_result::{
    JobResultApp, SandboxObservationFinalizeOutcome, SandboxObservationFinalizeRequest,
};
use proto::jobworkerp::data::{
    JobId, JobResultData, JobResultId, ResultStatus, SandboxExecutionAnomalyCode,
    SandboxExecutionEndState, SandboxExecutionObservation, SandboxExecutionProducerState, WorkerId,
};
use std::sync::Arc;
use std::time::Duration;

use crate::worker::runner::sandbox_observation::{
    RawSandboxAnomaly, RawSandboxFacts, RawSandboxObservationReceiver, RawSandboxOutcome,
    SandboxObservationDispatchBinding,
};

const FINALIZE_TIMEOUT: Duration = Duration::from_secs(5);

/// Raw source evidence paired with the binding captured by the runner after its
/// concrete native SANDBOX downcast.
pub(crate) struct PendingSandboxObservation {
    pub binding: SandboxObservationDispatchBinding,
    pub receiver: RawSandboxObservationReceiver,
}

pub(crate) fn pair_raw_observation(
    binding: Option<SandboxObservationDispatchBinding>,
    receiver: Option<RawSandboxObservationReceiver>,
) -> Option<PendingSandboxObservation> {
    match (binding, receiver) {
        (Some(binding), Some(receiver)) => Some(PendingSandboxObservation { binding, receiver }),
        // A stream without a native dispatch binding, or a binding without a
        // raw stream receiver, is not durable evidence.
        _ => None,
    }
}

pub(super) fn after_initial_store(
    job_result_app: Arc<dyn JobResultApp>,
    result_id: JobResultId,
    data: &JobResultData,
    initial_store: anyhow::Result<bool>,
    pending: Option<PendingSandboxObservation>,
) -> anyhow::Result<Option<tokio::task::JoinHandle<()>>> {
    // False means no row should be handed off. True is necessary but may only
    // describe a cache write, so it is not proof that an RDB row exists.
    let store_reported_success = initial_store?;
    if !store_reported_success {
        return Ok(None);
    }
    match pending {
        Some(pending) => Ok(start_background_admission_task(
            job_result_app,
            result_id,
            data,
            pending,
        )),
        None => Ok(None),
    }
}

/// Start bounded row admission immediately without joining the result lifecycle.
/// This task owns no runner, pool handle, or Worker/AppModule lifecycle object.
pub(super) fn start_background_admission_task(
    job_result_app: Arc<dyn JobResultApp>,
    result_id: JobResultId,
    data: &JobResultData,
    pending: PendingSandboxObservation,
) -> Option<tokio::task::JoinHandle<()>> {
    if !dispatch_binding_matches_result_data(&pending.binding, &result_id, data) {
        return None;
    }

    let identity = PersistedResultIdentity::from_data(&result_id, data)?;
    let binding = pending.binding;
    let receiver = pending.receiver;
    Some(tokio::spawn(async move {
        // Storage implementations may report success for a cache-only write.
        // Keep this bounded RDB admission off the dispatcher/result path.
        let persisted = tokio::time::timeout(
            FINALIZE_TIMEOUT,
            job_result_app.find_job_result_from_db(&identity.result_id),
        )
        .await;
        let row_matches = matches!(
            persisted,
            Ok(Ok(Some(ref row))) if persisted_row_matches(row, &identity)
        );
        // The row can contain raw args/output. Retain only the digest-based
        // identity while waiting for a potentially long-lived source stream.
        drop(persisted);
        if !row_matches {
            return;
        }

        // Hold source facts only after a real RDB row proves their binding.
        let facts = receiver.await.unwrap_or_default();
        persist_sealed_observation(job_result_app, &binding, &identity, facts).await;
    }))
}

#[derive(Debug)]
struct PersistedResultIdentity {
    result_id: JobResultId,
    job_id: JobId,
    worker_id: WorkerId,
    args_sha256: [u8; 32],
    retry_ordinal: u32,
    using: Option<String>,
    status: i32,
}

impl PersistedResultIdentity {
    fn from_data(result_id: &JobResultId, data: &JobResultData) -> Option<Self> {
        Some(Self {
            result_id: *result_id,
            job_id: *data.job_id.as_ref()?,
            worker_id: *data.worker_id.as_ref()?,
            args_sha256: proto::sandbox_observation::sha256_digest(&data.args),
            retry_ordinal: data.retried,
            using: data.using.clone(),
            status: data.status,
        })
    }
}

fn dispatch_binding_matches_result_data(
    binding: &SandboxObservationDispatchBinding,
    result_id: &JobResultId,
    data: &JobResultData,
) -> bool {
    result_id.value > 0
        && binding.job_id > 0
        && binding.worker_id > 0
        && binding.runner_id > 0
        && data
            .job_id
            .as_ref()
            .is_some_and(|id| binding.job_id == id.value)
        && data
            .worker_id
            .as_ref()
            .is_some_and(|id| binding.worker_id == id.value)
        && binding.dispatch_args_sha256.as_slice()
            == proto::sandbox_observation::sha256_digest(&data.args)
        && binding.retry_ordinal == data.retried
        && binding.using == data.using.as_deref().unwrap_or_default()
        && binding.using.len() <= 255
        && ResultStatus::try_from(data.status).is_ok()
}

fn persisted_row_matches(
    row: &proto::jobworkerp::data::JobResult,
    identity: &PersistedResultIdentity,
) -> bool {
    let Some(saved) = row.data.as_ref() else {
        return false;
    };
    row.id
        .as_ref()
        .is_some_and(|saved_id| saved_id.value == identity.result_id.value)
        && saved.job_id.as_ref() == Some(&identity.job_id)
        && saved.worker_id.as_ref() == Some(&identity.worker_id)
        && proto::sandbox_observation::sha256_digest(&saved.args) == identity.args_sha256
        && saved.retried == identity.retry_ordinal
        && saved.status == identity.status
        && saved.using == identity.using
}

async fn persist_sealed_observation(
    job_result_app: Arc<dyn JobResultApp>,
    binding: &SandboxObservationDispatchBinding,
    identity: &PersistedResultIdentity,
    facts: RawSandboxFacts,
) {
    let mut observation =
        build_sealed_observation(binding, &identity.result_id, identity.status, facts);
    if observation.seal().is_err() {
        tracing::warn!(
            result_id = identity.result_id.value,
            job_id = binding.job_id,
            "sandbox observation failed local validation"
        );
        return;
    }

    let request = SandboxObservationFinalizeRequest {
        result_id: identity.result_id,
        observation,
    };
    let finalized = tokio::time::timeout(
        FINALIZE_TIMEOUT,
        job_result_app.finalize_sandbox_observation(&request),
    )
    .await;
    match finalized {
        Ok(Ok(
            SandboxObservationFinalizeOutcome::Stored
            | SandboxObservationFinalizeOutcome::AlreadyIdentical,
        )) => {}
        Ok(Ok(outcome)) => tracing::warn!(
            result_id = identity.result_id.value,
            job_id = binding.job_id,
            outcome = ?outcome,
            "sandbox observation was not finalized"
        ),
        Ok(Err(_)) | Err(_) => tracing::warn!(
            result_id = identity.result_id.value,
            job_id = binding.job_id,
            "sandbox observation finalization failed or timed out"
        ),
    }
}

fn build_sealed_observation(
    binding: &SandboxObservationDispatchBinding,
    result_id: &JobResultId,
    stored_result_status: i32,
    facts: RawSandboxFacts,
) -> SandboxExecutionObservation {
    let producer_state = match facts.outcome {
        RawSandboxOutcome::CleanTermination
            if facts.exit_code.is_some() && facts.anomaly_flags == 0 =>
        {
            SandboxExecutionProducerState::Clean
        }
        RawSandboxOutcome::CleanTermination | RawSandboxOutcome::ProtocolInvalid => {
            SandboxExecutionProducerState::ProtocolInvalid
        }
        RawSandboxOutcome::Aborted => SandboxExecutionProducerState::Aborted,
        RawSandboxOutcome::Unknown => SandboxExecutionProducerState::Unknown,
    };
    let end_state = if facts.has_anomaly(RawSandboxAnomaly::ErrorEnd) {
        SandboxExecutionEndState::Error
    } else {
        match facts.outcome {
            RawSandboxOutcome::CleanTermination
                if producer_state == SandboxExecutionProducerState::Clean =>
            {
                SandboxExecutionEndState::Normal
            }
            RawSandboxOutcome::ProtocolInvalid | RawSandboxOutcome::CleanTermination => {
                SandboxExecutionEndState::Malformed
            }
            RawSandboxOutcome::Aborted | RawSandboxOutcome::Unknown => {
                SandboxExecutionEndState::Unknown
            }
        }
    };

    SandboxExecutionObservation {
        schema_version: proto::sandbox_observation::SANDBOX_OBSERVATION_SCHEMA_VERSION,
        job_id: binding.job_id,
        worker_id: binding.worker_id,
        runner_id: binding.runner_id,
        result_id: result_id.value,
        dispatch_args_sha256: binding.dispatch_args_sha256.to_vec(),
        worker_settings_sha256: binding.worker_settings_sha256.to_vec(),
        method_schema_sha256: binding.method_schema_sha256.to_vec(),
        host_settings_sha256: binding.host_settings_sha256.to_vec(),
        using: binding.using.clone(),
        retry_ordinal: binding.retry_ordinal,
        stored_result_status,
        cli_exit_code: facts.exit_code,
        end_state: end_state as i32,
        producer_state: producer_state as i32,
        anomaly_codes: anomaly_codes(facts.anomaly_flags),
        // The raw observer counts output bytes as one undifferentiated stream;
        // don't mislabel those bytes as stdout/stderr or persist guest payloads.
        stdout_bytes: None,
        stderr_bytes: None,
        stdout_sha256: None,
        stderr_sha256: None,
        trailer_bytes: None,
        trailer_sha256: None,
        observation_sha256: Vec::new(),
    }
}

fn anomaly_codes(flags: u16) -> Vec<i32> {
    let mappings = [
        (
            RawSandboxAnomaly::MissingExit,
            SandboxExecutionAnomalyCode::ExitCodeMissing,
        ),
        (
            RawSandboxAnomaly::MissingEnd,
            SandboxExecutionAnomalyCode::UnknownTerminal,
        ),
        (
            RawSandboxAnomaly::DuplicateEnd,
            SandboxExecutionAnomalyCode::DuplicateTerminal,
        ),
        (
            RawSandboxAnomaly::DuplicateExit,
            SandboxExecutionAnomalyCode::DuplicateTerminal,
        ),
        (
            RawSandboxAnomaly::DataAfterEnd,
            SandboxExecutionAnomalyCode::UnexpectedFrame,
        ),
        (
            RawSandboxAnomaly::MalformedProtobuf,
            SandboxExecutionAnomalyCode::MalformedTerminal,
        ),
        (
            RawSandboxAnomaly::MalformedStreamError,
            SandboxExecutionAnomalyCode::MalformedTerminal,
        ),
        (
            RawSandboxAnomaly::InvalidItem,
            SandboxExecutionAnomalyCode::MalformedTerminal,
        ),
    ];
    let mut codes = Vec::new();
    for (anomaly, code) in mappings {
        if flags & (1_u16 << anomaly as u8) != 0 && !codes.contains(&(code as i32)) {
            codes.push(code as i32);
        }
    }
    codes
}

#[cfg(test)]
mod tests {
    use super::*;
    use app::app::job_result::JobResultApp;
    use proto::jobworkerp::data::{JobId, JobResultData, ResultOutput, WorkerId};
    use std::pin::Pin;
    use std::sync::atomic::{AtomicI64, Ordering};
    use tokio_stream::Stream;

    static NEXT_ID: AtomicI64 = AtomicI64::new(9_100_000);

    #[derive(Debug, Default)]
    struct CacheOnlyJobResultApp {
        find_calls: AtomicI64,
        find_error: bool,
        find_started: Option<Arc<tokio::sync::Notify>>,
        release_find: Option<Arc<tokio::sync::Notify>>,
        finalize_calls: AtomicI64,
        db_result: std::sync::Mutex<Option<proto::jobworkerp::data::JobResult>>,
        finalize_started: Option<Arc<tokio::sync::Notify>>,
        release_finalize: Option<Arc<tokio::sync::Notify>>,
        finalize_done: Option<Arc<tokio::sync::Notify>>,
    }

    #[async_trait::async_trait]
    #[allow(unused_variables)]
    impl JobResultApp for CacheOnlyJobResultApp {
        async fn create_job_result_if_necessary(
            &self,
            id: &JobResultId,
            data: &JobResultData,
            broadcast_result: bool,
        ) -> anyhow::Result<bool> {
            Ok(true)
        }

        async fn finalize_sandbox_observation(
            &self,
            request: &app::app::job_result::SandboxObservationFinalizeRequest,
        ) -> anyhow::Result<app::app::job_result::SandboxObservationFinalizeOutcome> {
            self.finalize_calls.fetch_add(1, Ordering::SeqCst);
            if let Some(started) = self.finalize_started.as_ref() {
                started.notify_one();
            }
            if let Some(release) = self.release_finalize.as_ref() {
                release.notified().await;
            }
            if let Some(done) = self.finalize_done.as_ref() {
                done.notify_one();
            }
            Ok(app::app::job_result::SandboxObservationFinalizeOutcome::Stored)
        }

        async fn delete_job_result(&self, id: &JobResultId) -> anyhow::Result<bool> {
            unimplemented!()
        }

        async fn find_job_result_from_db(
            &self,
            id: &JobResultId,
        ) -> anyhow::Result<Option<proto::jobworkerp::data::JobResult>> {
            self.find_calls.fetch_add(1, Ordering::SeqCst);
            if let Some(started) = self.find_started.as_ref() {
                started.notify_one();
            }
            if let Some(release) = self.release_find.as_ref() {
                release.notified().await;
            }
            if self.find_error {
                anyhow::bail!("RDB lookup failed");
            }
            Ok(self.db_result.lock().unwrap().clone())
        }

        async fn find_job_result_list(
            &self,
            limit: Option<&i32>,
            offset: Option<&i64>,
        ) -> anyhow::Result<Vec<proto::jobworkerp::data::JobResult>> {
            unimplemented!()
        }

        async fn find_job_result_list_by_job_id(
            &self,
            job_id: &JobId,
        ) -> anyhow::Result<Vec<proto::jobworkerp::data::JobResult>> {
            unimplemented!()
        }

        async fn listen_result(
            &self,
            job_id: &JobId,
            worker_id: Option<&WorkerId>,
            worker_name: Option<&String>,
            timeout: Option<u64>,
            request_streaming: bool,
            using: &str,
        ) -> anyhow::Result<(
            proto::jobworkerp::data::JobResult,
            Option<futures::stream::BoxStream<'static, proto::jobworkerp::data::ResultOutputItem>>,
        )> {
            unimplemented!()
        }

        async fn listen_result_by_job_id(
            &self,
            job_id: &JobId,
            timeout: Option<u64>,
            request_streaming: bool,
        ) -> anyhow::Result<(
            proto::jobworkerp::data::JobResult,
            Option<futures::stream::BoxStream<'static, proto::jobworkerp::data::ResultOutputItem>>,
        )> {
            unimplemented!()
        }

        async fn subscribe_stream_by_job_id(
            &self,
            job_id: &JobId,
            timeout: Option<u64>,
        ) -> anyhow::Result<
            Option<futures::stream::BoxStream<'static, proto::jobworkerp::data::ResultOutputItem>>,
        > {
            unimplemented!()
        }

        async fn listen_result_stream_by_worker(
            &self,
            worker_id: Option<&WorkerId>,
            worker_name: Option<&String>,
        ) -> anyhow::Result<
            Pin<Box<dyn Stream<Item = anyhow::Result<proto::jobworkerp::data::JobResult>> + Send>>,
        > {
            unimplemented!()
        }

        async fn count(&self) -> anyhow::Result<i64> {
            unimplemented!()
        }

        async fn find_list_by(
            &self,
            worker_ids: Vec<i64>,
            statuses: Vec<i32>,
            start_time_from: Option<i64>,
            start_time_to: Option<i64>,
            end_time_from: Option<i64>,
            end_time_to: Option<i64>,
            priorities: Vec<i32>,
            uniq_key: Option<String>,
            limit: Option<i32>,
            offset: Option<i64>,
            sort_by: Option<proto::jobworkerp::data::JobResultSortField>,
            ascending: Option<bool>,
        ) -> anyhow::Result<Vec<proto::jobworkerp::data::JobResult>> {
            unimplemented!()
        }

        async fn count_by(
            &self,
            worker_ids: Vec<i64>,
            statuses: Vec<i32>,
            start_time_from: Option<i64>,
            start_time_to: Option<i64>,
            end_time_from: Option<i64>,
            end_time_to: Option<i64>,
            priorities: Vec<i32>,
            uniq_key: Option<String>,
        ) -> anyhow::Result<i64> {
            unimplemented!()
        }

        async fn delete_bulk(
            &self,
            end_time_before: Option<i64>,
            statuses: Vec<i32>,
            worker_ids: Vec<i64>,
        ) -> anyhow::Result<i64> {
            unimplemented!()
        }
    }

    fn result_data() -> JobResultData {
        JobResultData {
            job_id: Some(JobId {
                value: NEXT_ID.fetch_add(2, Ordering::Relaxed),
            }),
            worker_id: Some(WorkerId { value: 73 }),
            args: b"persisted args".to_vec(),
            status: ResultStatus::Success as i32,
            store_success: true,
            store_failure: true,
            retried: 4,
            output: Some(ResultOutput {
                items: b"unchanged output".to_vec(),
            }),
            using: None,
            ..Default::default()
        }
    }

    fn result_id() -> JobResultId {
        JobResultId {
            value: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        }
    }

    fn dispatch_binding(data: &JobResultData) -> SandboxObservationDispatchBinding {
        SandboxObservationDispatchBinding {
            job_id: data.job_id.as_ref().unwrap().value,
            worker_id: data.worker_id.as_ref().unwrap().value,
            runner_id: 83,
            dispatch_args_sha256: proto::sandbox_observation::sha256_digest(&data.args),
            worker_settings_sha256: proto::sandbox_observation::sha256_digest(b"settings"),
            method_schema_sha256: proto::sandbox_observation::sha256_digest(b"method schema"),
            host_settings_sha256: proto::sandbox_observation::sha256_digest(b"dispatch snapshot"),
            using: String::new(),
            retry_ordinal: data.retried,
        }
    }

    fn clean_facts(exit_code: i32) -> RawSandboxFacts {
        RawSandboxFacts {
            outcome: RawSandboxOutcome::CleanTermination,
            exit_code: Some(exit_code),
            exit_count: 1,
            output_item_count: 1,
            output_byte_count: 16,
            anomaly_flags: 0,
        }
    }

    async fn test_app_module() -> app::module::AppModule {
        app::module::test::create_rdb_chan_test_app(true, false)
            .await
            .expect("RDB test app")
    }

    async fn test_result_app() -> Arc<dyn JobResultApp> {
        test_app_module().await.job_result_app.clone()
    }

    async fn save_result(app: &dyn JobResultApp, id: &JobResultId, data: &JobResultData) {
        assert!(
            app.create_job_result_if_necessary(id, data, false)
                .await
                .expect("save initial result")
        );
    }

    async fn observation(
        app: &dyn JobResultApp,
        id: &JobResultId,
    ) -> Option<SandboxExecutionObservation> {
        app.find_sandbox_observation_from_db(id)
            .await
            .expect("DB-backed observation lookup")
    }

    #[tokio::test]
    async fn raw_eof_before_initial_result_save_is_buffered_and_nonzero_exit_is_preserved() {
        let app = test_result_app().await;
        let data = result_data();
        let id = result_id();
        let binding = dispatch_binding(&data);
        let (sender, receiver) = tokio::sync::oneshot::channel();
        sender.send(clean_facts(7)).unwrap();

        save_result(app.as_ref(), &id, &data).await;
        let task = after_initial_store(
            app.clone(),
            id,
            &data,
            Ok(true),
            Some(PendingSandboxObservation { binding, receiver }),
        )
        .expect("valid native dispatch binding")
        .expect("handoff after confirmed initial save");
        task.await.expect("persistence task");

        let observation = observation(app.as_ref(), &id).await.unwrap();
        assert_eq!(observation.cli_exit_code, Some(7));
        assert_eq!(
            observation.stored_result_status,
            ResultStatus::Success as i32
        );
        assert_eq!(
            observation.producer_state,
            proto::jobworkerp::data::SandboxExecutionProducerState::Clean as i32
        );
        assert_eq!(
            observation.end_state,
            proto::jobworkerp::data::SandboxExecutionEndState::Normal as i32
        );
        assert_eq!(observation.stdout_bytes, None);
        assert_eq!(observation.stderr_bytes, None);
        assert_eq!(observation.stdout_sha256, None);
        assert_eq!(observation.stderr_sha256, None);
        assert_eq!(observation.trailer_bytes, None);
        assert_eq!(observation.trailer_sha256, None);
        assert!(!format!("{observation:?}").contains("unchanged output"));

        let stored = app.find_job_result_from_db(&id).await.unwrap().unwrap();
        assert_eq!(stored.data.unwrap().output, data.output);
    }

    #[tokio::test]
    async fn raw_eof_after_initial_result_save_finalizes_the_same_row() {
        let app = test_result_app().await;
        let data = result_data();
        let id = result_id();
        save_result(app.as_ref(), &id, &data).await;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let task = after_initial_store(
            app.clone(),
            id,
            &data,
            Ok(true),
            Some(PendingSandboxObservation {
                binding: dispatch_binding(&data),
                receiver,
            }),
        )
        .expect("valid initial store")
        .expect("valid native dispatch binding");
        sender.send(clean_facts(0)).unwrap();
        task.await.expect("persistence task");

        assert_eq!(
            observation(app.as_ref(), &id).await.unwrap().cli_exit_code,
            Some(0)
        );
    }

    #[tokio::test]
    async fn aborted_or_protocol_invalid_sources_never_become_clean() {
        let app = test_result_app().await;
        for facts in [
            RawSandboxFacts {
                outcome: RawSandboxOutcome::Aborted,
                exit_code: Some(0),
                exit_count: 1,
                ..Default::default()
            },
            RawSandboxFacts {
                outcome: RawSandboxOutcome::ProtocolInvalid,
                exit_code: Some(7),
                exit_count: 1,
                anomaly_flags: 1 << RawSandboxAnomaly::MissingEnd as u8,
                ..Default::default()
            },
        ] {
            let data = result_data();
            let id = result_id();
            save_result(app.as_ref(), &id, &data).await;
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let task = start_background_admission_task(
                app.clone(),
                id,
                &data,
                PendingSandboxObservation {
                    binding: dispatch_binding(&data),
                    receiver,
                },
            )
            .unwrap();
            sender.send(facts.clone()).unwrap();
            task.await.unwrap();
            let observation = observation(app.as_ref(), &id).await.unwrap();
            assert_ne!(
                observation.producer_state,
                proto::jobworkerp::data::SandboxExecutionProducerState::Clean as i32
            );
            assert_eq!(observation.cli_exit_code, facts.exit_code);
        }
    }

    #[test]
    fn error_end_and_missing_source_facts_keep_non_clean_end_states() {
        let data = result_data();
        let binding = dispatch_binding(&data);
        let id = result_id();
        let mut error_end = build_sealed_observation(
            &binding,
            &id,
            data.status,
            RawSandboxFacts {
                outcome: RawSandboxOutcome::ProtocolInvalid,
                exit_code: Some(9),
                exit_count: 1,
                anomaly_flags: 1 << RawSandboxAnomaly::ErrorEnd as u8,
                ..Default::default()
            },
        );
        error_end.seal().unwrap();
        assert_eq!(error_end.cli_exit_code, Some(9));
        assert_eq!(error_end.end_state, SandboxExecutionEndState::Error as i32);
        assert_eq!(
            error_end.producer_state,
            SandboxExecutionProducerState::ProtocolInvalid as i32
        );

        let mut unknown =
            build_sealed_observation(&binding, &id, data.status, RawSandboxFacts::default());
        unknown.seal().unwrap();
        assert_eq!(unknown.cli_exit_code, None);
        assert_eq!(unknown.end_state, SandboxExecutionEndState::Unknown as i32);
        assert_eq!(
            unknown.producer_state,
            SandboxExecutionProducerState::Unknown as i32
        );
    }

    #[tokio::test]
    async fn missing_result_row_and_mismatched_binding_do_not_create_or_seal_a_result() {
        let app = test_result_app().await;
        let data = result_data();
        let id = result_id();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let admission = start_background_admission_task(
            app.clone(),
            id,
            &data,
            PendingSandboxObservation {
                binding: dispatch_binding(&data),
                receiver,
            },
        )
        .expect("valid binding starts an admission task");
        admission.await.unwrap();
        assert!(
            sender.send(clean_facts(0)).is_err(),
            "missing DB proof drops the receiver without awaiting it"
        );
        assert!(app.find_job_result_from_db(&id).await.unwrap().is_none());

        let mut bad_binding = dispatch_binding(&data);
        bad_binding.dispatch_args_sha256 =
            proto::sandbox_observation::sha256_digest(b"different args");
        let (sender, receiver) = tokio::sync::oneshot::channel();
        assert!(
            start_background_admission_task(
                app.clone(),
                id,
                &data,
                PendingSandboxObservation {
                    binding: bad_binding,
                    receiver
                },
            )
            .is_none()
        );
        drop(sender);
        assert!(observation(app.as_ref(), &id).await.is_none());
    }

    #[tokio::test]
    async fn cache_only_store_success_is_not_rdb_proof_and_drops_task_ownership() {
        let fake = Arc::new(CacheOnlyJobResultApp::default());
        let app: Arc<dyn JobResultApp> = fake.clone();
        let data = result_data();
        let id = result_id();
        let initial_store = app.create_job_result_if_necessary(&id, &data, true).await;
        assert!(
            initial_store.as_ref().unwrap(),
            "the hybrid cache may report success without an RDB row"
        );
        let strong_count_before = Arc::strong_count(&app);
        let (sender, receiver) = tokio::sync::oneshot::channel();

        let admission = after_initial_store(
            app.clone(),
            id,
            &data,
            initial_store,
            Some(PendingSandboxObservation {
                binding: dispatch_binding(&data),
                receiver,
            }),
        )
        .unwrap()
        .expect("cache-only success begins detached RDB admission");
        admission.await.unwrap();

        assert_eq!(Arc::strong_count(&app), strong_count_before);
        assert_eq!(fake.find_calls.load(Ordering::SeqCst), 1);
        assert!(sender.send(clean_facts(0)).is_err());
        assert_eq!(fake.finalize_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn initial_store_error_drops_the_receiver_without_a_db_probe_or_background_task() {
        let fake = Arc::new(CacheOnlyJobResultApp::default());
        let app: Arc<dyn JobResultApp> = fake.clone();
        let data = result_data();
        let id = result_id();
        let strong_count_before = Arc::strong_count(&app);
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let error = anyhow::anyhow!("initial result save failed");

        let returned_error = after_initial_store(
            app.clone(),
            id,
            &data,
            Err(error),
            Some(PendingSandboxObservation {
                binding: dispatch_binding(&data),
                receiver,
            }),
        )
        .expect_err("initial result-storage errors remain authoritative");

        assert_eq!(returned_error.to_string(), "initial result save failed");
        assert_eq!(fake.find_calls.load(Ordering::SeqCst), 0);
        assert_eq!(fake.finalize_calls.load(Ordering::SeqCst), 0);
        assert_eq!(Arc::strong_count(&app), strong_count_before);
        assert!(sender.send(clean_facts(0)).is_err());
    }

    #[tokio::test]
    async fn initial_store_false_never_hands_off_even_if_a_matching_row_exists() {
        let data = result_data();
        let id = result_id();
        let fake = Arc::new(CacheOnlyJobResultApp {
            db_result: std::sync::Mutex::new(Some(proto::jobworkerp::data::JobResult {
                id: Some(id),
                data: Some(data.clone()),
                ..Default::default()
            })),
            ..Default::default()
        });
        let app: Arc<dyn JobResultApp> = fake.clone();
        let strong_count_before = Arc::strong_count(&app);
        let (sender, receiver) = tokio::sync::oneshot::channel();

        assert!(
            after_initial_store(
                app.clone(),
                id,
                &data,
                Ok(false),
                Some(PendingSandboxObservation {
                    binding: dispatch_binding(&data),
                    receiver,
                }),
            )
            .unwrap()
            .is_none()
        );

        assert_eq!(fake.find_calls.load(Ordering::SeqCst), 0);
        assert_eq!(fake.finalize_calls.load(Ordering::SeqCst), 0);
        assert_eq!(Arc::strong_count(&app), strong_count_before);
        assert!(sender.send(clean_facts(0)).is_err());
    }

    #[tokio::test]
    async fn database_lookup_error_drops_the_receiver_without_finalizing() {
        let data = result_data();
        let id = result_id();
        let fake = Arc::new(CacheOnlyJobResultApp {
            find_error: true,
            ..Default::default()
        });
        let app: Arc<dyn JobResultApp> = fake.clone();
        let (sender, receiver) = tokio::sync::oneshot::channel();

        let admission = after_initial_store(
            app,
            id,
            &data,
            Ok(true),
            Some(PendingSandboxObservation {
                binding: dispatch_binding(&data),
                receiver,
            }),
        )
        .unwrap()
        .expect("true store result starts detached admission");
        admission.await.unwrap();

        assert_eq!(fake.find_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fake.finalize_calls.load(Ordering::SeqCst), 0);
        assert!(sender.send(clean_facts(0)).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn blocked_database_admission_times_out_without_waiting_for_raw_eof() {
        let data = result_data();
        let id = result_id();
        let find_started = Arc::new(tokio::sync::Notify::new());
        let fake = Arc::new(CacheOnlyJobResultApp {
            find_started: Some(find_started.clone()),
            release_find: Some(Arc::new(tokio::sync::Notify::new())),
            ..Default::default()
        });
        let app: Arc<dyn JobResultApp> = fake.clone();
        let (sender, receiver) = tokio::sync::oneshot::channel();

        let task = after_initial_store(
            app,
            id,
            &data,
            Ok(true),
            Some(PendingSandboxObservation {
                binding: dispatch_binding(&data),
                receiver,
            }),
        )
        .expect("initial-save success starts a detached admission task")
        .expect("native dispatch binding is valid");
        find_started.notified().await;
        tokio::time::advance(FINALIZE_TIMEOUT).await;
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("timed-out admission task does not wait for raw EOF")
            .expect("admission task completes");

        assert_eq!(fake.find_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fake.finalize_calls.load(Ordering::SeqCst), 0);
        assert!(sender.send(clean_facts(0)).is_err());
    }

    #[tokio::test]
    async fn row_with_a_different_result_id_is_not_persistence_proof() {
        let data = result_data();
        let id = result_id();
        let fake = Arc::new(CacheOnlyJobResultApp {
            db_result: std::sync::Mutex::new(Some(proto::jobworkerp::data::JobResult {
                id: Some(result_id()),
                data: Some(data.clone()),
                ..Default::default()
            })),
            ..Default::default()
        });
        let app: Arc<dyn JobResultApp> = fake.clone();
        let (sender, receiver) = tokio::sync::oneshot::channel();

        let admission = after_initial_store(
            app,
            id,
            &data,
            Ok(true),
            Some(PendingSandboxObservation {
                binding: dispatch_binding(&data),
                receiver,
            }),
        )
        .unwrap()
        .expect("true store result starts detached admission");
        admission.await.unwrap();

        assert_eq!(fake.find_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fake.finalize_calls.load(Ordering::SeqCst), 0);
        assert!(sender.send(clean_facts(0)).is_err());
    }

    #[tokio::test]
    async fn reconstructed_worker_flags_do_not_replace_execution_row_proof() {
        let data = result_data();
        let id = result_id();
        let mut reconstructed = data.clone();
        reconstructed.store_success = !data.store_success;
        reconstructed.store_failure = !data.store_failure;
        reconstructed.broadcast_results = !data.broadcast_results;
        reconstructed.response_type = proto::jobworkerp::data::ResponseType::NoResult as i32;
        reconstructed.max_retry = data.max_retry.saturating_add(1);
        let fake = Arc::new(CacheOnlyJobResultApp {
            db_result: std::sync::Mutex::new(Some(proto::jobworkerp::data::JobResult {
                id: Some(id),
                data: Some(reconstructed),
                ..Default::default()
            })),
            ..Default::default()
        });
        let app: Arc<dyn JobResultApp> = fake.clone();
        let (sender, receiver) = tokio::sync::oneshot::channel();

        let task = after_initial_store(
            app,
            id,
            &data,
            Ok(true),
            Some(PendingSandboxObservation {
                binding: dispatch_binding(&data),
                receiver,
            }),
        )
        .unwrap()
        .expect("execution identity matches despite reconstructed worker flags");
        sender.send(clean_facts(0)).unwrap();
        task.await.unwrap();

        assert_eq!(fake.finalize_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn blocked_cas_stays_off_the_result_completion_path() {
        let data = result_data();
        let id = result_id();
        let entered_finalize = Arc::new(tokio::sync::Notify::new());
        let release_finalize = Arc::new(tokio::sync::Notify::new());
        let fake = Arc::new(CacheOnlyJobResultApp {
            db_result: std::sync::Mutex::new(Some(proto::jobworkerp::data::JobResult {
                id: Some(id),
                data: Some(data.clone()),
                ..Default::default()
            })),
            finalize_started: Some(entered_finalize.clone()),
            release_finalize: Some(release_finalize.clone()),
            ..Default::default()
        });
        let app: Arc<dyn JobResultApp> = fake.clone();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let task = after_initial_store(
            app,
            id,
            &data,
            Ok(true),
            Some(PendingSandboxObservation {
                binding: dispatch_binding(&data),
                receiver,
            }),
        )
        .unwrap()
        .expect("matching RDB row starts detached witness task");

        sender.send(clean_facts(0)).unwrap();
        entered_finalize.notified().await;
        assert_eq!(fake.finalize_calls.load(Ordering::SeqCst), 1);
        assert!(!task.is_finished(), "the CAS is deliberately blocked");

        release_finalize.notify_one();
        task.await.unwrap();
        assert_eq!(fake.finalize_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn result_processor_returns_and_completes_stream_before_blocked_database_admission() {
        let mut app_module = test_app_module().await;
        let find_started = Arc::new(tokio::sync::Notify::new());
        let release_find = Arc::new(tokio::sync::Notify::new());
        let entered_finalize = Arc::new(tokio::sync::Notify::new());
        let release_finalize = Arc::new(tokio::sync::Notify::new());
        let finalize_done = Arc::new(tokio::sync::Notify::new());
        let data = JobResultData {
            job_id: Some(JobId {
                value: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            }),
            worker_id: Some(WorkerId { value: 73 }),
            status: ResultStatus::Success as i32,
            response_type: proto::jobworkerp::data::ResponseType::Direct as i32,
            ..Default::default()
        };
        let id = result_id();
        let fake = Arc::new(CacheOnlyJobResultApp {
            db_result: std::sync::Mutex::new(Some(proto::jobworkerp::data::JobResult {
                id: Some(id),
                data: Some(data.clone()),
                ..Default::default()
            })),
            find_started: Some(find_started.clone()),
            release_find: Some(release_find.clone()),
            finalize_started: Some(entered_finalize.clone()),
            release_finalize: Some(release_finalize.clone()),
            finalize_done: Some(finalize_done.clone()),
            ..Default::default()
        });
        app_module.job_result_app = fake.clone();
        let processor = crate::worker::result_processor::ResultProcessorImpl::new(
            app_module.config_module.clone(),
            Arc::new(app_module),
        );
        let (raw_sender, raw_receiver) = tokio::sync::oneshot::channel();

        let mut process_result = Box::pin(processor.process_result_with_observation(
            proto::jobworkerp::data::JobResult {
                id: Some(id),
                data: Some(data.clone()),
                ..Default::default()
            },
            Some(Box::pin(futures::stream::iter(Vec::<
                proto::jobworkerp::data::ResultOutputItem,
            >::new()))),
            proto::jobworkerp::data::WorkerData {
                use_static: true,
                ..Default::default()
            },
            Some(PendingSandboxObservation {
                binding: dispatch_binding(&data),
                receiver: raw_receiver,
            }),
        ));
        let find_started_signal = find_started.notified();
        tokio::pin!(find_started_signal);
        let (processed_result, returned_before_probe_release) = tokio::select! {
            result = &mut process_result => {
                find_started_signal.await;
                (result, true)
            }
            _ = &mut find_started_signal => {
                match futures::poll!(&mut process_result) {
                    std::task::Poll::Ready(result) => (result, true),
                    std::task::Poll::Pending => {
                        release_find.notify_one();
                        (process_result.await, false)
                    }
                }
            }
        };
        let (result, completion_rx) = processed_result
            .expect("normal result processing is not coupled to database admission");

        assert_eq!(result.id, Some(id));
        assert_eq!(fake.find_calls.load(Ordering::SeqCst), 1);
        let completion_rx = completion_rx.expect("stream completion receiver");
        raw_sender.send(clean_facts(0)).unwrap();
        tokio::time::timeout(Duration::from_secs(2), completion_rx)
            .await
            .expect("stream publisher and job cleanup finish while admission is pending")
            .expect("completion guard is released");
        if returned_before_probe_release {
            assert_eq!(fake.finalize_calls.load(Ordering::SeqCst), 0);
            release_find.notify_one();
        }

        tokio::time::timeout(Duration::from_secs(2), entered_finalize.notified())
            .await
            .expect("raw EOF starts finalization");
        assert_eq!(fake.finalize_calls.load(Ordering::SeqCst), 1);

        release_finalize.notify_one();
        tokio::time::timeout(Duration::from_secs(2), finalize_done.notified())
            .await
            .expect("detached finalization task finishes after CAS unblocks");
        assert_eq!(fake.finalize_calls.load(Ordering::SeqCst), 1);
        assert!(
            returned_before_probe_release,
            "the production result processor must not wait for bounded RDB admission"
        );
    }

    #[tokio::test]
    async fn result_configured_not_to_store_has_no_witness_task() {
        let app = test_result_app().await;
        let mut data = result_data();
        data.store_success = false;
        data.store_failure = false;
        let id = result_id();
        let initial_store = app
            .create_job_result_if_necessary(&id, &data, false)
            .await
            .unwrap();
        assert!(!initial_store);

        let (sender, receiver) = tokio::sync::oneshot::channel();
        assert!(
            after_initial_store(
                app.clone(),
                id,
                &data,
                Ok(initial_store),
                Some(PendingSandboxObservation {
                    binding: dispatch_binding(&data),
                    receiver,
                }),
            )
            .unwrap()
            .is_none()
        );
        assert!(sender.send(clean_facts(0)).is_err());
        assert!(app.find_job_result_from_db(&id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn dispatch_identity_mismatches_are_rejected_before_storage() {
        let app = test_result_app().await;
        let data = result_data();
        let id = result_id();
        save_result(app.as_ref(), &id, &data).await;

        let mut invalid_bindings = Vec::new();
        let mut wrong_job = dispatch_binding(&data);
        wrong_job.job_id += 1;
        invalid_bindings.push(wrong_job);
        let mut wrong_worker = dispatch_binding(&data);
        wrong_worker.worker_id += 1;
        invalid_bindings.push(wrong_worker);
        let mut wrong_retry = dispatch_binding(&data);
        wrong_retry.retry_ordinal += 1;
        invalid_bindings.push(wrong_retry);
        let mut wrong_using = dispatch_binding(&data);
        wrong_using.using = "run_with_client".to_owned();
        invalid_bindings.push(wrong_using);

        for binding in invalid_bindings {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            assert!(
                start_background_admission_task(
                    app.clone(),
                    id,
                    &data,
                    PendingSandboxObservation { binding, receiver },
                )
                .is_none()
            );
            drop(sender);
        }
        assert!(observation(app.as_ref(), &id).await.is_none());
    }

    #[tokio::test]
    async fn mismatched_persisted_execution_fields_never_start_the_witness_task() {
        let app = test_result_app().await;
        let stored = result_data();
        let id = result_id();
        save_result(app.as_ref(), &id, &stored).await;

        let mut mismatches = Vec::new();
        let mut changed = stored.clone();
        changed.args.push(0);
        mismatches.push(changed);
        let mut changed = stored.clone();
        changed.status = ResultStatus::Cancelled as i32;
        mismatches.push(changed);
        let mut changed = stored.clone();
        changed.retried += 1;
        mismatches.push(changed);
        let mut changed = stored.clone();
        changed.using = Some("run_with_client".to_owned());
        mismatches.push(changed);
        let mut changed = stored.clone();
        changed.job_id.as_mut().unwrap().value += 1;
        mismatches.push(changed);
        let mut changed = stored.clone();
        changed.worker_id.as_mut().unwrap().value += 1;
        mismatches.push(changed);

        for expected_data in mismatches {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let mut binding = dispatch_binding(&expected_data);
            binding.using = expected_data.using.clone().unwrap_or_default();
            let admission = start_background_admission_task(
                app.clone(),
                id,
                &expected_data,
                PendingSandboxObservation { binding, receiver },
            )
            .expect("dispatch binding matches each candidate result");
            admission.await.unwrap();
            assert!(sender.send(clean_facts(0)).is_err());
        }
        assert!(observation(app.as_ref(), &id).await.is_none());
    }

    #[tokio::test]
    async fn finalization_is_backgrounded_and_conflicts_preserve_the_first_sealed_value() {
        let app = test_result_app().await;
        let data = result_data();
        let id = result_id();
        save_result(app.as_ref(), &id, &data).await;
        let binding = dispatch_binding(&data);
        let mut first = build_sealed_observation(&binding, &id, data.status, clean_facts(3));
        first.seal().unwrap();
        assert_eq!(
            app.finalize_sandbox_observation(
                &app::app::job_result::SandboxObservationFinalizeRequest {
                    result_id: id,
                    observation: first.clone(),
                }
            )
            .await
            .unwrap(),
            app::app::job_result::SandboxObservationFinalizeOutcome::Stored
        );

        let (sender, receiver) = tokio::sync::oneshot::channel();
        let task = start_background_admission_task(
            app.clone(),
            id,
            &data,
            PendingSandboxObservation { binding, receiver },
        )
        .unwrap();
        assert!(
            !task.is_finished(),
            "waiting for raw EOF must not block dispatch"
        );
        sender.send(clean_facts(4)).unwrap();
        task.await.unwrap();
        assert_eq!(observation(app.as_ref(), &id).await, Some(first));
    }
}
