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
use infra::infra::UseIdGenerator;
use infra_utils::infra::trace::Tracing;
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::runner::RunnerTrait;
use proto::jobworkerp::data::JobResult;
use proto::jobworkerp::data::JobResultId;
use proto::jobworkerp::data::ResultOutputItem;
use proto::jobworkerp::data::{
    Job, JobResultData, ResultOutput, ResultStatus, RunnerData, WorkerData, WorkerId,
};
use result::ResultOutputEnum;
use std::collections::HashMap;
use std::{panic::AssertUnwindSafe, time::Duration};
use tracing;

// execute runner
#[async_trait]
pub trait JobRunner:
    RunnerResultHandler
    + UseJobqueueAndCodec
    + UseRunnerFactory
    + UseRunnerPoolMap
    + UseIdGenerator
    + Tracing
    + Send
    + Sync
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
    ) -> (JobResult, Option<BoxStream<'static, ResultOutputItem>>) {
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
    ) -> JobResult {
        let end = datetime::now_millis();
        let error_message = if let Some(e) = err {
            let mes = format!(
                "error in loading runner for worker:{:?}, error:{e:?}",
                worker_data.name
            );
            tracing::warn!(mes);
            mes
        } else {
            tracing::info!("runner not found for static worker:{:?}", worker_data.name);
            format!("runner not found for static worker:{:?}", worker_data.name)
        };
        let metadata = job.metadata.clone();
        self.job_result_data(
            job,
            worker_data,
            ResultStatus::FatalError,
            ResultOutputEnum::Normal(Err(anyhow!(error_message)), HashMap::default())
                .result_output(),
            end,
            end,
            Some(metadata), // XXX unwrap or default
        )
    }

    /// Setup cancellation monitoring if the runner supports it
    /// Returns Some(JobResult) if the job should be cancelled immediately, None to continue execution
    async fn setup_cancellation_monitoring_if_supported(
        &self,
        job_id: &proto::jobworkerp::data::JobId,
        data: &proto::jobworkerp::data::JobData,
        runner_impl: &mut Box<dyn RunnerTrait + Send + Sync>,
    ) -> Option<JobResult> {
        use jobworkerp_runner::runner::cancellation::CancelMonitoring;

        // Check if runner supports CancelMonitoring
        // Note: We need to check for concrete types since CancelMonitoringCapable is not object safe
        if let Some(command_runner) = runner_impl
            .as_any_mut()
            .downcast_mut::<jobworkerp_runner::runner::command::CommandRunnerImpl>(
        ) {
            tracing::debug!(
                "Setting up cancellation monitoring for CommandRunner job {}",
                job_id.value
            );

            // Setup cancellation monitoring
            match command_runner
                .setup_cancellation_monitoring(*job_id, data)
                .await
            {
                Ok(Some(job_result)) => {
                    tracing::info!("Job {} should be cancelled immediately", job_id.value);
                    return Some(job_result);
                }
                Ok(None) => {
                    tracing::debug!(
                        "Cancellation monitoring setup successful for job {}",
                        job_id.value
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to setup cancellation monitoring for job {}: {:?}",
                        job_id.value,
                        e
                    );
                }
            }
        } else {
            tracing::debug!(
                "Runner does not support cancellation monitoring for job {}",
                job_id.value
            );
        }

        // No immediate cancellation needed, continue with normal execution
        None
    }

    /// Cleanup cancellation monitoring if the runner supports it
    async fn cleanup_cancellation_monitoring_if_supported(
        &self,
        job_id: &proto::jobworkerp::data::JobId,
        runner_impl: &mut Box<dyn RunnerTrait + Send + Sync>,
    ) {
        use jobworkerp_runner::runner::cancellation::CancelMonitoring;

        // Check if runner supports CancelMonitoring
        // Note: We need to check for concrete types since CancelMonitoringCapable is not object safe
        if let Some(command_runner) = runner_impl
            .as_any_mut()
            .downcast_mut::<jobworkerp_runner::runner::command::CommandRunnerImpl>(
        ) {
            tracing::trace!(
                "Cleaning up cancellation monitoring for CommandRunner job {}",
                job_id.value
            );

            if let Err(e) = command_runner.cleanup_cancellation_monitoring().await {
                tracing::warn!(
                    "Failed to cleanup cancellation monitoring for job {}: {:?}",
                    job_id.value,
                    e
                );
            }
        }
    }

    /// キャンセル済みジョブの結果を作成（プラン実装）
    fn create_cancelled_job_result(&self, job: Job, worker_data: &WorkerData) -> JobResult {
        use command_utils::util::datetime;
        use std::collections::HashMap;

        let data = job.data.unwrap_or_default();
        let metadata = HashMap::new();

        let job_result_data = JobResultData {
            job_id: job.id,
            worker_id: data.worker_id,
            worker_name: worker_data.name.clone(),
            args: data.args,
            uniq_key: data.uniq_key,
            status: ResultStatus::Cancelled as i32,
            output: Some(ResultOutput {
                items: b"Job was cancelled before execution".to_vec(),
            }),
            retried: data.retried,
            max_retry: 0, // No retry on cancellation
            priority: data.priority,
            timeout: data.timeout,
            request_streaming: data.request_streaming,
            enqueue_time: data.enqueue_time,
            run_after_time: data.run_after_time,
            start_time: datetime::now_millis(),
            end_time: datetime::now_millis(),
            response_type: worker_data.response_type,
            store_success: false,
            store_failure: true,
        };

        JobResult {
            id: Some(JobResultId {
                value: self.id_generator().generate_id().unwrap_or_default(),
            }),
            data: Some(job_result_data),
            metadata,
        }
    }

    #[allow(unstable_name_collisions)] // for flatten()
    async fn run_job_inner(
        &'static self,
        worker_data: &WorkerData,
        job: Job,
        runner_impl: &mut Box<dyn RunnerTrait + Send + Sync>,
    ) -> (JobResult, Option<BoxStream<'static, ResultOutputItem>>) {
        let data = job.data.as_ref().unwrap(); // XXX unwrap

        // Setup cancellation monitoring if runner supports it
        let job_id = job.id.as_ref().unwrap();
        if let Some(cancelled_result) = self
            .setup_cancellation_monitoring_if_supported(job_id, data, runner_impl)
            .await
        {
            // Job was already cancelled, return the cancellation result immediately
            return (cancelled_result, None);
        }

        let run_after_time = data.run_after_time;

        let wait = run_after_time - datetime::now_millis();
        // sleep short time if run_after_time is future (should less than STORAGE_FETCH_INTERVAL)
        if wait > 0 {
            // 1min
            if wait > 60000 {
                // logic error!
                tracing::error!("wait too long time: {wait}ms, job: {job:?}");
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
                "end runner(stream: {}): {}, duration:{}(ms)",
                if res.is_ok() { "success" } else { "error" },
                &name,
                end - start,
            );
            let (status, mes) = self.job_result_status(&worker_data.retry_policy, data, res);

            // For streaming jobs, we rely on timeout-based cleanup since the stream continues running
            // The cancellation monitoring will automatically timeout after job_timeout + 30 seconds
            tracing::debug!(
                "Streaming job {} monitoring will auto-cleanup via timeout",
                job_id.value
            );

            (
                self.job_result_data(
                    job,
                    worker_data,
                    status,
                    mes.result_output(),
                    start,
                    end,
                    mes.metadata().cloned(),
                ),
                mes.stream(),
            )
        } else {
            tracing::debug!("start runner: {}", &name);
            let res = self
                .run_and_result(&job, runner_impl)
                .await
                .map(|(a, b)| ResultOutputEnum::Normal(a, b));
            let end = datetime::now_millis();
            tracing::debug!("end runner: {}, duration:{}(ms)", &name, end - start);
            // TODO
            let (status, mes) = self.job_result_status(&worker_data.retry_policy, data, res);

            // Cleanup cancellation monitoring
            self.cleanup_cancellation_monitoring_if_supported(job_id, runner_impl)
                .await;

            (
                self.job_result_data(
                    job,
                    worker_data,
                    status,
                    mes.result_output(),
                    start,
                    end,
                    mes.metadata().cloned(),
                ),
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
        let metadata = job.metadata.clone();
        let data = job.data.as_ref().unwrap(); // XXX unwrap
        let args = &data.args; // XXX unwrap, clone
        let name = runner_impl.name();
        if data.timeout > 0 {
            tokio::select! {
                r = AssertUnwindSafe(
                    runner_impl.run_stream(args, metadata),
                ).catch_unwind() => {
                    r.map_err(|e| {
                        let msg = format!("Caught panic from runner {name}: {e:?}");
                        tracing::error!(msg);
                        anyhow!(msg)
                    }).flatten()
                },
                _ = tokio::time::sleep(Duration::from_millis(data.timeout)) => {
                    runner_impl.cancel().await;
                    tracing::warn!("timeout: {}ms, the job will be dropped: {job:?}", data.timeout);
                    Err(JobWorkerError::TimeoutError(format!("timeout: {}ms", data.timeout)).into())
                }
            }
        } else {
            AssertUnwindSafe(runner_impl.run_stream(args, metadata))
                .catch_unwind()
                .await
                .map_err(|e| {
                    let msg = format!("Caught panic from runner {name}: {e:?}");
                    tracing::error!(msg);
                    anyhow!(msg)
                })
                .flatten()
        }
    }
    #[allow(unstable_name_collisions)] // for flatten()
    async fn run_and_result(
        &self,
        job: &Job,
        runner_impl: &mut Box<dyn RunnerTrait + Send + Sync>,
    ) -> Result<(Result<Vec<u8>>, HashMap<String, String>)> {
        let data = job.data.as_ref().unwrap(); // XXX unwrap
        let metadata = job.metadata.clone();
        let args = &data.args; // XXX unwrap, clone
        let name = runner_impl.name();
        if data.timeout > 0 {
            tokio::select! {
                r = AssertUnwindSafe(
                    runner_impl.run(args, metadata)
                ).catch_unwind() => {
                    r.map_err(|e| {
                        let msg = format!("Caught panic from runner {name}: {e:?}");
                        tracing::error!(msg);
                        anyhow!(msg)
                    }).inspect_err(|e| tracing::warn!("error in running runner: {name} : {e:?}"))
                },
                _ = tokio::time::sleep(Duration::from_millis(data.timeout)) => {
                    runner_impl.cancel().await;
                    tracing::warn!("timeout: {}ms, the job will be dropped: {job:?}", data.timeout);
                    Err(JobWorkerError::TimeoutError(format!("timeout: {}ms", data.timeout)).into())
                }
            }
        } else {
            AssertUnwindSafe(runner_impl.run(args, metadata))
                .catch_unwind()
                .await
                .map_err(|e| {
                    let msg = format!("Caught panic from runner {name}: {e:?}");
                    tracing::error!(msg);
                    anyhow!(msg)
                })
                .inspect_err(|e| tracing::warn!("error in running runner: {name} : {e:?}"))
        }
    }
    // calculate job status and create JobResult (not increment retry count now)
    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn job_result_data(
        &self,
        job: Job,
        worker: &WorkerData,
        st: ResultStatus,
        res: Option<ResultOutput>,
        start_msec: i64,
        end_msec: i64,
        metadata: Option<HashMap<String, String>>,
    ) -> JobResult {
        let dat = job.data.unwrap_or_default(); // XXX unwrap or default
        let data = JobResultData {
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
        };
        JobResult {
            id: Some(JobResultId {
                value: self.id_generator().generate_id().unwrap_or_default(), // XXX unwrap or default
            }),
            data: Some(data),
            metadata: metadata.unwrap_or_default(), // XXX unwrap or default
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{map::RunnerFactoryWithPoolMap, *};
    use anyhow::Result;
    use app::app::WorkerConfig;
    use app_wrapper::runner::RunnerFactory;
    use infra::infra::IdGeneratorWrapper;
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
        id_generator: IdGeneratorWrapper,
    }
    impl MockJobRunner {
        async fn new() -> Self {
            let app_module = Arc::new(app::module::test::create_hybrid_test_app().await.unwrap());
            let app_wrapper_module = Arc::new(
                app_wrapper::modules::test::create_test_app_wrapper_module(app_module.clone()),
            );
            let mcp_clients =
                Arc::new(jobworkerp_runner::runner::mcp::proxy::McpServerFactory::default());
            MockJobRunner {
                runner_factory: Arc::new(RunnerFactory::new(
                    app_module.clone(),
                    app_wrapper_module.clone(),
                    mcp_clients.clone(),
                )),
                runner_pool: RunnerFactoryWithPoolMap::new(
                    Arc::new(RunnerFactory::new(
                        app_module,
                        app_wrapper_module,
                        mcp_clients,
                    )),
                    Arc::new(WorkerConfig::default()),
                ),
                id_generator: IdGeneratorWrapper::new_mock(),
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
    impl Tracing for MockJobRunner {}
    impl UseIdGenerator for MockJobRunner {
        fn id_generator(&self) -> &IdGeneratorWrapper {
            &self.id_generator
        }
    }

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
                ..Default::default()
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
            let res = res.data.unwrap();
            let output =
                ProstMessageCodec::deserialize_message::<CommandResult>(&res.output.unwrap().items)
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
                ..Default::default()
            };
            let (res, _) = JOB_RUNNER
                .get()
                .unwrap()
                .run_job(&runner_data, &worker_id, &worker, timeout_job.clone())
                .await;
            let res = res.data.unwrap();
            assert_eq!(res.status, ResultStatus::MaxRetry as i32); // no retry
            assert_eq!(
                res.output.unwrap().items,
                b"RuntimeError(timeout error: \"timeout: 1000ms\")"
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
