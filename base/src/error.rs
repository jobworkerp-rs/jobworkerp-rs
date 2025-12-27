use prost::DecodeError;
use redis::RedisError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum JobWorkerError {
    #[error("InvalidParameter({0})")]
    InvalidParameter(String),
    #[error("ParseError({0})")]
    ParseError(String),
    #[error("CodecError({0:?})")]
    CodecError(DecodeError),
    #[error("WorkerNotFound({0})")]
    WorkerNotFound(String),
    #[error("NotFound({0})")]
    NotFound(String),
    #[error("AlreadyExists({0})")]
    AlreadyExists(String),
    #[error("LockError({0:?})")]
    LockError(String),
    #[error("TimeoutError({0})")]
    TimeoutError(String),
    #[error("GenerateIdError({0})")]
    GenerateIdError(String),
    #[error("ChanError({0:?})")]
    ChanError(anyhow::Error),
    #[error("serde_json error({0:?})")]
    SerdeJsonError(serde_json::error::Error),
    #[error("RedisError({0:?})")]
    RedisError(RedisError),
    #[error("DBError({0:?})")]
    DBError(sqlx::Error),
    #[error("docker error({0:?})")]
    DockerError(bollard::errors::Error),
    #[error("TonicServerError({0:?})")]
    TonicServerError(tonic::transport::Error),
    #[error("TonicClientError({0:?})")]
    TonicClientError(tonic::Status),
    #[error("ReqwestError({0:?})")]
    ReqwestError(reqwest::Error),
    // #[error("kube error({0:?})")]
    // KubeClientError(kube_client::error::Error),
    #[error("RuntimeError({0})")]
    RuntimeError(String),
    #[error("CancelledError({0})")]
    CancelledError(String),
    #[error("OtherError({0})")]
    OtherError(String),
}

impl JobWorkerError {
    /// Returns true if the job status should be deleted when this error occurs.
    /// Status should be deleted for permanent failures (job cannot be executed).
    /// Status should NOT be deleted for:
    /// - AlreadyExists (another process may be executing the same job)
    /// - All other errors (for error tracking purposes)
    pub fn should_delete_job_status(&self) -> bool {
        matches!(
            self,
            JobWorkerError::InvalidParameter(_)
                | JobWorkerError::NotFound(_)
                | JobWorkerError::WorkerNotFound(_)
                | JobWorkerError::OtherError(_)
                | JobWorkerError::CodecError(_)
                | JobWorkerError::ParseError(_)
        )
    }
}

impl From<tonic::transport::Error> for JobWorkerError {
    fn from(e: tonic::transport::Error) -> Self {
        JobWorkerError::TonicServerError(e)
    }
}
impl From<RedisError> for JobWorkerError {
    fn from(e: RedisError) -> Self {
        JobWorkerError::RedisError(e)
    }
}
impl From<serde_json::Error> for JobWorkerError {
    fn from(e: serde_json::Error) -> Self {
        JobWorkerError::SerdeJsonError(e)
    }
}
// impl From<kube_client::error::Error> for JobWorkerError {
//     fn from(e: kube_client::error::Error) -> Self {
//         JobWorkerError::KubeClientError(e)
//     }
// }
impl From<bollard::errors::Error> for JobWorkerError {
    fn from(e: bollard::errors::Error) -> Self {
        JobWorkerError::DockerError(e)
    }
}
