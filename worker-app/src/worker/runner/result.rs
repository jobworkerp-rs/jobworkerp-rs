use anyhow::{Result, anyhow};
use futures::stream::BoxStream;
use jobworkerp_base::error::JobWorkerError;
use prost::Message;
use proto::jobworkerp::data::{
    JobData, ResultOutput, ResultOutputItem, ResultStatus, RetryPolicy, RetryType, StreamError,
    StreamErrorCode,
};
use std::collections::HashMap;

/// Context attached to a failure the worker itself detected (e.g. the job
/// timeout), so its classification names the worker rather than the runner.
#[derive(Debug, Clone, Copy)]
pub struct DetectedByWorker;

impl std::fmt::Display for DetectedByWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("detected by worker")
    }
}

pub enum ResultOutputEnum {
    Normal(Result<Vec<u8>>, HashMap<String, String>), // result and metadata
    Stream(BoxStream<'static, ResultOutputItem>),
}
impl ResultOutputEnum {
    pub fn bytes(&self) -> Vec<u8> {
        match self {
            ResultOutputEnum::Normal(v, _) => v.iter().flatten().copied().collect(),
            ResultOutputEnum::Stream(_) => ResultOutputItem { item: None }.encode_to_vec(),
        }
    }
    pub fn result_output(&self) -> Option<ResultOutput> {
        match self {
            ResultOutputEnum::Normal(Ok(v), _) => Some(ResultOutput { items: v.clone() }),
            // error output
            ResultOutputEnum::Normal(Err(e), _) => Some(ResultOutput {
                items: e.to_string().as_bytes().to_vec(),
            }), // empty result for error
            ResultOutputEnum::Stream(_) => None,
        }
    }
    pub fn result(&self) -> Result<ResultOutput> {
        match self {
            ResultOutputEnum::Normal(res, _) => res
                .as_ref()
                .map(|r| ResultOutput { items: r.clone() })
                .map_err(|e| anyhow!(e.to_string())),
            ResultOutputEnum::Stream(_) => Ok(ResultOutput { items: vec![] }), // empty result for stream
        }
    }
    pub fn stream(self) -> Option<BoxStream<'static, ResultOutputItem>> {
        match self {
            ResultOutputEnum::Normal(_, _) => None,
            ResultOutputEnum::Stream(s) => Some(s),
        }
    }
    pub fn is_stream(&self) -> bool {
        matches!(self, ResultOutputEnum::Stream(_))
    }
    pub fn metadata(&self) -> Option<&HashMap<String, String>> {
        match self {
            ResultOutputEnum::Normal(_, metadata) => Some(metadata),
            ResultOutputEnum::Stream(_) => None,
        }
    }
}
pub trait RunnerResultHandler {
    const DEFAULT_RETRY_POLICY: RetryPolicy = RetryPolicy {
        r#type: RetryType::None as i32,
        interval: 0,
        max_interval: 0,
        max_retry: 0,
        basis: 0.0,
    };

    /// Status, output and (on failure) classification of a job execution.
    /// `runner_origin` names the runner that executed the job.
    #[inline]
    fn job_result_status(
        &self,
        runner_origin: &str,
        retry_policy: &Option<RetryPolicy>,
        job: &JobData,
        res: Result<ResultOutputEnum>,
    ) -> (ResultStatus, ResultOutputEnum, Option<StreamError>) {
        let (err, metadata) = match res {
            Ok(ResultOutputEnum::Normal(Err(err), metadata)) => (err, metadata),
            Ok(output) => return (ResultStatus::Success, output, None),
            Err(err) => (err, HashMap::new()),
        };
        let (status, message, error) = Self::handle_error(err, runner_origin);
        let st = if Self::can_retry(&status)
            && job.retried
                >= retry_policy
                    .as_ref()
                    .unwrap_or(&Self::DEFAULT_RETRY_POLICY)
                    .max_retry
        {
            ResultStatus::MaxRetry
        } else {
            status
        };
        (
            st,
            ResultOutputEnum::Normal(
                Err(JobWorkerError::RuntimeError(
                    message.unwrap_or("unknown error(no result, no error)".to_string()),
                )
                .into()),
                metadata,
            ),
            Some(error),
        )
    }

