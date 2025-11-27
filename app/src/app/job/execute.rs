use crate::app::job::{JobApp, UseJobApp};
use crate::app::job_result::{JobResultApp, UseJobResultApp};
use crate::app::runner::{
    RunnerApp, RunnerDataWithDescriptor, UseRunnerApp, UseRunnerParserWithCache,
};
use crate::app::worker::{UseWorkerApp, WorkerApp};
use crate::module::AppModule;
use anyhow::{anyhow, Result};
use command_utils::cache_ok;
use command_utils::protobuf::ProtobufDescriptor;
use command_utils::util::scoped_cache::ScopedCache;
use futures::stream::BoxStream;
use infra::infra::runner::rows::RunnerWithSchema;
use jobworkerp_base::error::JobWorkerError;
use memory_utils::cache::moka::MokaCacheImpl;
use proto::jobworkerp::data::{
    JobId, JobResult, Priority, QueueType, ResponseType, ResultOutputItem, ResultStatus,
    RetryPolicy, RetryType, RunnerData, RunnerId, Worker, WorkerData, WorkerId,
};
use proto::ProtobufHelper;
use std::collections::HashMap;
use std::sync::Arc;

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
    fn descriptor_cache(&self) -> &MokaCacheImpl<Arc<String>, RunnerDataWithDescriptor> {
        &self.app_module.descriptor_cache
    }
}
impl ProtobufHelper for JobExecutorWrapper {}
impl UseJobExecutor for JobExecutorWrapper {}

