pub mod chan;
pub mod rdb;
pub mod redis;

use std::collections::HashSet;

use anyhow::Result;
use proto::jobworkerp::data::{Job, JobId, JobResult, JobResultData, JobResultId, Priority};

pub trait JobQueueRepository: Sync + 'static {
    // for front (send job to worker)
    // return: jobqueue size
    fn enqueue_job(
        &self,
        channel_name: Option<&String>,
        job: &Job,
    ) -> impl std::future::Future<Output = Result<i64>> + Send
    where
        Self: Send + Sync;

    // send job result from worker to front directly
    fn enqueue_result_direct(
        &self,
        id: &JobResultId,
        res: &JobResultData,
    ) -> impl std::future::Future<Output = Result<bool>> + Send;

    // wait response from worker for direct response job
    // TODO shutdown lock until receive result ? (but not recorded...)
    fn wait_for_result_queue_for_response(
        &self,
        job_id: &JobId,
        timeout: Option<&u64>,
    ) -> impl std::future::Future<Output = Result<JobResult>> + Send;

    fn find_from_queue(
        &self,
        channel: Option<&String>,
        priority: Priority,
        id: &JobId,
    ) -> impl std::future::Future<Output = Result<Option<Job>>> + Send;

    fn find_multi_from_queue(
        &self,
        channel: Option<&String>,
        priority: Priority,
        ids: &HashSet<i64>,
    ) -> impl std::future::Future<Output = Result<Vec<Job>>> + Send;

    fn delete_from_queue(
        &self,
        channel: Option<&String>,
        priority: Priority,
        job: &Job,
    ) -> impl std::future::Future<Output = Result<i32>> + Send;

    fn count_queue(
        &self,
        channel: Option<&String>,
        priority: Priority,
    ) -> impl std::future::Future<Output = Result<i64>> + Send;
}
