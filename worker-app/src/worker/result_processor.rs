use anyhow::Result;
use app::app::JobBuilder;
use app::app::StorageConfig;
use app::app::UseStorageConfig;
use app::app::UseWorkerConfig;
use app::app::WorkerConfig;
use app::app::job::JobApp;
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
                    let completion_rx = complete_or_retry_result?;
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
        data.status = ResultStatus::Cancelled as i32;
        data.output = Some(ResultOutput {
            items: b"Job was cancelled while retrying".to_vec(),
        });
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
