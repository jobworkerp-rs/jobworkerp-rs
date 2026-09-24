use std::{
    collections::HashMap,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use async_stream::stream;
use async_trait::async_trait;
use command_utils::protobuf::resolve::resolve_proto_imports;
use futures::{StreamExt, stream::BoxStream};
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use microsandbox::{ExecControl, ExecEvent};
use prost::Message;
use proto::DEFAULT_METHOD_NAME;
use proto::jobworkerp::data::{
    JobData, JobId, JobResult, ResultOutputItem, StreamingOutputType, Trailer,
    result_output_item::Item,
};
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    jobworkerp::runner::{
        SandboxExecArgs, SandboxExecExit, SandboxExecOutput, SandboxExecResult,
        SandboxRunnerSettings, sandbox_exec_result,
    },
    schema_to_json_string,
};

use super::{
    CollectStreamFuture, FeedData, RunnerSpec, RunnerTrait,
    cancellation::CancelMonitoring,
    cancellation_helper::{CancelMonitoringHelper, UseCancelMonitoringHelper},
};
#[cfg(test)]
use client_feed::{ClientInputSink, forward_client_feed};
use client_feed::{ExecInputSink, FeedTaskGuard};
use runtime::SandboxRuntime;

#[path = "sandbox/cleanup.rs"]
mod cleanup;
#[path = "sandbox/client_feed.rs"]
mod client_feed;
#[path = "sandbox/config.rs"]
mod config;
#[path = "sandbox/runtime.rs"]
mod runtime;

pub use cleanup::{SandboxCleanupFuture, SandboxCleanupRegistration, SandboxCleanupRegistry};
pub use config::SandboxMode;
pub use config::{
    ResolvedMount, ResolvedSandboxVm, ValidatedNetworkConfig, ValidatedSandboxSettings,
    build_network_policy, resolve_execution_settings, validate_settings,
};

const METHOD_RUN_WITH_CLIENT: &str = "run_with_client";
const CLIENT_FEED_CAPACITY: usize = 16;
const EXEC_KILL_TIMEOUT: Duration = Duration::from_secs(3);
const STREAM_ERROR_KEY: &str = proto::stream_error::STREAM_ERROR_METADATA_KEY;
const RUNNER_NAME: &str = "SANDBOX";

const SETTINGS_PROTO: &str =
    include_str!("../../protobuf/jobworkerp/runner/sandbox_settings.proto");
const ARGS_PROTO: &str = include_str!("../../protobuf/jobworkerp/runner/sandbox_args.proto");
const RESULT_PROTO: &str = include_str!("../../protobuf/jobworkerp/runner/sandbox_result.proto");
const COMMON_PROTO: &str = include_str!("../../protobuf/jobworkerp/runner/sandbox_common.proto");
const COMMON_PROTO_IMPORT: &str = "jobworkerp/runner/sandbox_common.proto";

static PROCESS_GENERATION: LazyLock<Uuid> = LazyLock::new(Uuid::new_v4);
static RESOLVED_SETTINGS_PROTO: LazyLock<String> = LazyLock::new(|| {
    resolve_proto_imports(SETTINGS_PROTO, &[(COMMON_PROTO_IMPORT, COMMON_PROTO)]).unwrap_or_else(
        |error| {
            tracing::error!(%error, "failed to inline SANDBOX settings proto imports");
            String::new()
        },
    )
});
static RESOLVED_ARGS_PROTO: LazyLock<String> = LazyLock::new(|| {
    resolve_proto_imports(ARGS_PROTO, &[(COMMON_PROTO_IMPORT, COMMON_PROTO)]).unwrap_or_else(
        |error| {
            tracing::error!(%error, "failed to inline SANDBOX args proto imports");
            String::new()
        },
    )
});

#[derive(Clone, Debug)]
pub struct SandboxExecutionContext {
    pub worker_id: proto::jobworkerp::data::WorkerId,
    pub mode: SandboxMode,
    pub job_id: Option<JobId>,
}

impl SandboxExecutionContext {
    pub fn static_worker(worker_id: proto::jobworkerp::data::WorkerId) -> Self {
        Self {
            worker_id,
            mode: SandboxMode::Static,
            job_id: None,
        }
    }

    pub fn non_static_worker(worker_id: proto::jobworkerp::data::WorkerId) -> Self {
        Self {
            worker_id,
            mode: SandboxMode::NonStatic,
            job_id: None,
        }
    }

    pub fn with_job_id(mut self, job_id: JobId) -> Self {
        self.job_id = Some(job_id);
        self
    }
}

pub struct SandboxRunner {
    worker_id: Option<proto::jobworkerp::data::WorkerId>,
    job_id: Option<JobId>,
    mode: Option<SandboxMode>,
    settings: Option<ValidatedSandboxSettings>,
    static_runtime: Option<Arc<SandboxRuntime>>,
    runner_id: Uuid,
    cancel_helper: Option<CancelMonitoringHelper>,
    active_cancel: Arc<Mutex<Option<CancellationToken>>>,
    active_control: Arc<Mutex<Option<ExecControl>>>,
    active_generation: Arc<Mutex<Option<Uuid>>>,
    active_idle_timeout: Arc<AtomicBool>,
    reusable: Arc<AtomicBool>,
    feed_receiver: Option<mpsc::Receiver<FeedData>>,
    cleanup_registry: Option<SandboxCleanupRegistry>,
}

impl std::fmt::Debug for SandboxRunner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SandboxRunner")
            .field("worker_id", &self.worker_id.as_ref().map(|id| id.value))
            .field("job_id", &self.job_id.as_ref().map(|id| id.value))
            .field("mode", &self.mode)
            .field("loaded", &self.settings.is_some())
            .field("has_static_runtime", &self.static_runtime.is_some())
            .field("reusable", &self.is_reusable())
            .finish_non_exhaustive()
    }
}

impl SandboxRunner {
    pub fn new() -> Self {
        Self::with_cancel_monitoring(None)
    }

    pub fn new_with_context(context: SandboxExecutionContext) -> Self {
        let mut runner = Self::new();
        runner.worker_id = Some(context.worker_id);
        runner.job_id = context.job_id;
        runner.mode = Some(context.mode);
        runner
    }

