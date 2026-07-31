use super::{JobProcessingStatusRecord, JobProcessingStatusRepository, StatusTransitionResult};
use anyhow::{Context, Result};
use async_trait::async_trait;
use infra_utils::infra::redis::{RedisPool, UseRedisPool};
use itertools::Itertools;
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{JobId, JobProcessingStatus};
use redis::{AsyncCommands, Script};

// manage job status (except for responseType:Direct worker)
// TODO use (listen after or create job status api)
#[async_trait]
impl JobProcessingStatusRepository for RedisJobProcessingStatusRepository {
    async fn upsert_status(&self, id: &JobId, status: &JobProcessingStatus) -> Result<bool> {
        tracing::debug!("upsert_status:{}={:?}", &id.value, status,);
        let retried = self
            .find_status_record(id)
            .await?
            .map(|record| record.retried)
            .unwrap_or(0);
        let res: Result<bool> = self
            .redis_pool()
            .get()
            .await?
            .hset(
                Self::STATUS_HASH_KEY,
                id.value,
                encode_record(JobProcessingStatusRecord {
                    status: *status,
                    retried,
                }),
            )
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
        let rv: Vec<(String, String)> = self
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
                        if let Some(record) = decode_record(&v) {
                            (JobId { value: id }, record.status)
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
        let res: Option<String> = self
            .redis_pool()
            .get()
            .await?
            .hget(Self::STATUS_HASH_KEY, id.value)
            .await?;
        if let Some(v) = res {
            if let Some(record) = decode_record(&v) {
                Ok(Some(record.status))
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

    async fn find_status_record(&self, id: &JobId) -> Result<Option<JobProcessingStatusRecord>> {
        let value: Option<String> = self
            .redis_pool()
            .get()
            .await?
            .hget(Self::STATUS_HASH_KEY, id.value)
            .await?;
        Ok(value.and_then(|value| decode_record(&value)))
    }

    async fn compare_and_set_status(
        &self,
        id: &JobId,
        expected: Option<JobProcessingStatusRecord>,
        next: Option<JobProcessingStatusRecord>,
    ) -> Result<StatusTransitionResult> {
        let expected = expected.map(encode_record).unwrap_or_default();
        let next = next.map(encode_record).unwrap_or_default();
        let mut connection = self.redis_pool().get().await?;
        let applied: i32 = Script::new(
            r#"
local current = redis.call('HGET', KEYS[1], ARGV[1])
if ARGV[2] == '' then
  if current then return 0 end
else
  if current ~= ARGV[2] then return 0 end
end
if ARGV[3] == '' then
  redis.call('HDEL', KEYS[1], ARGV[1])
else
  redis.call('HSET', KEYS[1], ARGV[1], ARGV[3])
end
return 1
"#,
        )
        .key(Self::STATUS_HASH_KEY)
        .arg(id.value)
        .arg(expected)
        .arg(next)
        .invoke_async(&mut *connection)
        .await
        .map_err(|e| anyhow::Error::from(JobWorkerError::RedisError(e)))?;
        if applied == 1 {
            Ok(StatusTransitionResult::Applied)
        } else {
            Ok(StatusTransitionResult::Conflict(
                self.find_status_record(id).await?,
            ))
        }
    }
}

fn encode_record(record: JobProcessingStatusRecord) -> String {
    format!("{}:{}", record.status as i32, record.retried)
}

fn decode_record(value: &str) -> Option<JobProcessingStatusRecord> {
    let (status, retried) = value.split_once(':')?;
    Some(JobProcessingStatusRecord {
        status: JobProcessingStatus::try_from(status.parse::<i32>().ok()?).ok()?,
        retried: retried.parse().ok()?,
    })
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
