//! Shared steps for running a child job and mapping its failure to an Open
//! Workflow standard error.
//!
//! The same error type means different things depending on where it was
//! raised (an `InvalidParameter` while converting arguments is a schema
//! violation, while enqueueing it is a worker that cannot run the request), so
//! callers tag each step with a [`ChildJobStage`] and the cause is read in the
//! context of that stage.

use crate::workflow::{
    definition::workflow::{
        self,
        errors::{ErrorFactory, StandardErrorType},
    },
    execute::{context::TaskContext, failure},
};
use anyhow::Context;
use app::app::{
    job::execute::{JobExecutorWrapper, JobFailedError, UseJobExecutor},
    runner::UseRunnerApp,
};
use infra::infra::runner::rows::RunnerWithSchema;
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{JobResult, StreamError};
use proto::stream_error::StreamFailedError;

/// Step of running a child job, attached to its errors with `anyhow::Context`.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ChildJobStage {
    /// Finding the runner or worker and converting the arguments.
    Prepare,
    /// Converting the runner settings.
    Settings,
    /// Submitting the job.
    Enqueue,
    /// Waiting for the result or stream items.
    Wait,
    /// Converting the job output.
    Decode,
}

impl std::fmt::Display for ChildJobStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Prepare => "failed to prepare the child job",
            Self::Settings => "failed to apply the runner settings",
            Self::Enqueue => "failed to enqueue the child job",
            Self::Wait => "failed while waiting for the child job",
            Self::Decode => "failed to decode the child job output",
        })
    }
}

/// Looks up a runner by name; an unknown name is a [`JobWorkerError::NotFound`].
pub(crate) async fn find_runner(
    job_executor_wrapper: &JobExecutorWrapper,
    runner_name: &str,
) -> anyhow::Result<RunnerWithSchema> {
    job_executor_wrapper
        .runner_app()
        .find_runner_by_name(runner_name)
        .await
        .and_then(|runner| {
            runner.ok_or_else(|| {
                JobWorkerError::NotFound(format!("Runner '{runner_name}' not found")).into()
            })
        })
        .context(ChildJobStage::Prepare)
}

/// Output bytes of an awaited child job, or why there are none.
pub(crate) fn finished_output<S>(
    job_executor_wrapper: &JobExecutorWrapper,
    wait_result: anyhow::Result<(Option<JobResult>, S)>,
) -> anyhow::Result<Vec<u8>> {
    let (result, _stream) = wait_result.context(ChildJobStage::Wait)?;
    let result = result
        .ok_or_else(|| anyhow::anyhow!("Failed to enqueue job or job result not found"))
        .context(ChildJobStage::Decode)?;
    job_executor_wrapper.extract_job_result_output(result)
}

/// Builds the workflow error for a failed child job. An error the job
/// reported (e.g. a child workflow's own error) is raised again at
/// `position`; otherwise `default_kind` is used when neither the stage nor the
/// job result tells the cause (runtime for run tasks, communication for call
/// tasks).
pub(crate) fn to_workflow_error(
    error: anyhow::Error,
    default_kind: StandardErrorType,
    title: &str,
    position: Option<String>,
) -> Box<workflow::Error> {
    let kind = match reported_failure(&error) {
        Some(classification) => {
            if let Some(raised) = classification
                .and_then(|c| c.problem.as_ref())
                .and_then(|problem| failure::raise_reported(problem, position.clone()))
            {
                return raised;
            }
            classification.and_then(|c| failure::kind_for_code(c.code))
        }
        None => classify_stage(&error),
    };
    ErrorFactory::new().standard(
        kind.unwrap_or(default_kind),
        title.to_string(),
        position,
        Some(format!("{error:#}")),
    )
}

/// [`to_workflow_error`] at the task's current position, logging the failure.
pub(crate) async fn fail(
    task_context: &TaskContext,
    error: anyhow::Error,
    default_kind: StandardErrorType,
    title: &str,
) -> Box<workflow::Error> {
    let position = task_context.position.read().await.as_error_instance();
    tracing::error!(error = ?error, position = %position, "{title}");
    to_workflow_error(error, default_kind, title, Some(position))
}

/// For a failure reported by the job itself, its classification (absent from
/// older workers); `None` when the job did not report the failure.
fn reported_failure(error: &anyhow::Error) -> Option<Option<&StreamError>> {
    if let Some(failure) = error.downcast_ref::<JobFailedError>() {
        return Some(failure.classification.as_ref());
    }
    error
        .downcast_ref::<StreamFailedError>()
        .map(|StreamFailedError(failure)| Some(failure))
}