    pub fn new_with_cancel_monitoring(cancel_helper: CancelMonitoringHelper) -> Self {
        Self::with_cancel_monitoring(Some(cancel_helper))
    }

    fn with_cancel_monitoring(cancel_helper: Option<CancelMonitoringHelper>) -> Self {
        Self {
            worker_id: None,
            job_id: None,
            mode: None,
            settings: None,
            static_runtime: None,
            runner_id: Uuid::new_v4(),
            cancel_helper,
            active_cancel: Arc::new(Mutex::new(None)),
            active_control: Arc::new(Mutex::new(None)),
            active_generation: Arc::new(Mutex::new(None)),
            active_idle_timeout: Arc::new(AtomicBool::new(false)),
            reusable: Arc::new(AtomicBool::new(true)),
            feed_receiver: None,
            cleanup_registry: None,
        }
    }

    /// Set trusted Worker identity and pool mode before `load()`.
    pub fn set_worker_context(
        &mut self,
        worker_id: proto::jobworkerp::data::WorkerId,
        mode: SandboxMode,
    ) -> Result<()> {
        ensure!(
            self.settings.is_none(),
            "SANDBOX worker context cannot change after load"
        );
        self.worker_id = Some(worker_id);
        self.mode = Some(mode);
        Ok(())
    }

    /// Set the trusted current job ID before registering client input or starting a job.
    pub fn set_job_context(&mut self, job_id: JobId) {
        self.job_id = Some(job_id);
    }

    /// Set the process cleanup coordinator before loading SANDBOX settings.
    pub fn set_cleanup_registry(&mut self, registry: SandboxCleanupRegistry) -> Result<()> {
        ensure!(
            self.settings.is_none(),
            "SANDBOX cleanup registry cannot change after load"
        );
        self.cleanup_registry = Some(registry);
        Ok(())
    }

    pub fn clear_job_context(&mut self) {
        self.job_id = None;
        self.feed_receiver = None;
    }

    /// Mark the active `run` stream as a Worker idle timeout before cancelling its exec.
    pub async fn signal_idle_timeout(&self) {
        self.active_idle_timeout.store(true, Ordering::Release);
        if let Some(token) = self.active_cancel.lock().await.as_ref() {
            token.cancel();
        }
    }

    pub fn is_reusable(&self) -> bool {
        self.reusable.load(Ordering::Acquire)
    }

    /// Confirm the owned static VM still has the same identity and a live agent connection.
    pub async fn verify_before_reuse(&self) -> Result<()> {
        ensure!(
            self.mode()? == SandboxMode::Static,
            "only static SANDBOX VMs are pool-reusable"
        );
        ensure!(
            self.is_reusable(),
            "SANDBOX Runner was marked unsafe to reuse"
        );
        let runtime = self
            .static_runtime
            .as_ref()
            .ok_or_else(|| anyhow!("static SANDBOX VM is not available"))?;
        if let Err(error) = runtime.verify_connection().await {
            self.reusable.store(false, Ordering::Release);
            return Err(error);
        }
        Ok(())
    }

    fn mode(&self) -> Result<SandboxMode> {
        self.mode
            .ok_or_else(|| anyhow!("trusted SANDBOX static mode was not provided by the host"))
    }

    fn worker_id(&self) -> Result<proto::jobworkerp::data::WorkerId> {
        self.worker_id
            .ok_or_else(|| anyhow!("trusted SANDBOX Worker ID was not provided by the host"))
    }

    async fn cancellation_token(&self) -> CancellationToken {
        match self.cancel_helper.as_ref() {
            Some(helper) => helper.get_cancellation_token().await,
            None => CancellationToken::new(),
        }
    }

