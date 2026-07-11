use std::fmt::Debug;
use std::sync::Arc;

use crate::proto::jobworkerp::service::job_processing_status_service_server::JobProcessingStatusService;
use crate::proto::jobworkerp::service::{
    JobProcessingStatusResponse, OptionalJobProcessingStatusResponse,
};
use crate::service::error_handle::handle_error;
use app::app::job::JobApp;
use app::module::AppModule;
use async_stream::stream;
use command_utils::trace::Tracing;
use futures::stream::BoxStream;
use jobworkerp_base::JOB_STATUS_CONFIG;
use proto::jobworkerp::data::{Empty, JobId};
use tonic::Response;

pub trait JobProcessingStatusGrpc {
    fn app(&self) -> &Arc<dyn JobApp + 'static>;
}

#[tonic::async_trait]
impl<T: JobProcessingStatusGrpc + Tracing + Send + Debug + Sync + 'static>
    JobProcessingStatusService for T
{
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "find"))]
    async fn find(
        &self,
        request: tonic::Request<JobId>,
    ) -> Result<tonic::Response<OptionalJobProcessingStatusResponse>, tonic::Status> {
        let _s = Self::trace_request("job_status", "find", &request);
        let req = request.get_ref();
        match self.app().find_job_status(req).await {
            Ok(res) => Ok(Response::new(OptionalJobProcessingStatusResponse {
                status: res.map(|a| a as i32),
            })),
            Err(e) => Err(handle_error(&e)),
        }
    }

    type FindAllStream = BoxStream<'static, Result<JobProcessingStatusResponse, tonic::Status>>;
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "find_all"))]
    async fn find_all(
        &self,
        request: tonic::Request<Empty>,
    ) -> Result<tonic::Response<Self::FindAllStream>, tonic::Status> {
        let _s = Self::trace_request("job_status", "find_all", &request);
        match self.app().find_all_job_status().await {
            Ok(list) => Ok(Response::new(Box::pin(stream! {
                for (i, s) in list {
                    yield Ok(JobProcessingStatusResponse { id: Some(i), status: s.into() })
                }
            }))),
            Err(e) => Err(handle_error(&e)),
        }
    }

    // New method (Sprint 3) - Stub implementation
    type FindByConditionStream = BoxStream<
        'static,
        Result<crate::proto::jobworkerp::service::JobProcessingStatusDetailResponse, tonic::Status>,
    >;

    #[tracing::instrument(
        level = "info",
        skip(self, request),
        fields(method = "find_by_condition")
    )]
    async fn find_by_condition(
        &self,
        request: tonic::Request<crate::proto::jobworkerp::service::FindJobProcessingStatusRequest>,
    ) -> Result<tonic::Response<Self::FindByConditionStream>, tonic::Status> {
        let _s = Self::trace_request("job_status", "find_by_condition", &request);
        if !JOB_STATUS_CONFIG.rdb_indexing_enabled {
            return Err(tonic::Status::unimplemented(concat!(
                "Job processing status RDB indexing is disabled. ",
                "Set JOB_STATUS_RDB_INDEXING=true to enable find_by_condition."
            )));
        }
        let req = request.get_ref();

        let status = parse_optional_job_processing_status(req.status)?;

        match self
            .app()
            .find_by_condition(
                status,
                req.worker_id,
                req.channel.clone(),
                req.min_elapsed_time_ms,
                req.limit.unwrap_or(100),
                req.offset.unwrap_or(0),
                req.descending.unwrap_or(false),
            )
            .await
        {
            Ok(list) => Ok(Response::new(Box::pin(stream! {
                for detail in list {
                    let proto_response = crate::proto::jobworkerp::service::JobProcessingStatusDetailResponse {
                        id: Some(detail.job_id),
                        status: detail.status.into(),
                        worker_id: detail.worker_id,
                        channel: detail.channel,
                        priority: detail.priority,
                        enqueue_time: detail.enqueue_time,
                        start_time: detail.start_time,
                        pending_time: detail.pending_time,
                        is_streamable: Some(detail.is_streamable),
                        broadcast_results: Some(detail.broadcast_results),
                        updated_at: detail.updated_at,
                    };
                    yield Ok(proto_response)
                }
            }))),
            Err(e) => Err(handle_error(&e)),
        }
    }

    #[tracing::instrument(
        level = "info",
        skip(self, request),
        fields(method = "count_by_condition")
    )]
    async fn count_by_condition(
        &self,
        request: tonic::Request<crate::proto::jobworkerp::service::CountJobProcessingStatusRequest>,
    ) -> Result<
        tonic::Response<crate::proto::jobworkerp::service::CountJobProcessingStatusResponse>,
        tonic::Status,
    > {
        let _s = Self::trace_request("job_status", "count_by_condition", &request);
        if !JOB_STATUS_CONFIG.rdb_indexing_enabled {
            return Err(tonic::Status::unimplemented(concat!(
                "Job processing status RDB indexing is disabled. ",
                "Set JOB_STATUS_RDB_INDEXING=true to enable count_by_condition."
            )));
        }
        let req = request.get_ref();
        // Decode the wire integer via the generated proto enum (TryFrom<i32>) so the
        // mapping stays in sync with the proto definition instead of hardcoding values.
        let mode = match crate::proto::jobworkerp::service::CountJobProcessingStatusMode::try_from(
            req.mode,
        ) {
            Ok(crate::proto::jobworkerp::service::CountJobProcessingStatusMode::Total) => {
                infra::infra::job::status::rdb::JobProcessingStatusCountMode::Total
            }
            Ok(crate::proto::jobworkerp::service::CountJobProcessingStatusMode::GroupByStatus) => {
                infra::infra::job::status::rdb::JobProcessingStatusCountMode::GroupByStatus
            }
            Err(_) => {
                return Err(tonic::Status::invalid_argument(format!(
                    "Invalid CountJobProcessingStatusMode: {}",
                    req.mode
                )));
            }
        };

        let status = parse_optional_job_processing_status(req.status)?;

        match self
            .app()
            .count_by_condition(
                status,
                req.worker_id,
                req.channel.clone(),
                req.min_elapsed_time_ms,
                mode,
            )
            .await
        {
            Ok(result) => {
                let counts = result
                    .counts
                    .into_iter()
                    .map(
                        |count| crate::proto::jobworkerp::service::JobProcessingStatusCount {
                            status: count.status.into(),
                            count: count.count,
                        },
                    )
                    .collect();
                Ok(Response::new(
                    crate::proto::jobworkerp::service::CountJobProcessingStatusResponse {
                        total: result.total,
                        counts,
                        mode: req.mode,
                    },
                ))
            }
            Err(e) => Err(handle_error(&e)),
        }
    }

    /// Cleanup logically deleted job_processing_status records
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "cleanup"))]
    async fn cleanup(
        &self,
        request: tonic::Request<crate::proto::jobworkerp::service::CleanupRequest>,
    ) -> Result<tonic::Response<crate::proto::jobworkerp::service::CleanupResponse>, tonic::Status>
    {
        let _s = Self::trace_request("job_status", "cleanup", &request);

        // Authentication check (explicit call to process_metadata)
        crate::service::process_metadata(request.metadata().clone())?;

        if !JOB_STATUS_CONFIG.rdb_indexing_enabled {
            return Err(tonic::Status::failed_precondition(
                "Job processing status RDB indexing is disabled. \
                 Set JOB_STATUS_RDB_INDEXING=true to enable cleanup.",
            ));
        }

        let retention_hours_override = request.get_ref().retention_hours_override;

        // Execute cleanup via JobApp
        match self
            .app()
            .cleanup_job_processing_status(retention_hours_override)
            .await
        {
            Ok((deleted_count, cutoff_time)) => {
                let retention_hours =
                    retention_hours_override.unwrap_or(JOB_STATUS_CONFIG.retention_hours);

                let message = if deleted_count > 0 {
                    format!(
                        "Successfully deleted {} job_processing_status records older than {} hours",
                        deleted_count, retention_hours
                    )
                } else {
                    "No records to delete".to_string()
                };

                tracing::info!(
                    deleted_count,
                    retention_hours,
                    cutoff_time,
                    "JobProcessingStatusService: cleanup completed"
                );

                Ok(tonic::Response::new(
                    crate::proto::jobworkerp::service::CleanupResponse {
                        deleted_count,
                        cutoff_time,
                        message,
                    },
                ))
            }
            Err(e) => {
                tracing::error!(error = ?e, "JobProcessingStatusService: cleanup failed");
                Err(handle_error(&e))
            }
        }
    }

    /// Purge stale job_processing_status records
    #[tracing::instrument(
        level = "info",
        skip(self, request),
        fields(method = "purge_stale_jobs")
    )]
    async fn purge_stale_jobs(
        &self,
        request: tonic::Request<crate::proto::jobworkerp::service::PurgeStaleJobsRequest>,
    ) -> Result<
        tonic::Response<crate::proto::jobworkerp::service::PurgeStaleJobsResponse>,
        tonic::Status,
    > {
        let _s = Self::trace_request("job_status", "purge_stale_jobs", &request);

        // Authentication check
        crate::service::process_metadata(request.metadata().clone())?;

        if !JOB_STATUS_CONFIG.rdb_indexing_enabled {
            return Err(tonic::Status::failed_precondition(
                "Job processing status RDB indexing is disabled. \
                 Set JOB_STATUS_RDB_INDEXING=true to enable purge_stale_jobs.",
            ));
        }

        let req = request.get_ref();

        let orphaned_only = req.orphaned_only.unwrap_or(false);
        validate_purge_stale_jobs_request(req.stale_threshold_hours, orphaned_only)?;

        match self
            .app()
            .purge_stale_job_processing_status(req.stale_threshold_hours, orphaned_only)
            .await
        {
            Ok((marked_count, cutoff_time)) => {
                let mode = if orphaned_only {
                    "orphaned-only"
                } else {
                    "all-stale"
                };
                let message = if marked_count > 0 {
                    format!(
                        "Purged {} stale job_processing_status records (mode: {}, threshold: {}h)",
                        marked_count, mode, req.stale_threshold_hours
                    )
                } else {
                    format!("No stale records to purge (mode: {})", mode)
                };

                tracing::info!(
                    marked_count,
                    cutoff_time,
                    orphaned_only,
                    stale_threshold_hours = req.stale_threshold_hours,
                    "JobProcessingStatusService: purge_stale_jobs completed"
                );

                Ok(tonic::Response::new(
                    crate::proto::jobworkerp::service::PurgeStaleJobsResponse {
                        marked_count,
                        cutoff_time,
                        message,
                    },
                ))
            }
            Err(e) => {
                tracing::error!(error = ?e, "JobProcessingStatusService: purge_stale_jobs failed");
                Err(handle_error(&e))
            }
        }
    }
}

