use anyhow::Result;
use app::app::JobBuilder;
use app::app::StorageConfig;
use app::app::UseStorageConfig;
use app::app::UseWorkerConfig;
use app::app::WorkerConfig;
use app::app::job::JobApp;
use app::app::job::OneshotCompletionGuard;
use app::app::job::StreamCompletionReceiver;
use app::app::job::UseJobApp;
use app::app::job_result::JobResultApp;
use app::app::job_result::UseJobResultApp;
use app::app::runner::RunnerApp;
use app::app::runner::UseRunnerApp;
use app::app::worker::UseWorkerApp;
use app::app::worker::WorkerApp;
use app::module::AppConfigModule;
use app::module::AppModule;
use command_utils::trace::Tracing;
use debug_stub_derive::DebugStub;
use futures::stream::BoxStream;
use infra::infra::job::{
    rows::UseJobqueueAndCodec,
    status::{JobProcessingStatusRecord, StatusTransitionResult},
};
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::JobResultData;
use proto::jobworkerp::data::JobResultId;
use proto::jobworkerp::data::ResultOutputItem;
use proto::jobworkerp::data::{JobResult, ResultOutput, ResultStatus, StreamingType, WorkerData};
use std::collections::HashMap;
use std::sync::Arc;
use tracing;

mod stream_outcome;

#[derive(DebugStub, Clone)]
pub struct ResultProcessorImpl {
    pub config_module: Arc<AppConfigModule>,
    #[debug_stub = "AppModule"]
    pub app_module: Arc<AppModule>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetryTransitionDecision {
    Requeue,
    CompleteCancelled,
    CompleteWithoutClaim,
    IgnoreStale,
}

fn retry_attempt_is_current(current: JobProcessingStatusRecord, result: &JobResultData) -> bool {
    current.retried == result.retried
}

fn retry_attempt_ownership_decision(
    current: JobProcessingStatusRecord,
    result: &JobResultData,
) -> Option<RetryTransitionDecision> {
    if !retry_attempt_is_current(current, result) {
        return Some(RetryTransitionDecision::IgnoreStale);
    }
    if current.status == proto::jobworkerp::data::JobProcessingStatus::Cancelling {
        return Some(RetryTransitionDecision::CompleteCancelled);
    }
    None
}

impl Tracing for ResultProcessorImpl {}
impl ResultProcessorImpl {
    pub fn new(
        config_module: Arc<AppConfigModule>,
        app_module: Arc<AppModule>,
    ) -> ResultProcessorImpl {
        // for shutdown notification (spmc broadcast)::<()>
        Self {
            config_module,
            app_module,
        }
    }

    pub async fn process_result(
        &self,
        jr: JobResult,
        st_data: Option<BoxStream<'static, ResultOutputItem>>,
        w: WorkerData,
    ) -> Result<(JobResult, StreamCompletionReceiver)> {
        self.process_result_inner(jr, st_data, w, false).await
    }