    async fn run_stream_inner(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
        client_stream: bool,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        let feed_receiver = if client_stream {
            Some(self.feed_receiver.take().ok_or_else(|| {
                anyhow!("run_with_client requires a registered client feed channel")
            })?)
        } else {
            self.feed_receiver = None;
            None
        };
        let mode = self.mode()?;
        let worker_id = self.worker_id()?;
        let settings = self
            .settings
            .as_ref()
            .ok_or_else(|| anyhow!("SANDBOX Runner has not been loaded"))?;
        ensure!(self.is_reusable(), "SANDBOX Runner is not safe to reuse");

        let args = ProstMessageCodec::deserialize_message::<SandboxExecArgs>(arg)
            .context("failed to decode SandboxExecArgs")?;
        ensure!(
            !args.command.trim().is_empty() && !args.command.contains('\0'),
            "SANDBOX command must be a non-empty literal without NUL bytes"
        );
        ensure!(
            args.args.iter().all(|value| !value.contains('\0')),
            "SANDBOX command arguments cannot contain NUL bytes"
        );
        if client_stream {
            ensure!(
                args.stdin.is_none() && args.tty.is_none(),
                "run_with_client does not accept stdin or tty arguments"
            );
        }
        let resolved = resolve_execution_settings(settings, &args, mode)?;

        let external_cancel = self.cancellation_token().await;
        ensure!(
            self.is_reusable() && !external_cancel.is_cancelled(),
            "SANDBOX job was cancelled before sandbox creation"
        );
        let non_static_name = if mode == SandboxMode::NonStatic {
            let job_id = self
                .job_id
                .ok_or_else(|| anyhow!("trusted SANDBOX job ID was not provided by the host"))?;
            let exec_id = Uuid::new_v4();
            Some(build_sandbox_name(
                worker_id,
                Some(job_id),
                &PROCESS_GENERATION.to_string(),
                &exec_id.to_string(),
            )?)
        } else {
            None
        };
        let local_cancel = CancellationToken::new();
        let mut cleanup_registration = match self.cleanup_registry.as_ref() {
            Some(registry) => Some(registry.register_task(Some(local_cancel.clone())).await?),
            None => None,
        };
        let generation = Uuid::new_v4();
        self.active_idle_timeout.store(false, Ordering::Release);
        *self.active_cancel.lock().await = Some(local_cancel.clone());
        *self.active_generation.lock().await = Some(generation);
        let active_cleanup = ActiveExecCleanupState::default();
        let mut setup_guard = ExecSetupGuard::new(active_cleanup.clone());
        let runtime = match mode {
            SandboxMode::Static => match self.static_runtime.clone() {
                Some(runtime) => runtime,
                None => {
                    clear_active_generation(
                        &self.active_generation,
                        &self.active_cancel,
                        &self.active_control,
                        generation,
                    )
                    .await;
                    if let Some(registration) = cleanup_registration.take() {
                        registration.finish();
                    }
                    bail!("static SANDBOX VM was not created during load")
                }
            },
            SandboxMode::NonStatic => {
                let name = non_static_name.expect("non-static VM name was prepared");
                if let Some(mut registration) = cleanup_registration.take() {
                    let vm_settings = resolved.vm.clone();
                    let network = resolved.network.clone();
                    let cleanup_cancel = local_cancel.clone();
                    let cleanup_state = active_cleanup.clone();
                    let active_generation = self.active_generation.clone();
                    let active_cancel = self.active_cancel.clone();
                    let active_control = self.active_control.clone();
                    let task =
                        tokio::spawn(async move {
                            let runtime =
                                match SandboxRuntime::create(name, &vm_settings, network.as_ref())
                                    .await
                                {
                                    Ok(runtime) => runtime,
                                    Err(error) => {
                                        registration.finish();
                                        return Err(error);
                                    }
                                };
                            if let Err(error) = registration
                                .set_cleanup(Box::pin(
                                    ActiveExecCleanup {
                                        runtime: runtime.clone(),
                                        cancellation: cleanup_cancel,
                                        cleanup_state,
                                        mode: SandboxMode::NonStatic,
                                        active_generation,
                                        active_cancel,
                                        active_control,
                                        generation,
                                    }
                                    .run(),
                                ))
                                .await
                            {
                                runtime.cleanup_now().await;
                                registration.finish();
                                return Err(error);
                            }
                            runtime.mark_registry_managed();
                            Ok((runtime, registration))
                        });
                    match task.await {
                        Ok(Ok((runtime, registration))) => {
                            cleanup_registration = Some(registration);
                            runtime
                        }
                        Ok(Err(error)) => {
                            clear_active_generation(
                                &self.active_generation,
                                &self.active_cancel,
                                &self.active_control,
                                generation,
                            )
                            .await;
                            return Err(error);
                        }
                        Err(error) => {
                            clear_active_generation(
                                &self.active_generation,
                                &self.active_cancel,
                                &self.active_control,
                                generation,
                            )
                            .await;
                            return Err(anyhow!(error).context("SANDBOX VM creation task failed"));
                        }
                    }
                } else {
                    match SandboxRuntime::create(name, &resolved.vm, resolved.network.as_ref())
                        .await
                    {
                        Ok(runtime) => runtime,
                        Err(error) => {
                            clear_active_generation(
                                &self.active_generation,
                                &self.active_cancel,
                                &self.active_control,
                                generation,
                            )
                            .await;
                            return Err(error);
                        }
                    }
                }
            }
        };
        if mode == SandboxMode::Static
            && let Some(registration) = cleanup_registration.as_mut()
        {
            let cleanup_cancel = local_cancel.clone();
            let cleanup_state = active_cleanup.clone();
            let active_generation = self.active_generation.clone();
            let active_cancel = self.active_cancel.clone();
            let active_control = self.active_control.clone();
            registration
                .set_cleanup(Box::pin(
                    ActiveExecCleanup {
                        runtime: runtime.clone(),
                        cancellation: cleanup_cancel,
                        cleanup_state,
                        mode,
                        active_generation,
                        active_cancel,
                        active_control,
                        generation,
                    }
                    .run(),
                ))
                .await?;
        }
        if !self.is_reusable() || external_cancel.is_cancelled() || local_cancel.is_cancelled() {
            active_cleanup.mark_ready(None);
            clear_active_generation(
                &self.active_generation,
                &self.active_cancel,
                &self.active_control,
                generation,
            )
            .await;
            cleanup_failed_handoff_runtime(mode, &runtime, &mut cleanup_registration).await;
            bail!("SANDBOX job was cancelled before command handoff");
        }

        let start_exec = runtime
            .start_exec(&args.command, &args.args, &resolved, client_stream)
            .await;
        let (mut exec, control, stdin) = match start_exec {
            Ok(result) => result,
            Err(error) => {
                setup_guard.finish(None);
                clear_active_generation(
                    &self.active_generation,
                    &self.active_cancel,
                    &self.active_control,
                    generation,
                )
                .await;
                cleanup_failed_handoff_runtime(mode, &runtime, &mut cleanup_registration).await;
                return Err(error);
            }
        };
        setup_guard.finish(Some(control.clone()));
        *self.active_control.lock().await = Some(control.clone());
        if local_cancel.is_cancelled() || external_cancel.is_cancelled() || !self.is_reusable() {
            let _ = bounded_kill(&control).await;
            active_cleanup.clear_control();
            clear_active_generation(
                &self.active_generation,
                &self.active_cancel,
                &self.active_control,
                generation,
            )
            .await;
            cleanup_failed_handoff_runtime(mode, &runtime, &mut cleanup_registration).await;
            bail!("SANDBOX job was cancelled before command handoff");
        }
        let started_at = Instant::now();

        let feed_task = match (feed_receiver, stdin) {
            (Some(receiver), Some(stdin)) => Some(FeedTaskGuard::spawn(
                receiver,
                ExecInputSink(stdin),
                local_cancel.clone(),
            )),
            (Some(_), None) => {
                let _ = bounded_kill(&control).await;
                active_cleanup.clear_control();
                local_cancel.cancel();
                clear_active_generation(
                    &self.active_generation,
                    &self.active_cancel,
                    &self.active_control,
                    generation,
                )
                .await;
                cleanup_failed_handoff_runtime(mode, &runtime, &mut cleanup_registration).await;
                bail!("microsandbox did not provide the required stdin sink")
            }
            (None, Some(_)) => {
                let _ = bounded_kill(&control).await;
                active_cleanup.clear_control();
                local_cancel.cancel();
                clear_active_generation(
                    &self.active_generation,
                    &self.active_cancel,
                    &self.active_control,
                    generation,
                )
                .await;
                cleanup_failed_handoff_runtime(mode, &runtime, &mut cleanup_registration).await;
                bail!("microsandbox unexpectedly provided a client stdin sink")
            }
            (None, None) => None,
        };

        let reusable = self.reusable.clone();
        let active_cancel = self.active_cancel.clone();
        let active_control = self.active_control.clone();
        let active_generation = self.active_generation.clone();
        let active_idle_timeout = self.active_idle_timeout.clone();
        let sandbox_id = runtime.stable_id().to_string();
        let timeout = resolved.exec_timeout;
        let deadline = timeout.and_then(|timeout| {
            started_at
                .checked_add(timeout)
                .map(tokio::time::Instant::from_std)
        });
        let treat_nonzero_as_error = resolved.treat_nonzero_as_error;
        let success_exit_codes = resolved.success_exit_codes.clone();
        let trailer_metadata = metadata;

        let cancel_guard = ExecCancelGuard::new(
            control.clone(),
            runtime.clone(),
            ExecCancelState {
                reusable: reusable.clone(),
                mode,
                local_cancel: local_cancel.clone(),
                active_cancel: active_cancel.clone(),
                active_control: active_control.clone(),
                active_generation: active_generation.clone(),
                generation,
                cleanup_registration: cleanup_registration.take(),
                active_cleanup: active_cleanup.clone(),
            },
        );
        let output_stream = stream! {
            let mut cancel_guard = cancel_guard;
            let _feed_task = feed_task;
            loop {
                let next_event = tokio::select! {
                    event = exec.recv() => Some(ExecutionWake::Event(event)),
                    _ = local_cancel.cancelled() => Some(ExecutionWake::Cancelled),
                    _ = external_cancel.cancelled() => Some(ExecutionWake::Cancelled),
                    _ = async {
                        match deadline {
                            Some(deadline) => tokio::time::sleep_until(deadline).await,
                            None => futures::future::pending::<()>().await,
                        }
                    } => Some(ExecutionWake::TimedOut),
                };

                match next_event {
                    Some(ExecutionWake::Event(Some(ExecEvent::Started { .. }))) => {}
                    Some(ExecutionWake::Event(Some(ExecEvent::Stdout(data)))) => {
                        let output = encode_output_result("stdout", data.to_vec());
                        yield ResultOutputItem { item: Some(Item::Data(output.encode_to_vec())) };
                    }
                    Some(ExecutionWake::Event(Some(ExecEvent::Stderr(data)))) => {
                        let stream_name = if client_stream || resolved.tty { "stdout" } else { "stderr" };
                        let output = encode_output_result(stream_name, data.to_vec());
                        yield ResultOutputItem { item: Some(Item::Data(output.encode_to_vec())) };
                    }
                    Some(ExecutionWake::Event(Some(ExecEvent::StdinError(error)))) => {
                        tracing::warn!(?error, sandbox_id = %sandbox_id, "sandbox stdin write failed; continuing to drain output");
                    }
                    Some(ExecutionWake::Event(Some(ExecEvent::Exited { code }))) => {
                        let exit = encode_exit_result(code, started_at.elapsed(), &sandbox_id);
                        cancel_guard.complete();
                        yield ResultOutputItem { item: Some(Item::Data(exit.encode_to_vec())) };
                        if treat_nonzero_as_error
                            && !(if success_exit_codes.is_empty() {
                                code == 0
                            } else {
                                success_exit_codes.contains(&code)
                            })
                        {
                            let message = format!("Sandbox command exited with code {code}");
                            yield error_end(trailer_metadata, "EXECUTION_FAILED", &message);
                        } else {
                            yield normal_end(trailer_metadata);
                        }
                        break;
                    }
                    Some(ExecutionWake::Event(Some(ExecEvent::Failed(_)))) => {
                        cancel_guard.complete();
                        yield error_end(
                            trailer_metadata,
                            "EXECUTION_FAILED",
                            "Sandbox command could not be started",
                        );
                        break;
                    }
                    Some(ExecutionWake::Event(None)) => {
                        reusable.store(false, Ordering::Release);
                        match bounded_kill(&control).await {
                            Ok(()) => cancel_guard.complete(),
                            Err(error) => tracing::error!(%error, sandbox_id = %sandbox_id, "failed to stop sandbox after losing its exec event stream"),
                        }
                        yield error_end(
                            trailer_metadata,
                            "EXECUTION_FAILED",
                            "Sandbox command ended without an exit result",
                        );
                        cancel_guard.complete();
                        break;
                    }
                    Some(ExecutionWake::Cancelled) => {
                        reusable.store(false, Ordering::Release);
                        let timed_out = active_idle_timeout.swap(false, Ordering::AcqRel);
                        match bounded_kill(&control).await {
                            Ok(()) => cancel_guard.complete(),
                            Err(error) => tracing::error!(%error, sandbox_id = %sandbox_id, "failed to stop cancelled sandbox command"),
                        }
                        yield error_end(
                            trailer_metadata,
                            if timed_out { "TIMEOUT" } else { "CANCELLED" },
                            if timed_out {
                                "Sandbox command was idle until its Worker timeout"
                            } else {
                                "Sandbox command was cancelled"
                            },
                        );
                        break;
                    }
                    Some(ExecutionWake::TimedOut) => {
                        reusable.store(false, Ordering::Release);
                        match bounded_kill(&control).await {
                            Ok(()) => cancel_guard.complete(),
                            Err(error) => tracing::error!(%error, sandbox_id = %sandbox_id, "failed to stop timed-out sandbox command"),
                        }
                        yield error_end(
                            trailer_metadata,
                            "TIMEOUT",
                            "Sandbox command exceeded its execution time limit",
                        );
                        break;
                    }
                    None => break,
                }
            }
            clear_active_generation(
                &active_generation,
                &active_cancel,
                &active_control,
                generation,
            )
            .await;
        };
        Ok(Box::pin(output_stream))
    }
}

