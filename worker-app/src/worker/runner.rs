pub mod feed_bridge;
pub mod map;
pub mod pool;
pub mod result;
pub mod stream_guard;

#[cfg(test)]
mod integration_tests;

use self::map::UseRunnerPoolMap;
use self::pool::RunnerPoolManagerImpl;
use self::result::RunnerResultHandler;
use self::stream_guard::{IdleTimeoutStream, StreamWithCancelGuard, StreamWithPoolGuard};
use anyhow::Result;
use anyhow::anyhow;
use app_wrapper::runner::UseRunnerFactory;
use async_trait::async_trait;
use command_utils::trace::Tracing;
use command_utils::util::datetime;
use deadpool::managed::Object;
use futures::{future::FutureExt, stream::BoxStream};
use infra::infra::UseIdGenerator;
use infra::infra::job::rows::UseJobqueueAndCodec;
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::runner::FeedData;
use jobworkerp_runner::runner::cancellation::CancellableRunner;
use proto::jobworkerp::data::JobResult;
use proto::jobworkerp::data::JobResultId;
use proto::jobworkerp::data::ResultOutputItem;
use proto::jobworkerp::data::{
    Job, JobResultData, ResultOutput, ResultStatus, RunnerData, StreamingType, WorkerData, WorkerId,
};
use result::ResultOutputEnum;
use std::collections::HashMap;
use std::{panic::AssertUnwindSafe, time::Duration};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing;

/// Whether a job finished normally or was dropped by its timeout. Used by run_job
/// to decide whether a timed-out runner must be detached from the pool.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum RunnerOutcome {
    #[default]
    Normal,
    TimedOut,
}

/// run_and_result / run_and_stream signal a timeout by returning
/// JobWorkerError::TimeoutError; recognize it so the runner can be detached.
fn outcome_of<T>(res: &Result<T>) -> RunnerOutcome {
    let is_timeout = res.as_ref().err().is_some_and(|e| {
        matches!(
            e.downcast_ref::<JobWorkerError>(),
            Some(JobWorkerError::TimeoutError(_))
        )
    });
    if is_timeout {
        RunnerOutcome::TimedOut
    } else {
        RunnerOutcome::Normal
    }
}

/// Remove a runner from its pool when `detach` is set (shrinking the pool so the
/// next get() creates a fresh instance), otherwise let it return to the pool on
/// drop. The taken instance is dropped here; its still-running blocking work, if
/// any, releases the internal lock only when it finishes.
fn detach_runner_if(detach: bool, runner: Object<RunnerPoolManagerImpl>) {
    if detach {
        let _ = Object::take(runner);
        tracing::warn!("runner detached from pool after timeout; pool will recreate it");
    }
    // else: `runner` drops here and returns to the pool normally.
}

/// Clone the runner's cancellation token while the runner mutex is
/// still held, so the idle-timeout callback below can fire `cancel()`
/// without re-locking. Returns `None` for runners without a cancel
/// helper (e.g. test stubs) — those simply skip idle-timeout wrapping.
///
/// `CancellableRunner::clone_cancel_helper_for_stream` is the only
/// trait-object-safe way to reach the helper from a `&Box<dyn ...>`
/// today; clone the helper, then resolve the token off it.
async fn cancellation_token_from_runner(
    runner: &(dyn CancellableRunner + Send + Sync),
) -> Option<CancellationToken> {
    let helper = runner.clone_cancel_helper_for_stream()?;
    Some(helper.get_cancellation_token().await)
}

/// Idle-timeout window for stream drain.
///
/// V2 plugins hand the BoxStream back before producing a chunk
/// (`run_stream_v2`), so the legacy `run_and_stream` `tokio::select!` only
/// guards the initial handoff. To keep `JobData::timeout` meaningful for
/// streaming jobs we re-use it as the maximum gap between consecutive
/// chunks — long enough to cover normal pauses (e.g. silence in audio),
/// but bounded so a hung plugin frees its static pool slot instead of
/// blocking subsequent jobs forever.
fn idle_window_from(job_data: Option<&proto::jobworkerp::data::JobData>) -> Option<Duration> {
    job_data
        .map(|d| d.timeout)
        .filter(|t| *t > 0)
        .map(Duration::from_millis)
}

/// Wrap a stream with an inter-chunk idle timeout that fires
/// `token.cancel()` synchronously when the window elapses.
///
/// `token` must be acquired before the stream starts being drained
/// (typically while the runner mutex is still held by `run_job`), so the
/// timeout callback never has to re-enter the runner's lock. That
/// matters for `use_static=true`: `StreamWithPoolGuard` releases the
/// pool slot synchronously when `IdleTimeoutStream` yields `None`, and
/// if cancellation were running on a separate task it could lose the
/// race against the next job that grabs the same runner — leaving the
/// previous plugin's `variant.write()` lock un-cancelled. Firing
/// `token.cancel()` (a lock-free atomic + waker) from the poll thread
/// guarantees the cancellation observation point precedes the pool
/// return.
fn maybe_idle_timeout_wrap(
    stream: BoxStream<'static, ResultOutputItem>,
    idle_window: Option<Duration>,
    cancel_token: Option<CancellationToken>,
) -> BoxStream<'static, ResultOutputItem> {
    let Some(window) = idle_window else {
        return stream;
    };
    let Some(token) = cancel_token else {
        // No cancellation channel available — wrapping would only delay
        // the stalled stream without being able to wake the plugin.
        return stream;
    };
    // Plant a synthetic End trailer carrying `V2_STREAM_ERROR_META_KEY`
    // so downstream consumers (notably `collect_stream` in the V2 plugin
    // wrapper) can distinguish "plugin stalled past idle window" from a
    // clean EOF. Without this, a `Poll::Ready(None)` after timeout would
    // be indistinguishable from a successful end-of-stream, and the
    // Internal-streaming path would persist whatever data accumulated
    // so far as a Success result.
    let mut timeout_metadata = HashMap::new();
    timeout_metadata.insert(
        jobworkerp_runner::runner::plugins::impls::V2_STREAM_ERROR_META_KEY.to_string(),
        format!(
            "stream idle timeout: no chunk emitted within {}ms",
            window.as_millis()
        ),
    );
    let timeout_item = ResultOutputItem {
        item: Some(proto::jobworkerp::data::result_output_item::Item::End(
            proto::jobworkerp::data::Trailer {
                metadata: timeout_metadata,
            },
        )),
    };
    Box::pin(IdleTimeoutStream::with_timeout_item(
        stream,
        window,
        move || {
            if !token.is_cancelled() {
                token.cancel();
                tracing::info!(
                    "IdleTimeoutStream: no chunk within window; cancellation token fired"
                );
            }
        },
        Some(timeout_item),
    ))
}