    #[inline]
    fn can_retry(st: &ResultStatus) -> bool {
        st == &ResultStatus::ErrorAndRetry
    }

    /// Retry status, diagnostic message and classification of a failure.
    /// `runner_origin` names the runner that executed the job.
    #[inline]
    fn handle_error(
        err: anyhow::Error,
        runner_origin: &str,
    ) -> (ResultStatus, Option<String>, StreamError) {
        // A failed stream End is classified by the stream error contract so
        // every runner reports cancellation and invalid input the same way.
        if let Some(proto::stream_error::StreamFailedError(failure)) =
            err.downcast_ref::<proto::stream_error::StreamFailedError>()
        {
            return (
                proto::stream_error::result_status_for(failure.code),
                Some(format!("stream error: {err:#}")),
                failure.clone(),
            );
        }
        // A classified failure gets the status, retry behavior and output of a
        // RuntimeError; the classification is the only addition.
        if let Some(classified) = err.downcast_ref::<proto::stream_error::ClassifiedJobError>() {
            return (
                ResultStatus::ErrorAndRetry,
                Some(runtime_error_message(&classified.diagnostic)),
                classified.error.clone(),
            );
        }
        let (status, message) = Self::status_and_message(&err);
        let origin = if err.downcast_ref::<DetectedByWorker>().is_some() {
            proto::stream_error::WORKER_ORIGIN
        } else {
            runner_origin
        };
        let error = proto::stream_error::classified_error(
            failure_code(&err),
            message.clone().unwrap_or_default(),
            origin,
        );
        (status, message, error)
    }