impl Default for SandboxRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl RunnerSpec for SandboxRunner {
    fn name(&self) -> String {
        RUNNER_NAME.to_string()
    }

    fn runner_settings_proto(&self) -> String {
        RESOLVED_SETTINGS_PROTO.clone()
    }

    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        let streaming = StreamingOutputType::Streaming as i32;
        HashMap::from([
            (
                DEFAULT_METHOD_NAME.to_string(),
                proto::jobworkerp::data::MethodSchema {
                    args_proto: RESOLVED_ARGS_PROTO.clone(),
                    result_proto: RESULT_PROTO.to_string(),
                    description: Some("Execute a command in a local microsandbox VM".to_string()),
                    output_type: streaming,
                    require_client_stream: false,
                    ..Default::default()
                },
            ),
            (
                METHOD_RUN_WITH_CLIENT.to_string(),
                proto::jobworkerp::data::MethodSchema {
                    args_proto: RESOLVED_ARGS_PROTO.clone(),
                    result_proto: RESULT_PROTO.to_string(),
                    description: Some(
                        "Execute a command in a local microsandbox VM with client stdin"
                            .to_string(),
                    ),
                    output_type: streaming,
                    require_client_stream: true,
                    client_stream_data_proto: Some(String::new()),
                },
            ),
        ])
    }

    fn settings_schema(&self) -> String {
        schema_to_json_string!(SandboxRunnerSettings, "settings_schema")
    }

    fn should_detach_on_timeout(&self) -> bool {
        true
    }

    fn collect_stream(
        &self,
        stream: BoxStream<'static, ResultOutputItem>,
        _using: Option<&str>,
    ) -> CollectStreamFuture {
        Box::pin(async move {
            let mut stream = stream;
            let mut last_data = None;
            let mut metadata = HashMap::new();
            let mut saw_end = false;
            while let Some(item) = stream.next().await {
                match item.item {
                    Some(Item::Data(data)) => last_data = Some(data),
                    Some(Item::FinalCollected(data)) => last_data = Some(data),
                    Some(Item::End(trailer)) => {
                        match proto::stream_error::parse_stream_error(&trailer) {
                            proto::stream_error::StreamErrorOutcome::Missing => {}
                            proto::stream_error::StreamErrorOutcome::Error(error) => {
                                bail!("SANDBOX {}: {}", error.code, error.message)
                            }
                            proto::stream_error::StreamErrorOutcome::Malformed(error) => {
                                bail!("SANDBOX stream ended with malformed stream_error: {error:?}")
                            }
                        }
                        metadata = trailer.metadata;
                        saw_end = true;
                        break;
                    }
                    None => {}
                }
            }
            ensure!(saw_end, "SANDBOX stream ended without an End item");
            Ok((last_data.unwrap_or_default(), metadata))
        })
    }
}