    /// `load_only`: the result comes from a pre-load (config-check) request, not
    /// a real job execution. Such a result must NOT go through the normal job
    /// lifecycle — no retry, no periodic re-enqueue, no result persistence, no
    /// temp-worker cleanup — otherwise pre-loading e.g. a periodic worker would
    /// start enqueuing real run() jobs as a side effect. Only the Direct result
    /// is published so the Load caller can observe the load outcome.
    pub async fn process_result_inner(
        &self,
        jr: JobResult,
        st_data: Option<BoxStream<'static, ResultOutputItem>>,
        w: WorkerData,
        load_only: bool,
    ) -> Result<(JobResult, StreamCompletionReceiver)> {
        tracing::debug!("got job_result: {:?}, worker: {:?}", &jr.id, &w.name);
        if let JobResult {
            id: Some(id),
            data: Some(data),
            metadata,
        } = jr
        {
            let mut data = self.cancel_retryable_result_if_requested(data).await?;
            // Load-only: skip retry/periodic/store/temp-cleanup entirely and only
            // publish the Direct result so the Load caller unblocks.
            if load_only {
                let completion_rx = self.job_app().complete_job(&id, &data, st_data).await?.1;
                return Ok((
                    JobResult {
                        id: Some(id),
                        data: Some(data),
                        metadata,
                    },
                    completion_rx,
                ));
            }

            // A streaming job's result was decided when its stream started; the
            // End decides whether it really succeeded, so watch for it.
            let (st_data, end_probe) = if stream_outcome::can_be_settled(&data) {
                st_data.map(stream_outcome::observe_end).unzip()
            } else {
                (st_data, None)
            };

            // Retry/complete first: complete_job publishes result via pubsub.
            // IMPORTANT: This must happen BEFORE create_job_result_if_necessary.
            // For streaming jobs (StreamingType != None), JobResultData.output is None
            // because the actual output is delivered via the stream. If we stored
            // the result to Redis/RDB first, listen_result's find_job_result_by_job_id
            // would find a cached entry with output=None and return it immediately
            // without the stream, causing callers to see empty output.
            let complete_or_retry_result = self
                .process_complete_or_retry_condition(&id, &mut data, st_data, &w, &metadata)
                .await;
            // Store result if necessary by result status and worker setting.
            // data.broadcast_results is already resolved by resolve_job_params()
            // in runner.rs (merging worker defaults with job-level overrides).
            match self
                .job_result_app()
                .create_job_result_if_necessary(&id, &data, data.broadcast_results)
                .await
            {
                Ok(_r) => {
                    let completion_rx =
                        match (end_probe, complete_or_retry_result?) {
                            (Some(probe), Some(published)) => Some(
                                self.settle_result_after_stream(id, data.clone(), probe, published),
                            ),
                            (_, completion_rx) => completion_rx,
                        };
                    Ok((
                        JobResult {
                            id: Some(id),
                            data: Some(data),
                            metadata,
                        },
                        completion_rx,
                    ))
                }
                Err(e) => {
                    tracing::error!(
                        "job result store error: {:?}, complete_or_retry: {:?}",
                        e,
                        complete_or_retry_result
                    );
                    Err(e)
                }
            }
        } else {
            tracing::warn!(
                "job result without id or data: {}",
                proto::log_ext::JobResultSummary(&jr)
            );
            Err(JobWorkerError::NotFound("job result without id or data".to_string()).into())
        }
    }

    /// Settles the stored result of a streaming job once its stream has been
    /// published to the end, and signals completion only afterwards so the
    /// worker holds the job (and graceful shutdown waits) until the stored
    /// result is final.
    fn settle_result_after_stream(
        &self,
        id: JobResultId,
        recorded: JobResultData,
        probe: stream_outcome::StreamEndProbe,
        published: tokio::sync::oneshot::Receiver<()>,
    ) -> tokio::sync::oneshot::Receiver<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let job_result_app = self.job_result_app().clone();
        tokio::spawn(async move {
            let _guard = OneshotCompletionGuard::new(tx);
            // A dropped sender also means the publish task has ended.
            let _ = published.await;
            let Some(outcome) = probe.outcome() else {
                tracing::warn!(
                    result_id = id.value,
                    job_id = ?recorded.job_id.map(|j| j.value),
                    "result stream ended without an End reaching its publisher; \
                     keeping the recorded result"
                );
                return;
            };
            let proto::stream_error::StreamErrorOutcome::Failed(error) = outcome else {
                return;
            };
            let settled = stream_outcome::settled_result(&recorded, &error);
            if let Err(e) = job_result_app
                .replace_job_result(&id, &recorded, &settled)
                .await
            {
                tracing::error!(
                    result_id = id.value,
                    job_id = ?recorded.job_id.map(|j| j.value),
                    "failed to store the result settled by the stream End: {e:?}"
                );
            }
        });
        rx
    }

