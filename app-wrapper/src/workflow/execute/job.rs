use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use app::app::job::{JobApp, UseJobApp};
use app::app::job_result::{JobResultApp, UseJobResultApp};
use app::app::runner::{
    RunnerApp, RunnerDataWithDescriptor, UseRunnerApp, UseRunnerParserWithCache,
};
use app::app::worker::{UseWorkerApp, WorkerApp};
use app::module::AppModule;
use command_utils::cache_ok;
use command_utils::protobuf::ProtobufDescriptor;
use command_utils::util::datetime;
use command_utils::util::scoped_cache::ScopedCache;
use futures::stream::BoxStream;
use infra::infra::runner::rows::RunnerWithSchema;
use infra_utils::infra::memory::MemoryCacheImpl;
use proto::jobworkerp::data::{
    JobId, JobResult, JobResultData, Priority, QueueType, ResponseType, ResultOutputItem,
    ResultStatus, RetryPolicy, RetryType, Worker, WorkerData, WorkerId,
};
use proto::ProtobufHelper;
use std::hash::{DefaultHasher, Hasher};

const DEFAULT_RETRY_POLICY: RetryPolicy = RetryPolicy {
    r#type: RetryType::Exponential as i32,
    interval: 3000,
    max_interval: 60000,
    max_retry: 3,
    basis: 2.0,
};

#[derive(Clone, Debug)]
pub struct JobExecutorWrapper {
    app_module: Arc<AppModule>,
}
impl JobExecutorWrapper {
    pub fn new(app_module: Arc<AppModule>) -> Self {
        Self { app_module }
    }
}
impl UseJobResultApp for JobExecutorWrapper {
    fn job_result_app(&self) -> &Arc<dyn JobResultApp + 'static> {
        &self.app_module.job_result_app
    }
}
impl UseJobApp for JobExecutorWrapper {
    fn job_app(&self) -> &Arc<dyn JobApp + 'static> {
        &self.app_module.job_app
    }
}
impl UseWorkerApp for JobExecutorWrapper {
    fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static> {
        &self.app_module.worker_app
    }
}
impl UseRunnerApp for JobExecutorWrapper {
    fn runner_app(&self) -> Arc<dyn RunnerApp + 'static> {
        self.app_module.runner_app.clone()
    }
}
impl UseRunnerParserWithCache for JobExecutorWrapper {
    fn descriptor_cache(&self) -> &MemoryCacheImpl<Arc<String>, RunnerDataWithDescriptor> {
        &self.app_module.descriptor_cache
    }
}
impl ProtobufHelper for JobExecutorWrapper {}
impl UseJobExecutorHelper for JobExecutorWrapper {}