#[async_trait]
impl RunnerTrait for SandboxRunner {
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        ensure!(self.settings.is_none(), "SANDBOX Runner was already loaded");
        let worker_id = self.worker_id()?;
        let mode = self.mode()?;
        let settings = ProstMessageCodec::deserialize_message::<SandboxRunnerSettings>(&settings)
            .context("failed to decode SandboxRunnerSettings")?;
        let validated = validate_settings(&settings)?;
        let runtime = if mode == SandboxMode::Static {
            let name = build_sandbox_name(
                worker_id,
                None,
                &PROCESS_GENERATION.to_string(),
                &self.runner_id.to_string(),
            )?;
            let runtime = if let Some(registry) = self.cleanup_registry.clone() {
                let registration = registry.register_task(None).await?;
                let vm_settings = validated.vm.clone();
                let network = validated.network.clone();
                let task = tokio::spawn(async move {
                    let runtime =
                        match SandboxRuntime::create(name, &vm_settings, network.as_ref()).await {
                            Ok(runtime) => runtime,
                            Err(error) => {
                                registration.finish();
                                return Err(error);
                            }
                        };
                    if let Err(error) = runtime.install_cleanup_registration(registration).await {
                        runtime.cleanup_now().await;
                        return Err(error);
                    }
                    Ok(runtime)
                });
                let runtime = task
                    .await
                    .context("SANDBOX static VM creation task failed")??;
                ensure!(
                    !registry.is_shutting_down().await,
                    "SANDBOX shutdown started while its static VM was being created"
                );
                runtime
            } else {
                SandboxRuntime::create(name, &validated.vm, validated.network.as_ref()).await?
            };
            Some(runtime)
        } else {
            None
        };
        self.settings = Some(validated);
        self.static_runtime = runtime;
        Ok(())
    }

    async fn run(
        &mut self,
        _arg: &[u8],
        metadata: HashMap<String, String>,
        _using: Option<&str>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        (
            Err(anyhow!(
                "SANDBOX methods are streaming-only; use EnqueueForStream"
            )),
            metadata,
        )
    }

    async fn run_stream(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
        using: Option<&str>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        let client_stream = match using {
            None | Some(DEFAULT_METHOD_NAME) => false,
            Some(METHOD_RUN_WITH_CLIENT) => true,
            Some(method) => bail!("unsupported SANDBOX method: {method}"),
        };
        self.run_stream_inner(arg, metadata, client_stream).await
    }

    fn supports_client_stream(&self, using: Option<&str>) -> bool {
        using == Some(METHOD_RUN_WITH_CLIENT)
    }

    fn setup_client_stream_channel(
        &mut self,
        using: Option<&str>,
    ) -> Option<mpsc::Sender<FeedData>> {
        if !self.supports_client_stream(using) {
            return None;
        }
        if self.job_id.is_none() {
            tracing::error!(
                "run_with_client requires trusted job context before feed registration"
            );
            return None;
        }
        let (sender, receiver) = mpsc::channel(CLIENT_FEED_CAPACITY);
        self.feed_receiver = Some(receiver);
        Some(sender)
    }
}

#[async_trait]
impl CancelMonitoring for SandboxRunner {
    async fn setup_cancellation_monitoring(
        &mut self,
        job_id: JobId,
        job_data: &JobData,
    ) -> Result<Option<JobResult>> {
        self.set_job_context(job_id);
        match self.cancel_helper.as_mut() {
            Some(helper) => helper.setup_monitoring_impl(job_id, job_data).await,
            None => Ok(None),
        }
    }