// TODO integrate with function calling logic
pub trait UseJobExecutor:
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
                self.runner_app().find_runner_by_name(name)
            )
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
            }) = self.runner_app().find_runner_by_name(name).await?
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

    #[allow(clippy::too_many_arguments)]
    fn enqueue_with_worker_or_temp(
        &self,
        metadata: Arc<HashMap<String, String>>, // metadata for job
        worker_id: Option<WorkerId>, // worker id (use if worker_id. if not exists, use default values)
        worker_data: WorkerData,     // change name (add random postfix) if not static
        job_args: Vec<u8>,
        uniq_key: Option<String>,
        job_timeout_sec: u32,
        streaming: bool,
        using: Option<String>, // using for MCP/Plugin runners
    ) -> impl std::future::Future<
        Output = Result<(
            JobId,
            Option<JobResult>,
            Option<BoxStream<'static, ResultOutputItem>>,
        )>,
    > + Send {
        async move {
            if let Some(wid) = worker_id {
                tracing::debug!("enqueue job with worker: {:?}", wid);
                self.job_app()
                    .enqueue_job(
                        metadata.clone(),
                        Some(&wid),
                        None,
                        job_args,
                        uniq_key,
                        0,
                        Priority::Medium as i32,
                        job_timeout_sec as u64 * 1000,
                        None,
                        streaming,
                        using,
                    )
                    .await
            } else {
                tracing::debug!("enqueue job without worker");
                self.job_app()
                    .enqueue_job_with_temp_worker(
                        metadata.clone(),
                        worker_data,
                        job_args,
                        uniq_key,
                        0,
                        Priority::Medium as i32,
                        job_timeout_sec as u64 * 1000,
                        None,
                        streaming,
                        true,
                        using,
                    )
                    .await
            }
        }
    }
    fn setup_runner_and_settings(
        &self,
        runner: &RunnerWithSchema,                  // runner(runner) name
        runner_settings: Option<serde_json::Value>, // runner_settings data
    ) -> impl std::future::Future<Output = Result<Vec<u8>>> + Send {
        async move {
            if let RunnerWithSchema {
                id: Some(rid),
                data: Some(rdata),
                ..
            } = runner
            {
                let descriptors = self.parse_proto_with_cache(rid, rdata).await?;
                let runner_settings_descriptor = descriptors
                    .runner_settings_descriptor
                    .and_then(|d| d.get_messages().first().cloned());
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
                Ok(runner_settings)
            } else {
                Err(anyhow::anyhow!("Illegal runner: {:#?}", &runner))
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn setup_worker_and_enqueue_with_json_full_output(
        &self,
        metadata: Arc<HashMap<String, String>>, // metadata for job
        runner_name: &str,                      // runner(runner) name
        worker_data: WorkerData, // worker parameters (if not exists, use default values)
        job_args: serde_json::Value, // enqueue job args
        uniq_key: Option<String>,
        job_timeout_sec: u32,  // job timeout in seconds
        streaming: bool,       // TODO request streaming
        using: Option<String>, // using parameter for MCP/Plugin runners
    ) -> impl std::future::Future<
        Output = Result<(
            JobId,
            Option<JobResult>,
            Option<BoxStream<'static, ResultOutputItem>>,
        )>,
    > + Send {
        async move {
            if let Some(RunnerWithSchema {
                id: Some(rid),
                data: Some(rdata),
                ..
            }) = self.runner_app().find_runner_by_name(runner_name).await?
            // TODO local cache? (2 times request in this function)
            {
                tracing::debug!("job args: {:#?}", &job_args);
                let job_args = self.transform_job_args(&rid, &rdata, &job_args).await?;
                let wid = if worker_data.use_static {
                    self.find_or_create_worker(&worker_data).await?.id
                } else {
                    None
                };
                self.enqueue_with_worker_or_temp(
                    metadata,
                    wid,
                    worker_data,
                    job_args,
                    uniq_key,
                    job_timeout_sec,
                    streaming,
                    using, // Pass using parameter for MCP/Plugin runners
                )
                .await
            } else {
                Err(anyhow::anyhow!("Not found runner: {}", runner_name))
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn setup_worker_and_enqueue_with_json(
        &self,
        metadata: Arc<HashMap<String, String>>, // metadata for job
        runner_name: &str,                      // runner(runner) name
        worker_data: WorkerData, // worker parameters (if not exists, use default values)
        job_args: serde_json::Value, // enqueue job args
        uniq_key: Option<String>,
        job_timeout_sec: u32,  // job timeout in seconds
        _streaming: bool,      // TODO request streaming
        using: Option<String>, // using for MCP/Plugin runners
    ) -> impl std::future::Future<Output = Result<serde_json::Value>> + Send {
        async move {
            if let Some(RunnerWithSchema {
                id: Some(rid),
                data: Some(rdata),
                ..
            }) = self.runner_app().find_runner_by_name(runner_name).await?
            // TODO local cache? (2 times request in this function)
            {
                tracing::debug!("job args: {:#?}", &job_args);
                let job_args = self.transform_job_args(&rid, &rdata, &job_args).await?;

                let wid = if worker_data.use_static {
                    self.find_or_create_worker(&worker_data).await?.id
                } else {
                    None
                };
                let (_jid, res, _stream) = self
                    .enqueue_with_worker_or_temp(
                        metadata,
                        wid,
                        worker_data,
                        job_args,
                        uniq_key,
                        job_timeout_sec,
                        false, // no streaming
                        using,
                    )
                    .await?;
                let output = res
                    .map(|r| self.extract_job_result_output(r))
                    .ok_or(anyhow::anyhow!(
                        "Failed to enqueue job or job result not found"
                    ))
                    .and_then(|output| output)?;
                self.transform_raw_output(&rid, &rdata, &output).await
            } else {
                Err(anyhow::anyhow!("Not found runner: {}", runner_name))
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn enqueue_with_worker_name(
        &self,
        metadata: Arc<HashMap<String, String>>, // metadata for job
        worker_name: &str,                      // runner(runner) name
        job_args: &serde_json::Value,           // enqueue job args
        uniq_key: Option<String>, // unique key for job (if not exists, use default values)
        job_timeout_sec: u32,     // job timeout in seconds
        streaming: bool,          // TODO request streaming
        using: Option<String>,    // using parameter for MCP/Plugin runners
    ) -> impl std::future::Future<
        Output = Result<(
            JobId,
            Option<JobResult>,
            Option<BoxStream<'static, ResultOutputItem>>,
        )>,
    > + Send {
        async move {
            let worker = self
                .worker_app()
                .find_by_name(worker_name)
                .await?
                .ok_or_else(|| {
                    JobWorkerError::WorkerNotFound(format!("Not found worker: {worker_name}"))
                })?;
            if let Worker {
                id: Some(wid),
                data: Some(worker_data),
            } = worker
            {
                // use memory cache?
                if let Some(RunnerWithSchema {
                    id: Some(rid),
                    data: Some(rdata),
                    ..
                }) =
                    self.runner_app()
                        .find_runner(worker_data.runner_id.as_ref().ok_or(
                            JobWorkerError::NotFound(format!(
                                "Not found runner for worker {}: {:?}",
                                worker_name,
                                worker_data.runner_id.as_ref()
                            )),
                        )?)
                        .await?
                {
                    let job_args = self.transform_job_args(&rid, &rdata, job_args).await?;
                    self.enqueue_with_worker_or_temp(
                        metadata,
                        Some(wid), // worker
                        worker_data,
                        job_args,
                        uniq_key,
                        job_timeout_sec,
                        streaming,
                        using, // Pass using parameter for MCP/Plugin workers
                    )
                    .await
                } else {
                    Err(anyhow::anyhow!(
                        "Not found runner: {:?}",
                        worker_data.runner_id.as_ref()
                    ))
                }
            } else {
                Err(anyhow::anyhow!("Not found worker: {}", worker_name))
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn enqueue_with_worker_name_and_output_json(
        &self,
        metadata: Arc<HashMap<String, String>>, // metadata for job
        worker_name: &str,                      // runner(runner) name
        job_args: &serde_json::Value,           // enqueue job args
        uniq_key: Option<String>, // unique key for job (if not exists, use default values)
        job_timeout_sec: u32,     // job timeout in seconds
        streaming: bool,          // TODO request streaming
        using: Option<String>,    // using parameter for MCP/Plugin runners
    ) -> impl std::future::Future<Output = Result<serde_json::Value>> + Send {
        async move {
            let worker = self
                .worker_app()
                .find_by_name(worker_name)
                .await?
                .ok_or_else(|| {
                    JobWorkerError::WorkerNotFound(format!("Not found worker: {worker_name}"))
                })?;
            if let Worker {
                id: Some(wid),
                data: Some(worker_data),
            } = worker
            {
                // use memory cache?
                if let Some(RunnerWithSchema {
                    id: Some(rid),
                    data: Some(rdata),
                    ..
                }) =
                    self.runner_app()
                        .find_runner(worker_data.runner_id.as_ref().ok_or(
                            JobWorkerError::NotFound(format!(
                                "Not found runner for worker {}: {:?}",
                                worker_name,
                                worker_data.runner_id.as_ref()
                            )),
                        )?)
                        .await?
                {
                    let job_args = self.transform_job_args(&rid, &rdata, job_args).await?;
                    let res = self
                        .enqueue_with_worker_or_temp(
                            metadata,
                            Some(wid), // worker
                            worker_data,
                            job_args,
                            uniq_key,
                            job_timeout_sec,
                            streaming,
                            using, // Pass using parameter for MCP/Plugin workers
                        )
                        .await?;
                    if let Some(res) = res.1 {
                        let output = self.extract_job_result_output(res)?;
                        self.transform_raw_output(&rid, &rdata, output.as_slice())
                            .await
                    } else {
                        Err(anyhow::anyhow!(
                            "Failed to enqueue job or job result not found"
                        ))
                    }
                } else {
                    Err(anyhow::anyhow!(
                        "Not found runner: {:?}",
                        worker_data.runner_id.as_ref()
                    ))
                }
            } else {
                Err(anyhow::anyhow!("Not found worker: {}", worker_name))
            }
        }
    }

    fn transform_job_args(
        &self,
        rid: &RunnerId,
        rdata: &RunnerData,
        job_args: &serde_json::Value, // enqueue job args
    ) -> impl std::future::Future<Output = Result<Vec<u8>>> + Send {
        async move {
            let descriptors = self.parse_proto_with_cache(rid, rdata).await?;
            let args_descriptor = descriptors
                .args_descriptor
                .and_then(|d| d.get_messages().first().cloned());

            tracing::debug!("job args: {:#?}", &job_args);
            if let Some(desc) = args_descriptor.clone() {
                Ok(
                    ProtobufDescriptor::json_value_to_message(desc, job_args, true).map_err(
                        |e| anyhow::anyhow!("Failed to parse job_args schema: {:#?}", e),
                    )?,
                )
            } else {
                Ok(serde_json::to_string(&job_args)
                    .map_err(|e| anyhow::anyhow!("Failed to serialize job_args: {:#?}", e))?
                    .as_bytes()
                    .to_vec())
            }
        }
    }
    #[inline]
    fn extract_job_result_output(&self, job_result: JobResult) -> Result<Vec<u8>> {
        if let JobResult {
            id: _jid,
            data: Some(jdata),
            metadata: _,
        } = job_result
        {
            if jdata.status() == ResultStatus::Success && jdata.output.is_some() {
                // output is Vec<Vec<u8>> but actually 1st Vec<u8> is valid.
                Ok(jdata
                    .output
                    .as_ref()
                    .ok_or(anyhow!("job result output is empty: {:?}", &jdata))?
                    .items
                    .to_owned())
            } else {
                Err(anyhow!(
                    "job failed: {:?}",
                    jdata
                        .output
                        .and_then(|o| String::from_utf8_lossy(&o.items).into_owned().into())
                ))
            }
        } else {
            Err(anyhow::anyhow!("job result not found"))
        }
    }

    #[inline]
    fn transform_raw_output(
        &self,
        rid: &RunnerId,
        rdata: &RunnerData,
        output: &[u8], // enqueue job args
    ) -> impl std::future::Future<Output = Result<serde_json::Value>> + Send {
        async move {
            // with cache
            let descriptors = self.parse_proto_with_cache(rid, rdata).await?;
            let result_descriptor = descriptors
                .result_descriptor
                .and_then(|d| d.get_messages().first().cloned());

            tracing::debug!("job output length: {}", output.len());
            if let Some(desc) = result_descriptor {
                match ProtobufDescriptor::get_message_from_bytes(desc.clone(), output) {
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
                        serde_json::from_slice(output)
                    }
                }
            } else {
                let text = String::from_utf8_lossy(output);
                tracing::debug!("No result schema: {}", text);
                Ok(serde_json::Value::String(text.to_string()))
            }
            .map_err(|e| anyhow::anyhow!("Failed to parse output: {:#?}", e))
        }
    }
    fn parse_job_result(
        job_result: JobResult,
        runner_data: &RunnerData,
    ) -> Result<Option<serde_json::Value>> {
        let output = job_result
            .data
            .as_ref()
            .and_then(|r| r.output.as_ref().map(|o| &o.items));
        if let Some(output) = output {
            let result_descriptor = Self::parse_job_result_schema_descriptor(runner_data)?;
            if let Some(desc) = result_descriptor {
                match ProtobufDescriptor::get_message_from_bytes(desc, output) {
                    Ok(m) => {
                        let j = ProtobufDescriptor::message_to_json_value(&m)?;
                        tracing::debug!(
                            "Result schema exists. decode message with proto: {:#?}",
                            j
                        );
                        Ok(Some(j))
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse result schema: {:#?}", e);
                        Err(JobWorkerError::RuntimeError(format!(
                            "Failed to parse result schema: {e:#?}"
                        )))
                    }
                }
            } else {
                let text = String::from_utf8_lossy(output);
                tracing::debug!("No result schema: {}", text);
                Ok(Some(serde_json::Value::String(text.to_string())))
            }
            .map_err(|e| {
                JobWorkerError::RuntimeError(format!("Failed to parse output: {e:#?}")).into()
            })
        } else {
            tracing::warn!("No output found");
            Ok(None)
        }
    }

    #[inline]
    fn decode_job_result_output(
        &self,
        runner_id: Option<&RunnerId>, // runner(runner) id
        runner_name: Option<&str>,    // runner(runner) name
        output: &[u8],                // enqueue job args
    ) -> impl std::future::Future<Output = Result<serde_json::Value>> + Send {
        async move {
            if let Some(RunnerWithSchema {
                id: Some(rid),
                data: Some(rdata),
                ..
            }) = match (runner_id, runner_name) {
                (Some(rid), _) => self.runner_app().find_runner(rid).await?,
                (_, Some(runner_name)) => {
                    self.runner_app().find_runner_by_name(runner_name).await?
                }
                (_, _) => {
                    return Err(anyhow::anyhow!(
                        "runner_id or runner_name must be specified"
                    ));
                }
            } {
                self.transform_raw_output(&rid, &rdata, output).await
            } else {
                Err(anyhow::anyhow!("Not found runner for job result output"))
            }
        }
    }
}