    #[inline]
    fn status_and_message(err: &anyhow::Error) -> (ResultStatus, Option<String>) {
        match err.downcast_ref::<JobWorkerError>() {
            Some(JobWorkerError::RuntimeError(mes)) => (
                ResultStatus::ErrorAndRetry,
                Some(runtime_error_message(mes)),
            ),
            Some(JobWorkerError::TimeoutError(mes)) => (
                ResultStatus::ErrorAndRetry,
                Some(format!("timeout error: {mes:?}")),
            ),
            Some(JobWorkerError::CodecError(e)) => (
                ResultStatus::OtherError,
                Some(format!("codec error: {e:?}")),
            ),
            Some(JobWorkerError::NotFound(mes)) => {
                (ResultStatus::OtherError, Some(format!("not found: {mes}")))
            }
            Some(JobWorkerError::LockError(e)) => (
                ResultStatus::ErrorAndRetry,
                Some(format!("invalid parameter: {e:?}")),
            ),
            Some(JobWorkerError::InvalidParameter(e)) => (
                ResultStatus::OtherError,
                Some(format!("invalid parameter: {e:?}")),
            ),
            Some(JobWorkerError::FailedPrecondition(e)) => (
                ResultStatus::OtherError,
                Some(format!("failed precondition: {e:?}")),
            ),
            Some(JobWorkerError::WorkerNotFound(e)) => (
                ResultStatus::OtherError,
                Some(format!("worker not found: {e:?}")),
            ),
            Some(JobWorkerError::ChanError(e)) => (
                ResultStatus::ErrorAndRetry, // ?
                Some(format!("chan error: {e:?}")),
            ),
            Some(JobWorkerError::RedisError(e)) => (
                ResultStatus::ErrorAndRetry,
                Some(format!("redis error: {e:?}")),
            ),
            Some(JobWorkerError::DBError(err)) => {
                // TODO not retryable case
                (
                    ResultStatus::ErrorAndRetry,
                    Some(format!("db error: {err:?}")),
                )
            }
            Some(JobWorkerError::GenerateIdError(mes)) => {
                // should not used (worker already created error)
                (
                    ResultStatus::OtherError,
                    Some(format!("generate id error: {mes:?}")),
                )
            }
            Some(JobWorkerError::AlreadyExists(mes)) => {
                // should not used (worker already created error)
                (
                    ResultStatus::OtherError,
                    Some(format!("conflict error: {mes:?}")),
                )
            }
            Some(JobWorkerError::TonicServerError(err)) => {
                // tonic server error should not occur
                (
                    ResultStatus::OtherError,
                    Some(format!("unexpected error: {err:?}")),
                )
            }
            Some(JobWorkerError::TonicClientError(status)) => {
                // tonic client error
                // TODO
                let st = match status.code() {
                    tonic::Code::Ok => ResultStatus::Success, // shoud not occur
                    tonic::Code::DeadlineExceeded | tonic::Code::Unavailable => {
                        ResultStatus::ErrorAndRetry
                    }
                    tonic::Code::Cancelled | tonic::Code::Aborted => ResultStatus::Abort,
                    tonic::Code::NotFound
                    | tonic::Code::ResourceExhausted
                    | tonic::Code::FailedPrecondition
                    | tonic::Code::InvalidArgument
                    | tonic::Code::PermissionDenied
                    | tonic::Code::Unauthenticated
                    | tonic::Code::AlreadyExists => ResultStatus::FatalError,
                    tonic::Code::OutOfRange
                    | tonic::Code::DataLoss
                    | tonic::Code::Unimplemented
                    | tonic::Code::Internal => ResultStatus::FatalError,
                    tonic::Code::Unknown => ResultStatus::OtherError,
                };
                (st, Some(format!("client error: {status:?}")))
            }
            Some(JobWorkerError::SerdeJsonError(e)) => {
                // parse error by serde (cannot retry)
                (
                    ResultStatus::OtherError,
                    Some(format!("parse arg json error: {e:?}")),
                )
            }
            Some(JobWorkerError::ParseError(e)) => {
                // parse error (cannot retry)
                (
                    ResultStatus::OtherError,
                    Some(format!("parse error: {e:?}")),
                )
            }
            // Some(JobWorkerError::KubeClientError(e)) => {
            //     // kube error (cannot retry)
            //     (
            //         ResultStatus::OtherError,
            //         Some(format!("kube error: {:?}", e)),
            //     )
            // }
            Some(JobWorkerError::DockerError(e)) => {
                // docker error (cannot retry?) // TODO
                (
                    ResultStatus::OtherError,
                    Some(format!("docker error: {e:?}")),
                )
            }
            Some(JobWorkerError::ReqwestError(err)) => {
                // maybe able to retry (timeout or 5xx status)
                if err.is_timeout() || err.is_status() && err.status().unwrap().is_server_error() {
                    (
                        ResultStatus::ErrorAndRetry,
                        Some(format!("request error: {err:?}")),
                    )
                } else {
                    (
                        ResultStatus::OtherError,
                        Some(format!("reqwest unknown error: {err:?}")),
                    )
                }
            }
            Some(JobWorkerError::CancelledError(mes)) => {
                (ResultStatus::Cancelled, Some(format!("cancelled: {mes:?}")))
            }
            Some(JobWorkerError::OtherError(msg)) => (
                ResultStatus::OtherError,
                Some(format!("other error: {msg:?}")),
            ),
            None => (
                ResultStatus::OtherError,
                Some(format!("unknown error: {err:?}")),
            ),
        }
    }
}

fn runtime_error_message(message: &str) -> String {
    format!("runtime error: {message:?}")
}

/// Cause of a failure as a stream error code. Retry status is decided
/// separately, so the same cause may be retryable or not.
fn failure_code(err: &anyhow::Error) -> StreamErrorCode {
    match err.downcast_ref::<JobWorkerError>() {
        Some(JobWorkerError::TimeoutError(_)) => StreamErrorCode::Timeout,
        Some(
            JobWorkerError::InvalidParameter(_)
            | JobWorkerError::SerdeJsonError(_)
            | JobWorkerError::ParseError(_),
        ) => StreamErrorCode::InvalidArgument,
        Some(JobWorkerError::CancelledError(_)) => StreamErrorCode::Cancelled,
        Some(JobWorkerError::TonicClientError(status)) => tonic_failure_code(status),
        Some(JobWorkerError::ReqwestError(error)) => reqwest_failure_code(error),
        Some(_) => StreamErrorCode::ExecutionFailed,
        // Some runners (e.g. GRPC) return the client error without wrapping it.
        None => {
            if let Some(status) = err.downcast_ref::<tonic::Status>() {
                tonic_failure_code(status)
            } else if let Some(error) = err.downcast_ref::<reqwest::Error>() {
                reqwest_failure_code(error)
            } else {
                StreamErrorCode::ExecutionFailed
            }
        }
    }
}