    async fn process_complete_or_retry_condition(
        &self,
        id: &JobResultId,
        dat: &mut JobResultData,
        stream: Option<BoxStream<'static, ResultOutputItem>>,
        worker: &WorkerData,
        metadata: &HashMap<String, String>,
    ) -> Result<StreamCompletionReceiver> {
        // retry or periodic job
        let jopt = Self::build_retry_job(dat, worker, metadata);
        // need to retry
        if let Some(j) = jopt {
            match self.transition_retry_to_pending_or_cancel(dat, &j).await? {
                RetryTransitionDecision::Requeue => {}
                RetryTransitionDecision::CompleteCancelled
                | RetryTransitionDecision::CompleteWithoutClaim => {
                    return self
                        .job_app()
                        .complete_job(id, dat, stream)
                        .await
                        .map(|(_, rx)| rx);
                }
                RetryTransitionDecision::IgnoreStale => {
                    tracing::info!(
                        job_id = ?dat.job_id,
                        retried = dat.retried,
                        "Discarding stale retry result without completing the current job"
                    );
                    return Ok(None);
                }
            }
            // update or insert job for retry or periodic
            tracing::debug!(
                "need to retry worker: {:?}, job: {}",
                &worker.name,
                proto::log_ext::JobSummary(&j)
            );
            self.job_app().update_job(&j).await?;
            Ok(None)
        } else {
            // complete job (delete first for unique key)
            // Preserve error for later propagation while still running periodic/cleanup logic
            let (complete_result, completion_rx) =
                match self.job_app().complete_job(id, dat, stream).await {
                    Ok((b, rx)) => (Ok(b), rx),
                    Err(e) => (Err(e), None),
                };

            // the job finished
            // if finished periodic job, enqueue next periodic job
            if let Some(pj) = Self::build_next_periodic_job(dat, worker) {
                let pjres = self
                    .job_app()
                    .enqueue_job(
                        Arc::new(metadata.clone()),
                        pj.worker_id.as_ref(),
                        None,
                        pj.args,
                        pj.uniq_key,
                        pj.run_after_time,
                        pj.priority,
                        pj.timeout,
                        dat.job_id, // use same job id for periodic job if possible
                        StreamingType::try_from(pj.streaming_type).unwrap_or(StreamingType::None),
                        pj.using,     // preserve using for periodic re-execution
                        pj.overrides, // preserve resolved overrides for periodic re-execution
                    )
                    .await?;
                tracing::info!(
                    "Next periodic job id: {:?}, worker id:{:?}",
                    pjres.0,
                    &pj.worker_id
                );
            };

            // Delete temp worker if use_static is false (after all processing is done).
            // No runner pool release needed: non-static workers never have pooled runners.
            if !worker.use_static
                && let Some(wid) = dat.worker_id.as_ref()
            {
                if let Err(e) = self.worker_app().delete_temp(wid).await {
                    tracing::info!("failed to delete temp worker {:?}: {:?}", wid, e);
                } else {
                    tracing::debug!("deleted temp worker: {:?}", wid);
                }
            }

            // Propagate complete_job error after cleanup
            complete_result?;
            Ok(completion_rx)
        }
    }

    async fn cancel_retryable_result_if_requested(
        &self,
        mut data: JobResultData,
    ) -> Result<JobResultData> {
        if data.status != ResultStatus::ErrorAndRetry as i32 {
            return Ok(data);
        }
        let Some(job_id) = data.job_id.as_ref() else {
            return Ok(data);
        };
        let cancelled = matches!(
            self.app_module
                .job_processing_status_repository()
                .find_status_record(job_id)
                .await?,
            Some(record)
                if record.status == proto::jobworkerp::data::JobProcessingStatus::Cancelling
                    && record.retried == data.retried
        );
        if cancelled {
            Self::mark_retry_cancelled(&mut data);
        }
        Ok(data)
    }