#[derive(DebugStub)]
pub(crate) struct JobProcessingStatusGrpcImpl {
    #[debug_stub = "AppModule"]
    app_module: Arc<AppModule>,
}

impl JobProcessingStatusGrpcImpl {
    pub fn new(app_module: Arc<AppModule>) -> Self {
        JobProcessingStatusGrpcImpl { app_module }
    }
}
impl JobProcessingStatusGrpc for JobProcessingStatusGrpcImpl {
    fn app(&self) -> &Arc<dyn JobApp + 'static> {
        &self.app_module.job_app
    }
}

// use tracing
impl Tracing for JobProcessingStatusGrpcImpl {}

fn parse_optional_job_processing_status(
    status: Option<i32>,
) -> Result<Option<proto::jobworkerp::data::JobProcessingStatus>, tonic::Status> {
    status
        .map(|s| {
            proto::jobworkerp::data::JobProcessingStatus::try_from(s).map_err(|_| {
                tonic::Status::invalid_argument(format!("Invalid JobProcessingStatus: {s}"))
            })
        })
        .transpose()
}

fn validate_purge_stale_jobs_request(
    stale_threshold_hours: u64,
    orphaned_only: bool,
) -> Result<(), tonic::Status> {
    if orphaned_only {
        return Ok(());
    }

    if stale_threshold_hours == 0 {
        return Err(tonic::Status::invalid_argument(
            "stale_threshold_hours must be greater than 0 when orphaned_only is false",
        ));
    }
    // Cap at 1 year: an admin purge threshold beyond this is almost certainly a
    // mistake, and it keeps the value well within the millis conversion range.
    if stale_threshold_hours > 8760 {
        return Err(tonic::Status::invalid_argument(
            "stale_threshold_hours must be at most 8760 (1 year)",
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tonic::Code;

    #[test]
    fn parse_optional_job_processing_status_accepts_unspecified_filter() {
        assert_eq!(parse_optional_job_processing_status(None).unwrap(), None);
    }

    #[test]
    fn parse_optional_job_processing_status_accepts_known_status() {
        assert_eq!(
            parse_optional_job_processing_status(Some(
                proto::jobworkerp::data::JobProcessingStatus::Pending as i32
            ))
            .unwrap(),
            Some(proto::jobworkerp::data::JobProcessingStatus::Pending)
        );
    }

    #[test]
    fn parse_optional_job_processing_status_rejects_unknown_status() {
        let err = parse_optional_job_processing_status(Some(999)).unwrap_err();

        assert_eq!(err.code(), Code::InvalidArgument);
        assert!(err.message().contains("Invalid JobProcessingStatus"));
    }

    #[test]
    fn validate_purge_stale_jobs_allows_zero_threshold_for_orphaned_only() {
        let result = validate_purge_stale_jobs_request(0, true);

        assert!(result.is_ok());
    }

    #[test]
    fn validate_purge_stale_jobs_rejects_zero_threshold_for_bulk_purge() {
        let err = validate_purge_stale_jobs_request(0, false).unwrap_err();

        assert_eq!(err.code(), Code::InvalidArgument);
        assert!(err.message().contains("orphaned_only is false"));
    }

    #[test]
    fn validate_purge_stale_jobs_allows_any_threshold_for_orphaned_only() {
        assert!(validate_purge_stale_jobs_request(1, true).is_ok());
        assert!(validate_purge_stale_jobs_request(8761, true).is_ok());
        assert!(validate_purge_stale_jobs_request(u64::MAX, true).is_ok());
    }

    #[test]
    fn validate_purge_stale_jobs_rejects_bulk_threshold_over_one_year() {
        let err = validate_purge_stale_jobs_request(8761, false).unwrap_err();

        assert_eq!(err.code(), Code::InvalidArgument);
        assert!(err.message().contains("at most 8760"));
    }
}