fn tonic_failure_code(status: &tonic::Status) -> StreamErrorCode {
    match status.code() {
        tonic::Code::DeadlineExceeded => StreamErrorCode::Timeout,
        tonic::Code::Unavailable => StreamErrorCode::TransportFailed,
        _ => StreamErrorCode::ExecutionFailed,
    }
}

fn reqwest_failure_code(error: &reqwest::Error) -> StreamErrorCode {
    if error.is_timeout() {
        StreamErrorCode::Timeout
    } else {
        StreamErrorCode::TransportFailed
    }
}

#[cfg(test)]
mod tests {
    #![allow(deprecated)]
    use super::*;
    use anyhow::Result;
    use jobworkerp_base::codec::UseProstCodec;
    use jobworkerp_runner::jobworkerp::runner::CommandArgs;
    use proto::jobworkerp::data::{
        Job, JobData, JobId, ResponseType, RetryType, WorkerData, WorkerId,
    };
    use serde::de::Error;

    // create JobRunner for test
    struct MockResultHandler {}
    impl MockResultHandler {
        fn new() -> Self {
            MockResultHandler {}
        }
    }
    impl RunnerResultHandler for MockResultHandler {}

    const ORIGIN: &str = "COMMAND";

    fn classify(err: anyhow::Error) -> StreamError {
        MockResultHandler::handle_error(err, ORIGIN).2
    }

    fn workflow_problem() -> proto::jobworkerp::data::ProblemDetails {
        proto::jobworkerp::data::ProblemDetails {
            r#type: "https://example.com/errors/custom".to_string(),
            status: 418,
            title: Some("Custom".to_string()),
            detail: Some("raised by the child".to_string()),
            instance: Some("/do/0/fail".to_string()),
        }
    }

    #[test]
    fn failures_are_classified_by_cause() {
        let deadline = || tonic::Status::new(tonic::Code::DeadlineExceeded, "slow");
        let unavailable = || tonic::Status::new(tonic::Code::Unavailable, "down");
        let cases: Vec<(&str, anyhow::Error, StreamErrorCode)> = vec![
            (
                "timeout",
                JobWorkerError::TimeoutError("t".into()).into(),
                StreamErrorCode::Timeout,
            ),
            (
                "invalid parameter",
                JobWorkerError::InvalidParameter("p".into()).into(),
                StreamErrorCode::InvalidArgument,
            ),
            (
                "serde json",
                JobWorkerError::SerdeJsonError(serde_json::from_str::<i32>("x").unwrap_err())
                    .into(),
                StreamErrorCode::InvalidArgument,
            ),
            (
                "parse",
                JobWorkerError::ParseError("p".into()).into(),
                StreamErrorCode::InvalidArgument,
            ),
            (
                "cancelled",
                JobWorkerError::CancelledError("c".into()).into(),
                StreamErrorCode::Cancelled,
            ),
            (
                "wrapped tonic deadline",
                JobWorkerError::TonicClientError(deadline()).into(),
                StreamErrorCode::Timeout,
            ),
            (
                "wrapped tonic unavailable",
                JobWorkerError::TonicClientError(unavailable()).into(),
                StreamErrorCode::TransportFailed,
            ),
            (
                "bare tonic deadline (GRPC runner)",
                anyhow::anyhow!(deadline()),
                StreamErrorCode::Timeout,
            ),
            (
                "bare tonic unavailable",
                anyhow::Error::from(unavailable()),
                StreamErrorCode::TransportFailed,
            ),
            (
                "bare tonic other code",
                anyhow::anyhow!(tonic::Status::new(tonic::Code::NotFound, "nope")),
                StreamErrorCode::ExecutionFailed,
            ),
            (
                "runtime",
                JobWorkerError::RuntimeError("r".into()).into(),
                StreamErrorCode::ExecutionFailed,
            ),
            (
                "unknown",
                anyhow::anyhow!("plain failure"),
                StreamErrorCode::ExecutionFailed,
            ),
        ];
        for (name, err, code) in cases {
            let error = classify(err);
            assert_eq!(error.code, code as i32, "{name}");
            assert_eq!(error.origin, ORIGIN, "{name}");
            assert!(!error.message.is_empty(), "{name}");
            assert_eq!(error.problem, None, "{name}");
        }
    }