// execute runner
#[async_trait]
pub trait JobRunner:
    RunnerResultHandler
    + UseJobqueueAndCodec
    + UseRunnerFactory
    + UseRunnerPoolMap
    + UseIdGenerator
    + Tracing
    + Send
    + Sync
{
    /// Register a feed sender for a running job.
    /// Dispatchers register in ChanFeedSenderStore (Standalone) or spawn Redis bridge (Scalable).
    /// Test/mock impls are no-op.
    fn register_feed_sender(&self, job_id: i64, sender: mpsc::Sender<FeedData>);

    /// Unregister a feed sender for a job (cleanup on early termination).
    /// Standalone dispatchers remove from ChanFeedSenderStore.
    /// Test/mock impls are no-op.
    fn unregister_feed_sender(&self, job_id: i64);

    /// Pre-load this worker's runner for config validation / pre-loading,
    /// performing the worker's `load()` exactly as a job would before execution
    /// but without running it. For `use_static=true` this warms up the runner
    /// pool so an initialized runner stays resident; for `use_static=false` a
    /// runner is instantiated and loaded to verify the settings, then dropped.
    /// The runner's `load()` (e.g. an LLM model download) runs here, so failures
    /// such as a missing model surface as a FatalError JobResult.
    ///
    /// Named distinctly from the app-layer `JobApp::load_worker` (which enqueues
    /// and awaits): this is the worker-side execution that actually drives load().
    ///
    /// Note: this intentionally does not go through `run_job_inner`, so no
    /// cancellation monitoring is set up — load-only requests have no run() to
    /// cancel and must not leave job-specific monitoring state behind.
    async fn preload_runner(
        &'static self,
        runner_data: &RunnerData,
        worker_id: &WorkerId,
        worker_data: &WorkerData,
        job: Job,
    ) -> JobResult {
        tracing::debug!("preload_runner: worker: {:?}", &worker_id);
        let start = datetime::now_millis();
        let load_result = if worker_data.use_static {
            // Warm up the static pool: creating the pool runs load() once and the
            // initialized runner stays resident for subsequent jobs. Dropping the
            // pool object just returns it to the pool.
            self.runner_pool_map()
                .get_or_create_static_runner(runner_data, worker_id, worker_data, None)
                .await
                .map(|_runner| ())
        } else {
            // Non-static: instantiate and load() to verify the settings, then drop.
            self.runner_pool_map()
                .get_non_static_runner(runner_data, worker_data)
                .await
                .map(|_runner| ())
        };
        match load_result {
            Ok(()) => {
                let end = datetime::now_millis();
                let metadata = job.metadata.clone();
                self.job_result_data(
                    job,
                    worker_data,
                    ResultStatus::Success,
                    ResultOutputEnum::Normal(Ok(Vec::new()), HashMap::default()).result_output(),
                    start,
                    end,
                    Some(metadata),
                )
            }
            Err(e) => self.handle_error_option(worker_data, job, Some(e)),
        }
    }

    //#[tracing::instrument(name = "JobRunner", skip(self))]
    #[inline]
    async fn run_job(
        &'static self,
        runner_data: &RunnerData,
        worker_id: &WorkerId,
        worker_data: &WorkerData,
        job: Job,
    ) -> (JobResult, Option<BoxStream<'static, ResultOutputItem>>) {
        tracing::debug!("run_job: {:?}, worker: {:?}", &job.id, &worker_id);
        // XXX for keeping pool object
        if worker_data.use_static {
            let to = if let Some(data) = job.data.as_ref() {
                if data.timeout == 0 {
                    None
                } else {
                    Some(Duration::from_millis(data.timeout))
                }
            } else {
                None
            };
            let p = self
                .runner_pool_map()
                .get_or_create_static_runner(runner_data, worker_id, worker_data, to)
                .await;
            match p {
                Ok(Some(runner)) => {
                    let is_streaming = job
                        .data
                        .as_ref()
                        .is_some_and(|data| data.streaming_type != 0);

                    if is_streaming {
                        // Streaming: Keep Pool Object for later return
                        let mut r = runner.lock().await;
                        tracing::debug!("static runner found (streaming): {:?}", r.name());

                        // Check feed support and set up feed channel
                        let using = job.data.as_ref().and_then(|d| d.using.clone());
                        let using_ref = using.as_deref();
                        let feed_sender = if r.supports_client_stream(using_ref) {
                            r.setup_client_stream_channel(using_ref)
                        } else {
                            None
                        };

                        let job_id_value = job.id.as_ref().map(|id| id.value);

                        // Register feed sender before run_job_inner to avoid race condition
                        let registered_feed_job_id = match (feed_sender, job_id_value) {
                            (Some(sender), Some(id)) => {
                                self.register_feed_sender(id, sender);
                                Some(id)
                            }
                            (Some(_), None) => {
                                tracing::warn!(
                                    "feed_sender exists but job_id is None; skipping feed registration"
                                );
                                None
                            }
                            _ => None,
                        };

                        // Capture the data needed for the idle-timeout
                        // window before `job` is moved into run_job_inner.
                        let idle_window = idle_window_from(job.data.as_ref());
                        let (job_result, stream, outcome) =
                            self.run_job_inner(worker_data, job, &mut r).await;
                        let detach =
                            outcome == RunnerOutcome::TimedOut && r.should_detach_on_timeout();
                        // Acquire the cancellation token AFTER run_job_inner:
                        // `setup_cancellation_monitoring_if_supported` runs
                        // inside run_job_inner and is the call that
                        // populates `RunnerCancellationManager.cancellation_token`.
                        // Grabbing the token earlier would return
                        // `unwrap_or_default()` — a fresh, never-published
                        // token nobody listens to — so the idle-timeout
                        // would fire but the plugin would not observe
                        // cancellation. The runner mutex is still held by
                        // `r` here, so the helper read is race-free with
                        // the next job's setup. Cloning the token is cheap
                        // (Arc), and the BoxStream we just received is
                        // still draining the plugin task — exactly when
                        // we need an effective cancel channel.
                        let cancel_token_for_idle = cancellation_token_from_runner(&**r).await;
                        drop(r); // unlock

                        let final_stream = if let Some(s) = stream {
                            // Wrap with an inter-chunk idle timeout when the
                            // job specifies one. V2 plugins now hand the
                            // BoxStream back before producing any chunk
                            // (see run_stream_v2), so the original
                            // tokio::select! in `run_and_stream` only guards
                            // the initial handoff. Without this idle guard,
                            // a plugin that stalls mid-drain (e.g. whisper
                            // after its final feed) would hold the static
                            // runner pool slot forever.
                            let s = maybe_idle_timeout_wrap(s, idle_window, cancel_token_for_idle);
                            if let Some(id) = registered_feed_job_id {
                                Some(Box::pin(StreamWithPoolGuard::with_on_complete(
                                    s,
                                    runner,
                                    move || self.unregister_feed_sender(id),
                                )) as BoxStream<'static, _>)
                            } else {
                                Some(Box::pin(StreamWithPoolGuard::new(s, runner))
                                    as BoxStream<'static, _>)
                            }
                        } else {
                            // No stream returned (cancellation, error, etc.):
                            // clean up feed sender registration immediately
                            if let Some(id) = registered_feed_job_id {
                                self.unregister_feed_sender(id);
                            }
                            // A timed-out plugin keeps its lock on a blocking thread,
                            // so discard the instance instead of returning it to the pool.
                            detach_runner_if(detach, runner);
                            None
                        };

                        (job_result, final_stream)
                    } else {
                        // Non-streaming: Existing behavior (immediate Pool return)
                        let mut r = runner.lock().await;
                        tracing::debug!("static runner found (non-streaming): {:?}", r.name());

                        let (job_result, stream, outcome) =
                            self.run_job_inner(worker_data, job, &mut r).await;
                        let detach =
                            outcome == RunnerOutcome::TimedOut && r.should_detach_on_timeout();
                        drop(r); // unlock before detach so take() does not move a locked runner

                        // A timed-out plugin keeps its lock on a blocking thread, so
                        // discard the instance; otherwise it returns to the pool on drop.
                        detach_runner_if(detach, runner);
                        (job_result, stream)
                    }
                }
                Ok(None) => (self.handle_error_option(worker_data, job, None), None),
                Err(e) => (self.handle_error_option(worker_data, job, Some(e)), None),
            }
        } else {
            let rres = self
                .runner_pool_map()
                .get_non_static_runner(runner_data, worker_data)
                .await;
            match rres {
                Ok(runner) => {
                    tracing::debug!("non-static runner found: {:?}", runner.name());

                    // Streaming determination
                    let is_streaming = job
                        .data
                        .as_ref()
                        .is_some_and(|data| data.streaming_type != 0);

                    if is_streaming {
                        // Streaming: Clone CancelHelper using type-safe access.
                        // The helper clone shares the same underlying
                        // `Arc<Mutex<RunnerCancellationManager>>`, so a
                        // token published later via `setup_cancellation_monitoring`
                        // is visible to every clone — we only need to
                        // defer the *token value* read until setup has
                        // run (see the run_job_inner-completed read below).
                        let cancel_helper = runner.clone_cancel_helper_for_stream();
                        let cancel_helper_for_idle = cancel_helper.clone();

                        let mut runner = runner;
                        // Capture idle-window before `job` is moved.
                        let idle_window = idle_window_from(job.data.as_ref());
                        // Non-static runners are not pooled, so timeout detach does not apply.
                        let (job_result, stream, _outcome) =
                            self.run_job_inner(worker_data, job, &mut runner).await;

                        // Resolve the token AFTER run_job_inner so we get
                        // the real cancellation channel published by
                        // `setup_cancellation_monitoring_if_supported` and
                        // not the throw-away `unwrap_or_default()` token
                        // an earlier read would return. The idle-timeout
                        // callback must run synchronously (no spawn, no
                        // re-lock) so the cancel happens before any
                        // downstream cleanup observes the stream end.
                        let cancel_token_for_idle = match cancel_helper_for_idle.as_ref() {
                            Some(h) => Some(h.get_cancellation_token().await),
                            None => None,
                        };

                        let final_stream = if let Some(stream) = stream {
                            let stream =
                                maybe_idle_timeout_wrap(stream, idle_window, cancel_token_for_idle);
                            if let Some(cancel_helper) = cancel_helper {
                                Some(Box::pin(StreamWithCancelGuard::new(stream, cancel_helper))
                                    as BoxStream<'static, _>)
                            } else {
                                Some(stream)
                            }
                        } else {
                            None
                        };

                        (job_result, final_stream)
                    } else {
                        // Non-streaming: Existing behavior
                        let mut runner = runner;
                        // Non-static runners are not pooled, so timeout detach does not apply.
                        let (job_result, stream, _outcome) =
                            self.run_job_inner(worker_data, job, &mut runner).await;
                        (job_result, stream)
                    }
                }
                Err(e) => (self.handle_error_option(worker_data, job, Some(e)), None),
            }
        }
    }
    fn handle_error_option(
        &self,
        worker_data: &WorkerData,
        job: Job,
        err: Option<anyhow::Error>,
    ) -> JobResult {
        let end = datetime::now_millis();
        let error_message = if let Some(e) = err {
            let mes = format!(
                "error in loading runner for worker:{:?}, error:{e:?}",
                worker_data.name
            );
            tracing::warn!(mes);
            mes
        } else {
            tracing::info!("runner not found for static worker:{:?}", worker_data.name);
            format!("runner not found for static worker:{:?}", worker_data.name)
        };
        let metadata = job.metadata.clone();
        self.job_result_data(
            job,
            worker_data,
            ResultStatus::FatalError,
            ResultOutputEnum::Normal(Err(anyhow!(error_message)), HashMap::default())
                .result_output(),
            end,
            end,
            Some(metadata), // XXX unwrap or default
        )
    }

    /// Setup cancellation monitoring if the runner supports it
    /// Returns Some(JobResult) if the job should be cancelled immediately, None to continue execution
    async fn setup_cancellation_monitoring_if_supported(
        &self,
        job_id: &proto::jobworkerp::data::JobId,
        data: &proto::jobworkerp::data::JobData,
        runner_impl: &mut Box<dyn CancellableRunner + Send + Sync>,
    ) -> Option<JobResult> {
        // Type-safe cancellation monitoring setup using CancellableRunner trait
        tracing::debug!(
            "Setting up cancellation monitoring for job {}",
            job_id.value
        );

        match runner_impl
            .as_cancel_monitoring()
            .setup_cancellation_monitoring(*job_id, data)
            .await
        {
            Ok(Some(job_result)) => {
                tracing::info!("Job {} should be cancelled immediately", job_id.value);
                Some(job_result)
            }
            Ok(None) => {
                tracing::debug!(
                    "Cancellation monitoring setup successful for job {}",
                    job_id.value
                );
                None
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to setup cancellation monitoring for job {}: {:?}",
                    job_id.value,
                    e
                );
                None
            }
        }
    }

    /// Cleanup cancellation monitoring if the runner supports it
    async fn cleanup_cancellation_monitoring_if_supported(
        &self,
        job_id: &proto::jobworkerp::data::JobId,
        runner_impl: &mut Box<dyn CancellableRunner + Send + Sync>,
    ) {
        // Type-safe cancellation monitoring cleanup using CancellableRunner trait
        tracing::trace!(
            "Cleaning up cancellation monitoring for job {}",
            job_id.value
        );

        if let Err(e) = runner_impl
            .as_cancel_monitoring()
            .cleanup_cancellation_monitoring()
            .await
        {
            tracing::warn!(
                "Failed to cleanup cancellation monitoring for job {}: {:?}",
                job_id.value,
                e
            );
        }
    }

    /// Reset cancellation monitoring state for pooling if the runner supports it
    async fn reset_for_pooling_if_supported(
        &self,
        runner_impl: &mut Box<dyn CancellableRunner + Send + Sync>,
    ) {
        // Type-safe pooling reset using CancellableRunner trait
        tracing::trace!("Resetting cancellation monitoring for pooling");

        if let Err(e) = runner_impl.as_cancel_monitoring().reset_for_pooling().await {
            tracing::warn!(
                "Failed to reset cancellation monitoring for pooling: {:?}",
                e
            );
        }
    }

    async fn run_job_inner(
        &'static self,
        worker_data: &WorkerData,
        job: Job,
        runner_impl: &mut Box<dyn CancellableRunner + Send + Sync>,
    ) -> (
        JobResult,
        Option<BoxStream<'static, ResultOutputItem>>,
        RunnerOutcome,
    ) {
        let data = job.data.as_ref().unwrap(); // XXX unwrap

        // Setup cancellation monitoring if runner supports it
        let job_id = job.id.as_ref().unwrap();
        if let Some(cancelled_result) = self
            .setup_cancellation_monitoring_if_supported(job_id, data, runner_impl)
            .await
        {
            // Job was already cancelled, return the cancellation result immediately
            return (cancelled_result, None, RunnerOutcome::Normal);
        }

        let run_after_time = data.run_after_time;

        let wait = run_after_time - datetime::now_millis();
        // sleep short time if run_after_time is future (should less than STORAGE_FETCH_INTERVAL)
        if wait > 0 {
            // 1min
            if wait > 60000 {
                // logic error!
                tracing::error!("wait too long time: {wait}ms, job: {job:?}");
            }
            tokio::time::sleep(Duration::from_millis(wait as u64)).await;
        }
        // job time: complete job time include setup runner time.
        let start = datetime::now_millis();

        let name = runner_impl.name();
        let streaming_type =
            StreamingType::try_from(data.streaming_type).unwrap_or(StreamingType::None);

        // Resolve once, reuse for status check and result construction
        let resolved = app::app::job::resolve_job_params(worker_data, data.overrides.as_ref());

        match streaming_type {
            StreamingType::Response => {
                // Response streaming: run_stream and return stream to client
                tracing::debug!("start runner(stream response): {}", &name);
                let res = self
                    .run_and_stream(&job, runner_impl)
                    .await
                    .map(ResultOutputEnum::Stream);
                let outcome = outcome_of(&res);
                let end = datetime::now_millis();
                tracing::debug!(
                    "end runner(stream response: {}): {}, duration:{}(ms)",
                    if res.is_ok() { "success" } else { "error" },
                    &name,
                    end - start,
                );
                let (status, mes) = self.job_result_status(&resolved.retry_policy, data, res);

                // For streaming jobs, DO NOT cleanup cancellation monitoring immediately
                // The process continues running and needs to receive cancellation signals
                // Cancellation monitoring will timeout automatically after job_timeout + 30 seconds
                tracing::debug!(
                    "Streaming job {} keeping cancellation monitoring active for process lifetime",
                    job_id.value
                );

                (
                    self.job_result_data_from_resolved(
                        job,
                        worker_data,
                        &resolved,
                        status,
                        mes.result_output(),
                        start,
                        end,
                        mes.metadata().cloned(),
                    ),
                    mes.stream(),
                    outcome,
                )
            }
            StreamingType::Internal => {
                // Internal streaming: same as Response - return stream to caller
                // App layer will call RunnerSpec::collect_stream for aggregation
                tracing::debug!("start runner(stream internal): {}", &name);
                let res = self
                    .run_and_stream(&job, runner_impl)
                    .await
                    .map(ResultOutputEnum::Stream);
                let outcome = outcome_of(&res);
                let end = datetime::now_millis();
                tracing::debug!(
                    "end runner(stream internal: {}): {}, duration:{}(ms)",
                    if res.is_ok() { "success" } else { "error" },
                    &name,
                    end - start,
                );
                let (status, mes) = self.job_result_status(&resolved.retry_policy, data, res);

                // For internal streaming jobs, keep cancellation monitoring active
                // The stream continues and needs to receive cancellation signals
                tracing::debug!(
                    "Internal streaming job {} keeping cancellation monitoring active",
                    job_id.value
                );

                (
                    self.job_result_data_from_resolved(
                        job,
                        worker_data,
                        &resolved,
                        status,
                        mes.result_output(),
                        start,
                        end,
                        mes.metadata().cloned(),
                    ),
                    mes.stream(),
                    outcome,
                )
            }
            StreamingType::None => {
                // Non-streaming: run and return single result
                tracing::debug!("start runner: {}", &name);
                let res = self
                    .run_and_result(&job, runner_impl)
                    .await
                    .map(|(a, b)| ResultOutputEnum::Normal(a, b));
                let outcome = outcome_of(&res);
                let end = datetime::now_millis();
                tracing::debug!("end runner: {}, duration:{}(ms)", &name, end - start);

                let (status, mes) = self.job_result_status(&resolved.retry_policy, data, res);

                // Cleanup cancellation monitoring
                self.cleanup_cancellation_monitoring_if_supported(job_id, runner_impl)
                    .await;

                (
                    self.job_result_data_from_resolved(
                        job,
                        worker_data,
                        &resolved,
                        status,
                        mes.result_output(),
                        start,
                        end,
                        mes.metadata().cloned(),
                    ),
                    None,
                    outcome,
                )
            }
        }
    }

    async fn run_and_stream<'a>(
        &'static self,
        job: &Job,
        runner_impl: &mut Box<dyn CancellableRunner + Send + Sync>,
    ) -> Result<BoxStream<'a, ResultOutputItem>> {
        let metadata = job.metadata.clone();
        let data = job.data.as_ref().unwrap(); // XXX unwrap
        let args = &data.args; // XXX unwrap, clone
        let name = runner_impl.name();
        let using = data.using.as_deref();
        if data.timeout > 0 {
            tokio::select! {
                r = AssertUnwindSafe(
                    runner_impl.run_stream(args, metadata, using),
                ).catch_unwind() => {
                    r.map_err(|e| {
                        let msg = format!("Caught panic from runner {name}: {e:?}");
                        tracing::error!(msg);
                        anyhow!(msg)
                    }).and_then(|st| st)
                },
                _ = tokio::time::sleep(Duration::from_millis(data.timeout)) => {
                    runner_impl.as_cancel_monitoring().request_cancellation().await.unwrap();
                    tracing::warn!("timeout: {}ms, the job will be dropped: {job:?}", data.timeout);
                    Err(JobWorkerError::TimeoutError(format!("timeout: {}ms", data.timeout)).into())
                }
            }
        } else {
            AssertUnwindSafe(runner_impl.run_stream(args, metadata, using))
                .catch_unwind()
                .await
                .map_err(|e| {
                    let msg = format!("Caught panic from runner {name}: {e:?}");
                    tracing::error!(msg);
                    anyhow!(msg)
                })
                .and_then(|st| st)
        }
    }
    async fn run_and_result(
        &self,
        job: &Job,
        runner_impl: &mut Box<dyn CancellableRunner + Send + Sync>,
    ) -> Result<(Result<Vec<u8>>, HashMap<String, String>)> {
        let data = job.data.as_ref().unwrap(); // XXX unwrap
        let metadata = job.metadata.clone();
        let args = &data.args; // XXX unwrap, clone
        let name = runner_impl.name();
        let using = data.using.as_deref();

        if using.is_some() {
            tracing::debug!(
                "Executing job with using '{}' for runner '{}'",
                using.unwrap_or(""),
                name
            );
        }

        let run_future = runner_impl.run(args, metadata.clone(), using);

        if data.timeout > 0 {
            tokio::select! {
                r = AssertUnwindSafe(run_future).catch_unwind() => {
                    r.map_err(|e| {
                        let msg = format!("Caught panic from runner {name}: {e:?}");
                        tracing::error!(msg);
                        anyhow!(msg)
                    }).inspect_err(|e| tracing::warn!("error in running runner: {name} : {e:?}"))
                },
                _ = tokio::time::sleep(Duration::from_millis(data.timeout)) => {
                    runner_impl.as_cancel_monitoring().request_cancellation().await.unwrap();
                    tracing::warn!("timeout: {}ms, the job will be dropped: {job:?}", data.timeout);
                    Err(JobWorkerError::TimeoutError(format!("timeout: {}ms", data.timeout)).into())
                }
            }
        } else {
            AssertUnwindSafe(run_future)
                .catch_unwind()
                .await
                .map_err(|e| {
                    let msg = format!("Caught panic from runner {name}: {e:?}");
                    tracing::error!(msg);
                    anyhow!(msg)
                })
                .inspect_err(|e| tracing::warn!("error in running runner: {name} : {e:?}"))
        }
    }
    // calculate job status and create JobResult (not increment retry count now)
    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn job_result_data(
        &self,
        job: Job,
        worker: &WorkerData,
        st: ResultStatus,
        res: Option<ResultOutput>,
        start_msec: i64,
        end_msec: i64,
        metadata: Option<HashMap<String, String>>,
    ) -> JobResult {
        let dat = job.data.unwrap_or_default(); // XXX unwrap or default
        let resolved = app::app::job::resolve_job_params(worker, dat.overrides.as_ref());
        Self::build_job_result(
            self.id_generator(),
            job.id,
            dat,
            worker,
            &resolved,
            st,
            res,
            start_msec,
            end_msec,
            metadata,
        )
    }

    /// Build JobResult from pre-resolved params (avoids redundant resolve_job_params calls).
    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn job_result_data_from_resolved(
        &self,
        job: Job,
        worker: &WorkerData,
        resolved: &app::app::job::ResolvedJobParams,
        st: ResultStatus,
        res: Option<ResultOutput>,
        start_msec: i64,
        end_msec: i64,
        metadata: Option<HashMap<String, String>>,
    ) -> JobResult {
        let dat = job.data.unwrap_or_default(); // XXX unwrap or default
        Self::build_job_result(
            self.id_generator(),
            job.id,
            dat,
            worker,
            resolved,
            st,
            res,
            start_msec,
            end_msec,
            metadata,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build_job_result(
        id_gen: &infra::infra::IdGeneratorWrapper,
        job_id: Option<proto::jobworkerp::data::JobId>,
        dat: proto::jobworkerp::data::JobData,
        worker: &WorkerData,
        resolved: &app::app::job::ResolvedJobParams,
        st: ResultStatus,
        res: Option<ResultOutput>,
        start_msec: i64,
        end_msec: i64,
        metadata: Option<HashMap<String, String>>,
    ) -> JobResult {
        #[allow(deprecated)]
        let data = JobResultData {
            job_id,
            worker_id: dat.worker_id,
            worker_name: worker.name.clone(),
            args: dat.args,
            uniq_key: dat.uniq_key,
            status: st as i32,
            output: res,
            retried: dat.retried,
            max_retry: resolved
                .retry_policy
                .as_ref()
                .unwrap_or(&Self::DEFAULT_RETRY_POLICY)
                .max_retry,
            priority: dat.priority,
            timeout: dat.timeout,
            streaming_type: dat.streaming_type,
            enqueue_time: dat.enqueue_time,
            run_after_time: dat.run_after_time,
            start_time: start_msec,
            end_time: end_msec,
            response_type: resolved.response_type,
            store_success: resolved.store_success,
            store_failure: resolved.store_failure,
            using: dat.using,
            // Propagated to result_processor::create_job_result_if_necessary
            broadcast_results: resolved.broadcast_results,
            resolved_retry_policy: resolved.retry_policy,
        };
        JobResult {
            id: Some(JobResultId {
                value: id_gen.generate_id().unwrap_or_default(),
            }),
            data: Some(data),
            metadata: metadata.unwrap_or_default(),
        }
    }
}

#[allow(unused_imports)]
#[cfg(any(test, feature = "test-utils"))]
pub(crate) mod tests {
    use super::{map::RunnerFactoryWithPoolMap, *};
    use anyhow::Result;
    use app::app::WorkerConfig;
    use app_wrapper::runner::RunnerFactory;
    use infra::infra::IdGeneratorWrapper;
    use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
    use jobworkerp_runner::jobworkerp::runner::{CommandArgs, CommandResult};
    use proto::jobworkerp::data::{
        Job, JobData, JobId, ResponseType, RunnerType, WorkerData, WorkerId,
    };
    use std::sync::Arc;
    use tokio::sync::OnceCell;

    // create JobRunner for test
    #[allow(dead_code)]
    struct MockJobRunner {
        runner_factory: Arc<RunnerFactory>,
        runner_pool: RunnerFactoryWithPoolMap,
        id_generator: IdGeneratorWrapper,
    }
    impl MockJobRunner {
        #[allow(dead_code)]
        async fn new() -> Self {
            let app_module = Arc::new(app::module::test::create_hybrid_test_app().await.unwrap());
            let app_wrapper_module = Arc::new(
                app_wrapper::modules::test::create_test_app_wrapper_module(app_module.clone()),
            );
            let mcp_clients =
                Arc::new(jobworkerp_runner::runner::mcp::proxy::McpServerFactory::default());
            MockJobRunner {
                runner_factory: Arc::new(RunnerFactory::new(
                    app_module.clone(),
                    app_wrapper_module.clone(),
                    mcp_clients.clone(),
                )),
                runner_pool: RunnerFactoryWithPoolMap::new(
                    Arc::new(RunnerFactory::new(
                        app_module,
                        app_wrapper_module,
                        mcp_clients,
                    )),
                    Arc::new(WorkerConfig::default()),
                ),
                id_generator: IdGeneratorWrapper::new_mock(),
            }
        }
    }
    impl jobworkerp_base::codec::UseProstCodec for MockJobRunner {}
    impl UseJobqueueAndCodec for MockJobRunner {}
    impl UseRunnerFactory for MockJobRunner {
        fn runner_factory(&self) -> &RunnerFactory {
            &self.runner_factory
        }
    }
    impl RunnerResultHandler for MockJobRunner {}
    impl UseRunnerPoolMap for MockJobRunner {
        fn runner_pool_map(&self) -> &RunnerFactoryWithPoolMap {
            &self.runner_pool
        }
    }
    impl JobRunner for MockJobRunner {
        fn register_feed_sender(&self, _job_id: i64, _sender: mpsc::Sender<FeedData>) {}

        fn unregister_feed_sender(&self, _job_id: i64) {}
    }
    impl Tracing for MockJobRunner {}
    impl UseIdGenerator for MockJobRunner {
        fn id_generator(&self) -> &IdGeneratorWrapper {
            &self.id_generator
        }
    }

    #[test]
    fn outcome_of_detects_timeout_error() {
        let timeout: Result<()> =
            Err(JobWorkerError::TimeoutError("timeout: 1000ms".to_string()).into());
        assert_eq!(outcome_of(&timeout), RunnerOutcome::TimedOut);

        let other: Result<()> = Err(anyhow!("some other failure"));
        assert_eq!(outcome_of(&other), RunnerOutcome::Normal);

        let ok: Result<()> = Ok(());
        assert_eq!(outcome_of(&ok), RunnerOutcome::Normal);
    }

    #[allow(dead_code)]
    fn load_test_job(worker_id: i64) -> Job {
        Job {
            id: Some(JobId { value: 1 }),
            data: Some(JobData {
                worker_id: Some(WorkerId { value: worker_id }),
                args: vec![],
                uniq_key: None,
                retried: 0,
                priority: 0,
                timeout: 0,
                enqueue_time: 0,
                run_after_time: 0,
                grabbed_until_time: None,
                streaming_type: 0,
                using: None,
                overrides: None,
            }),
            ..Default::default()
        }
    }

    // preload_runner() on a non-static worker instantiates and loads a runner to
    // verify the settings; for the COMMAND runner load() is a no-op so it
    // succeeds and does not leave anything resident in the pool.
    #[test]
    fn test_preload_runner_non_static_success() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            static JOB_RUNNER: OnceCell<Box<MockJobRunner>> = OnceCell::const_new();
            JOB_RUNNER
                .get_or_init(|| async { Box::new(MockJobRunner::new().await) })
                .await;
            let jr = JOB_RUNNER.get().unwrap();

            // Unique worker id: the MockJobRunner (and its pool) is shared across
            // tests via a static OnceCell, so assert on this id specifically.
            let worker_id = WorkerId { value: 9001 };
            let worker = WorkerData {
                name: "load-non-static".to_string(),
                runner_settings: vec![],
                channel: Some("test".to_string()),
                use_static: false,
                ..Default::default()
            };
            let runner_data = RunnerData {
                name: RunnerType::Command.as_str_name().to_string(),
                ..Default::default()
            };
            let res = jr
                .preload_runner(&runner_data, &worker_id, &worker, load_test_job(9001))
                .await;
            assert_eq!(res.data.unwrap().status, ResultStatus::Success as i32);
            // non-static load must not create a resident pool entry for this worker
            assert!(
                !jr.runner_pool_map()
                    .pools
                    .read()
                    .await
                    .contains_key(&worker_id.value)
            );
            Ok(())
        })
    }

    // preload_runner() on a static worker warms up the pool: after a successful
    // load the initialized runner stays resident (pool entry for the worker id).
    #[test]
    fn test_preload_runner_static_warms_up_pool() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            static JOB_RUNNER: OnceCell<Box<MockJobRunner>> = OnceCell::const_new();
            JOB_RUNNER
                .get_or_init(|| async { Box::new(MockJobRunner::new().await) })
                .await;
            let jr = JOB_RUNNER.get().unwrap();

            let worker_id = WorkerId { value: 4242 };
            // Default channel so the pool's concurrency lookup resolves against
            // WorkerConfig::default() (which only configures the default channel).
            let worker = WorkerData {
                name: "load-static".to_string(),
                runner_settings: vec![],
                channel: None,
                use_static: true,
                ..Default::default()
            };
            let runner_data = RunnerData {
                name: RunnerType::Command.as_str_name().to_string(),
                ..Default::default()
            };
            let res = jr
                .preload_runner(&runner_data, &worker_id, &worker, load_test_job(4242))
                .await;
            assert_eq!(res.data.unwrap().status, ResultStatus::Success as i32);
            assert!(
                jr.runner_pool_map()
                    .pools
                    .read()
                    .await
                    .contains_key(&worker_id.value),
                "static preload_runner should warm up the pool"
            );
            Ok(())
        })
    }

    // A worker whose runner name does not resolve fails the load (e.g. the
    // moral equivalent of a missing LLM model), surfacing as a FatalError.
    #[test]
    fn test_preload_runner_unknown_runner_fails() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            static JOB_RUNNER: OnceCell<Box<MockJobRunner>> = OnceCell::const_new();
            JOB_RUNNER
                .get_or_init(|| async { Box::new(MockJobRunner::new().await) })
                .await;
            let jr = JOB_RUNNER.get().unwrap();

            let worker_id = WorkerId { value: 2 };
            let worker = WorkerData {
                name: "load-bad".to_string(),
                runner_settings: vec![],
                channel: Some("test".to_string()),
                use_static: false,
                ..Default::default()
            };
            let runner_data = RunnerData {
                name: "NoSuchRunner".to_string(),
                ..Default::default()
            };
            let res = jr
                .preload_runner(&runner_data, &worker_id, &worker, load_test_job(2))
                .await;
            assert_eq!(res.data.unwrap().status, ResultStatus::FatalError as i32);
            Ok(())
        })
    }

    // create test for run_job() using command runner (sleep)
    // and create timeout test (using command runner)

    #[test]
    fn test_run_job() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            static JOB_RUNNER: OnceCell<Box<MockJobRunner>> = OnceCell::const_new();
            JOB_RUNNER
                .get_or_init(|| async { Box::new(MockJobRunner::new().await) })
                .await;

            let run_after = datetime::now_millis() + 1000;
            let jargs = ProstMessageCodec::serialize_message(&CommandArgs {
                command: "sleep".to_string(),
                args: vec!["1".to_string()],
                with_memory_monitoring: true,
                treat_nonzero_as_error: false,
                success_exit_codes: vec![],
                working_dir: String::new(),
            })
            .unwrap();
            let job = Job {
                id: Some(JobId { value: 1 }),
                data: Some(JobData {
                    worker_id: Some(WorkerId { value: 1 }),
                    args: jargs,
                    uniq_key: Some("test".to_string()),
                    retried: 0,
                    priority: 0,
                    timeout: 0,
                    enqueue_time: 0,
                    run_after_time: run_after,
                    grabbed_until_time: None,
                    streaming_type: 0,
                    using: None,
                    overrides: None,
                }),
                ..Default::default()
            };
            let worker_id = WorkerId { value: 1 };
            let runner_settings = vec![];

            let worker = WorkerData {
                name: "test".to_string(),
                runner_settings,
                retry_policy: None,
                channel: Some("test".to_string()),
                response_type: ResponseType::NoResult as i32,
                store_success: false,
                store_failure: false,
                ..Default::default()
            };
            let runner_data = RunnerData {
                name: RunnerType::Command.as_str_name().to_string(),
                ..Default::default()
            };
            let (res, _) = JOB_RUNNER
                .get()
                .unwrap()
                .run_job(&runner_data, &worker_id, &worker, job.clone())
                .await;
            let res = res.data.unwrap();
            let output =
                ProstMessageCodec::deserialize_message::<CommandResult>(&res.output.unwrap().items)
                    .unwrap();
            assert_eq!(res.status, ResultStatus::Success as i32);
            assert_eq!(output.exit_code.unwrap(), 0);
            assert_eq!(res.retried, 0);
            assert_eq!(res.max_retry, 0);
            assert_eq!(res.priority, 0);
            assert_eq!(res.timeout, 0);
            assert_eq!(res.enqueue_time, 0);
            assert_eq!(res.run_after_time, run_after);
            assert!(res.start_time >= run_after); // wait until run_after (expect short time)
            assert!(res.end_time > res.start_time);
            assert!(res.end_time - res.start_time >= 1000); // sleep
            let jargs = ProstMessageCodec::serialize_message(&CommandArgs {
                command: "sleep".to_string(),
                args: vec!["2".to_string()],
                with_memory_monitoring: true,
                treat_nonzero_as_error: false,
                success_exit_codes: vec![],
                working_dir: String::new(),
            })
            .unwrap();

            let timeout_job = Job {
                id: Some(JobId { value: 2 }),
                data: Some(JobData {
                    args: jargs,   // sleep 2sec
                    timeout: 1000, // timeout 1sec
                    ..job.data.as_ref().unwrap().clone()
                }),
                ..Default::default()
            };
            let (res, _) = JOB_RUNNER
                .get()
                .unwrap()
                .run_job(&runner_data, &worker_id, &worker, timeout_job.clone())
                .await;
            let res = res.data.unwrap();
            assert_eq!(res.status, ResultStatus::MaxRetry as i32); // no retry
            assert_eq!(
                res.output.unwrap().items,
                b"RuntimeError(timeout error: \"timeout: 1000ms\")"
            ); // timeout error
            assert_eq!(res.retried, 0);
            assert_eq!(res.max_retry, 0);
            assert_eq!(res.priority, 0);
            assert_eq!(res.timeout, 1000);
            assert_eq!(res.enqueue_time, 0);
            assert_eq!(res.run_after_time, run_after);
            assert!(res.start_time > run_after);
            assert!(res.end_time > 0);
            assert!(res.end_time - res.start_time >= 1000); // wait timeout 1sec
            assert!(res.end_time - res.start_time < 2000); // timeout 1sec
        });
        Ok(())
    }

    /// Test StreamingType::Response returns stream
    #[test]
    fn test_run_job_streaming_response() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            static JOB_RUNNER: OnceCell<Box<MockJobRunner>> = OnceCell::const_new();
            JOB_RUNNER
                .get_or_init(|| async { Box::new(MockJobRunner::new().await) })
                .await;

            let jargs = ProstMessageCodec::serialize_message(&CommandArgs {
                command: "echo".to_string(),
                args: vec!["streaming_response_test".to_string()],
                with_memory_monitoring: false,
                treat_nonzero_as_error: false,
                success_exit_codes: vec![],
                working_dir: String::new(),
            })
            .unwrap();

            let job = Job {
                id: Some(JobId { value: 100 }),
                data: Some(JobData {
                    worker_id: Some(WorkerId { value: 1 }),
                    args: jargs,
                    uniq_key: Some("streaming_response_test".to_string()),
                    retried: 0,
                    priority: 0,
                    timeout: 5000,
                    enqueue_time: 0,
                    run_after_time: 0,
                    grabbed_until_time: None,
                    streaming_type: StreamingType::Response as i32, // Response streaming
                    using: None,
                    overrides: None,
                }),
                ..Default::default()
            };
            let worker_id = WorkerId { value: 1 };
            let worker = WorkerData {
                name: "streaming_test".to_string(),
                runner_settings: vec![],
                retry_policy: None,
                channel: Some("test".to_string()),
                response_type: ResponseType::Direct as i32,
                store_success: false,
                store_failure: false,
                ..Default::default()
            };
            let runner_data = RunnerData {
                name: RunnerType::Command.as_str_name().to_string(),
                ..Default::default()
            };

            let (result, stream) = JOB_RUNNER
                .get()
                .unwrap()
                .run_job(&runner_data, &worker_id, &worker, job.clone())
                .await;

            // Response streaming should return a stream
            assert!(
                stream.is_some(),
                "StreamingType::Response should return a stream"
            );

            let res_data = result.data.unwrap();
            assert_eq!(
                res_data.streaming_type,
                StreamingType::Response as i32,
                "Result should preserve streaming_type"
            );

            tracing::info!("✅ StreamingType::Response test passed");
        });
        Ok(())
    }

    /// Test StreamingType::Internal returns stream (App layer calls collect_stream)
    #[test]
    fn test_run_job_streaming_internal() -> Result<()> {
        use futures::StreamExt;
        use proto::jobworkerp::data::result_output_item;

        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            static JOB_RUNNER: OnceCell<Box<MockJobRunner>> = OnceCell::const_new();
            JOB_RUNNER
                .get_or_init(|| async { Box::new(MockJobRunner::new().await) })
                .await;

            let jargs = ProstMessageCodec::serialize_message(&CommandArgs {
                command: "echo".to_string(),
                args: vec!["internal_streaming_test".to_string()],
                with_memory_monitoring: false,
                treat_nonzero_as_error: false,
                success_exit_codes: vec![],
                working_dir: String::new(),
            })
            .unwrap();

            let job = Job {
                id: Some(JobId { value: 101 }),
                data: Some(JobData {
                    worker_id: Some(WorkerId { value: 1 }),
                    args: jargs,
                    uniq_key: Some("internal_streaming_test".to_string()),
                    retried: 0,
                    priority: 0,
                    timeout: 5000,
                    enqueue_time: 0,
                    run_after_time: 0,
                    grabbed_until_time: None,
                    streaming_type: StreamingType::Internal as i32, // Internal streaming
                    using: None,
                    overrides: None,
                }),
                ..Default::default()
            };
            let worker_id = WorkerId { value: 1 };
            let worker = WorkerData {
                name: "internal_test".to_string(),
                runner_settings: vec![],
                retry_policy: None,
                channel: Some("test".to_string()),
                response_type: ResponseType::Direct as i32,
                store_success: false,
                store_failure: false,
                ..Default::default()
            };
            let runner_data = RunnerData {
                name: RunnerType::Command.as_str_name().to_string(),
                ..Default::default()
            };

            let (result, stream) = JOB_RUNNER
                .get()
                .unwrap()
                .run_job(&runner_data, &worker_id, &worker, job.clone())
                .await;

            // Internal streaming now returns a stream (App layer calls collect_stream)
            assert!(
                stream.is_some(),
                "StreamingType::Internal should return a stream for App layer to collect"
            );

            let res_data = result.data.unwrap();
            assert_eq!(res_data.status, ResultStatus::Success as i32);
            assert_eq!(
                res_data.streaming_type,
                StreamingType::Internal as i32,
                "Result should preserve streaming_type"
            );

            // Consume stream and verify content
            let mut stream = stream.unwrap();
            let mut found_data = false;
            let mut found_end = false;

            while let Some(item) = stream.next().await {
                match item.item {
                    Some(result_output_item::Item::Data(data)) => {
                        let cmd_result =
                            ProstMessageCodec::deserialize_message::<CommandResult>(&data);
                        if let Ok(cmd_result) = cmd_result
                            && cmd_result
                                .stdout
                                .as_ref()
                                .is_some_and(|s| s.contains("internal_streaming_test"))
                        {
                            found_data = true;
                        }
                    }
                    Some(result_output_item::Item::End(_)) => {
                        found_end = true;
                        break;
                    }
                    _ => {}
                }
            }

            assert!(found_data, "Stream should contain test output data");
            assert!(found_end, "Stream should have end marker");

            tracing::info!(
                "✅ StreamingType::Internal test passed - stream returned for App layer collection"
            );
        });
        Ok(())
    }

    /// Test StreamingType::None returns no stream (existing behavior)
    #[test]
    fn test_run_job_streaming_none() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            static JOB_RUNNER: OnceCell<Box<MockJobRunner>> = OnceCell::const_new();
            JOB_RUNNER
                .get_or_init(|| async { Box::new(MockJobRunner::new().await) })
                .await;

            let jargs = ProstMessageCodec::serialize_message(&CommandArgs {
                command: "echo".to_string(),
                args: vec!["non_streaming_test".to_string()],
                with_memory_monitoring: false,
                treat_nonzero_as_error: false,
                success_exit_codes: vec![],
                working_dir: String::new(),
            })
            .unwrap();

            let job = Job {
                id: Some(JobId { value: 102 }),
                data: Some(JobData {
                    worker_id: Some(WorkerId { value: 1 }),
                    args: jargs,
                    uniq_key: Some("non_streaming_test".to_string()),
                    retried: 0,
                    priority: 0,
                    timeout: 5000,
                    enqueue_time: 0,
                    run_after_time: 0,
                    grabbed_until_time: None,
                    streaming_type: StreamingType::None as i32, // Non-streaming
                    using: None,
                    overrides: None,
                }),
                ..Default::default()
            };
            let worker_id = WorkerId { value: 1 };
            let worker = WorkerData {
                name: "non_streaming_test".to_string(),
                runner_settings: vec![],
                retry_policy: None,
                channel: Some("test".to_string()),
                response_type: ResponseType::Direct as i32,
                store_success: false,
                store_failure: false,
                ..Default::default()
            };
            let runner_data = RunnerData {
                name: RunnerType::Command.as_str_name().to_string(),
                ..Default::default()
            };

            let (result, stream) = JOB_RUNNER
                .get()
                .unwrap()
                .run_job(&runner_data, &worker_id, &worker, job.clone())
                .await;

            // Non-streaming should NOT return a stream
            assert!(
                stream.is_none(),
                "StreamingType::None should NOT return a stream"
            );

            let res_data = result.data.unwrap();
            assert_eq!(res_data.status, ResultStatus::Success as i32);
            assert_eq!(
                res_data.streaming_type,
                StreamingType::None as i32,
                "Result should preserve streaming_type"
            );

            // Verify output
            let output = res_data
                .output
                .expect("Non-streaming should produce output");
            let cmd_result =
                ProstMessageCodec::deserialize_message::<CommandResult>(&output.items).unwrap();
            assert!(
                cmd_result
                    .stdout
                    .as_ref()
                    .is_some_and(|s| s.contains("non_streaming_test")),
                "Output should contain test string"
            );

            tracing::info!("✅ StreamingType::None test passed");
        });
        Ok(())
    }
}