    async fn cleanup_cancellation_monitoring(&mut self) -> Result<()> {
        self.clear_job_context();
        if let Some(helper) = self.cancel_helper.as_mut() {
            helper.cleanup_monitoring_impl().await?;
        }
        Ok(())
    }

    async fn request_cancellation(&mut self) -> Result<()> {
        self.reusable.store(false, Ordering::Release);
        self.active_idle_timeout.store(false, Ordering::Release);
        if let Some(token) = self.active_cancel.lock().await.as_ref() {
            token.cancel();
        }
        if let Some(control) = self.active_control.lock().await.as_ref() {
            bounded_kill(control).await?;
        }
        Ok(())
    }
}

impl UseCancelMonitoringHelper for SandboxRunner {
    fn cancel_monitoring_helper(&self) -> Option<&CancelMonitoringHelper> {
        self.cancel_helper.as_ref()
    }
}

enum ExecutionWake {
    Event(Option<ExecEvent>),
    Cancelled,
    TimedOut,
}

#[derive(Clone, Default)]
struct ActiveExecCleanupState {
    inner: Arc<ActiveExecCleanupInner>,
}

#[derive(Default)]
struct ActiveExecCleanupInner {
    state: std::sync::Mutex<ActiveExecCleanupStatus>,
    ready: tokio::sync::Notify,
}

#[derive(Default)]
struct ActiveExecCleanupStatus {
    ready: bool,
    control: Option<ExecControl>,
}

impl ActiveExecCleanupState {
    fn mark_ready(&self, control: Option<ExecControl>) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.control = control;
        state.ready = true;
        self.inner.ready.notify_waiters();
    }

    fn clear_control(&self) {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .control = None;
    }

    async fn wait_for_control(&self) -> Option<ExecControl> {
        loop {
            let notified = self.inner.ready.notified();
            {
                let state = self
                    .inner
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.ready {
                    return state.control.clone();
                }
            }
            notified.await;
        }
    }
}

struct ActiveExecCleanup {
    runtime: Arc<SandboxRuntime>,
    cancellation: CancellationToken,
    cleanup_state: ActiveExecCleanupState,
    mode: SandboxMode,
    active_generation: Arc<Mutex<Option<Uuid>>>,
    active_cancel: Arc<Mutex<Option<CancellationToken>>>,
    active_control: Arc<Mutex<Option<ExecControl>>>,
    generation: Uuid,
}

impl ActiveExecCleanup {
    async fn run(self) {
        if self.cancellation.is_cancelled() {
            match tokio::time::timeout(EXEC_KILL_TIMEOUT, self.cleanup_state.wait_for_control())
                .await
            {
                Ok(Some(control)) => {
                    if let Err(error) = bounded_kill(&control).await {
                        tracing::error!(%error, "failed to stop SANDBOX exec during cleanup");
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(%error, "timed out waiting for SANDBOX exec control during cleanup");
                }
            }
        }
        if self.mode == SandboxMode::NonStatic {
            self.runtime.cleanup_now().await;
        }
        clear_active_generation(
            &self.active_generation,
            &self.active_cancel,
            &self.active_control,
            self.generation,
        )
        .await;
    }
}

struct ExecSetupGuard {
    state: ActiveExecCleanupState,
    finished: bool,
}

impl ExecSetupGuard {
    fn new(state: ActiveExecCleanupState) -> Self {
        Self {
            state,
            finished: false,
        }
    }

    fn finish(&mut self, control: Option<ExecControl>) {
        self.state.mark_ready(control);
        self.finished = true;
    }
}

impl Drop for ExecSetupGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.state.mark_ready(None);
        }
    }
}

struct ExecCancelGuard {
    control: Option<ExecControl>,
    runtime: Option<Arc<SandboxRuntime>>,
    reusable: Arc<AtomicBool>,
    mode: SandboxMode,
    local_cancel: CancellationToken,
    active_cancel: Arc<Mutex<Option<CancellationToken>>>,
    active_control: Arc<Mutex<Option<ExecControl>>>,
    active_generation: Arc<Mutex<Option<Uuid>>>,
    generation: Uuid,
    cleanup_registration: Option<SandboxCleanupRegistration>,
    active_cleanup: ActiveExecCleanupState,
    completed: bool,
}

struct ExecCancelState {
    reusable: Arc<AtomicBool>,
    mode: SandboxMode,
    local_cancel: CancellationToken,
    active_cancel: Arc<Mutex<Option<CancellationToken>>>,
    active_control: Arc<Mutex<Option<ExecControl>>>,
    active_generation: Arc<Mutex<Option<Uuid>>>,
    generation: Uuid,
    cleanup_registration: Option<SandboxCleanupRegistration>,
    active_cleanup: ActiveExecCleanupState,
}

impl ExecCancelGuard {
    fn new(control: ExecControl, runtime: Arc<SandboxRuntime>, state: ExecCancelState) -> Self {
        Self {
            control: Some(control),
            runtime: Some(runtime),
            reusable: state.reusable,
            mode: state.mode,
            local_cancel: state.local_cancel,
            active_cancel: state.active_cancel,
            active_control: state.active_control,
            active_generation: state.active_generation,
            generation: state.generation,
            cleanup_registration: state.cleanup_registration,
            active_cleanup: state.active_cleanup,
            completed: false,
        }
    }

    fn complete(&mut self) {
        self.completed = true;
        self.active_cleanup.clear_control();
        self.control = None;
        self.runtime = None;
        if self.mode == SandboxMode::NonStatic
            && let Some(registration) = self.cleanup_registration.take()
        {
            registration.start_cleanup();
        }
    }
}