    #[test]
    fn classification_keeps_the_existing_status_and_message() {
        let err: anyhow::Error = JobWorkerError::TimeoutError("timeout: 10ms".into()).into();
        let (status, message, error) = MockResultHandler::handle_error(err, ORIGIN);
        assert_eq!(status, ResultStatus::ErrorAndRetry);
        let message = message.unwrap();
        assert_eq!(message, "timeout error: \"timeout: 10ms\"");
        assert_eq!(error.message, message);
    }

    #[test]
    fn job_timeout_detected_by_the_worker_has_the_worker_origin() {
        let err = anyhow::Error::from(JobWorkerError::TimeoutError("timeout: 10ms".into()))
            .context(DetectedByWorker);
        let (status, message, error) = MockResultHandler::handle_error(err, ORIGIN);
        assert_eq!(status, ResultStatus::ErrorAndRetry);
        assert_eq!(message.unwrap(), "timeout error: \"timeout: 10ms\"");
        assert_eq!(error.code, StreamErrorCode::Timeout as i32);
        assert_eq!(error.origin, proto::stream_error::WORKER_ORIGIN);
    }

    #[test]
    fn stream_failure_keeps_the_reported_error() {
        let mut reported =
            proto::stream_error::classified_error(StreamErrorCode::TransportFailed, "cut", "HTTP");
        reported.problem = Some(workflow_problem());
        let trailer = proto::stream_error::error_trailer_from(Default::default(), reported.clone());
        let err = proto::stream_error::ensure_end_success(&trailer).unwrap_err();
        let (status, _, error) = MockResultHandler::handle_error(err, ORIGIN);
        assert_eq!(status, ResultStatus::MaxRetry);
        assert_eq!(error, reported);
    }

    #[test]
    fn classified_job_failure_is_recorded_like_a_runtime_error() {
        let diagnostic = "Failed to execute workflow: boom".to_string();
        let mut classified = proto::stream_error::classified_error(
            StreamErrorCode::Timeout,
            diagnostic.clone(),
            "WORKFLOW",
        );
        classified.problem = Some(workflow_problem());
        let err: anyhow::Error =
            proto::stream_error::ClassifiedJobError::new(classified.clone(), diagnostic.clone())
                .into();
        let (status, message, error) = MockResultHandler::handle_error(err, ORIGIN);

        let (runtime_status, runtime_message, _) = MockResultHandler::handle_error(
            JobWorkerError::RuntimeError(diagnostic).into(),
            ORIGIN,
        );
        assert_eq!(status, runtime_status);
        assert_eq!(message, runtime_message);
        assert_eq!(error, classified);
    }

    #[test]
    fn job_result_status_reports_the_classification_only_on_failure() {
        let runner = MockResultHandler::new();
        let job = JobData::default();
        let (status, _, error) = runner.job_result_status(
            ORIGIN,
            &None,
            &job,
            Ok(ResultOutputEnum::Normal(Ok(vec![1]), HashMap::new())),
        );
        assert_eq!(status, ResultStatus::Success);
        assert_eq!(error, None);

        // Retry exhaustion changes the status but keeps the classification.
        let (status, _, error) = runner.job_result_status(
            ORIGIN,
            &None,
            &job,
            Err(JobWorkerError::TimeoutError("t".into()).into()),
        );
        assert_eq!(status, ResultStatus::MaxRetry);
        let error = error.expect("failed results are classified");
        assert_eq!(error.code, StreamErrorCode::Timeout as i32);
        assert_eq!(error.origin, ORIGIN);

        let (_, _, error) = runner.job_result_status(
            ORIGIN,
            &None,
            &job,
            Ok(ResultOutputEnum::Normal(
                Err(JobWorkerError::InvalidParameter("bad".into()).into()),
                HashMap::new(),
            )),
        );
        assert_eq!(
            error.map(|e| e.code),
            Some(StreamErrorCode::InvalidArgument as i32)
        );
    }

