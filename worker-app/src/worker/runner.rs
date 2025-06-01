pub mod map;
pub mod pool;
pub mod result;

use self::map::UseRunnerPoolMap;
use self::result::RunnerResultHandler;
use anyhow::anyhow;
use anyhow::Result;
use app_wrapper::runner::UseRunnerFactory;
use async_trait::async_trait;
use command_utils::util::{datetime, result::Flatten};
use futures::{future::FutureExt, stream::BoxStream};
use infra::infra::job::rows::UseJobqueueAndCodec;
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::runner::RunnerTrait;
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
        tracing::debug!("run_job: {:?}, worker: {:?}", &job.id, &worker_id);
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
                Ok(mut runner) => {
                    tracing::debug!("non-static runner found: {:?}", runner.name());
                    self.run_job_inner(worker_data, job, &mut runner).await
                }
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
        if data.request_streaming {
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
        let dat = job.data.unwrap_or_default(); // XXX unwrap or default
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
            request_streaming: dat.request_streaming,
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
    use app_wrapper::runner::RunnerFactory;
    use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
    use jobworkerp_runner::jobworkerp::runner::{CommandArgs, CommandResult};
    use proto::jobworkerp::data::{
        Job, JobData, JobId, ResponseType, RunnerType, WorkerData, WorkerId,
    };
    use std::sync::Arc;
    use tokio::sync::OnceCell;

    // create JobRunner for test
    struct MockJobRunner {
        runner_factory: Arc<RunnerFactory>,
        runner_pool: RunnerFactoryWithPoolMap,
    }
    impl MockJobRunner {
        async fn new() -> Self {
            let app_module = Arc::new(app::module::test::create_hybrid_test_app().await.unwrap());
            let mcp_clients =
                Arc::new(jobworkerp_runner::runner::mcp::proxy::McpServerFactory::default());
            MockJobRunner {
                runner_factory: Arc::new(RunnerFactory::new(
                    app_module.clone(),
                    mcp_clients.clone(),
                )),
                runner_pool: RunnerFactoryWithPoolMap::new(
                    Arc::new(RunnerFactory::new(app_module, mcp_clients)),
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

    #[test]
    fn test_run_job() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            static JOB_RUNNER: OnceCell<Box<MockJobRunner>> = OnceCell::const_new();
            JOB_RUNNER
                .get_or_init(|| async { Box::new(MockJobRunner::new().await) })
                .await;

            let run_after = datetime::now_millis() + 1000;
            let jargs = ProstMessageCodec::serialize_message(&CommandArgs {
                command: "sleep".to_string(),
                args: vec!["1".to_string()],
                with_memory_monitoring: true,
            })
            .unwrap();
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
                    request_streaming: false,
                }),
            };
            let worker_id = WorkerId { value: 1 };
            let runner_settings = vec![];

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
                .get()
                .unwrap()
                .run_job(&runner_data, &worker_id, &worker, job.clone())
                .await;
            let output = ProstMessageCodec::deserialize_message::<CommandResult>(
                &res.output.unwrap().items[0],
            )
            .unwrap();
            assert_eq!(res.status, ResultStatus::Success as i32);
            assert_eq!(output.exit_code.unwrap(), 0);
            assert_eq!(res.retried, 0);
            assert_eq!(res.max_retry, 0);
            assert_eq!(res.priority, 0);
            assert_eq!(res.timeout, 0);
            assert_eq!(res.enqueue_time, 0);
            assert_eq!(res.run_after_time, run_after);
            assert!(res.start_time >= run_after); // wait until run_after (expect short time)
            assert!(res.end_time > res.start_time);
            assert!(res.end_time - res.start_time >= 1000); // sleep
            let jargs = ProstMessageCodec::serialize_message(&CommandArgs {
                command: "sleep".to_string(),
                args: vec!["2".to_string()],
                with_memory_monitoring: true,
            })
            .unwrap();

            let timeout_job = Job {
                id: Some(JobId { value: 2 }),
                data: Some(JobData {
                    args: jargs,   // sleep 2sec
                    timeout: 1000, // timeout 1sec
                    ..job.data.as_ref().unwrap().clone()
                }),
            };
            let (res, _) = JOB_RUNNER
                .get()
                .unwrap()
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
        });
        Ok(())
    }
}