impl Drop for ExecCancelGuard {
    fn drop(&mut self) {
        let needs_kill = !self.completed;
        if needs_kill && self.mode == SandboxMode::Static {
            self.reusable.store(false, Ordering::Release);
        }
        if needs_kill {
            self.local_cancel.cancel();
        }
        if let Some(registration) = self.cleanup_registration.take() {
            self.control = None;
            self.runtime = None;
            if needs_kill {
                drop(registration);
            } else {
                registration.start_cleanup();
            }
            return;
        }
        let control = needs_kill.then(|| self.control.take()).flatten();
        self.control = None;
        let runtime = needs_kill.then(|| self.runtime.take()).flatten();
        self.runtime = None;
        let active_cancel = self.active_cancel.clone();
        let active_control = self.active_control.clone();
        let active_generation = self.active_generation.clone();
        let generation = self.generation;
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    if let Some(control) = control
                        && let Err(error) = bounded_kill(&control).await
                    {
                        tracing::error!(%error, "failed to stop dropped sandbox exec stream");
                    }
                    drop(runtime);
                    clear_active_generation(
                        &active_generation,
                        &active_cancel,
                        &active_control,
                        generation,
                    )
                    .await;
                });
            }
            Err(error) => {
                tracing::error!(%error, "no Tokio runtime available to stop dropped sandbox exec stream")
            }
        }
    }
}

pub fn build_sandbox_name(
    worker_id: proto::jobworkerp::data::WorkerId,
    job_id: Option<JobId>,
    process_id: &str,
    runner_id: &str,
) -> Result<String> {
    let name = match job_id {
        Some(job_id) => format!(
            "jw-sbx-w{}-j{}-p{}-e{}",
            worker_id.value, job_id.value, process_id, runner_id
        ),
        None => format!("jw-sbx-w{}-p{}-r{}", worker_id.value, process_id, runner_id),
    };
    ensure!(
        name.len() <= microsandbox::MAX_SANDBOX_NAME_BYTES,
        "generated SANDBOX VM name exceeds microsandbox's name limit"
    );
    microsandbox::validate_sandbox_name(&name).context("generated SANDBOX VM name is invalid")?;
    Ok(name)
}

pub fn encode_output_result(stream: &str, data: Vec<u8>) -> SandboxExecResult {
    SandboxExecResult {
        result: Some(sandbox_exec_result::Result::Output(SandboxExecOutput {
            stream: stream.to_string(),
            data,
        })),
    }
}

pub fn encode_exit_result(
    exit_code: i32,
    execution_time: Duration,
    sandbox_id: &str,
) -> SandboxExecResult {
    SandboxExecResult {
        result: Some(sandbox_exec_result::Result::Exit(SandboxExecExit {
            exit_code,
            execution_time_ms: execution_time.as_millis().min(u128::from(u64::MAX)) as u64,
            sandbox_id: sandbox_id.to_string(),
        })),
    }
}

fn normal_end(mut metadata: HashMap<String, String>) -> ResultOutputItem {
    metadata.remove(STREAM_ERROR_KEY);
    ResultOutputItem {
        item: Some(Item::End(Trailer { metadata })),
    }
}

fn error_end(mut metadata: HashMap<String, String>, code: &str, message: &str) -> ResultOutputItem {
    metadata.remove(STREAM_ERROR_KEY);
    match proto::stream_error::build_stream_error_trailer(metadata, code, message, RUNNER_NAME) {
        Ok(trailer) => ResultOutputItem {
            item: Some(Item::End(trailer)),
        },
        Err(error) => {
            tracing::error!(%error, "failed to serialize SANDBOX stream error trailer");
            ResultOutputItem {
                item: Some(Item::End(Trailer {
                    metadata: HashMap::from([(STREAM_ERROR_KEY.to_string(), "{".to_string())]),
                })),
            }
        }
    }
}

async fn bounded_kill(control: &ExecControl) -> Result<()> {
    tokio::time::timeout(EXEC_KILL_TIMEOUT, control.kill())
        .await
        .context("timed out requesting sandbox exec termination")??;
    Ok(())
}

async fn cleanup_failed_handoff_runtime(
    mode: SandboxMode,
    runtime: &Arc<SandboxRuntime>,
    registration: &mut Option<SandboxCleanupRegistration>,
) {
    if let Some(registration) = registration.take() {
        // The registry owns the cleanup task and its shutdown lock.
        drop(registration);
    } else if mode == SandboxMode::NonStatic {
        runtime.cleanup_now().await;
    }
}