    async fn transition_retry_to_pending_or_cancel(
        &self,
        data: &mut JobResultData,
        retry_job: &proto::jobworkerp::data::Job,
    ) -> Result<RetryTransitionDecision> {
        let Some(job_id) = data.job_id.as_ref() else {
            return Ok(RetryTransitionDecision::Requeue);
        };
        let Some(next_data) = retry_job.data.as_ref() else {
            return Ok(RetryTransitionDecision::Requeue);
        };
        let repository = self.app_module.job_processing_status_repository();
        let Some(current) = repository.find_status_record(job_id).await? else {
            return Ok(RetryTransitionDecision::CompleteWithoutClaim);
        };
        if let Some(decision) = retry_attempt_ownership_decision(current, data) {
            match decision {
                RetryTransitionDecision::IgnoreStale => {
                    tracing::info!(
                        job_id = job_id.value,
                        result_retried = data.retried,
                        current_retried = current.retried,
                        "Ignoring stale retry result for a newer job attempt"
                    );
                }
                RetryTransitionDecision::CompleteCancelled => Self::mark_retry_cancelled(data),
                RetryTransitionDecision::Requeue
                | RetryTransitionDecision::CompleteWithoutClaim => {}
            }
            return Ok(decision);
        }
        let next = JobProcessingStatusRecord {
            status: proto::jobworkerp::data::JobProcessingStatus::Pending,
            retried: next_data.retried,
        };
        match repository
            .compare_and_set_status(job_id, Some(current), Some(next))
            .await?
        {
            StatusTransitionResult::Applied => Ok(RetryTransitionDecision::Requeue),
            StatusTransitionResult::Conflict(Some(record))
                if record.status == proto::jobworkerp::data::JobProcessingStatus::Cancelling
                    && record.retried == data.retried =>
            {
                Self::mark_retry_cancelled(data);
                Ok(RetryTransitionDecision::CompleteCancelled)
            }
            StatusTransitionResult::Conflict(Some(_)) => Ok(RetryTransitionDecision::IgnoreStale),
            StatusTransitionResult::Conflict(None) => {
                Ok(RetryTransitionDecision::CompleteWithoutClaim)
            }
        }
    }

    fn mark_retry_cancelled(data: &mut JobResultData) {
        const MESSAGE: &str = "Job was cancelled while retrying";
        data.status = ResultStatus::Cancelled as i32;
        data.output = Some(ResultOutput {
            items: MESSAGE.as_bytes().to_vec(),
        });
        // Replace the failed attempt's classification: the outcome is the cancel.
        data.error = Some(proto::stream_error::cancelled_by_worker(MESSAGE));
        data.max_retry = 0;
    }
}

impl jobworkerp_base::codec::UseProstCodec for ResultProcessorImpl {}
impl UseJobqueueAndCodec for ResultProcessorImpl {}
impl UseJobResultApp for ResultProcessorImpl {
    fn job_result_app(&self) -> &Arc<dyn JobResultApp + 'static> {
        &self.app_module.job_result_app
    }
}
impl UseJobApp for ResultProcessorImpl {
    fn job_app(&self) -> &Arc<dyn JobApp + 'static> {
        &self.app_module.job_app
    }
}
impl UseWorkerApp for ResultProcessorImpl {
    fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static> {
        &self.app_module.worker_app
    }
}
impl UseRunnerApp for ResultProcessorImpl {
    fn runner_app(&self) -> Arc<dyn RunnerApp + 'static> {
        self.app_module.runner_app.clone()
    }
}

impl JobBuilder for ResultProcessorImpl {}

impl UseWorkerConfig for ResultProcessorImpl {
    fn worker_config(&self) -> &WorkerConfig {
        &self.config_module.worker_config
    }
}
impl UseStorageConfig for ResultProcessorImpl {
    fn storage_config(&self) -> &StorageConfig {
        &self.config_module.storage_config
    }
}
pub trait UseResultProcessor {
    fn result_processor(&self) -> &ResultProcessorImpl;
}

//impl UseIdGenerator for ResultProcessorImpl {
//    fn id_generator(&self) -> &IdGeneratorWrapper {
//        &self.id_generator
//    }
//}

