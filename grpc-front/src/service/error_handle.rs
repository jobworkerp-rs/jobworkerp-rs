use app::app::job::JobEnqueueOutcomeUnknown;
use jobworkerp_base::error::JobWorkerError;
use prost::Message;
use sqlx::{error::DatabaseError, mysql::MySqlDatabaseError, sqlite::SqliteError};

// TODO map redis etc error
pub fn handle_error(err: &anyhow::Error) -> tonic::Status {
    if let Some(unknown) = err.downcast_ref::<JobEnqueueOutcomeUnknown>() {
        return map_enqueue_outcome_unknown(unknown);
    }

    // TODO search with err.chain()
    match err.downcast_ref::<JobWorkerError>() {
        Some(JobWorkerError::DBError(sqlx::Error::Database(e))) => map_db_error(e.as_ref()),
        Some(JobWorkerError::DBError(sqlx::Error::RowNotFound)) => {
            tracing::warn!("row not found occurred: {:?}", err);
            tonic::Status::not_found("Resource not found")
        }
        Some(e) => map_jobworker_error(e),
        None => {
            tracing::error!("internal error occurred: {:?}", err);
            tonic::Status::internal("Internal server error")
        }
    }
}

fn map_enqueue_outcome_unknown(unknown: &JobEnqueueOutcomeUnknown) -> tonic::Status {
    let job_id = &unknown.job_id;
    let message = format!(
        "Communication with the queue backend failed while enqueueing job ID {}. The job may already be queued. Do not retry automatically; check the job status and result first.",
        job_id.value
    );
    let mut status = tonic::Status::unknown(message);
    status.metadata_mut().insert_bin(
        super::JOB_ID_HEADER_NAME,
        tonic::metadata::MetadataValue::from_bytes(&job_id.encode_to_vec()),
    );
    status
}

// TODO cover all cases...
fn map_db_error(err: &dyn DatabaseError) -> tonic::Status {
    if let Some(e) = err.try_downcast_ref::<SqliteError>() {
        if e.code().as_deref() == Some("2067") {
            // SQLITE_CONSTRAINT_UNIQUE
            tracing::info!("unique constraint violation: {:?}", e);
            tonic::Status::already_exists("Resource already exists")
        } else {
            tracing::error!("sqlite error occurred: {:?}", e);
            tonic::Status::unavailable("Database temporarily unavailable")
        }
    } else if let Some(e) = err.try_downcast_ref::<MySqlDatabaseError>() {
        if e.number() == 1062 {
            // duplicate entry
            tracing::info!("unique constraint violation: {:?}", e);
            tonic::Status::already_exists("Resource already exists")
        } else {
            tracing::error!("mysql error occurred: {:?}", e);
            tonic::Status::unavailable("Database temporarily unavailable")
        }
    } else {
        tracing::error!("unknown db error occurred: {:?}", err);
        tonic::Status::unavailable("Database temporarily unavailable")
    }
}

fn map_jobworker_error(err: &JobWorkerError) -> tonic::Status {
    match err {
        JobWorkerError::RedisError(_) => {
            tracing::error!("redis error occurred: {:?}", err);
            tonic::Status::unavailable("Cache service temporarily unavailable")
        }
        JobWorkerError::NotFound(msg) => {
            tracing::info!("not found: {}", msg);
            tonic::Status::not_found("Resource not found")
        }
        JobWorkerError::WorkerNotFound(msg) => {
            tracing::info!("worker not found: {}", msg);
            tonic::Status::invalid_argument("Worker not found")
        }
        JobWorkerError::AlreadyExists(msg) => {
            tracing::info!("already exists: {}", msg);
            tonic::Status::already_exists("Resource already exists")
        }
        JobWorkerError::InvalidParameter(msg) => {
            tracing::debug!("invalid parameter: {}", msg);
            tonic::Status::invalid_argument(msg)
        }
        JobWorkerError::FailedPrecondition(msg) => {
            tracing::debug!("failed precondition: {}", msg);
            tonic::Status::failed_precondition(msg)
        }
        JobWorkerError::TimeoutError(msg) => {
            tracing::debug!("timeout: {}", msg);
            tonic::Status::deadline_exceeded(msg)
        }
        e => {
            tracing::error!("unhandled error: {:?}", e);
            tonic::Status::internal("Internal server error")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::handle_error;
    use app::app::job::JobEnqueueOutcomeUnknown;
    use jobworkerp_base::error::JobWorkerError;
    use prost::Message;
    use proto::jobworkerp::data::JobId;

    fn redis_error() -> JobWorkerError {
        let redis_error = std::io::Error::other("private redis details").into();
        JobWorkerError::RedisError(redis_error)
    }

    fn enqueue_outcome_unknown_error(job_id: JobId, cause: JobWorkerError) -> anyhow::Error {
        anyhow::Error::new(cause)
            .context(JobEnqueueOutcomeUnknown { job_id })
            .context("outer contextual detail")
    }

    #[test]
    fn timeout_error_maps_to_deadline_exceeded() {
        let error = anyhow::Error::from(JobWorkerError::TimeoutError("listener expired".into()));

        assert_eq!(handle_error(&error).code(), tonic::Code::DeadlineExceeded);
    }

    #[test]
    fn unknown_enqueue_outcome_returns_unknown_with_protobuf_job_id() {
        for value in [42, i64::MAX] {
            let error = enqueue_outcome_unknown_error(JobId { value }, redis_error());

            let status = handle_error(&error);

            assert_eq!(status.code(), tonic::Code::Unknown);
            let message = status.message().to_ascii_lowercase();
            assert!(message.contains(&format!(
                "communication with the queue backend failed while enqueueing job id {value}"
            )));
            assert!(message.contains("the job may already be queued"));
            assert!(message.contains("do not retry automatically"));
            assert!(message.contains("check the job status and result first"));
            assert!(!status.message().contains("private redis details"));
            assert!(!status.message().contains("outer contextual detail"));
            let job_id = status
                .metadata()
                .get_bin("x-job-id-bin")
                .expect("job ID metadata")
                .to_bytes()
                .expect("binary metadata bytes");
            assert_eq!(JobId::decode(job_id.as_ref()).unwrap(), JobId { value });
        }
    }

    #[test]
    fn unknown_enqueue_outcome_takes_priority_over_timeout_classification() {
        let error = enqueue_outcome_unknown_error(
            JobId { value: 42 },
            JobWorkerError::TimeoutError("private timeout detail".into()),
        );

        let status = handle_error(&error);

        assert_eq!(status.code(), tonic::Code::Unknown);
        assert!(status.metadata().get_bin("x-job-id-bin").is_some());
        assert!(!status.message().contains("private timeout detail"));
    }

    #[test]
    fn errors_without_unknown_enqueue_marker_keep_their_existing_classification() {
        let redis = anyhow::Error::new(redis_error());
        let general = anyhow::anyhow!("unexpected failure");

        for (error, code) in [
            (redis, tonic::Code::Unavailable),
            (general, tonic::Code::Internal),
        ] {
            let status = handle_error(&error);
            assert_eq!(status.code(), code);
            assert!(status.metadata().get_bin("x-job-id-bin").is_none());
        }
    }
}
