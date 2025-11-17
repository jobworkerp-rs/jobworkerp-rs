use jobworkerp_base::error::JobWorkerError;
use sqlx::{error::DatabaseError, mysql::MySqlDatabaseError, sqlite::SqliteError};

// TODO map redis etc error
pub fn handle_error(err: &anyhow::Error) -> tonic::Status {
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
