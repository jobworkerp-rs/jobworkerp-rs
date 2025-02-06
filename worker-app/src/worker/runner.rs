pub mod map;
pub mod pool;
pub mod result;

use self::map::UseRunnerPoolMap;
use self::result::RunnerResultHandler;
use anyhow::anyhow;
use anyhow::Result;
use async_trait::async_trait;
use command_utils::util::{datetime, result::Flatten};
use futures::{future::FutureExt, stream::BoxStream};
use infra::infra::{job::rows::UseJobqueueAndCodec, runner::factory::UseRunnerFactory};
use infra::{error::JobWorkerError, infra::runner::RunnerTrait};
use proto::jobworkerp::data::ResultOutputItem;
use proto::jobworkerp::data::{
    Job, JobResultData, ResultOutput, ResultStatus, RunnerData, WorkerData, WorkerId,
};
use result::ResultOutputEnum;
use std::{panic::AssertUnwindSafe, time::Duration};
use tracing;

// execute runner
#[async_trait]
pub trait JobRunner:
    RunnerResultHandler + UseJobqueueAndCodec + UseRunnerFactory + UseRunnerPoolMap + Send + Sync
{
    #[allow(unstable_name_collisions)] // for flatten()
    //#[tracing::instrument(name = "JobRunner", skip(self))]
    #[inline]
    async fn run_job(
        &'static self,
        runner_data: &RunnerData,
        worker_id: &WorkerId,
        worker_data: &WorkerData,
        job: Job,
    ) -> (JobResultData, Option<BoxStream<'static, ResultOutputItem>>) {
        tracing::debug!("run_job: {:?}", job);
        // XXX for keeping pool object
        if worker_data.use_static {
            let to = if let Some(data) = job.data.as_ref() {
                if data.timeout == 0 {
                    None
                } else {
                    Some(Duration::from_millis(data.timeout))
                }
            } else {
                None
            };
            let p = self
                .runner_pool_map()
                .get_or_create_static_runner(runner_data, worker_id, worker_data, to)
                .await;
            match p {
                Ok(Some(runner)) => {
                    let mut r = runner.lock().await;
                    tracing::debug!("static runner found: {:?}", r.name());
                    self.run_job_inner(worker_data, job, &mut r).await
                }
                Ok(None) => (self.handle_error_option(worker_data, job, None), None),
                Err(e) => (self.handle_error_option(worker_data, job, Some(e)), None),
            }
        } else {
            let rres = self
                .runner_pool_map()
                .get_non_static_runner(runner_data, worker_data)
                .await;
            match rres {
                Ok(mut runner) => self.run_job_inner(worker_data, job, &mut runner).await,
                Err(e) => (self.handle_error_option(worker_data, job, Some(e)), None),
            }
        }
    }
    fn handle_error_option(
        &self,
        worker_data: &WorkerData,
        job: Job,
        err: Option<anyhow::Error>,
    ) -> JobResultData {
        let end = datetime::now_millis();
        let error_message = if let Some(e) = err {
            let mes = format!(
                "error in loading runner for worker:{:?}, error:{:?}",
                worker_data.name, e
            );
            tracing::warn!(mes);
            mes
        } else {
            tracing::info!("runner not found for static worker:{:?}", worker_data.name);
            format!("runner not found for static worker:{:?}", worker_data.name)
        };
        Self::job_result_data(
            job,
            worker_data,
            ResultStatus::FatalError,
            ResultOutputEnum::Normal(vec![error_message.into_bytes()]).result_output(),
            end,
            end,
        )
    }

    #[allow(unstable_name_collisions)] // for flatten()
    async fn run_job_inner(
        &'static self,
        worker_data: &WorkerData,
        job: Job,
        runner_impl: &mut Box<dyn RunnerTrait + Send + Sync>,
    ) -> (JobResultData, Option<BoxStream<'static, ResultOutputItem>>) {
        let data = job.data.as_ref().unwrap(); // XXX unwrap
        let run_after_time = data.run_after_time;

        let wait = run_after_time - datetime::now_millis();
        // sleep short time if run_after_time is future (should less than STORAGE_FETCH_INTERVAL)
        if wait > 0 {
            // 1min
            if wait > 60000 {
                // logic error!
                tracing::error!("wait too long time: {}ms, job: {:?}", wait, &job);
            }
            tokio::time::sleep(Duration::from_millis(wait as u64)).await;
        }
        // job time: complete job time include setup runner time.
        let start = datetime::now_millis();

        let name = runner_impl.name();
        if runner_impl.output_as_stream() {
            tracing::debug!("start runner(stream): {}", &name);
            let res = self
                .run_and_stream(&job, runner_impl)
                .await
                .map(ResultOutputEnum::Stream);
            let end = datetime::now_millis();
            tracing::debug!(
                "end runner(stream): {}, duration:{}(ms)",
                &name,
                end - start,
            );
            let (status, mes) = self.job_result_status(&worker_data.retry_policy, data, res);
            (
                Self::job_result_data(job, worker_data, status, mes.result_output(), start, end),
                mes.stream(),
            )
        } else {
            tracing::debug!("start runner: {}", &name);
            let res = self
                .run_and_result(&job, runner_impl)
                .await
                .map(ResultOutputEnum::Normal);
            let end = datetime::now_millis();
            tracing::debug!("end runner: {}, duration:{}(ms)", &name, end - start);
            // TODO
            let (status, mes) = self.job_result_status(&worker_data.retry_policy, data, res);
            (
                Self::job_result_data(job, worker_data, status, mes.result_output(), start, end),
                None,
            )
        }
    }

    #[allow(unstable_name_collisions)] // for flatten()
    async fn run_and_stream<'a>(
        &'static self,
        job: &Job,
        runner_impl: &mut Box<dyn RunnerTrait + Send + Sync>,
    ) -> Result<BoxStream<'a, ResultOutputItem>> {
        let data = job.data.as_ref().unwrap(); // XXX unwrap
        let args = &data.args; // XXX unwrap, clone
        let name = runner_impl.name();
        if data.timeout > 0 {
            tokio::select! {
                r = AssertUnwindSafe(
                        runner_impl.run_stream(args)
                ).catch_unwind() => {
                    r.map_err(|e| {
                        let msg = format!("Caught panic from runner {}: {:?}", &name, e);
                        tracing::error!(msg);
                        anyhow!(msg)
                    })
                    .inspect_err(|e| tracing::warn!("error in running runner: {} : {:?}", &name, e))
                    .flatten()
                },
                _ = tokio::time::sleep(Duration::from_millis(data.timeout)) => {
                    runner_impl.cancel().await;
                    tracing::warn!("timeout: {}ms, the job will be dropped: {:?}", data.timeout, &job);
                    Err(JobWorkerError::TimeoutError(format!("timeout: {}ms", data.timeout)).into())
                }
            }
        } else {
            AssertUnwindSafe(runner_impl.run_stream(args))
                .catch_unwind()
                .await
                .map_err(|e| {
                    let msg = format!("Caught panic from runner {}: {:?}", &name, e);
                    tracing::error!(msg);
                    anyhow!(msg)
                })
                .inspect_err(|e| tracing::warn!("error in running runner: {} : {:?}", &name, e))
                .flatten()
        }
    }
    #[allow(unstable_name_collisions)] // for flatten()
    async fn run_and_result(
        &self,
        job: &Job,
        runner_impl: &mut Box<dyn RunnerTrait + Send + Sync>,
    ) -> Result<Vec<Vec<u8>>> {
        let data = job.data.as_ref().unwrap(); // XXX unwrap
        let args = &data.args; // XXX unwrap, clone
        let name = runner_impl.name();
        if data.timeout > 0 {
            tokio::select! {
                r = AssertUnwindSafe(
                        runner_impl.run(args)
                ).catch_unwind() => {
                    r.map_err(|e| {
                        let msg = format!("Caught panic from runner {}: {:?}", &name, e);
                        tracing::error!(msg);
                        anyhow!(msg)
                    })
                    .inspect_err(|e| tracing::warn!("error in running runner: {} : {:?}", &name, e))
                    .flatten()
                },
                _ = tokio::time::sleep(Duration::from_millis(data.timeout)) => {
                    runner_impl.cancel().await;
                    tracing::warn!("timeout: {}ms, the job will be dropped: {:?}", data.timeout, &job);
                    Err(JobWorkerError::TimeoutError(format!("timeout: {}ms", data.timeout)).into())
                }
            }
        } else {
            AssertUnwindSafe(runner_impl.run(args))
                .catch_unwind()
                .await
                .map_err(|e| {
                    let msg = format!("Caught panic from runner {}: {:?}", &name, e);
                    tracing::error!(msg);
                    anyhow!(msg)
                })
                .inspect_err(|e| tracing::warn!("error in running runner: {} : {:?}", &name, e))
                .flatten()
        }
    }
    // calculate job status and create JobResult (not increment retry count now)
    #[inline]
    fn job_result_data(
        job: Job,
        worker: &WorkerData,
        st: ResultStatus,
        res: Option<ResultOutput>,
        start_msec: i64,
        end_msec: i64,
    ) -> JobResultData {
        let dat = job.data.unwrap(); // TODO unwrap
        JobResultData {
            job_id: job.id,
            worker_id: dat.worker_id,
            worker_name: worker.name.clone(),
            args: dat.args,
            uniq_key: dat.uniq_key,
            status: st as i32,
            output: res, // should be None if empty ?
            retried: dat.retried,
            max_retry: worker
                .retry_policy
                .as_ref()
                .unwrap_or(&Self::DEFAULT_RETRY_POLICY)
                .max_retry,
            priority: dat.priority,
            timeout: dat.timeout,
            enqueue_time: dat.enqueue_time,
            run_after_time: dat.run_after_time,
            start_time: start_msec,
            end_time: end_msec,
            response_type: worker.response_type,
            store_success: worker.store_success,
            store_failure: worker.store_failure,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{map::RunnerFactoryWithPoolMap, *};
    use anyhow::Result;
    use app::app::WorkerConfig;
    use infra::infra::{
        job::rows::JobqueueAndCodec,
        runner::factory::{RunnerFactory, UseRunnerFactory},
    };
    use proto::jobworkerp::data::{
        Job, JobData, JobId, ResponseType, RunnerType, WorkerData, WorkerId,
    };
    use std::sync::Arc;

    // create JobRunner for test
    struct MockJobRunner {
        runner_factory: Arc<RunnerFactory>,
        runner_pool: RunnerFactoryWithPoolMap,
    }
    impl MockJobRunner {
        fn new() -> Self {
            MockJobRunner {
                runner_factory: Arc::new(RunnerFactory::new()),
                runner_pool: RunnerFactoryWithPoolMap::new(
                    Arc::new(RunnerFactory::new()),
                    Arc::new(WorkerConfig::default()),
                ),
            }
        }
    }
    impl UseJobqueueAndCodec for MockJobRunner {}
    impl UseRunnerFactory for MockJobRunner {
        fn runner_factory(&self) -> &RunnerFactory {
            &self.runner_factory
        }
    }
    impl RunnerResultHandler for MockJobRunner {}
    impl UseRunnerPoolMap for MockJobRunner {
        fn runner_pool_map(&self) -> &RunnerFactoryWithPoolMap {
            &self.runner_pool
        }
    }
    impl JobRunner for MockJobRunner {}

    // create test for run_job() using command runner (sleep)
    // and create timeout test (using command runner)

    #[tokio::test]
    async fn test_run_job() -> Result<()> {
        use once_cell::sync::Lazy;

        static JOB_RUNNER: Lazy<Box<MockJobRunner>> = Lazy::new(|| Box::new(MockJobRunner::new()));

        let run_after = datetime::now_millis() + 1000;
        let jargs = JobqueueAndCodec::serialize_message(&infra::jobworkerp::runner::CommandArgs {
            args: vec!["1".to_string()],
        });
        let job = Job {
            id: Some(JobId { value: 1 }),
            data: Some(JobData {
                worker_id: Some(WorkerId { value: 1 }),
                args: jargs,
                uniq_key: Some("test".to_string()),
                retried: 0,
                priority: 0,
                timeout: 0,
                enqueue_time: 0,
                run_after_time: run_after,
                grabbed_until_time: None,
            }),
        };
        let worker_id = WorkerId { value: 1 };
        let runner_settings = JobqueueAndCodec::serialize_message(
            &infra::jobworkerp::runner::CommandRunnerSettings {
                name: "sleep".to_string(),
            },
        );

        let worker = WorkerData {
            name: "test".to_string(),
            runner_settings,
            retry_policy: None,
            channel: Some("test".to_string()),
            response_type: ResponseType::NoResult as i32,
            store_success: false,
            store_failure: false,
            ..Default::default()
        };
        let runner_data = RunnerData {
            name: RunnerType::Command.as_str_name().to_string(),
            ..Default::default()
        };
        let (res, _) = JOB_RUNNER
            .run_job(&runner_data, &worker_id, &worker, job.clone())
            .await;
        assert_eq!(res.status, ResultStatus::Success as i32);
        assert_eq!(res.output.unwrap().items, vec![b""]);
        assert_eq!(res.retried, 0);
        assert_eq!(res.max_retry, 0);
        assert_eq!(res.priority, 0);
        assert_eq!(res.timeout, 0);
        assert_eq!(res.enqueue_time, 0);
        assert_eq!(res.run_after_time, run_after);
        assert!(res.start_time >= run_after); // wait until run_after (expect short time)
        assert!(res.end_time > res.start_time);
        assert!(res.end_time - res.start_time >= 1000); // sleep
        let jargs = JobqueueAndCodec::serialize_message(&infra::jobworkerp::runner::CommandArgs {
            args: vec!["2".to_string()],
        });

        let timeout_job = Job {
            id: Some(JobId { value: 2 }),
            data: Some(JobData {
                args: jargs,   // sleep 2sec
                timeout: 1000, // timeout 1sec
                ..job.data.as_ref().unwrap().clone()
            }),
        };
        let (res, _) = JOB_RUNNER
            .run_job(&runner_data, &worker_id, &worker, timeout_job.clone())
            .await;
        assert_eq!(res.status, ResultStatus::MaxRetry as i32); // no retry
        assert_eq!(
            res.output.unwrap().items,
            vec![b"timeout error: \"timeout: 1000ms\""]
        ); // timeout error
        assert_eq!(res.retried, 0);
        assert_eq!(res.max_retry, 0);
        assert_eq!(res.priority, 0);
        assert_eq!(res.timeout, 1000);
        assert_eq!(res.enqueue_time, 0);
        assert_eq!(res.run_after_time, run_after);
        assert!(res.start_time > run_after);
        assert!(res.end_time > 0);
        assert!(res.end_time - res.start_time >= 1000); // wait timeout 1sec
        assert!(res.end_time - res.start_time < 2000); // timeout 1sec

        Ok(())
    }
}
