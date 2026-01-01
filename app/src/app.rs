// may divide to other crate in future
pub mod function;
// jobworkerp mods
pub mod job;
pub mod job_result;
pub mod runner;
pub mod worker;
pub mod worker_instance;

use std::collections::HashMap;

use async_trait::async_trait;
use command_utils::util::datetime;
use infra::infra::job::rows::UseJobqueueAndCodec;
use proto::jobworkerp::data::{
    Job, JobData, JobResultData, ResultStatus, RetryType, StorageType, WorkerData,
};
use serde::Deserialize;

#[derive(Deserialize, Clone, Debug)]
pub struct StorageConfig {
    pub r#type: StorageType,
    pub restore_at_startup: Option<bool>,
}

impl StorageConfig {
    // only valid for scalable storage
    pub fn should_restore_at_startup(&self) -> bool {
        self.r#type == StorageType::Scalable && self.restore_at_startup.unwrap_or(false)
    }
}
impl Default for StorageConfig {
    // use standalone (sqlite+memory) in default
    fn default() -> Self {
        tracing::info!("Use default StorageConfig (Standalone).");
        Self {
            r#type: StorageType::Standalone,
            restore_at_startup: Some(false),
        }
    }
}

pub trait UseStorageConfig {
    fn storage_config(&self) -> &StorageConfig;
}

/// worker setting (per worker instance)
#[derive(Deserialize, Debug, Clone)]
pub struct WorkerConfig {
    /// concurrency num for default channel (worker tokio thread num)
    pub default_concurrency: u32,
    /// worker channel name (for tokio thread)
    pub channels: Vec<String>,
    /// concurrency num for channels (worker tokio thread num)
    pub channel_concurrencies: Vec<u32>,
}
impl Default for WorkerConfig {
    fn default() -> Self {
        let cn = num_cpus::get() as u32;
        tracing::info!(
            "Use default WorkerConfig (core number concurrency: {}, default channel only).",
            cn
        );
        Self {
            default_concurrency: cn,
            channels: vec![],
            channel_concurrencies: vec![],
        }
    }
}
impl WorkerConfig {
    const DEFAULT_CHANNEL_NAME: &'static str = <infra::infra::job::redis::RedisJobRepositoryImpl as UseJobqueueAndCodec>::DEFAULT_CHANNEL_NAME;
    pub fn get_channels(&self) -> Vec<String> {
        self.channels
            .iter()
            .cloned()
            .chain(vec![WorkerConfig::DEFAULT_CHANNEL_NAME.to_string()])
            .collect()
    }
    pub fn channel_concurrency_pair(&self) -> Vec<(String, u32)> {
        let mut pairs = self
            .channels
            .iter()
            .zip(self.channel_concurrencies.clone())
            .map(|(a, b)| (a.clone(), b))
            .collect::<Vec<(String, u32)>>();
        pairs.push((
            WorkerConfig::DEFAULT_CHANNEL_NAME.to_string(),
            self.default_concurrency,
        ));
        pairs
    }
    pub fn get_concurrency(&self, channel: Option<&String>) -> Option<u32> {
        self.channel_concurrency_pair()
            .into_iter()
            .find(|(c, _)| {
                c.as_str()
                    == channel
                        .map(|s| s.as_str())
                        .unwrap_or(Self::DEFAULT_CHANNEL_NAME)
            })
            .map(|(_, n)| n)
    }
}

pub trait UseWorkerConfig {
    fn worker_config(&self) -> &WorkerConfig;
}

// build job from result status (for retry job)
#[async_trait]
pub trait JobBuilder {
    // return Some if it is necessary to retry with result and retry policy
    fn build_retry_job(
        dat: &JobResultData,
        worker: &WorkerData,
        metadata: &HashMap<String, String>,
    ) -> Option<Job> {
        // store result to db if necessary by worker setting
        if dat.status != ResultStatus::ErrorAndRetry as i32 {
            return None;
        }
        if let Some(policy) = worker.retry_policy.as_ref() {
            // not retry (perhaps above 'if dat.status' condition is enough)
            if policy.r#type == RetryType::None as i32 || dat.retried >= policy.max_retry {
                return None;
            }
            let previous_retry_count = dat.retried;
            let retry_interval = policy.interval;
            let rtype = RetryType::try_from(policy.r#type).unwrap_or(RetryType::None);
            let next_interval_msec: Option<u32> = match rtype {
                RetryType::None => None,
                RetryType::Exponential => Some(
                    (retry_interval as f32 * policy.basis.powf(previous_retry_count as f32)).round()
                        as u32,
                ),
                RetryType::Linear => Some(retry_interval * (previous_retry_count + 1)),
                RetryType::Constant => Some(retry_interval),
            };
            let run_after_time = next_interval_msec
                .map(|n| dat.end_time + n as i64)
                .unwrap_or(0);
            #[allow(deprecated)]
            Some(Job {
                id: dat.job_id,
                data: Some(JobData {
                    worker_id: dat.worker_id,
                    args: dat.args.clone(),
                    uniq_key: dat.uniq_key.clone(),
                    retried: previous_retry_count + 1, // increment retried
                    grabbed_until_time: None,
                    enqueue_time: dat.enqueue_time, // preserve first enqueue time
                    run_after_time,
                    priority: dat.priority,
                    timeout: dat.timeout,
                    streaming_type: dat.streaming_type,
                    using: dat.using.clone(), // preserve using for retry
                }),
                metadata: metadata.clone(),
            })
        } else {
            None
        }
    }

