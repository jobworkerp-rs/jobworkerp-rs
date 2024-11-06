pub mod hybrid;
pub mod rdb_chan;
pub mod redis;

use super::JobBuilder;
use anyhow::Result;
use async_trait::async_trait;
use infra::infra::{
    job::{
        queue::redis::RedisJobQueueRepository,
        redis::{schedule::RedisJobScheduleRepository, UseRedisJobRepository},
        status::UseJobStatusRepository,
    },
    UseJobQueueConfig,
};
use proto::jobworkerp::data::{
    Job, JobId, JobResult, JobResultData, JobResultId, JobStatus, ResponseType, WorkerData,
    WorkerId,
};
use std::{fmt, sync::Arc, time::Duration};

pub trait JobCacheKeys {
    // cache keys
    fn find_cache_key(id: &JobId) -> String {
        ["j:eid:", &id.value.to_string()].join("")
    }

    fn find_list_cache_key(limit: Option<&i32>, offset: &i64) -> String {
        [
            "j:list:",
            limit
                .as_ref()
                .map(|l| l.to_string())
                .unwrap_or_else(|| "none".to_string())
                .as_str(),
            ":",
            offset.to_string().as_str(),
        ]
        .join("")
    }
}
#[async_trait]
pub trait JobApp: fmt::Debug + Send + Sync {
    #[allow(clippy::too_many_arguments)]
    async fn enqueue_job(
        &self,
        worker_id: Option<&WorkerId>,
        worker_name: Option<&String>,
        arg: Vec<u8>,
        uniq_key: Option<String>,
        run_after_time: i64,
        priority: i32,
        timeout: u64,
        reserved_job_id: Option<JobId>,
    ) -> Result<(JobId, Option<JobResult>)>;

    // update job with id (redis: upsert, rdb: update)
    async fn update_job(&self, job: &Job) -> Result<()>;

    /// Complete job if the job finished
    ///
    /// # Arguments
    ///
    /// * `result` - JobResult
    /// * `worker` - WorkerData
    ///
    /// # Returns
    ///
    /// * `Result<bool>` - Result of operation (true if changed data)
    ///
    async fn complete_job(&self, id: &JobResultId, result: &JobResultData) -> Result<bool>;
    async fn delete_job(&self, id: &JobId) -> Result<bool>;
    async fn find_job(&self, id: &JobId, ttl: Option<&Duration>) -> Result<Option<Job>>
    where
        Self: Send + 'static;

    async fn find_job_list(
        &self,
        limit: Option<&i32>,
        offset: Option<&i64>,
        ttl: Option<&Duration>,
    ) -> Result<Vec<Job>>
    where
        Self: Send + 'static;

    async fn count(&self) -> Result<i64>
    where
        Self: Send + 'static;

    async fn find_job_status(&self, id: &JobId) -> Result<Option<JobStatus>>
    where
        Self: Send + 'static;

    async fn find_all_job_status(&self) -> Result<Vec<(JobId, JobStatus)>>
    where
        Self: Send + 'static;

    async fn pop_run_after_jobs_to_run(&self) -> Result<Vec<Job>>;

    async fn restore_jobs_from_rdb(&self, include_grabbed: bool, limit: Option<&i32>)
        -> Result<()>;

    async fn find_restore_jobs_from_rdb(
        &self,
        include_grabbed: bool,
        limit: Option<&i32>,
    ) -> Result<Vec<Job>>;

