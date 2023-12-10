use infra::error::JobWorkerError;
use sqlx::{error::DatabaseError, mysql::MySqlDatabaseError, sqlite::SqliteError};

// TODO map redis etc error
pub fn handle_error(err: &anyhow::Error) -> tonic::Status {
    // TODO search with err.chain()
    match err.downcast_ref::<JobWorkerError>() {
        Some(JobWorkerError::DBError(sqlx::Error::Database(e))) => map_db_error(e.as_ref()),
        Some(JobWorkerError::DBError(sqlx::Error::RowNotFound)) => {
            tracing::warn!("row not found occurred: {:?}", err);
            tonic::Status::not_found(format!("not found: {:?}", err))
        }
        Some(e) => map_jobworker_error(e),
        None => {
            tracing::warn!("other error occurred: {:?}", err);
            tonic::Status::internal(format!("other error: {:?}", err))
        }
    }
}

// TODO cover all cases...
fn map_db_error(err: &dyn DatabaseError) -> tonic::Status {
    if let Some(e) = err.try_downcast_ref::<SqliteError>() {
        if e.code().as_deref() == Some("2067") {
            // SQLITE_CONSTRAINT_UNIQUE
            tonic::Status::already_exists(format!("{:?}", e))
        } else {
            tracing::warn!("sqlite error occurred: {:?}", e);
            tonic::Status::unavailable(format!("db error: {:?}", e))
        }
    } else if let Some(e) = err.try_downcast_ref::<MySqlDatabaseError>() {
        if e.number() == 1062 {
            // duplicate entry
            tonic::Status::already_exists(format!("{:?}", e))
        } else {
            tracing::warn!("mysql error occurred: {:?}", e);
            tonic::Status::unavailable(format!("db error: {:?}", e))
        }
    } else {
        tracing::warn!("unknown db error occurred: {:?}", err);
        tonic::Status::unavailable(format!("db error: {:?}", err))
    }
}

fn map_jobworker_error(err: &JobWorkerError) -> tonic::Status {
    match err {
        //TODO implement properly
        JobWorkerError::RedisError(e) => {
            tracing::warn!("redis error occurred: {:?}", err);
            tonic::Status::unavailable(format!("redis error: {:?}", e))
        }
        JobWorkerError::NotFound(_) => {
            tracing::info!("not found: {:?}", err);
            tonic::Status::not_found(format!("{:?}", err))
        }
        JobWorkerError::WorkerNotFound(_) => {
            tracing::info!("worker not found: {:?}", err);
            tonic::Status::invalid_argument(format!("{:?}", err))
        }
        JobWorkerError::AlreadyExists(_) => {
            tracing::info!("already exists: {:?}", err);
            tonic::Status::already_exists(format!("{:?}", err))
        }
        JobWorkerError::InvalidParameter(_) => {
            tracing::debug!("invalid parameter: {:?}", err);
            tonic::Status::invalid_argument(format!("{:?}", err))
        }
        JobWorkerError::TimeoutError(_) => {
            tracing::debug!("timeout: {:?}", err);
            tonic::Status::deadline_exceeded(format!("{:?}", err))
        }
        // TODO implement properly
        e => {
            tracing::warn!("unknown error occurred: {:?}", e);
            tonic::Status::internal(format!("unknown: {:?}", e))
        }
    }
}