fn classify_stage(error: &anyhow::Error) -> Option<StandardErrorType> {
    let cause = error.downcast_ref::<JobWorkerError>();
    let kind = match error.downcast_ref::<ChildJobStage>()? {
        ChildJobStage::Prepare => match cause {
            Some(JobWorkerError::NotFound(_) | JobWorkerError::WorkerNotFound(_)) => {
                StandardErrorType::Configuration
            }
            Some(JobWorkerError::InvalidParameter(_)) => StandardErrorType::Validation,
            _ => StandardErrorType::Runtime,
        },
        ChildJobStage::Settings => StandardErrorType::Configuration,
        ChildJobStage::Enqueue => match cause {
            Some(
                JobWorkerError::InvalidParameter(_)
                | JobWorkerError::FailedPrecondition(_)
                | JobWorkerError::NotFound(_)
                | JobWorkerError::WorkerNotFound(_),
            ) => StandardErrorType::Configuration,
            _ => StandardErrorType::Runtime,
        },
        ChildJobStage::Wait => match cause {
            Some(JobWorkerError::TimeoutError(_)) => StandardErrorType::Timeout,
            _ => StandardErrorType::Runtime,
        },
        ChildJobStage::Decode => StandardErrorType::Runtime,
    };
    Some(kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::execute::failure::workflow_failure;
    use proto::jobworkerp::data::{ResultStatus, StreamErrorCode};

    const RUNTIME: StandardErrorType = StandardErrorType::Runtime;
    const COMMUNICATION: StandardErrorType = StandardErrorType::Communication;

    fn kind_of(error: anyhow::Error, default_kind: StandardErrorType) -> String {
        to_workflow_error(
            error,
            default_kind,
            "failed",
            Some("/do/0/task".to_string()),
        )
        .type_
        .0
    }

    fn staged(cause: JobWorkerError, stage: ChildJobStage) -> anyhow::Error {
        anyhow::Error::new(cause).context(stage)
    }

    fn job_failed(code: Option<StreamErrorCode>) -> anyhow::Error {
        JobFailedError {
            status: ResultStatus::MaxRetry,
            output: Some("failed".to_string()),
            classification: code
                .map(|code| proto::stream_error::classified_error(code, "failed", "X")),
        }
        .into()
    }

    #[test]
    fn stage_and_cause_decide_the_type() {
        use ChildJobStage::*;
        use StandardErrorType::*;
        let not_found = || JobWorkerError::NotFound("missing".into());
        let worker_not_found = || JobWorkerError::WorkerNotFound("missing".into());
        let invalid = || JobWorkerError::InvalidParameter("bad".into());
        let precondition = || JobWorkerError::FailedPrecondition("no".into());
        let timeout = || JobWorkerError::TimeoutError("slow".into());
        let other = || JobWorkerError::RuntimeError("db down".into());
        let cases = [
            (Prepare, not_found(), Configuration),
            (Prepare, worker_not_found(), Configuration),
            (Prepare, invalid(), Validation),
            (Prepare, other(), Runtime),
            (Settings, invalid(), Configuration),
            (Settings, other(), Configuration),
            (Enqueue, invalid(), Configuration),
            (Enqueue, precondition(), Configuration),
            (Enqueue, worker_not_found(), Configuration),
            (Enqueue, not_found(), Configuration),
            (Enqueue, other(), Runtime),
            (Wait, timeout(), Timeout),
            (Wait, other(), Runtime),
            (Decode, invalid(), Runtime),
        ];
        for (stage, cause, expected) in cases {
            let label = format!("{stage:?} / {cause:?}");
            // The stage, not the task default, decides: call tasks get the same type.
            assert_eq!(
                kind_of(staged(cause, stage), COMMUNICATION),
                expected.uri(),
                "{label}"
            );
        }
    }

    #[test]
    fn reported_codes_decide_the_type_of_a_failed_job() {
        let cases = [
            (StreamErrorCode::Timeout, Some(StandardErrorType::Timeout)),
            (StreamErrorCode::TransportFailed, Some(COMMUNICATION)),
            (StreamErrorCode::ProviderStreamFailed, Some(COMMUNICATION)),
            (
                StreamErrorCode::InvalidArgument,
                Some(StandardErrorType::Validation),
            ),
            (StreamErrorCode::ExecutionFailed, None),
            (StreamErrorCode::Cancelled, None),
            (StreamErrorCode::Unspecified, None),
        ];
        for (code, expected) in cases {
            for default_kind in [RUNTIME, COMMUNICATION] {
                let want = expected.unwrap_or(default_kind).uri();
                assert_eq!(
                    kind_of(job_failed(Some(code)), default_kind),
                    want,
                    "{code:?}"
                );

                // Built directly: producers may not emit UNSPECIFIED, but readers
                // must still handle it.
                let trailer = proto::jobworkerp::data::Trailer {
                    error: Some(StreamError {
                        code: code as i32,
                        message: "failed".into(),
                        origin: "X".into(),
                        problem: None,
                    }),
                    ..Default::default()
                };
                let stream_failure = proto::stream_error::ensure_end_success(&trailer)
                    .unwrap_err()
                    .context(ChildJobStage::Wait);
                assert_eq!(
                    kind_of(stream_failure, default_kind),
                    want,
                    "stream {code:?}"
                );
            }
        }
    }

    #[test]
    fn failures_without_a_cause_use_the_task_default() {
        assert_eq!(kind_of(job_failed(None), RUNTIME), RUNTIME.uri());
        assert_eq!(
            kind_of(job_failed(None), COMMUNICATION),
            COMMUNICATION.uri()
        );
        // An unknown code from a newer worker is still a failure of unknown cause.
        let mut unknown =
            proto::stream_error::classified_error(StreamErrorCode::ExecutionFailed, "failed", "X");
        unknown.code = 99;
        let error: anyhow::Error = JobFailedError {
            status: ResultStatus::OtherError,
            output: None,
            classification: Some(unknown),
        }
        .into();
        assert_eq!(kind_of(error, RUNTIME), RUNTIME.uri());
        assert_eq!(
            kind_of(anyhow::anyhow!("untagged"), COMMUNICATION),
            COMMUNICATION.uri()
        );
    }

    const CUSTOM_TYPE: &str = "https://example.com/errors/custom";

    fn child_error(type_uri: &str, status: i64) -> workflow::Error {
        workflow::Error {
            type_: workflow::UriTemplate(type_uri.to_string()),
            status,
            title: Some("Child failed".to_string()),
            detail: Some("child detail".to_string()),
            instance: Some("/do/0/inner".to_string()),
        }
    }

    fn failed_with(classification: StreamError) -> anyhow::Error {
        JobFailedError {
            status: ResultStatus::ErrorAndRetry,
            output: Some("runtime error".to_string()),
            classification: Some(classification),
        }
        .into()
    }

    #[test]
    fn parent_error_takes_the_child_type_status_and_title_at_its_own_position() {
        let classification = workflow_failure(&child_error(CUSTOM_TYPE, 418), "diagnostic");
        let stream_failure: anyhow::Error = StreamFailedError(classification.clone()).into();
        for error in [failed_with(classification), stream_failure] {
            let error = to_workflow_error(
                error,
                RUNTIME,
                "Failed to execute run alias by jobworkerp",
                Some("/do/1/parent".to_string()),
            );
            assert_eq!(error.type_.0, CUSTOM_TYPE);
            assert_eq!(error.status, 418);
            assert_eq!(error.title.as_deref(), Some("Child failed"));
            assert_eq!(error.instance.as_deref(), Some("/do/1/parent"));
            let detail = error.detail.unwrap();
            assert!(detail.contains("child detail"), "{detail}");
            assert!(detail.contains("/do/0/inner"), "{detail}");
        }
    }

    #[test]
    fn problem_without_a_type_falls_back_to_the_code() {
        let mut failure =
            workflow_failure(&child_error(StandardErrorType::Timeout.uri(), 408), "d");
        failure.problem.as_mut().unwrap().r#type.clear();
        let error = to_workflow_error(failed_with(failure), RUNTIME, "failed", None);
        assert_eq!(error.type_.0, StandardErrorType::Timeout.uri());
        assert_eq!(error.title.as_deref(), Some("failed"));
    }

    #[test]
    fn error_keeps_the_title_position_and_full_cause() {
        let error = to_workflow_error(
            staged(
                JobWorkerError::NotFound("runner 'X'".into()),
                ChildJobStage::Prepare,
            ),
            RUNTIME,
            "Failed to execute by jobworkerp",
            Some("/do/0/task".to_string()),
        );
        assert_eq!(
            error.title.as_deref(),
            Some("Failed to execute by jobworkerp")
        );
        assert_eq!(error.instance.as_deref(), Some("/do/0/task"));
        let detail = error.detail.unwrap();
        assert!(
            detail.contains("failed to prepare the child job"),
            "{detail}"
        );
        assert!(detail.contains("runner 'X'"), "{detail}");
    }
}
