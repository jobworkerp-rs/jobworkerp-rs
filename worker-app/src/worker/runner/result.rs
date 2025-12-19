use anyhow::{anyhow, Result};
use futures::stream::BoxStream;
use jobworkerp_base::error::JobWorkerError;
use prost::Message;
use proto::jobworkerp::data::{
    JobData, ResultOutput, ResultOutputItem, ResultStatus, RetryPolicy, RetryType,
};
use std::collections::HashMap;

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

    #[inline]
    #[allow(clippy::type_complexity)]
    fn job_status(
        res: Result<ResultOutputEnum>,
    ) -> (
        ResultStatus,
        Option<ResultOutputEnum>,
        Option<String>,
        Option<HashMap<String, String>>, // in error only
    ) {
        match res {
            Ok(mes) => match mes {
                ResultOutputEnum::Normal(Ok(v), metadata) => (
                    ResultStatus::Success,
                    Some(ResultOutputEnum::Normal(Ok(v), metadata)),
                    None,
                    None,
                ),
                ResultOutputEnum::Normal(Err(err), metadata) => {
                    let (st, er) = Self::handle_error(err);
                    (st, None, er, Some(metadata))
                }
                ResultOutputEnum::Stream(_) => (ResultStatus::Success, Some(mes), None, None),
            },
            Err(err) => {
                let (st, er) = Self::handle_error(err);
                (st, None, er, None)
            }
        }
    }

    #[inline]
    fn job_stream_status(
        res: Result<BoxStream<'_, Vec<u8>>>,
    ) -> (ResultStatus, Option<BoxStream<'_, Vec<u8>>>, Option<String>) {
        match res {
            Ok(mes) => (ResultStatus::Success, Some(mes), None),
            Err(err) => {
                let (st, er) = Self::handle_error(err);
                (st, None, er)
            }
        }
    }

    #[inline]
    fn job_result_status(
        &self,
        retry_policy: &Option<RetryPolicy>,
        job: &JobData,
        res: Result<ResultOutputEnum>,
    ) -> (ResultStatus, ResultOutputEnum) {
        match Self::job_status(res) {
            (st, Some(mes), None, _) => {
                // success
                (st, mes)
            }
            (status, _, err_mes, metadata) => {
                // error
                let retry = Self::can_retry(&status);
                let st = if retry
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
                            err_mes.unwrap_or("unknown error(no result, no error)".to_string()),
                        )
                        .into()),
                        metadata.unwrap_or_default(),
                    ),
                )
            }
        }
    }

    #[inline]
    fn can_retry(st: &ResultStatus) -> bool {
        st == &ResultStatus::ErrorAndRetry
    }

    #[inline]
    fn handle_error(err: anyhow::Error) -> (ResultStatus, Option<String>) {
        match err.downcast_ref::<JobWorkerError>() {
            Some(JobWorkerError::RuntimeError(mes)) => (
                ResultStatus::ErrorAndRetry,
                Some(format!("runtime error: {mes:?}")),
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

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use infra::infra::job::rows::{JobqueueAndCodec, UseJobqueueAndCodec};
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
        let args = JobqueueAndCodec::serialize_message(&CommandArgs {
            command: "echo".to_string(),
            args: vec!["test".to_string()],
            with_memory_monitoring: false,
        });
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
            }),
            metadata: HashMap::new(),
        };
        let (status, mes) = runner.job_result_status(
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
        // let (status, mes) = runner.job_result_status(&worker.retry_policy, &job.data.clone().unwrap(), Err(JobWorkerError::TonicClientError(tonic::Status::new(tonic::Code::Ok, "test")).into()));
        // assert_eq!(status, JobStatus::Success);
        // assert_eq!(mes, vec![116, 101, 115, 116]);

        let (status, mes) = runner.job_result_status(
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::RuntimeError("test".to_string()).into()),
        );
        assert_eq!(status, ResultStatus::ErrorAndRetry);
        assert!(mes.result().is_err());
        let (status, mes) = runner.job_result_status(
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::TimeoutError("test".to_string()).into()),
        );
        assert_eq!(status, ResultStatus::ErrorAndRetry);
        assert!(mes.result().is_err());
        let (status, mes) = runner.job_result_status(
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::LockError("test".to_string()).into()),
        );
        assert_eq!(status, ResultStatus::ErrorAndRetry);
        assert!(mes.result().is_err());
        let redis_error = redis::RedisError::from(std::io::Error::other("test"));
        let (status, mes) = runner.job_result_status(
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::RedisError(redis_error).into()),
        );
        assert_eq!(status, ResultStatus::ErrorAndRetry);
        assert!(mes.result().is_err());
        // let db_error = JobWorkerError::DBError(sqlx::Error::new(ErrorKind::Other, "test"));
        // let (status, mes) = runner.job_result_status(&worker.retry_policy, &job.data.unwrap(), Err(db_error.into()));
        // assert_eq!(status, JobStatus::ErrorAndRetry);
        // assert!(!mes.is_empty());
        let (status, mes) = runner.job_result_status(
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
        let (status, mes) = runner.job_result_status(
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
        let (status, mes) = runner.job_result_status(
            &no_retry_worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::RuntimeError("test".to_string()).into()),
        );
        assert_eq!(status, ResultStatus::MaxRetry);
        assert!(mes.result().is_err());
        let (status, mes) = runner.job_result_status(
            &no_retry_worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::TimeoutError("test".to_string()).into()),
        );
        assert_eq!(status, ResultStatus::MaxRetry);
        assert!(mes.result().is_err());
        let (status, mes) = runner.job_result_status(
            &no_retry_worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::LockError("test".to_string()).into()),
        );
        assert_eq!(status, ResultStatus::MaxRetry);
        assert!(mes.result().is_err());
        let redis_error = redis::RedisError::from(std::io::Error::other("test"));
        let (status, mes) = runner.job_result_status(
            &no_retry_worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::RedisError(redis_error).into()),
        );
        assert_eq!(status, ResultStatus::MaxRetry);
        assert!(mes.result().is_err());
        // let db_error = JobWorkerError::DBError(sqlx::Error::new(ErrorKind::Other, "test"));
        // let (status, mes) = runner.job_result_status(&no_retry_worker.retry_policy, &job.data.unwrap(), Err(db_error.into()));
        // assert_eq!(status, JobStatus::ErrorAndRetry);
        // assert!(!mes.is_empty());
        let (status, mes) = runner.job_result_status(
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
        let (status, mes) = runner.job_result_status(
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

        let (status, mes) = runner.job_result_status(
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
        let (status, mes) = runner.job_result_status(
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(
                JobWorkerError::TonicClientError(tonic::Status::new(tonic::Code::Aborted, "test"))
                    .into(),
            ),
        );
        assert_eq!(status, ResultStatus::Abort);
        assert!(mes.result().is_err());

        let (status, mes) = runner.job_result_status(
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
        let (status, mes) = runner.job_result_status(
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

        let (status, mes) = runner.job_result_status(
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::CodecError(prost::DecodeError::new("test")).into()),
        );
        assert_eq!(status, ResultStatus::OtherError);
        assert!(mes.result().is_err());
        let (status, mes) = runner.job_result_status(
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::NotFound("test".to_string()).into()),
        );
        assert_eq!(status, ResultStatus::OtherError);
        assert!(mes.result().is_err());
        let (status, mes) = runner.job_result_status(
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::InvalidParameter("test".to_string()).into()),
        );
        assert_eq!(status, ResultStatus::OtherError);
        assert!(mes.result().is_err());
        let (status, mes) = runner.job_result_status(
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::WorkerNotFound("test".to_string()).into()),
        );
        assert_eq!(status, ResultStatus::OtherError);
        assert!(mes.result().is_err());
        let (status, mes) = runner.job_result_status(
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::GenerateIdError("test".to_string()).into()),
        );
        assert_eq!(status, ResultStatus::OtherError);
        assert!(mes.result().is_err());
        let (status, mes) = runner.job_result_status(
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::AlreadyExists("test".to_string()).into()),
        );
        assert_eq!(status, ResultStatus::OtherError);
        assert!(mes.result().is_err());
        // let (status, mes) = runner.job_result_status(&worker.retry_policy, &job.data.clone().unwrap(), Err(JobWorkerError::TonicServerError(tonic::transport::Error::from()).into()));
        // assert_eq!(status, JobStatus::OtherError);
        // assert!(!mes.is_empty());
        let (status, mes) = runner.job_result_status(
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::SerdeJsonError(serde_json::Error::custom("test")).into()),
        );
        assert_eq!(status, ResultStatus::OtherError);
        assert!(mes.result().is_err());
        let (status, mes) = runner.job_result_status(
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::ParseError("test".to_string()).into()),
        );
        assert_eq!(status, ResultStatus::OtherError);
        assert!(mes.result().is_err());
        // let (status, mes) = runner.job_result_status(&worker.retry_policy, &job.data.clone().unwrap(), Err(JobWorkerError::KubeClientError(kube::Error::RequestValidation(msg))).into());
        // assert_eq!(status, JobStatus::OtherError);
        // assert!(!mes.is_empty());
        // let (status, mes) = runner.job_result_status(&worker.retry_policy, &job.data.clone().unwrap(), Err(JobWorkerError::KubeClientError(kube::Error::Api(msg))).into());
        // assert_eq!(status, JobStatus::OtherError);
        // assert!(!mes.is_empty());
        // let (status, mes) = runner.job_result_status(&worker.retry_policy, &job.data.clone().unwrap(), Err(JobWorkerError::KubeClientError(kube::Error::Http(msg))).into());
        // assert_eq!(status, JobStatus::OtherError);
        // assert!(!mes.is_empty());
        let (status, mes) = runner.job_result_status(
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(
                JobWorkerError::DockerError(bollard::errors::Error::APIVersionParseError {}).into(),
            ),
        );
        assert_eq!(status, ResultStatus::OtherError);
        assert!(mes.result().is_err());
        let (status, mes) = runner.job_result_status(
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(anyhow::anyhow!("test")),
        );
        assert_eq!(status, ResultStatus::OtherError);
        assert!(mes.result().is_err());

        // CancelledError should result in Cancelled status (not retryable)
        let (status, mes) = runner.job_result_status(
            &worker.retry_policy,
            &job.data.clone().unwrap(),
            Err(JobWorkerError::CancelledError("job was cancelled".to_string()).into()),
        );
        assert_eq!(status, ResultStatus::Cancelled);
        assert!(mes.result().is_err());

        // CancelledError with no_retry_worker should also result in Cancelled (not MaxRetry)
        let (status, mes) = runner.job_result_status(
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