    #[tokio::test]
    async fn reqwest_failures_are_classified_as_timeout_or_transport() {
        // A listener that never answers makes the request time out.
        let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(50))
            .build()
            .unwrap();
        let timeout = client
            .get(format!("http://{}/", silent.local_addr().unwrap()))
            .send()
            .await
            .unwrap_err();
        assert!(timeout.is_timeout());

        // A port with nothing listening refuses the connection.
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closed_addr = closed.local_addr().unwrap();
        drop(closed);
        let refused = reqwest::get(format!("http://{closed_addr}/"))
            .await
            .unwrap_err();
        assert!(refused.is_connect());

        assert_eq!(
            classify(JobWorkerError::ReqwestError(timeout).into()).code,
            StreamErrorCode::Timeout as i32
        );
        assert_eq!(
            classify(JobWorkerError::ReqwestError(refused).into()).code,
            StreamErrorCode::TransportFailed as i32
        );
    }

    #[test]
    fn stream_failures_map_to_the_contract_status() {
        for (code, status) in [
            (StreamErrorCode::Cancelled, ResultStatus::Cancelled),
            (StreamErrorCode::InvalidArgument, ResultStatus::OtherError),
            (StreamErrorCode::ExecutionFailed, ResultStatus::MaxRetry),
            (StreamErrorCode::TransportFailed, ResultStatus::MaxRetry),
        ] {
            let trailer =
                proto::stream_error::error_trailer(Default::default(), code, "failed", "COMMAND");
            let error = proto::stream_error::ensure_end_success(&trailer)
                .unwrap_err()
                .context("collect failed");
            let (actual, message, _) = MockResultHandler::handle_error(error, ORIGIN);
            assert_eq!(actual, status, "{code:?}");
            assert!(message.unwrap().contains("failed"));
        }
    }

    // create test for job_result_status(): variation test
    #[tokio::test]
    async fn test_job_result_status() -> Result<()> {
        let runner = MockResultHandler::new();
        let runner_settings = vec![];
        let worker = WorkerData {
            name: "test".to_string(),
            runner_settings: runner_settings.clone(),
            retry_policy: Some(RetryPolicy {
                r#type: RetryType::Linear as i32,
                interval: 1000,
                max_retry: 3,
                ..Default::default()
            }),
            channel: None,
            response_type: ResponseType::NoResult as i32,
            store_success: false,
            store_failure: false,
            ..Default::default()
        };
        let no_retry_worker = WorkerData {
            name: "test".to_string(),
            runner_settings,
            retry_policy: None,
            channel: None,
            response_type: ResponseType::NoResult as i32,
            store_success: false,
            store_failure: false,
            ..Default::default()
        };
        let args = jobworkerp_base::codec::ProstMessageCodec::serialize_message(&CommandArgs {
            command: "echo".to_string(),
            args: vec!["test".to_string()],
            with_memory_monitoring: false,
            treat_nonzero_as_error: false,
            success_exit_codes: vec![],
            working_dir: String::new(),
            stdin: String::new(),
        })?;
        let job = Job {
            id: Some(JobId { value: 1 }),
            data: Some(JobData {
                worker_id: Some(WorkerId { value: 1 }),
                args,
                uniq_key: Some("test".to_string()),
                retried: 0,
                priority: 0,
                timeout: 0,
                streaming_type: 0,
                enqueue_time: 0,
                run_after_time: 0,
                grabbed_until_time: None,
                using: None,
                overrides: None,
            }),
            metadata: HashMap::new(),
        };
        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Ok(ResultOutputEnum::Normal(Ok(vec![1, 2, 3]), HashMap::new())),
        );
        assert_eq!(status, ResultStatus::Success);
        assert_eq!(
            mes.result_output().unwrap(),
            ResultOutput {
                items: vec![1, 2, 3]
            }
        );
        // should not occur
        // let (status, mes, _) = runner.job_result_status(ORIGIN, &worker.retry_policy, &job.data.clone().unwrap(), Err(JobWorkerError::TonicClientError(tonic::Status::new(tonic::Code::Ok, "test")).into()));
        // assert_eq!(status, JobStatus::Success);
        // assert_eq!(mes, vec![116, 101, 115, 116]);

        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::RuntimeError("test".to_string()).into()),
        );
        assert_eq!(status, ResultStatus::ErrorAndRetry);
        assert!(mes.result().is_err());
        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::TimeoutError("test".to_string()).into()),
        );
        assert_eq!(status, ResultStatus::ErrorAndRetry);
        assert!(mes.result().is_err());
        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::LockError("test".to_string()).into()),
        );
        assert_eq!(status, ResultStatus::ErrorAndRetry);
        assert!(mes.result().is_err());
        let redis_error = redis::RedisError::from(std::io::Error::other("test"));
        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::RedisError(redis_error).into()),
        );
        assert_eq!(status, ResultStatus::ErrorAndRetry);
        assert!(mes.result().is_err());
        // let db_error = JobWorkerError::DBError(sqlx::Error::new(ErrorKind::Other, "test"));
        // let (status, mes, _) = runner.job_result_status(ORIGIN, &worker.retry_policy, &job.data.unwrap(), Err(db_error.into()));
        // assert_eq!(status, JobStatus::ErrorAndRetry);
        // assert!(!mes.is_empty());
        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::TonicClientError(tonic::Status::new(
                tonic::Code::DeadlineExceeded,
                "test",
            ))
            .into()),
        );
        assert_eq!(status, ResultStatus::ErrorAndRetry);
        assert!(mes.result().is_err());
        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::TonicClientError(tonic::Status::new(
                tonic::Code::Unavailable,
                "test",
            ))
            .into()),
        );
        assert_eq!(status, ResultStatus::ErrorAndRetry);
        assert!(mes.result().is_err());

        // max retry cases with no_retry_worker
        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &no_retry_worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::RuntimeError("test".to_string()).into()),
        );
        assert_eq!(status, ResultStatus::MaxRetry);
        assert!(mes.result().is_err());
        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &no_retry_worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::TimeoutError("test".to_string()).into()),
        );
        assert_eq!(status, ResultStatus::MaxRetry);
        assert!(mes.result().is_err());
        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &no_retry_worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::LockError("test".to_string()).into()),
        );
        assert_eq!(status, ResultStatus::MaxRetry);
        assert!(mes.result().is_err());
        let redis_error = redis::RedisError::from(std::io::Error::other("test"));
        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &no_retry_worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::RedisError(redis_error).into()),
        );
        assert_eq!(status, ResultStatus::MaxRetry);
        assert!(mes.result().is_err());
        // let db_error = JobWorkerError::DBError(sqlx::Error::new(ErrorKind::Other, "test"));
        // let (status, mes, _) = runner.job_result_status(ORIGIN, &no_retry_worker.retry_policy, &job.data.unwrap(), Err(db_error.into()));
        // assert_eq!(status, JobStatus::ErrorAndRetry);
        // assert!(!mes.is_empty());
        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &no_retry_worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::TonicClientError(tonic::Status::new(
                tonic::Code::DeadlineExceeded,
                "test",
            ))
            .into()),
        );
        assert_eq!(status, ResultStatus::MaxRetry);
        assert!(mes.result().is_err());
        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &no_retry_worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::TonicClientError(tonic::Status::new(
                tonic::Code::Unavailable,
                "test",
            ))
            .into()),
        );
        assert_eq!(status, ResultStatus::MaxRetry);
        assert!(mes.result().is_err());

        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::TonicClientError(tonic::Status::new(
                tonic::Code::Cancelled,
                "test",
            ))
            .into()),
        );
        assert_eq!(status, ResultStatus::Abort);
        assert!(mes.result().is_err());
        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(
                JobWorkerError::TonicClientError(tonic::Status::new(tonic::Code::Aborted, "test"))
                    .into(),
            ),
        );
        assert_eq!(status, ResultStatus::Abort);
        assert!(mes.result().is_err());

        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::TonicClientError(tonic::Status::new(
                tonic::Code::NotFound,
                "test",
            ))
            .into()),
        );
        assert_eq!(status, ResultStatus::FatalError);
        assert!(mes.result().is_err());
        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::TonicClientError(tonic::Status::new(
                tonic::Code::ResourceExhausted,
                "test",
            ))
            .into()),
        );
        assert_eq!(status, ResultStatus::FatalError);
        assert!(mes.result().is_err());

        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::CodecError(prost::DecodeError::new("test")).into()),
        );
        assert_eq!(status, ResultStatus::OtherError);
        assert!(mes.result().is_err());
        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::NotFound("test".to_string()).into()),
        );
        assert_eq!(status, ResultStatus::OtherError);
        assert!(mes.result().is_err());
        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::InvalidParameter("test".to_string()).into()),
        );
        assert_eq!(status, ResultStatus::OtherError);
        assert!(mes.result().is_err());
        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::WorkerNotFound("test".to_string()).into()),
        );
        assert_eq!(status, ResultStatus::OtherError);
        assert!(mes.result().is_err());
        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::GenerateIdError("test".to_string()).into()),
        );
        assert_eq!(status, ResultStatus::OtherError);
        assert!(mes.result().is_err());
        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::AlreadyExists("test".to_string()).into()),
        );
        assert_eq!(status, ResultStatus::OtherError);
        assert!(mes.result().is_err());
        // let (status, mes, _) = runner.job_result_status(ORIGIN, &worker.retry_policy, &job.data.clone().unwrap(), Err(JobWorkerError::TonicServerError(tonic::transport::Error::from()).into()));
        // assert_eq!(status, JobStatus::OtherError);
        // assert!(!mes.is_empty());
        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::SerdeJsonError(serde_json::Error::custom("test")).into()),
        );
        assert_eq!(status, ResultStatus::OtherError);
        assert!(mes.result().is_err());
        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::ParseError("test".to_string()).into()),
        );
        assert_eq!(status, ResultStatus::OtherError);
        assert!(mes.result().is_err());
        // let (status, mes, _) = runner.job_result_status(ORIGIN, &worker.retry_policy, &job.data.clone().unwrap(), Err(JobWorkerError::KubeClientError(kube::Error::RequestValidation(msg))).into());
        // assert_eq!(status, JobStatus::OtherError);
        // assert!(!mes.is_empty());
        // let (status, mes, _) = runner.job_result_status(ORIGIN, &worker.retry_policy, &job.data.clone().unwrap(), Err(JobWorkerError::KubeClientError(kube::Error::Api(msg))).into());
        // assert_eq!(status, JobStatus::OtherError);
        // assert!(!mes.is_empty());
        // let (status, mes, _) = runner.job_result_status(ORIGIN, &worker.retry_policy, &job.data.clone().unwrap(), Err(JobWorkerError::KubeClientError(kube::Error::Http(msg))).into());
        // assert_eq!(status, JobStatus::OtherError);
        // assert!(!mes.is_empty());
        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(
                JobWorkerError::DockerError(bollard::errors::Error::APIVersionParseError {}).into(),
            ),
        );
        assert_eq!(status, ResultStatus::OtherError);
        assert!(mes.result().is_err());
        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(anyhow::anyhow!("test")),
        );
        assert_eq!(status, ResultStatus::OtherError);
        assert!(mes.result().is_err());

        // CancelledError should result in Cancelled status (not retryable)
        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::CancelledError("job was cancelled".to_string()).into()),
        );
        assert_eq!(status, ResultStatus::Cancelled);
        assert!(mes.result().is_err());

        // CancelledError with no_retry_worker should also result in Cancelled (not MaxRetry)
        let (status, mes, _) = runner.job_result_status(
            ORIGIN,
            &no_retry_worker.retry_policy,
            &job.data.unwrap(),
            Err(JobWorkerError::CancelledError("job was cancelled".to_string()).into()),
        );
        assert_eq!(status, ResultStatus::Cancelled);
        assert!(mes.result().is_err());

        Ok(())
    }
    // create test for run_job() using command runner (sleep)
    // and create timeout test (using command runner)
}