#[cfg(test)]
mod retry_transition_tests {
    use super::*;
    use proto::jobworkerp::data::JobProcessingStatus;

    #[test]
    fn retry_cancellation_replaces_the_failed_attempt_classification() {
        use proto::jobworkerp::data::StreamErrorCode;
        let mut data = JobResultData {
            status: ResultStatus::ErrorAndRetry as i32,
            error: Some(proto::stream_error::classified_error(
                StreamErrorCode::Timeout,
                "timeout error",
                "COMMAND",
            )),
            max_retry: 3,
            ..Default::default()
        };
        ResultProcessorImpl::mark_retry_cancelled(&mut data);
        assert_eq!(data.status, ResultStatus::Cancelled as i32);
        let error = data.error.expect("classification");
        assert_eq!(error.code, StreamErrorCode::Cancelled as i32);
        assert_eq!(error.origin, proto::stream_error::WORKER_ORIGIN);
        assert_eq!(data.max_retry, 0);
    }

    #[test]
    fn stale_result_cannot_transition_a_newer_attempt_to_pending() {
        let current = JobProcessingStatusRecord {
            status: JobProcessingStatus::Running,
            retried: 2,
        };
        let result = JobResultData {
            retried: 1,
            ..Default::default()
        };
        assert!(!retry_attempt_is_current(current, &result));
    }

    #[test]
    fn current_result_can_transition_its_own_attempt_to_pending() {
        let current = JobProcessingStatusRecord {
            status: JobProcessingStatus::Running,
            retried: 1,
        };
        let result = JobResultData {
            retried: 1,
            ..Default::default()
        };
        assert!(retry_attempt_is_current(current, &result));
    }

    #[test]
    fn stale_retry_outcome_does_not_complete_the_current_job() {
        let current = JobProcessingStatusRecord {
            status: JobProcessingStatus::Pending,
            retried: 2,
        };
        let result = JobResultData {
            retried: 1,
            ..Default::default()
        };

        assert_eq!(
            retry_attempt_ownership_decision(current, &result),
            Some(RetryTransitionDecision::IgnoreStale)
        );
    }
}

#[cfg(test)]
mod stream_settlement_tests {
    use super::*;
    use app::module::test::{create_hybrid_test_app, create_rdb_chan_test_app};
    use futures::StreamExt;
    use infra_utils::infra::test::TEST_RUNTIME;
    use proto::jobworkerp::data::{JobId, ResponseType, StreamErrorCode, WorkerId};
    use std::time::Duration;

    fn worker() -> WorkerData {
        WorkerData {
            name: "stream_settlement_test".to_string(),
            response_type: ResponseType::Direct as i32,
            store_success: true,
            store_failure: true,
            use_static: true,
            ..Default::default()
        }
    }

    fn stream_started(streaming_type: StreamingType) -> JobResult {
        JobResult {
            id: Some(JobResultId {
                value: rand::random::<i64>().abs(),
            }),
            data: Some(JobResultData {
                job_id: Some(JobId {
                    value: rand::random::<i64>().abs(),
                }),
                worker_id: Some(WorkerId { value: 1 }),
                status: ResultStatus::Success as i32,
                output: Some(ResultOutput { items: vec![] }),
                streaming_type: streaming_type as i32,
                response_type: ResponseType::Direct as i32,
                store_success: true,
                store_failure: true,
                ..Default::default()
            }),
            metadata: HashMap::new(),
        }
    }

