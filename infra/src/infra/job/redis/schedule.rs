use std::collections::VecDeque;

use crate::error::JobWorkerError;
use crate::infra::job::rows::UseJobqueueAndCodec;
use anyhow::Result;
use async_trait::async_trait;
use common::infra::redis::UseRedisLock;
use proto::jobworkerp::data::{Job, JobId};
use redis::AsyncCommands;

// not efficient for large number of run_after jobs, workers (using lock to keep consistency for zset and serialized_job key)
// (should I use hash key and multi command for redis cluster?)
// XXX store and find run after job without priority consideration (TODO same run after time job must be executed in order of priority)
#[async_trait]
pub trait RedisJobScheduleRepository: UseRedisLock + UseJobqueueAndCodec + Sync + 'static
where
    Self: Send + 'static,
{
    const _LOCK_KEY: &'static str = "lock_run_after_jobs";

    #[inline]
    fn _lock_expire_sec() -> i32 {
        10 // set duration enough to process range job ids from zset and remove job id from zset and serialized_job key
    }

    //lock run_after_job zset and serialized_job key to run job only once using setnx with exire by redis
    // (should edit zset and serialized_job key only from one worker at once)
    // return Err if lock is failed
    // XXX retry 5 times
    async fn _lock_run_after_jobs_with_retry(&self) -> Result<()> {
        for i in 0..5 {
            if self
                .lock(Self::_LOCK_KEY, Self::_lock_expire_sec())
                .await
                .is_ok()
            {
                return Ok(());
            }
            if i < 4 {
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            }
        }
        Err(JobWorkerError::LockError("fail to lock run_after jobs".to_string()).into())
    }

    async fn _unlock_run_after_jobs(&self) -> Result<()> {
        self.unlock(Self::_LOCK_KEY).await
    }

    // store job id as zset (using 'zadd' redis command) with epoch msec score (score is run_after_time)
    async fn add_run_after_job(&self, job: Job) -> Result<Job> {
        let job_id = job
            .id
            .as_ref()
            .ok_or(JobWorkerError::InvalidParameter(format!(
                "job id is empty?:{:?}",
                &job
            )))?;
        let run_after_time = job
            .data
            .as_ref()
            .ok_or(JobWorkerError::InvalidParameter(format!(
                "job data is empty?:{:?}",
                &job
            )))?
            .run_after_time;
        if run_after_time == 0 {
            // bug?
            tracing::warn!(
                "register not run_after job as run_after job to redis queue: {:?}",
                job
            );
        }
        self._lock_run_after_jobs_with_retry().await?;
        self.redis_pool()
            .get()
            .await?
            .zadd::<String, i64, i64, i32>(
                Self::run_after_job_zset_key(),
                job_id.value,
                run_after_time,
            )
            .await
            .map_err(JobWorkerError::RedisError)?;
        let res = self._set_serialized_job(job).await?;
        self._unlock_run_after_jobs().await?;
        Ok(res)
    }

    // store serialized job to redis without expire for cancel job (remove run_after_job_key key from redis for cancelation)
    async fn _set_serialized_job(&self, job: Job) -> Result<Job> {
        let job_id = job
            .id
            .as_ref()
            .ok_or(JobWorkerError::InvalidParameter(format!(
                "job id is empty?:{:?}",
                &job
            )))?;
        self.redis_pool()
            .get()
            .await?
            .set::<String, Vec<u8>, bool>(
                Self::run_after_job_key(job_id),
                Self::serialize_job(&job),
            )
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into())
            .map(|_| job)
    }

    // remove serialized job from redis for cancel job (with lock)
    async fn cancel_serialized_job(&self, job_id: &JobId) -> Result<i32> {
        self._lock_run_after_jobs_with_retry().await?;
        let res = self._remove_serialized_jobs(vec![job_id.value]).await?;
        self._unlock_run_after_jobs().await?;
        Ok(res)
    }

    async fn pop_run_after_jobs_to_run(&self) -> Result<Vec<Job>> {
        let now_millis = chrono::Utc::now().timestamp_millis();
        //lock
        self._lock_run_after_jobs_with_retry().await?;
        let jobs = self._find_run_after_jobs_to_run(now_millis).await?;
        let job_ids = jobs
            .iter()
            .map(|job| job.id.as_ref().unwrap().value)
            .collect::<Vec<i64>>();
        let _ = self._remove_run_after_job(now_millis).await?;
        let _ = self._remove_serialized_jobs(job_ids).await?;
        // unlock
        self._unlock_run_after_jobs().await?;
        Ok(jobs)
    }

    // use lock to run job only once
    // range job id from zset (using 'zrangebyscore' redis command) with epoch msec score (score is run_after_time)
    async fn _find_run_after_jobs_to_run(&self, now_millis: i64) -> Result<Vec<Job>> {
        let mut redis = self.redis_pool().get().await?;
        let job_ids = redis
            .zrangebyscore::<String, i64, i64, Vec<i64>>(
                Self::run_after_job_zset_key(),
                0,
                now_millis,
            )
            .await
            .map_err(JobWorkerError::RedisError)?;
        // TODO use pipeline
        let mut jobs = VecDeque::new();
        for job_id in job_ids {
            let job = redis
                .get::<String, Vec<u8>>(Self::run_after_job_key(&JobId { value: job_id }))
                .await
                .map_err(JobWorkerError::RedisError)?;
            if job.is_empty() {
                continue;
            }
            let job = Self::deserialize_job(&job)?;
            // id: unwrapped by caller
            if job.id.is_some() {
                jobs.push_front(job); // order by score(run_after_time) asc
            }
        }
        Ok(jobs.into())
    }

    // remove job id from zset (using 'zremrangebyrank' redis command) with epoch msec score (score is run_after_time)
    async fn _remove_run_after_job(&self, now_millis: i64) -> Result<i32> {
        let mut redis = self.redis_pool().get().await?;
        redis
            .zrembyscore::<String, i64, i64, i32>(Self::run_after_job_zset_key(), 0, now_millis)
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into())
    }

    // remove serialized jobs from redis (using 'del' to multiple keys)
    async fn _remove_serialized_jobs(&self, job_ids: Vec<i64>) -> Result<i32> {
        if job_ids.is_empty() {
            return Ok(0);
        }
        let mut redis = self.redis_pool().get().await?;
        let mut keys = Vec::new();
        for job_id in job_ids {
            keys.push(Self::run_after_job_key(&JobId { value: job_id }));
        }
        redis.del::<Vec<String>, i32>(keys).await.map_err(|e| {
            tracing::error!("failed to remove serialized jobs from redis: {:?}", e);
            JobWorkerError::RedisError(e).into()
        })
    }

    async fn count_all(&self) -> Result<i64> {
        let mut redis = self.redis_pool().get().await?;
        redis
            .zcard::<String, i64>(Self::run_after_job_zset_key())
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into())
    }

    // for test
    async fn delete_all(&self) -> Result<i32> {
        let mut redis = self.redis_pool().get().await?;
        redis
            .del::<String, i32>(Self::run_after_job_zset_key())
            .await
            .map_err(|e| JobWorkerError::RedisError(e).into())
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::*;
    use common::infra::redis::UseRedisPool;
    use deadpool_redis::Pool;
    use proto::jobworkerp::data::{Job, JobData, JobId};

    struct RedisJobScheduleRepositoryImpl {
        redis_pool: &'static Pool,
    }
    impl UseRedisPool for RedisJobScheduleRepositoryImpl {
        fn redis_pool(&self) -> &Pool {
            self.redis_pool
        }
    }
    impl UseJobqueueAndCodec for RedisJobScheduleRepositoryImpl {}
    impl UseRedisLock for RedisJobScheduleRepositoryImpl {}
    impl RedisJobScheduleRepository for RedisJobScheduleRepositoryImpl {}

    #[tokio::test]
    async fn test_set_and_pop_jobs() -> Result<()> {
        let pool = common::infra::test::setup_test_redis_pool().await;
        let repo = RedisJobScheduleRepositoryImpl { redis_pool: pool };
        repo.delete_all().await?;

        // test set multiple jobs with run_after_time and pop jobs to run properly (with lock)
        let mut job_ids = Vec::new();
        let now_millis = chrono::Utc::now().timestamp_millis();
        // 9 past jobs
        for i in 1..10 {
            job_ids.push(
                repo.add_run_after_job(Job {
                    id: Some(JobId { value: i }),
                    data: Some(JobData {
                        run_after_time: now_millis - i, // past
                        ..Default::default()
                    }),
                })
                .await
                .unwrap()
                .id
                .unwrap(),
            );
        }
        // 1 cancel job
        let canceled = repo
            .cancel_serialized_job(&JobId { value: 5 })
            .await
            .unwrap();
        assert_eq!(canceled, 1i32);
        // 1 future job
        job_ids.push(
            repo.add_run_after_job(Job {
                id: Some(JobId { value: 100 }),
                data: Some(JobData {
                    run_after_time: now_millis + 1000, // future
                    ..Default::default()
                }),
            })
            .await
            .unwrap()
            .id
            .unwrap(),
        );
        // not remove zset key in canceling
        let cnt = repo.count_all().await?;
        assert_eq!(cnt, 10i64);

        // execute pop_run_after_jobs_to_run()
        let jobs = repo.pop_run_after_jobs_to_run().await.unwrap();

        // past job acquired (except canceled and future job)
        assert_eq!(jobs.len(), 8);
        // remain future job
        let cnt = repo.count_all().await?;
        assert_eq!(cnt, 1i64);

        for i in 1..10 {
            match i.cmp(&5) {
                Ordering::Less => {
                    assert_eq!(&jobs[i - 1].id.as_ref().unwrap().value, &(i as i64));
                }
                Ordering::Equal => continue,
                Ordering::Greater => {
                    assert_eq!(&jobs[i - 2].id.as_ref().unwrap().value, &(i as i64));
                }
            }
        }
        Ok(())
    }
}