pub trait UseJobExecutorHelper:
    UseJobApp
    + UseWorkerApp
    + UseRunnerApp
    + UseJobResultApp
    + UseRunnerParserWithCache
    + ProtobufHelper
    + Send
    + Sync
{
    fn find_runner_by_name_with_cache(
        &self,
        cache: &ScopedCache<String, Option<RunnerWithSchema>>,
        name: &str,
    ) -> impl std::future::Future<Output = Result<Option<RunnerWithSchema>>> + Send
    where
        Self: Send + Sync,
    {
        async move {
            cache_ok!(
                cache,
                format!("runner:{}", name),
                self.find_runner_by_name(name)
            )
        }
    }
    fn find_runner_by_name(
        &self,
        name: &str,
    ) -> impl std::future::Future<Output = Result<Option<RunnerWithSchema>>> + Send
    where
        Self: Send + Sync,
    {
        async move {
            // TODO find by name method
            let list = self
                .runner_app()
                .find_runner_all_list(Some(&Duration::from_secs(60)))
                .await?;
            Ok(list
                .iter()
                .find(|r| r.data.as_ref().is_some_and(|d| d.name == name))
                .cloned())
        }
    }
    fn find_or_create_worker(
        &self,
        worker_data: &WorkerData,
    ) -> impl std::future::Future<Output = Result<Worker>> + Send
    where
        Self: Send + Sync,
    {
        async move {
            let worker = self
                .worker_app()
                .find_by_name(worker_data.name.as_str())
                .await?;

            // if not found, create sentence embedding worker
            let worker = if let Some(w) = worker {
                w
            } else {
                tracing::debug!(
                    "worker {} not found. create new worker: {:?}",
                    &worker_data.name,
                    &worker_data.runner_id
                );
                let wid = self.worker_app().create(worker_data).await?;
                // wait for worker creation? (replica db)
                // tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                Worker {
                    id: Some(wid),
                    data: Some(worker_data.to_owned()),
                }
            };
            Ok(worker)
        }
    }

    // fillin runner_id and enqueue job and get result data for worker
    fn enqueue_and_get_result_worker_job_with_runner(
        &self,
        runner_name: &str,
        mut worker_data: WorkerData,
        args: Vec<u8>,
        timeout_sec: u32,
    ) -> impl std::future::Future<Output = Result<JobResultData>> + Send {
        async move {
            let runner = self.find_runner_by_name(runner_name).await?;
            worker_data.runner_id = runner.and_then(|r| r.id);
            tracing::debug!("resolved runner_id: {:?}", &worker_data.runner_id);
            let res = self
                .enqueue_and_get_result_worker_job(&worker_data, args, timeout_sec)
                .await?;
            if res.status() == ResultStatus::Success {
                Ok(res)
            } else {
                Err(anyhow!(
                    "job failed: {:?}",
                    res.output.and_then(|o| o
                        .items
                        .first()
                        .cloned()
                        .map(|e| String::from_utf8_lossy(&e).into_owned()))
                ))
            }
        }
    }
    // enqueue job and get result data for worker
    // XXX result data is JobResultData (not stream)
    fn enqueue_and_get_result_worker_job(
        &self,
        worker_data: &WorkerData,
        args: Vec<u8>,
        timeout_sec: u32,
    ) -> impl std::future::Future<Output = Result<JobResultData>> + Send {
        async move {
            self.enqueue_worker_job(worker_data, args, timeout_sec)
                .await?
                .1
                .ok_or(anyhow!("result not found"))?
                .data
                .ok_or(anyhow!("result data not found"))
        }
    }
    // enqueue job and get only output data for worker
    fn enqueue_and_get_output_worker_job(
        &self,
        worker_data: &WorkerData,
        args: Vec<u8>,
        timeout_sec: u32,
    ) -> impl std::future::Future<Output = Result<Vec<u8>>> + Send {
        async move {
            let res = self
                .enqueue_and_get_result_worker_job(worker_data, args, timeout_sec)
                .await?;
            if res.status() == ResultStatus::Success && res.output.is_some() {
                // output is Vec<Vec<u8>> but actually 1st Vec<u8> is valid.
                let output = res
                    .output
                    .as_ref()
                    .ok_or(anyhow!("job result output is empty: {:?}", res))?
                    .items
                    .first()
                    .ok_or(anyhow!(
                        "{} job result output first is empty: {:?}",
                        &worker_data.name,
                        res
                    ))?
                    .to_owned();
                Ok(output)
            } else {
                Err(anyhow!(
                    "job failed: {:?}",
                    res.output.and_then(|o| o
                        .items
                        .first()
                        .cloned()
                        .map(|e| String::from_utf8_lossy(&e).into_owned()))
                ))
            }
        }
    }
    // enqueue job for worker (use find_or_create_worker)
    fn enqueue_worker_job(
        &self,
        worker_data: &WorkerData,
        args: Vec<u8>,
        timeout_sec: u32,
    ) -> impl std::future::Future<
        Output = Result<(
            JobId,
            Option<JobResult>,
            Option<BoxStream<'static, ResultOutputItem>>,
        )>,
    > + Send {
        async move {
            let worker = self.find_or_create_worker(worker_data).await?;
            self.job_app()
                .enqueue_job(
                    worker.id.as_ref(),
                    None,
                    args,
                    None,
                    0,
                    Priority::High as i32,
                    timeout_sec as u64 * 1000,
                    None,
                    false, // TODO can treat as stream job?
                )
                .await
        }
    }
    // enqueue job for worker and get output data
    fn enqueue_job_and_get_output(
        &self,
        worker_id: &WorkerId,
        args: Vec<u8>,
        timeout_sec: u32,
    ) -> impl std::future::Future<Output = Result<Vec<u8>>> + Send {
        async move {
            tracing::debug!("enqueue_job_and_get_output: {:?}", &worker_id);
            let res = self
                .job_app()
                .enqueue_job(
                    Some(worker_id),
                    None,
                    args,
                    None,
                    0,
                    Priority::High as i32, // higher priority for user slack response
                    timeout_sec as u64 * 1000,
                    None,
                    false, // TODO can treat as stream job?
                )
                .await
                .map(|r| r.1)
                .context("enqueue_worker_job")?
                .ok_or(anyhow!("result not found"))?
                .data
                .ok_or(anyhow!("result data not found"))?;
            if res.status() == ResultStatus::Success && res.output.is_some() {
                // output is Vec<Vec<u8>> but actually 1st Vec<u8> is valid.
                let output = res
                    .output
                    .as_ref()
                    .ok_or(anyhow!("job result output is empty: {:?}", res))?
                    .items
                    .first()
                    .unwrap_or(&vec![])
                    .to_owned();
                Ok(output)
            } else {
                Err(anyhow!(
                    "job failed: {:?}",
                    res.output.and_then(|o| o
                        .items
                        .first()
                        .cloned()
                        .map(|e| String::from_utf8_lossy(&e).into_owned()))
                ))
            }
        }
    }

    fn create_worker_data_from(
        &self,
        name: &str,
        worker_params: Option<serde_json::Value>,
        runner_settings: Vec<u8>,
    ) -> impl std::future::Future<Output = Result<WorkerData>> + Send {
        async move {
            if let Some(RunnerWithSchema {
                id: Some(sid),
                data: Some(_sdata),
                ..
            }) = self.find_runner_by_name(name).await?
            {
                if let Some(serde_json::Value::Object(obj)) = worker_params {
                    // Override values with workflow metadata
                    Ok(WorkerData {
                        name: obj
                            .get("name")
                            .and_then(|v| v.as_str().map(|s| s.to_string()))
                            .unwrap_or_else(|| name.to_string().clone()),
                        description: obj
                            .get("description")
                            .and_then(|v| v.as_str().map(|s| s.to_string()))
                            .unwrap_or_else(|| "".to_string()),
                        runner_id: Some(sid),
                        runner_settings,
                        periodic_interval: 0,
                        channel: obj
                            .get("channel")
                            .and_then(|v| v.as_str().map(|s| s.to_string())),
                        queue_type: obj
                            .get("queue_type")
                            .and_then(|v| v.as_str().map(|s| s.to_string()))
                            .and_then(|s| QueueType::from_str_name(&s).map(|q| q as i32))
                            .unwrap_or(QueueType::Normal as i32),
                        response_type: ResponseType::Direct as i32,
                        store_success: obj
                            .get("store_success")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                        store_failure: obj
                            .get("store_success")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(true), //
                        use_static: obj
                            .get("use_static")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                        retry_policy: Some(DEFAULT_RETRY_POLICY), //TODO
                        broadcast_results: true,
                    })
                } else {
                    // default values
                    Ok(WorkerData {
                        name: name.to_string().clone(),
                        description: "".to_string(),
                        runner_id: Some(sid),
                        runner_settings,
                        periodic_interval: 0,
                        channel: None,
                        queue_type: QueueType::Normal as i32,
                        response_type: ResponseType::Direct as i32,
                        store_success: false,
                        store_failure: true, //
                        use_static: false,
                        retry_policy: Some(DEFAULT_RETRY_POLICY), //TODO
                        broadcast_results: true,
                    })
                }
            } else {
                Err(anyhow::anyhow!("Not found runner: {}", name))
            }
        }
    }

    /// Enqueues a job for a worker and retrieves the raw output data.
    ///
    /// This function creates a worker if it doesn't exist and uses a temporary name.
    /// The worker is deleted after processing unless `use_static` is set to true.
    ///
    /// # Parameters
    /// * `name` - The name of the runner
    /// * `runner_settings` - Binary data for runner configuration
    /// * `worker_params` - Optional worker parameters (uses defaults if not provided)
    /// * `job_args` - Binary data for job arguments
    /// * `job_timeout_sec` - Timeout in seconds for the job execution
    ///
    /// # Returns
    /// Raw binary output data
    fn setup_worker_and_enqueue_with_raw_output(
        &self,
        worker_data: &mut WorkerData, // change name (add random postfix) if not static
        job_args: Vec<u8>,
        job_timeout_sec: u32,
    ) -> impl std::future::Future<Output = Result<Vec<u8>>> + Send {
        async move {
            // random name (temporary name for not static worker)
            if !worker_data.use_static {
                let mut hasher = DefaultHasher::default();
                hasher.write_i64(datetime::now_millis());
                hasher.write_i64(rand::random()); // random
                worker_data.name = format!("{}_{:x}", worker_data.name, hasher.finish());
                tracing::debug!("Worker name with hash: {}", &worker_data.name);
            }
            // TODO unwind and delete not static worker if failed to enqueue job
            if let Worker {
                id: Some(wid),
                data: Some(wdata),
            } = self.find_or_create_worker(worker_data).await?
            {
                let output = self
                    .enqueue_job_and_get_output(&wid, job_args, job_timeout_sec)
                    .await
                    .inspect_err(|e| {
                        tracing::warn!("Execute task failed: enqueue job and get output: {:#?}", e)
                    });
                // use worker one-time
                // XXX use_static means static worker in jobworkerp, not in workflow (but use as a temporary worker or not)
                if !wdata.use_static {
                    match self.worker_app().delete(&wid).await {
                        Ok(deleted) => {
                            if !deleted {
                                tracing::warn!("Worker not deleted: {:?}", wid);
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Failed to delete worker: {:?}, {:?}", wid, e);
                        }
                    }
                }
                output
            } else {
                Err(anyhow::anyhow!(
                    "Failed to find or create worker: {:#?}",
                    worker_data
                ))
            }
        }
    }
    fn setup_worker_and_enqueue(
        &self,
        runner_name: &str,           // runner(runner) name
        mut worker_data: WorkerData, // worker parameters (if not exists, use default values)
        job_args: Vec<u8>,           // enqueue job args
        job_timeout_sec: u32,        // job timeout in seconds
    ) -> impl std::future::Future<Output = Result<serde_json::Value>> + Send {
        async move {
            // use memory cache?
            if let Some(RunnerWithSchema {
                id: Some(rid),
                data: Some(rdata),
                ..
            }) = self.find_runner_by_name(runner_name).await?
            {
                // let mut worker = self
                //     .create_worker_data_from(runner_name, worker_params, runner_settings)
                //     .await?;

                let descriptors = self.parse_proto_with_cache(&rid, &rdata).await?;
                let result_descriptor = descriptors
                    .result_descriptor
                    .and_then(|d| d.get_messages().first().cloned());
                let output = self
                    .setup_worker_and_enqueue_with_raw_output(
                        &mut worker_data,
                        job_args,
                        job_timeout_sec,
                    )
                    .await?;
                let output: Result<serde_json::Value> = if let Some(desc) = result_descriptor {
                    match ProtobufDescriptor::get_message_from_bytes(desc.clone(), &output) {
                        Ok(m) => {
                            let j = ProtobufDescriptor::message_to_json(&m)?;
                            tracing::debug!(
                                "Result schema exists. decode message with proto: {:#?}",
                                j
                            );
                            serde_json::from_str(j.as_str())
                        }
                        Err(e) => {
                            tracing::warn!("Failed to parse result schema: {:#?}", e);
                            serde_json::from_slice(&output)
                        }
                    }
                } else {
                    let text = String::from_utf8_lossy(&output);
                    tracing::debug!("No result schema: {}", text);
                    Ok(serde_json::Value::String(text.to_string()))
                }
                .map_err(|e| anyhow::anyhow!("Failed to parse output: {:#?}", e));
                output
            // Ok(output)
            } else {
                Err(anyhow::anyhow!("Not found runner: {}", runner_name))
            }
        }
    }
    fn setup_worker_and_enqueue_with_json(
        &self,
        runner_name: &str,                          // runner(runner) name
        runner_settings: Option<serde_json::Value>, // runner_settings data
        mut worker_data: WorkerData, // worker parameters (if not exists, use default values)
        job_args: serde_json::Value, // enqueue job args
        job_timeout_sec: u32,        // job timeout in seconds
    ) -> impl std::future::Future<Output = Result<serde_json::Value>> + Send {
        async move {
            if let Some(RunnerWithSchema {
                id: Some(rid),
                data: Some(rdata),
                ..
            }) = self.find_runner_by_name(runner_name).await?
            // TODO local cache? (2 times request in this function)
            {
                worker_data.runner_id = Some(rid);
                let descriptors = self.parse_proto_with_cache(&rid, &rdata).await?;
                let runner_settings_descriptor = descriptors
                    .runner_settings_descriptor
                    .and_then(|d| d.get_messages().first().cloned());
                let args_descriptor = descriptors
                    .args_descriptor
                    .and_then(|d| d.get_messages().first().cloned());
                // let runner_settings_descriptor =
                //     Self::parse_runner_settings_schema_descriptor(&rdata).map_err(|e| {
                //         anyhow::anyhow!(
                //             "Failed to parse runner_settings schema descriptor: {:#?}",
                //             e
                //         )
                //     })?;
                // let args_descriptor =
                //     Self::parse_job_args_schema_descriptor(&rdata).map_err(|e| {
                //         anyhow::anyhow!("Failed to parse job_args schema descriptor: {:#?}", e)
                //     })?;

                let runner_settings = if let Some(ope_desc) = runner_settings_descriptor {
                    tracing::debug!("runner settings schema exists: {:#?}", &runner_settings);
                    runner_settings
                        .map(|j| ProtobufDescriptor::json_value_to_message(ope_desc, &j, true))
                        .unwrap_or(Ok(vec![]))
                        .map_err(|e| {
                            anyhow::anyhow!("Failed to parse runner_settings schema: {:#?}", e)
                        })?
                } else {
                    tracing::debug!("runner settings schema empty");
                    vec![]
                };
                tracing::debug!("job args: {:#?}", &job_args);
                let job_args = if let Some(desc) = args_descriptor.clone() {
                    ProtobufDescriptor::json_value_to_message(desc, &job_args, true)
                        .map_err(|e| anyhow::anyhow!("Failed to parse job_args schema: {:#?}", e))?
                } else {
                    serde_json::to_string(&job_args)
                        .map_err(|e| anyhow::anyhow!("Failed to serialize job_args: {:#?}", e))?
                        .as_bytes()
                        .to_vec()
                };
                worker_data.runner_settings = runner_settings;
                self.setup_worker_and_enqueue(
                    runner_name,     // runner(runner) name
                    worker_data,     // worker parameters (if not exists, use default values)
                    job_args,        // enqueue job args
                    job_timeout_sec, // job timeout in seconds
                )
                .await
            } else {
                Err(anyhow::anyhow!("Not found runner: {}", runner_name))
            }
        }
    }
}
