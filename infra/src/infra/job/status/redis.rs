use super::JobProcessingStatusRepository;
use anyhow::{Context, Result};
use async_trait::async_trait;
use infra_utils::infra::redis::{RedisPool, UseRedisPool};
use itertools::Itertools;
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{JobId, JobProcessingStatus};
use redis::AsyncCommands;

// manage job status (except for responseType:Direct worker)
// TODO use (listen after or create job status api)
#[async_trait]
impl JobProcessingStatusRepository for RedisJobProcessingStatusRepository {
    async fn upsert_status(&self, id: &JobId, status: &JobProcessingStatus) -> Result<bool> {
        tracing::debug!("upsert_status:{}={:?}", &id.value, status,);
        let res: Result<bool> = self
            .redis_pool()
            .get()
            .await?
            .hset(Self::STATUS_HASH_KEY, id.value, *status as i32)
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into());
        res
    }

    async fn delete_status(&self, id: &JobId) -> Result<bool> {
        tracing::debug!("delete_status:{}", &id.value);
        self.redis_pool()
            .get()
            .await?
            .hdel(Self::STATUS_HASH_KEY, id.value)
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into())
    }

    async fn find_status_all(&self) -> Result<Vec<(JobId, JobProcessingStatus)>> {
        let rv: Vec<(String, i32)> = self
            .redis_pool()
            .get()
            .await?
            .hgetall(Self::STATUS_HASH_KEY)
            .await?;
        Ok(rv
            .into_iter()
            .filter_map(|(k, v)| {
                k.parse::<i64>()
                    .context("in parse job id of status")
                    .map(|id| {
                        if v == JobProcessingStatus::Pending as i32 {
                            (JobId { value: id }, JobProcessingStatus::Pending)
                        } else if v == JobProcessingStatus::Running as i32 {
                            (JobId { value: id }, JobProcessingStatus::Running)
                        } else if v == JobProcessingStatus::WaitResult as i32 {
                            (JobId { value: id }, JobProcessingStatus::WaitResult)
                        } else if v == JobProcessingStatus::Cancelling as i32 {
                            (JobId { value: id }, JobProcessingStatus::Cancelling)
                        } else {
                            tracing::warn!(
                                "unknown status: id: {}, status :{}. returning as Unknown",
                                &id,
                                v
                            );
                            (JobId { value: id }, JobProcessingStatus::Unknown)
                        }
                    })
                    .ok()
            })
            .collect_vec())
    }
    async fn find_status(&self, id: &JobId) -> Result<Option<JobProcessingStatus>> {
        let res: Option<i32> = self
            .redis_pool()
            .get()
            .await?
            .hget(Self::STATUS_HASH_KEY, id.value)
            .await?;
        if let Some(v) = res {
            if v == JobProcessingStatus::Pending as i32 {
                Ok(Some(JobProcessingStatus::Pending))
            } else if v == JobProcessingStatus::Running as i32 {
                Ok(Some(JobProcessingStatus::Running))
            } else if v == JobProcessingStatus::WaitResult as i32 {
                Ok(Some(JobProcessingStatus::WaitResult))
            } else if v == JobProcessingStatus::Cancelling as i32 {
                Ok(Some(JobProcessingStatus::Cancelling))
            } else {
                tracing::warn!(
                    "unknown status: id: {}, status :{}. returning as Unknown",
                    &id.value,
                    v
                );
                Ok(Some(JobProcessingStatus::Unknown))
            }
        } else {
            Ok(None)
        }
    }
}

#[derive(Clone, Debug)]
pub struct RedisJobProcessingStatusRepository {
    redis_pool: &'static RedisPool,
}

impl RedisJobProcessingStatusRepository {
    const STATUS_HASH_KEY: &'static str = "JOB_STATUS";
    pub fn new(redis_pool: &'static RedisPool) -> Self {
        Self { redis_pool }
    }
}
impl UseRedisPool for RedisJobProcessingStatusRepository {
    fn redis_pool(&self) -> &'static RedisPool {
        self.redis_pool
    }
}