async fn clear_active_generation(
    active_generation: &Arc<Mutex<Option<Uuid>>>,
    active_cancel: &Arc<Mutex<Option<CancellationToken>>>,
    active_control: &Arc<Mutex<Option<ExecControl>>>,
    generation: Uuid,
) {
    let mut active_generation = active_generation.lock().await;
    if *active_generation == Some(generation) {
        *active_control.lock().await = None;
        *active_cancel.lock().await = None;
        *active_generation = None;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn both_end_variants_sanitize_untrusted_reserved_metadata() {
        let metadata = HashMap::from([
            (STREAM_ERROR_KEY.to_string(), "forged".to_string()),
            ("trace_id".to_string(), "request-1".to_string()),
        ]);
        let normal = normal_end(metadata.clone());
        let error = error_end(metadata, "EXECUTION_FAILED", "Sandbox command failed");
        let Some(Item::End(normal)) = normal.item else {
            panic!("a successful command must emit End");
        };
        let Some(Item::End(error)) = error.item else {
            panic!("a failed command must emit End");
        };
        assert_eq!(
            normal.metadata.get("trace_id").map(String::as_str),
            Some("request-1")
        );
        assert!(!normal.metadata.contains_key(STREAM_ERROR_KEY));
        assert_eq!(
            error.metadata.get("trace_id").map(String::as_str),
            Some("request-1")
        );
        assert!(matches!(
            proto::stream_error::parse_stream_error(&error),
            proto::stream_error::StreamErrorOutcome::Error(ref parsed)
                if parsed.origin == RUNNER_NAME && parsed.code == "EXECUTION_FAILED"
        ));
    }

    #[derive(Clone, Default)]
    struct RecordedInput {
        chunks: Arc<Mutex<Vec<Vec<u8>>>>,
        closed: Arc<AtomicBool>,
        fail_next_write: Arc<AtomicBool>,
        writes: Arc<std::sync::atomic::AtomicUsize>,
        fail_on_write: Option<usize>,
    }

    #[async_trait]
    impl ClientInputSink for RecordedInput {
        async fn write(&mut self, bytes: &[u8]) -> Result<()> {
            let write_number = self.writes.fetch_add(1, Ordering::AcqRel) + 1;
            if self.fail_next_write.swap(false, Ordering::AcqRel)
                || self.fail_on_write == Some(write_number)
            {
                bail!("simulated guest stdin error");
            }
            self.chunks.lock().await.push(bytes.to_vec());
            Ok(())
        }

        async fn close(&mut self) -> Result<()> {
            self.closed.store(true, Ordering::Release);
            Ok(())
        }
    }

    #[tokio::test]
    async fn client_feed_forwards_ordered_bytes_and_closes_on_final_chunk() {
        let (sender, receiver) = mpsc::channel(4);
        let input = RecordedInput::default();
        let observed = input.clone();
        let task = tokio::spawn(forward_client_feed(
            receiver,
            input,
            CancellationToken::new(),
        ));
        sender
            .send(FeedData {
                data: vec![0, 255],
                is_final: false,
            })
            .await
            .unwrap();
        sender
            .send(FeedData {
                data: b"last".to_vec(),
                is_final: true,
            })
            .await
            .unwrap();
        task.await.unwrap();

        assert_eq!(
            *observed.chunks.lock().await,
            vec![vec![0, 255], b"last".to_vec(), vec![0x04, 0x04]]
        );
        assert!(observed.closed.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn empty_final_feed_still_sends_canonical_pty_eof() {
        let (sender, receiver) = mpsc::channel(1);
        let input = RecordedInput::default();
        let observed = input.clone();
        let task = tokio::spawn(forward_client_feed(
            receiver,
            input,
            CancellationToken::new(),
        ));
        sender
            .send(FeedData {
                data: Vec::new(),
                is_final: true,
            })
            .await
            .unwrap();
        task.await.unwrap();

        assert_eq!(
            *observed.chunks.lock().await,
            vec![Vec::new(), vec![0x04, 0x04]]
        );
        assert!(observed.closed.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn failed_pty_eof_write_still_closes_guest_stdin() {
        let (sender, receiver) = mpsc::channel(1);
        let input = RecordedInput {
            fail_on_write: Some(2),
            ..Default::default()
        };
        let observed = input.clone();
        let task = tokio::spawn(forward_client_feed(
            receiver,
            input,
            CancellationToken::new(),
        ));
        sender
            .send(FeedData {
                data: b"payload".to_vec(),
                is_final: true,
            })
            .await
            .unwrap();
        task.await.unwrap();

        assert_eq!(*observed.chunks.lock().await, vec![b"payload".to_vec()]);
        assert!(observed.closed.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn client_feed_write_failure_does_not_stop_output_drain_or_later_feed() {
        let (sender, receiver) = mpsc::channel(4);
        let input = RecordedInput {
            fail_next_write: Arc::new(AtomicBool::new(true)),
            ..Default::default()
        };
        let observed = input.clone();
        let task = tokio::spawn(forward_client_feed(
            receiver,
            input,
            CancellationToken::new(),
        ));
        sender
            .send(FeedData {
                data: b"unwritable".to_vec(),
                is_final: false,
            })
            .await
            .unwrap();
        sender
            .send(FeedData {
                data: b"still delivered".to_vec(),
                is_final: true,
            })
            .await
            .unwrap();
        task.await.unwrap();

        assert_eq!(
            *observed.chunks.lock().await,
            vec![b"still delivered".to_vec(), vec![0x04, 0x04]]
        );
        assert!(observed.closed.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn client_feed_cancellation_stops_forwarding_without_waiting_for_final_feed() {
        let (sender, receiver) = mpsc::channel(4);
        let input = RecordedInput::default();
        let observed = input.clone();
        let cancel = CancellationToken::new();
        let task = tokio::spawn(forward_client_feed(receiver, input, cancel.clone()));
        sender
            .send(FeedData {
                data: b"partial".to_vec(),
                is_final: false,
            })
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if observed.chunks.lock().await.len() == 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        cancel.cancel();
        task.await.unwrap();

        assert_eq!(*observed.chunks.lock().await, vec![b"partial".to_vec()]);
        assert!(!observed.closed.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn idle_timeout_signals_the_exec_token_and_preserves_timeout_reason() {
        let runner = SandboxRunner::new();
        let token = CancellationToken::new();
        *runner.active_cancel.lock().await = Some(token.clone());

        runner.signal_idle_timeout().await;

        assert!(token.is_cancelled());
        assert!(runner.active_idle_timeout.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn cancellation_marks_static_runner_unusable_before_signalling_exec() {
        let mut runner = SandboxRunner::new();
        runner.mode = Some(SandboxMode::Static);
        let token = CancellationToken::new();
        *runner.active_cancel.lock().await = Some(token.clone());

        runner.request_cancellation().await.unwrap();

        assert!(token.is_cancelled());
        assert!(!runner.is_reusable());
        assert!(!runner.active_idle_timeout.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn non_static_load_validates_settings_without_precreating_a_vm() {
        let mut runner =
            SandboxRunner::new_with_context(SandboxExecutionContext::non_static_worker(
                proto::jobworkerp::data::WorkerId { value: 17 },
            ));
        let settings = SandboxRunnerSettings {
            vm: Some(crate::jobworkerp::runner::SandboxVmConfig {
                image: Some("python:3.12".to_string()),
                cpus: Some(1),
                memory_mib: Some(512),
                root_disk_mib: Some(4096),
                ..Default::default()
            }),
            allowed_images: vec!["python:3.12".to_string()],
            ..Default::default()
        };

        runner.load(settings.encode_to_vec()).await.unwrap();

        assert!(runner.static_runtime.is_none());
        assert!(runner.settings.is_some());
    }

    #[tokio::test]
    async fn invalid_static_settings_fail_before_local_vm_creation() {
        let mut runner = SandboxRunner::new_with_context(SandboxExecutionContext::static_worker(
            proto::jobworkerp::data::WorkerId { value: 18 },
        ));

        assert!(
            runner
                .load(SandboxRunnerSettings::default().encode_to_vec())
                .await
                .is_err()
        );
        assert!(runner.static_runtime.is_none());
    }
}