    fn failing_stream() -> BoxStream<'static, ResultOutputItem> {
        futures::stream::iter(vec![
            ResultOutputItem {
                item: Some(proto::jobworkerp::data::result_output_item::Item::Data(
                    b"partial".to_vec(),
                )),
            },
            proto::stream_error::error_end_item(
                HashMap::new(),
                StreamErrorCode::ExecutionFailed,
                "boom",
                "COMMAND",
            ),
        ])
        .boxed()
    }

    fn successful_stream() -> BoxStream<'static, ResultOutputItem> {
        futures::stream::iter(vec![proto::stream_error::normal_end_item(HashMap::new())]).boxed()
    }

    /// Processes a result whose stream already started and returns the stored
    /// result once the worker would release the job (completion received).
    async fn process_and_wait(
        app: Arc<AppModule>,
        result: JobResult,
        stream: BoxStream<'static, ResultOutputItem>,
    ) -> Result<Option<JobResultData>> {
        let id = result.id.unwrap();
        let processor = ResultProcessorImpl::new(app.config_module.clone(), app.clone());
        let (_, completion_rx) = processor
            .process_result(result, Some(stream), worker())
            .await?;
        tokio::time::timeout(
            Duration::from_secs(10),
            completion_rx.expect("a streamed result has a completion"),
        )
        .await??;
        Ok(app
            .job_result_app
            .find_job_result_from_db(&id)
            .await?
            .and_then(|stored| stored.data))
    }

    type StreamFixture = fn() -> BoxStream<'static, ResultOutputItem>;

    fn status_of(stored: &Option<JobResultData>) -> Option<ResultStatus> {
        stored.as_ref().map(|data| data.status())
    }

    #[test]
    fn stored_result_follows_the_stream_end() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let app = Arc::new(create_hybrid_test_app().await?);
            let cases: [(StreamingType, StreamFixture, ResultStatus); 3] = [
                (
                    StreamingType::Response,
                    failing_stream,
                    ResultStatus::MaxRetry,
                ),
                (
                    StreamingType::Response,
                    successful_stream,
                    ResultStatus::Success,
                ),
                (
                    StreamingType::Internal,
                    failing_stream,
                    ResultStatus::MaxRetry,
                ),
            ];
            for (streaming_type, stream, expected) in cases {
                let stored =
                    process_and_wait(app.clone(), stream_started(streaming_type), stream()).await?;
                assert_eq!(status_of(&stored), Some(expected), "{streaming_type:?}");
            }
            Ok(())
        })
    }

    #[test]
    fn failed_stream_stores_its_diagnostic_without_retry() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let app = Arc::new(create_hybrid_test_app().await?);
            let result = stream_started(StreamingType::Response);
            let job_id = result.data.as_ref().unwrap().job_id.unwrap();
            let stored = process_and_wait(app.clone(), result, failing_stream())
                .await?
                .expect("the failure is stored");

            let output = String::from_utf8(stored.output.unwrap().items)?;
            assert_eq!(output, "stream error: COMMAND EXECUTION_FAILED: boom");
            assert!(
                app.job_app.find_job(&job_id).await?.is_none(),
                "a failed stream must not be retried"
            );
            Ok(())
        })
    }

    #[test]
    fn rdb_chan_settles_a_stream_that_reached_its_subscriber() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let app = Arc::new(create_rdb_chan_test_app(true, false).await?);
            let result = stream_started(StreamingType::Response);
            let job_id = result.data.as_ref().unwrap().job_id.unwrap();
            let subscribed = app
                .job_result_app
                .subscribe_stream_by_job_id(&job_id, Some(10_000))
                .await?
                .expect("stream subscription");
            let consumer = tokio::spawn(subscribed.collect::<Vec<_>>());

            let stored = process_and_wait(app, result, failing_stream()).await?;
            assert_eq!(status_of(&stored), Some(ResultStatus::MaxRetry));
            consumer.abort();
            Ok(())
        })
    }

    // Standalone mode drops a stream nobody subscribed to without reading it, so
    // its End is never seen and the recorded result stays as it was.
    #[test]
    fn rdb_chan_keeps_the_result_of_a_stream_without_subscriber() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let app = Arc::new(create_rdb_chan_test_app(true, false).await?);
            let stored = process_and_wait(
                app,
                stream_started(StreamingType::Response),
                failing_stream(),
            )
            .await?;
            assert_eq!(status_of(&stored), Some(ResultStatus::Success));
            Ok(())
        })
    }
}
