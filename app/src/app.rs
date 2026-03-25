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
use proto::RetryPolicyExt;
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
        // Use resolved retry policy stored in JobResultData;
        // fall back to worker.retry_policy for legacy data without resolved_retry_policy.
        let policy = dat
            .resolved_retry_policy
            .as_ref()
            .or(worker.retry_policy.as_ref());
        if let Some(policy) = policy {
            // not retry (perhaps above 'if dat.status' condition is enough)
            if policy.r#type == RetryType::None as i32 || dat.retried >= policy.max_retry {
                return None;
            }
            let previous_retry_count = dat.retried;
            let next_interval_msec = policy.calculate_next_interval(previous_retry_count);
            let run_after_time = next_interval_msec
                .map(|n| dat.end_time + n as i64)
                .unwrap_or(0);
            // Snapshot resolved values as explicit overrides so that retries
            // behave consistently even if the worker config changes mid-chain.
            let overrides = Some(proto::jobworkerp::data::JobExecutionOverrides {
                response_type: Some(dat.response_type),
                store_success: Some(dat.store_success),
                store_failure: Some(dat.store_failure),
                broadcast_results: Some(dat.broadcast_results),
                retry_policy: dat.resolved_retry_policy,
            });
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
                    overrides,
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
            // Snapshot resolved values as explicit overrides so that periodic
            // re-executions behave consistently even if the worker config changes.
            let overrides = Some(proto::jobworkerp::data::JobExecutionOverrides {
                response_type: Some(dat.response_type),
                store_success: Some(dat.store_success),
                store_failure: Some(dat.store_failure),
                broadcast_results: Some(dat.broadcast_results),
                retry_policy: dat.resolved_retry_policy,
            });
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
                overrides,
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

    mod build_retry_job_tests {
        use super::*;
        use proto::jobworkerp::data::{JobId, Priority, RetryPolicy, RetryType, WorkerId};

        struct TestImpl;
        impl JobBuilder for TestImpl {}

        fn constant_retry_policy(max_retry: u32, interval: u32) -> RetryPolicy {
            RetryPolicy {
                r#type: RetryType::Constant as i32,
                max_retry,
                interval,
                basis: 2.0,
                max_interval: 0,
            }
        }

        fn make_worker(retry_policy: Option<RetryPolicy>) -> WorkerData {
            WorkerData {
                name: "test-worker".to_string(),
                retry_policy,
                ..Default::default()
            }
        }

        fn make_result_data(
            status: ResultStatus,
            retried: u32,
            resolved_retry_policy: Option<RetryPolicy>,
        ) -> JobResultData {
            JobResultData {
                job_id: Some(JobId { value: 1 }),
                worker_id: Some(WorkerId { value: 1 }),
                status: status as i32,
                retried,
                end_time: 1000,
                priority: Priority::Medium as i32,
                resolved_retry_policy,
                ..Default::default()
            }
        }

        #[test]
        fn test_no_retry_on_success() {
            let worker = make_worker(Some(constant_retry_policy(3, 1000)));
            // resolved_retry_policy is set but status is Success → no retry
            let dat = make_result_data(
                ResultStatus::Success,
                0,
                Some(constant_retry_policy(3, 1000)),
            );
            assert!(TestImpl::build_retry_job(&dat, &worker, &HashMap::new()).is_none());
        }

        #[test]
        fn test_retry_with_worker_policy() {
            let policy = Some(constant_retry_policy(3, 1000));
            let worker = make_worker(policy);
            let dat = make_result_data(ResultStatus::ErrorAndRetry, 0, policy);
            let job = TestImpl::build_retry_job(&dat, &worker, &HashMap::new());
            assert!(job.is_some());
            let job = job.unwrap();
            assert_eq!(job.data.as_ref().unwrap().retried, 1);
            assert_eq!(job.data.as_ref().unwrap().run_after_time, 2000); // end_time(1000) + interval(1000)
        }

        #[test]
        fn test_override_increases_max_retry() {
            // Worker: max_retry=1, already retried=1 → would stop with worker policy
            let worker = make_worker(Some(constant_retry_policy(1, 500)));
            // resolved_retry_policy: max_retry=5 → should allow retry
            let dat = make_result_data(
                ResultStatus::ErrorAndRetry,
                1,
                Some(constant_retry_policy(5, 500)),
            );
            let job = TestImpl::build_retry_job(&dat, &worker, &HashMap::new());
            assert!(
                job.is_some(),
                "resolved max_retry=5 should allow retry when retried=1"
            );
        }

        #[test]
        fn test_override_disables_retry() {
            // Worker: max_retry=10
            let worker = make_worker(Some(constant_retry_policy(10, 1000)));
            // resolved_retry_policy: RetryType::None → should stop retry
            let dat = make_result_data(
                ResultStatus::ErrorAndRetry,
                0,
                Some(RetryPolicy {
                    r#type: RetryType::None as i32,
                    max_retry: 0,
                    interval: 0,
                    basis: 0.0,
                    max_interval: 0,
                }),
            );
            let job = TestImpl::build_retry_job(&dat, &worker, &HashMap::new());
            assert!(
                job.is_none(),
                "resolved RetryType::None should disable retry"
            );
        }

        #[test]
        fn test_override_reduces_max_retry() {
            // Worker: max_retry=10, retried=1
            let worker = make_worker(Some(constant_retry_policy(10, 1000)));
            // resolved_retry_policy: max_retry=0 → should stop
            let dat = make_result_data(
                ResultStatus::ErrorAndRetry,
                1,
                Some(constant_retry_policy(0, 1000)),
            );
            let job = TestImpl::build_retry_job(&dat, &worker, &HashMap::new());
            assert!(job.is_none(), "resolved max_retry=0 should stop retry");
        }

        #[test]
        fn test_no_worker_policy_no_resolved() {
            let worker = make_worker(None);
            let dat = make_result_data(ResultStatus::ErrorAndRetry, 0, None);
            assert!(TestImpl::build_retry_job(&dat, &worker, &HashMap::new()).is_none());
        }

        #[test]
        fn test_resolved_policy_adds_retry_to_worker_without_policy() {
            // Worker has no retry policy
            let worker = make_worker(None);
            // resolved_retry_policy adds one
            let dat = make_result_data(
                ResultStatus::ErrorAndRetry,
                0,
                Some(constant_retry_policy(3, 200)),
            );
            let job = TestImpl::build_retry_job(&dat, &worker, &HashMap::new());
            assert!(
                job.is_some(),
                "resolved_retry_policy should enable retry when worker has none"
            );
            assert_eq!(job.unwrap().data.unwrap().run_after_time, 1200); // 1000 + 200
        }

        #[test]
        fn test_retry_job_carries_overrides() {
            // Verify retry job reconstructs overrides from resolved fields
            let worker = make_worker(None);
            let mut dat = make_result_data(
                ResultStatus::ErrorAndRetry,
                0,
                Some(constant_retry_policy(3, 100)),
            );
            dat.response_type = 1; // Direct
            dat.store_success = true;
            dat.store_failure = true;
            dat.broadcast_results = true;

            let job = TestImpl::build_retry_job(&dat, &worker, &HashMap::new());
            assert!(job.is_some());
            let overrides = job.unwrap().data.unwrap().overrides.unwrap();
            assert_eq!(overrides.response_type, Some(1));
            assert_eq!(overrides.store_success, Some(true));
            assert_eq!(overrides.store_failure, Some(true));
            assert_eq!(overrides.broadcast_results, Some(true));
            assert!(overrides.retry_policy.is_some());
        }

        #[test]
        fn test_exponential_max_interval_clamp() {
            let worker = make_worker(None);
            // basis=2.0, interval=1000, retried=5 → unclamped = 1000 * 2^5 = 32000
            // max_interval=10000 → clamped to 10000
            let dat = make_result_data(
                ResultStatus::ErrorAndRetry,
                5,
                Some(RetryPolicy {
                    r#type: RetryType::Exponential as i32,
                    max_retry: 10,
                    interval: 1000,
                    basis: 2.0,
                    max_interval: 10000,
                }),
            );
            let job = TestImpl::build_retry_job(&dat, &worker, &HashMap::new());
            assert!(job.is_some());
            // run_after_time = end_time(1000) + clamped_interval(10000) = 11000
            assert_eq!(job.unwrap().data.unwrap().run_after_time, 11000);
        }

        #[test]
        fn test_exponential_no_clamp_when_max_interval_zero() {
            let worker = make_worker(None);
            // basis=2.0, interval=1000, retried=3 → 1000 * 2^3 = 8000
            let dat = make_result_data(
                ResultStatus::ErrorAndRetry,
                3,
                Some(RetryPolicy {
                    r#type: RetryType::Exponential as i32,
                    max_retry: 10,
                    interval: 1000,
                    basis: 2.0,
                    max_interval: 0,
                }),
            );
            let job = TestImpl::build_retry_job(&dat, &worker, &HashMap::new());
            assert!(job.is_some());
            // run_after_time = end_time(1000) + 8000 = 9000
            assert_eq!(job.unwrap().data.unwrap().run_after_time, 9000);
        }
    }

    mod build_next_periodic_job_tests {
        use super::*;
        use proto::jobworkerp::data::{
            JobId, Priority, ResponseType, RetryPolicy, RetryType, WorkerId,
        };

        struct TestImpl;
        impl JobBuilder for TestImpl {}

        fn make_periodic_worker(periodic_interval: u32) -> WorkerData {
            WorkerData {
                name: "periodic-worker".to_string(),
                periodic_interval,
                ..Default::default()
            }
        }

        fn make_result_data_for_periodic(
            resolved_retry_policy: Option<RetryPolicy>,
        ) -> JobResultData {
            JobResultData {
                job_id: Some(JobId { value: 1 }),
                worker_id: Some(WorkerId { value: 1 }),
                status: ResultStatus::Success as i32,
                retried: 0,
                start_time: 1000,
                run_after_time: 0,
                priority: Priority::Medium as i32,
                response_type: ResponseType::Direct as i32,
                store_success: true,
                store_failure: true,
                broadcast_results: true,
                resolved_retry_policy,
                ..Default::default()
            }
        }

        #[test]
        fn test_non_periodic_worker_returns_none() {
            let worker = make_periodic_worker(0);
            let dat = make_result_data_for_periodic(None);
            assert!(TestImpl::build_next_periodic_job(&dat, &worker).is_none());
        }

        #[test]
        fn test_periodic_job_reconstructs_overrides() {
            let worker = make_periodic_worker(60000); // 1 minute
            let policy = Some(RetryPolicy {
                r#type: RetryType::Constant as i32,
                max_retry: 3,
                interval: 500,
                basis: 2.0,
                max_interval: 0,
            });
            let dat = make_result_data_for_periodic(policy);

            let pj = TestImpl::build_next_periodic_job(&dat, &worker);
            assert!(pj.is_some(), "periodic worker should produce next job");

            let pj = pj.unwrap();
            let overrides = pj
                .overrides
                .expect("periodic job must carry reconstructed overrides");
            assert_eq!(
                overrides.response_type,
                Some(ResponseType::Direct as i32),
                "response_type should be reconstructed from result"
            );
            assert_eq!(overrides.store_success, Some(true));
            assert_eq!(overrides.store_failure, Some(true));
            assert_eq!(overrides.broadcast_results, Some(true));

            let rp = overrides
                .retry_policy
                .expect("retry_policy should be carried");
            assert_eq!(rp.r#type, RetryType::Constant as i32);
            assert_eq!(rp.max_retry, 3);
            assert_eq!(rp.interval, 500);
        }

        #[test]
        fn test_periodic_job_overrides_without_retry_policy() {
            let worker = make_periodic_worker(10000);
            let dat = make_result_data_for_periodic(None);

            let pj = TestImpl::build_next_periodic_job(&dat, &worker).unwrap();
            let overrides = pj
                .overrides
                .expect("periodic job must carry overrides even without retry_policy");
            assert_eq!(overrides.response_type, Some(ResponseType::Direct as i32));
            assert_eq!(overrides.store_success, Some(true));
            assert!(
                overrides.retry_policy.is_none(),
                "retry_policy should be None when resolved was None"
            );
        }
    }
}
