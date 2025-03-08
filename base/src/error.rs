use prost::DecodeError;
use redis::RedisError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum JobWorkerError {
    #[error("TonicServerError({0:?})")]
    TonicServerError(tonic::transport::Error),
    #[error("TonicClientError({0:?})")]
    TonicClientError(tonic::Status),
    #[error("ReqwestError({0:?})")]
    ReqwestError(reqwest::Error),
    #[error("TimeoutError({0})")]
    TimeoutError(String),
    #[error("RuntimeError({0})")]
    RuntimeError(String),
    #[error("InvalidParameter({0})")]
    InvalidParameter(String),
    #[error("CodecError({0:?})")]
    CodecError(DecodeError),
    #[error("WorkerNotFound({0})")]
    WorkerNotFound(String),
    #[error("NotFound({0})")]
    NotFound(String),
    #[error("AlreadyExists({0})")]
    AlreadyExists(String),
    #[error("ChanError({0:?})")]
    ChanError(anyhow::Error),
    #[error("RedisError({0:?})")]
    RedisError(RedisError),
    #[error("DBError({0:?})")]
    DBError(sqlx::Error),
    #[error("LockError({0:?})")]
    LockError(String),
    #[error("GenerateIdError({0})")]
    GenerateIdError(String),
    #[error("ParseError({0})")]
    ParseError(String),
    #[error("serde_json error({0:?})")]
    SerdeJsonError(serde_json::error::Error),
    #[error("docker error({0:?})")]
    DockerError(bollard::errors::Error),
    // #[error("kube error({0:?})")]
    // KubeClientError(kube_client::error::Error),
    #[error("OtherError({0})")]
    OtherError(String),
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