    // TODO check valid type of worker operation and job arg
    // fn validate_worker_and_job_arg(
    //     &self,
    //     worker: &WorkerData,
    //     job_arg: Option<&DynamicMessage>,
    // ) -> Result<()> {
    //     match worker.operation {
    //         Some(Operation::Command(_)) => match job_arg {
    //             Some(RunnerArg {
    //                 data: Some(Data::Command(_)),
    //             }) => Ok(()),
    //             _ => Err(JobWorkerError::InvalidParameter(format!(
    //                 "invalid job arg for command worker: {:?}",
    //                 &job_arg
    //             ))
    //             .into()),
    //         },
    //         Some(WorkerOperation {
    //             operation: Some(Operation::Plugin(_)),
    //         }) => match job_arg {
    //             Some(RunnerArg {
    //                 data: Some(Data::Plugin(_)),
    //             }) => Ok(()),
    //             _ => Err(JobWorkerError::InvalidParameter(format!(
    //                 "invalid job arg for plugin worker: {:?}",
    //                 job_arg
    //             ))
    //             .into()),
    //         },
    //         Some(WorkerOperation {
    //             operation: Some(Operation::GrpcUnary(_)),
    //         }) => match job_arg {
    //             Some(RunnerArg {
    //                 data: Some(Data::GrpcUnary(_)),
    //             }) => Ok(()),
    //             _ => Err(JobWorkerError::InvalidParameter(format!(
    //                 "invalid job arg for grpc unary worker: {:?}",
    //                 job_arg
    //             ))
    //             .into()),
    //         },
    //         Some(WorkerOperation {
    //             operation: Some(Operation::HttpRequest(_)),
    //         }) => match job_arg {
    //             Some(RunnerArg {
    //                 data: Some(Data::HttpRequest(_)),
    //             }) => Ok(()),
    //             _ => Err(JobWorkerError::InvalidParameter(format!(
    //                 "invalid job arg for http request worker: {:?}",
    //                 job_arg
    //             ))
    //             .into()),
    //         },
    //         Some(WorkerOperation {
    //             operation: Some(Operation::Docker(_)),
    //         }) => match job_arg {
    //             Some(RunnerArg {
    //                 data: Some(Data::Docker(_)),
    //             }) => Ok(()),
    //             _ => Err(JobWorkerError::InvalidParameter(format!(
    //                 "invalid job arg for docker worker: {:?}",
    //                 job_arg
    //             ))
    //             .into()),
    //         },
    //         Some(WorkerOperation {
    //             operation: Some(Operation::SlackInternal(_)),
    //         }) => match job_arg {
    //             Some(RunnerArg {
    //                 data: Some(Data::SlackJobResult(_)),
    //             }) => Ok(()),
    //             _ => Err(JobWorkerError::InvalidParameter(format!(
    //                 "invalid job arg for docker worker: {:?}",
    //                 job_arg
    //             ))
    //             .into()),
    //         },
    //         Some(WorkerOperation { operation: None }) => Err(JobWorkerError::WorkerNotFound(
    //             "invalid worker operation: operation=None".to_string(),
    //         )
    //         .into()),
    //         None => Err(JobWorkerError::WorkerNotFound(
    //             "invalid worker operation: None".to_string(),
    //         )
    //         .into()),
    //     }
    // }
}

pub trait UseJobApp {
    fn job_app(&self) -> &Arc<dyn JobApp + 'static>;
}

// for redis and hybrid
#[async_trait]
pub trait RedisJobAppHelper: UseRedisJobRepository + JobBuilder + UseJobQueueConfig
where
    Self: Sized + 'static,
{
    // for redis
    async fn enqueue_job_to_redis_with_wait_if_needed(
        &self,
        job: &Job,
        worker: &WorkerData,
    ) -> Result<(JobId, Option<JobResult>)> {
        let job_id = job.id.unwrap();
        // job in the future(need to wait) (use for redis only mode in future)
        let res = match if self.is_run_after_job(job) {
            // schedule
            self.redis_job_repository()
                .add_run_after_job(job.clone())
                .await
                .map(|_| 1i64) // dummy
        } else {
            // run immediately
            self.redis_job_repository()
                .enqueue_job(worker.channel.as_ref(), job)
                .await
        } {
            Ok(_) => {
                // update status (not use direct response)
                self.redis_job_repository()
                    .job_status_repository()
                    .upsert_status(&job_id, &JobStatus::Pending)
                    .await?;
                // wait for result if direct response type
                if worker.response_type == ResponseType::Direct as i32 {
                    // XXX keep redis connection until response
                    self._wait_job_for_direct_response(
                        &job_id,
                        job.data.as_ref().map(|d| d.timeout),
                    )
                    .await
                    .map(|r| (job_id, Some(r)))
                } else {
                    Ok((job_id, None))
                }
            }
            Err(e) => Err(e),
        }?;
        Ok(res)
    }

    #[inline]
    async fn _wait_job_for_direct_response(
        &self,
        job_id: &JobId,
        timeout: Option<u64>,
    ) -> Result<JobResult> {
        // wait for and return result
        self.redis_job_repository()
            .wait_for_result_queue_for_response(job_id, timeout.as_ref()) // no timeout
            .await
    }
}