    // return Some if periodic job
    fn build_next_periodic_job(dat: &JobResultData, worker: &WorkerData) -> Option<JobData> {
        if worker.periodic_interval == 0 {
            None
        } else {
            let run_after_time = Self::_calc_next_run_after_time(
                dat.start_time,
                dat.run_after_time,
                worker.periodic_interval as i64,
            );
            Some(JobData {
                worker_id: dat.worker_id,
                args: dat.args.clone(),
                uniq_key: dat.uniq_key.clone(),
                enqueue_time: dat.enqueue_time,
                grabbed_until_time: None,
                run_after_time,
                retried: dat.retried + 1,
                priority: dat.priority,
                timeout: dat.timeout,
                streaming_type: dat.streaming_type,
                using: dat.using.clone(), // preserve using for periodic re-execution
            })
        }
    }
    fn _calc_next_run_after_time(
        start_time: i64,
        old_run_after_time: i64,
        periodic_interval: i64,
    ) -> i64 {
        // no run_after_time, set next_run_after_time basis time to start_time of previous job
        let run_after_time = if old_run_after_time == 0 {
            start_time
        } else {
            old_run_after_time
        };
        let mut next_run_after_time = run_after_time + periodic_interval;

        let now = datetime::now_millis();
        if now > next_run_after_time {
            // if next_run_after_time is past or now, set next_run_after_time basis time to now
            // (prevent from executing too many jobs continuously with old timestamp when worker is down etc)
            let new = ((now - next_run_after_time) / periodic_interval) * periodic_interval
                + next_run_after_time
                + periodic_interval;
            tracing::warn!(
                "next_run_after_time is past (old job?):{}, set next_run_after_time for future. next: {}",
                datetime::from_epoch_milli(next_run_after_time).to_rfc3339(),
                datetime::from_epoch_milli(new).to_rfc3339(),
            );
            next_run_after_time = new;
        }
        next_run_after_time
    }
}

// test
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Days, Timelike};

    // test for _calc_next_run_after_time()
    // old_run_after_time is 0:00 last year and new run_after_time is 0:00 tomorrow if periodic_interval is 1 day
    #[test]
    fn calc_next_run_after_time_test() {
        use chrono::Datelike;

        struct TestImpl;
        impl JobBuilder for TestImpl {}

        let now = datetime::now();
        let start_time = now.timestamp_millis() - 2000;
        let periodic_interval = 24 * 60 * 60 * 1000; // 1 day millis

        // old run_after_time: last year, last month, first day (current day may be not found in last month)
        let old_run_after_time = datetime::ymdhms(now.year() - 1, now.month(), 1, 0, 0, 0).unwrap();
        let run_after_time = TestImpl::_calc_next_run_after_time(
            start_time,
            old_run_after_time.timestamp_millis(),
            periodic_interval,
        );
        // expect: tomorrow 0:00 + 9:00 (preserve hour min sec)
        let tomorrow = datetime::ymdhms(
            now.year(),
            now.month(),
            now.day(),
            old_run_after_time.hour(),
            old_run_after_time.minute(),
            old_run_after_time.second(),
        )
        .unwrap()
        .checked_add_days(Days::new(1))
        .unwrap();
        assert_eq!(run_after_time, tomorrow.timestamp_millis());

        let run_after_time2 = TestImpl::_calc_next_run_after_time(start_time, 0, periodic_interval);
        // less than 1 sec
        // assert!(((start_time + periodic_interval) - run_after_time2).abs() < 1000);
        assert_eq!(start_time + periodic_interval, run_after_time2);
    }
}
