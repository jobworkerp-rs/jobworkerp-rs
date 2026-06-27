use crate::app::job::{ChannelJobResultFuture, JobApp, UseJobApp};
use crate::app::job_result::{JobResultApp, UseJobResultApp};
use crate::app::runner::{
    RunnerApp, RunnerDataWithDescriptor, UseRunnerApp, UseRunnerParserWithCache,
};
use crate::app::worker::{UseWorkerApp, WorkerApp};
use crate::module::AppModule;
use anyhow::{Result, anyhow};
use command_utils::cache_ok;
use command_utils::protobuf::ProtobufDescriptor;
use command_utils::util::scoped_cache::ScopedCache;
use futures::stream::BoxStream;
use infra::infra::runner::rows::RunnerWithSchema;
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::runner::factory::RunnerSpecFactory;
use memory_utils::cache::moka::MokaCacheImpl;
use proto::jobworkerp::data::{
    JobExecutionOverrides, JobId, JobResult, Priority, QueueType, ResponseType, ResultOutputItem,
    ResultStatus, RetryPolicy, RetryType, RunnerData, RunnerId, StreamingType, Worker, WorkerData,
    WorkerId,
};
use proto::{DEFAULT_METHOD_NAME, ProtobufHelper};
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

    /// Access to repository module for advanced streaming operations
    /// Returns None if redis module is not available (standalone mode)
    pub fn redis_job_result_pubsub_repository(
        &self,
    ) -> Option<&infra::infra::job_result::pubsub::redis::RedisJobResultPubSubRepositoryImpl> {
        self.app_module
            .repositories
            .redis_module
            .as_ref()
            .map(|m| &m.redis_job_result_pubsub_repository)
    }

    /// Access to channel-based job result pubsub repository for standalone mode
    pub fn chan_job_result_pubsub_repository(
        &self,
    ) -> Option<&infra::infra::job_result::pubsub::chan::ChanJobResultPubSubRepositoryImpl> {
        self.app_module
            .repositories
            .rdb_module
            .as_ref()
            .map(|m| &m.chan_job_result_pubsub_repository)
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
impl UseRunnerSpecFactory for JobExecutorWrapper {
    fn runner_spec_factory(&self) -> &Arc<RunnerSpecFactory> {
        &self.app_module.config_module.runner_factory
    }
}
impl ProtobufHelper for JobExecutorWrapper {}
impl UseJobExecutor for JobExecutorWrapper {}

pub struct NamedWorkerChannelJob {
    pub job_id: JobId,
    pub result_fut: ChannelJobResultFuture,
    pub runner_id: RunnerId,
    pub runner_data: RunnerData,
    pub using: Option<String>,
}

pub enum WorkerForEnqueue {
    Existing(Worker),
    Temp(WorkerData),
}

impl WorkerForEnqueue {
    /// Build an `Existing` variant from a resolved worker id and data.
    pub fn existing(worker_id: WorkerId, worker_data: WorkerData) -> Self {
        WorkerForEnqueue::Existing(Worker {
            id: Some(worker_id),
            data: Some(worker_data),
        })
    }
}

/// Trait for accessing RunnerSpecFactory
pub trait UseRunnerSpecFactory {
    fn runner_spec_factory(&self) -> &Arc<RunnerSpecFactory>;
}

// TODO integrate with function calling logic
pub trait UseJobExecutor:
    UseJobApp
    + UseWorkerApp
    + UseRunnerApp
    + UseJobResultApp
    + UseRunnerParserWithCache
    + UseRunnerSpecFactory
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
        worker_data: WorkerData,
    ) -> impl std::future::Future<Output = Result<Worker>> + Send
    where
        Self: Send + Sync,
    {
        async move {
            if let Some(worker) = self
                .worker_app()
                .find_by_name(worker_data.name.as_str())
                .await?
            {
                if let Worker {
                    id: Some(wid),
                    data: Some(existing_data),
                } = worker
                {
                    if worker_definition_matches_expected(&existing_data, &worker_data) {
                        return Ok(Worker {
                            id: Some(wid),
                            data: Some(existing_data),
                        });
                    }

                    let worker_data =
                        worker_data_with_expected_definition(existing_data, worker_data);
                    let wid = self.worker_app().upsert_by_name(&worker_data).await?;
                    return Ok(Worker {
                        id: Some(wid),
                        data: Some(worker_data),
                    });
                }

                return Err(JobWorkerError::WorkerNotFound(format!(
                    "worker id or data is missing: name={}",
                    worker_data.name
                ))
                .into());
            }

            let wid = self.worker_app().upsert_by_name(&worker_data).await?;
            Ok(Worker {
                id: Some(wid),
                data: Some(worker_data),
            })
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
        worker: WorkerForEnqueue,
        job_args: Vec<u8>,
        uniq_key: Option<String>,
        job_timeout_sec: u32,
        streaming_type: StreamingType,
        using: Option<String>, // using for MCP/Plugin runners
        // Per-job overrides (e.g. response_type). Required for static workers
        // whose persisted definition cannot be mutated per enqueue.
        overrides: Option<JobExecutionOverrides>,
    ) -> impl std::future::Future<
        Output = Result<(
            JobId,
            Option<JobResult>,
            Option<BoxStream<'static, ResultOutputItem>>,
        )>,
    > + Send {
        async move {
            match worker {
                WorkerForEnqueue::Existing(worker) => {
                    tracing::debug!("enqueue job with worker: {:?}", worker.id);
                    self.job_app()
                        .enqueue_job_with_worker(
                            metadata.clone(),
                            worker,
                            job_args,
                            uniq_key,
                            0,
                            Priority::Medium as i32,
                            job_timeout_sec as u64 * 1000,
                            None,
                            streaming_type,
                            using,
                            overrides,
                        )
                        .await
                }
                WorkerForEnqueue::Temp(worker_data) => {
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
                            streaming_type,
                            true,
                            using,
                            overrides,
                        )
                        .await
                }
            }
        }
    }
    /// Channel variant of [`enqueue_with_worker_or_temp`]: returns the `JobId`
    /// eagerly and the result as a `'static` future, so a caller can record the
    /// id (e.g. a workflow tracking in-flight child jobs for cancellation)
    /// before awaiting the result. Temp workers are created up-front so the
    /// `JobId` is available before the wait.
    #[allow(clippy::too_many_arguments)]
    fn enqueue_with_worker_or_temp_channel(
        &self,
        metadata: Arc<HashMap<String, String>>,
        worker: WorkerForEnqueue,
        job_args: Vec<u8>,
        uniq_key: Option<String>,
        job_timeout_sec: u32,
        streaming_type: StreamingType,
        using: Option<String>,
        // Per-job overrides (e.g. response_type). Required for static workers
        // whose persisted definition cannot be mutated per enqueue.
        overrides: Option<JobExecutionOverrides>,
    ) -> impl std::future::Future<Output = Result<(JobId, ChannelJobResultFuture)>> + Send {
        async move {
            let worker = match worker {
                WorkerForEnqueue::Existing(worker) => worker,
                WorkerForEnqueue::Temp(worker_data) => {
                    // If the subsequent enqueue fails, the temp worker is left
                    // behind with the same best-effort trade-off as
                    // `enqueue_job_with_temp_worker` (temp workers carry a TTL
                    // and are reaped, so this is not a lasting leak).
                    let wid = self
                        .worker_app()
                        .create_temp(worker_data.clone(), true)
                        .await?;
                    Worker {
                        id: Some(wid),
                        data: Some(worker_data),
                    }
                }
            };
            self.job_app()
                .enqueue_job_with_channel(
                    metadata,
                    worker,
                    job_args,
                    uniq_key,
                    0,
                    Priority::Medium as i32,
                    job_timeout_sec as u64 * 1000,
                    None,
                    streaming_type,
                    using,
                    overrides,
                )
                .await
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
                    tracing::debug!(
                        "runner settings schema exists: {:#?}",
                        runner_settings.as_ref().map(redact_sensitive_args)
                    );
                    runner_settings
                        .map(|j| {
                            ProtobufDescriptor::json_value_to_message(ope_desc, &j, true, true)
                        })
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
        job_timeout_sec: u32,          // job timeout in seconds
        streaming_type: StreamingType, // streaming type for job
        using: Option<String>,         // using parameter for MCP/Plugin runners
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
                tracing::debug!("job args: {:#?}", redact_sensitive_args(&job_args));
                let job_args = self
                    .transform_job_args(&rid, &rdata, &job_args, using.as_deref())
                    .await?;
                let worker = if worker_data.use_static {
                    WorkerForEnqueue::Existing(self.find_or_create_worker(worker_data).await?)
                } else {
                    WorkerForEnqueue::Temp(worker_data)
                };
                self.enqueue_with_worker_or_temp(
                    metadata,
                    worker,
                    job_args,
                    uniq_key,
                    job_timeout_sec,
                    streaming_type,
                    using, // Pass using parameter for MCP/Plugin runners
                    None,
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
        job_timeout_sec: u32,           // job timeout in seconds
        _streaming_type: StreamingType, // streaming type (ignored, always uses None)
        using: Option<String>,          // using for MCP/Plugin runners
    ) -> impl std::future::Future<Output = Result<serde_json::Value>> + Send {
        async move {
            if let Some(RunnerWithSchema {
                id: Some(rid),
                data: Some(rdata),
                ..
            }) = self.runner_app().find_runner_by_name(runner_name).await?
            // TODO local cache? (2 times request in this function)
            {
                tracing::debug!("job args: {:#?}", redact_sensitive_args(&job_args));
                let job_args = self
                    .transform_job_args(&rid, &rdata, &job_args, using.as_deref())
                    .await?;

                let worker = if worker_data.use_static {
                    WorkerForEnqueue::Existing(self.find_or_create_worker(worker_data).await?)
                } else {
                    WorkerForEnqueue::Temp(worker_data)
                };
                let (_jid, res, _stream) = self
                    .enqueue_with_worker_or_temp(
                        metadata,
                        worker,
                        job_args,
                        uniq_key,
                        job_timeout_sec,
                        StreamingType::None, // no streaming
                        using.clone(),
                        None,
                    )
                    .await?;
                let output = res
                    .map(|r| self.extract_job_result_output(r))
                    .ok_or(anyhow::anyhow!(
                        "Failed to enqueue job or job result not found"
                    ))
                    .and_then(|output| output)?;
                self.transform_raw_output(&rid, &rdata, &output, using.as_deref())
                    .await
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
        streaming_type: StreamingType, // streaming type for job
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
                    let job_args = self
                        .transform_job_args(&rid, &rdata, job_args, using.as_deref())
                        .await?;
                    self.enqueue_with_worker_or_temp(
                        metadata,
                        WorkerForEnqueue::existing(wid, worker_data),
                        job_args,
                        uniq_key,
                        job_timeout_sec,
                        streaming_type,
                        using, // Pass using parameter for MCP/Plugin workers
                        None,
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

    /// Look up a worker by name, resolve its runner, and transform the JSON
    /// `job_args` into the protobuf binary the runner expects. Shared front
    /// stage of the `enqueue_with_worker_name_*` family so the worker/runner
    /// resolution lives in one place.
    fn resolve_worker_and_transform_args(
        &self,
        worker_name: &str,
        job_args: &serde_json::Value,
        using: Option<&str>,
    ) -> impl std::future::Future<
        Output = Result<(WorkerId, WorkerData, RunnerId, RunnerData, Vec<u8>)>,
    > + Send {
        async move {
            let worker = self
                .worker_app()
                .find_by_name(worker_name)
                .await?
                .ok_or_else(|| {
                    JobWorkerError::WorkerNotFound(format!("Not found worker: {worker_name}"))
                })?;
            let Worker {
                id: Some(wid),
                data: Some(worker_data),
            } = worker
            else {
                return Err(anyhow::anyhow!("Not found worker: {}", worker_name));
            };
            let runner_id = worker_data.runner_id.as_ref().ok_or_else(|| {
                JobWorkerError::NotFound(format!(
                    "Not found runner for worker {}: {:?}",
                    worker_name, worker_data.runner_id
                ))
            })?;
            let Some(RunnerWithSchema {
                id: Some(rid),
                data: Some(rdata),
                ..
            }) = self.runner_app().find_runner(runner_id).await?
            else {
                return Err(anyhow::anyhow!(
                    "Not found runner: {:?}",
                    worker_data.runner_id
                ));
            };
            let args = self
                .transform_job_args(&rid, &rdata, job_args, using)
                .await?;
            Ok((wid, worker_data, rid, rdata, args))
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn enqueue_with_worker_name_channel(
        &self,
        metadata: Arc<HashMap<String, String>>, // metadata for job
        worker_name: &str,                      // worker name to look up
        job_args: &serde_json::Value,           // enqueue job args
        uniq_key: Option<String>, // unique key for job (if not exists, use default values)
        job_timeout_sec: u32,     // job timeout in seconds
        streaming_type: StreamingType, // streaming type for job
        using: Option<String>,    // using parameter for MCP/Plugin runners
        // Per-job overrides (e.g. response_type) for the named (often static) worker.
        overrides: Option<JobExecutionOverrides>,
    ) -> impl std::future::Future<Output = Result<NamedWorkerChannelJob>> + Send {
        async move {
            let (wid, worker_data, rid, rdata, job_args) = self
                .resolve_worker_and_transform_args(worker_name, job_args, using.as_deref())
                .await?;
            let (job_id, result_fut) = self
                .enqueue_with_worker_or_temp_channel(
                    metadata,
                    WorkerForEnqueue::existing(wid, worker_data),
                    job_args,
                    uniq_key,
                    job_timeout_sec,
                    streaming_type,
                    using.clone(), // Pass using parameter for MCP/Plugin workers
                    overrides,
                )
                .await?;
            Ok(NamedWorkerChannelJob {
                job_id,
                result_fut,
                runner_id: rid,
                runner_data: rdata,
                using,
            })
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn enqueue_with_worker_name_and_output_json(
        &self,
        metadata: Arc<HashMap<String, String>>, // metadata for job
        worker_name: &str,                      // worker name to look up
        job_args: &serde_json::Value,           // enqueue job args
        uniq_key: Option<String>, // unique key for job (if not exists, use default values)
        job_timeout_sec: u32,     // job timeout in seconds
        streaming_type: StreamingType, // streaming type for job
        using: Option<String>,    // using parameter for MCP/Plugin runners
    ) -> impl std::future::Future<Output = Result<serde_json::Value>> + Send {
        async move {
            let (wid, worker_data, rid, rdata, job_args) = self
                .resolve_worker_and_transform_args(worker_name, job_args, using.as_deref())
                .await?;
            let res = self
                .enqueue_with_worker_or_temp(
                    metadata,
                    WorkerForEnqueue::existing(wid, worker_data),
                    job_args,
                    uniq_key,
                    job_timeout_sec,
                    streaming_type,
                    using.clone(), // Pass using parameter for MCP/Plugin workers
                    None,
                )
                .await?;
            let Some(res) = res.1 else {
                return Err(anyhow::anyhow!(
                    "Failed to enqueue job or job result not found"
                ));
            };
            let output = self.extract_job_result_output(res)?;
            self.transform_raw_output(&rid, &rdata, output.as_slice(), using.as_deref())
                .await
        }
    }

    /// Transform job arguments from JSON to Protobuf binary.
    /// Wraps flat args for Workflow/ReusableWorkflow into 'input' field (idempotent — safe to call
    /// even if upstream already wrapped, guarded by `contains_key("input")` check).
    fn transform_job_args(
        &self,
        rid: &RunnerId,
        rdata: &RunnerData,
        job_args: &serde_json::Value, // enqueue job args
        using: Option<&str>,          // method name for MCP/Plugin runners
    ) -> impl std::future::Future<Output = Result<Vec<u8>>> + Send {
        async move {
            let job_args = crate::app::function::wrap_workflow_args_if_needed(
                rdata.runner_type(),
                using,
                job_args.clone(),
            );

            let descriptors = self.parse_proto_with_cache(rid, rdata).await?;

            let mut args_descriptor =
                descriptors
                    .get_job_args_message_for_method(using)
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "Failed to get args descriptor for method '{}': {:#?}",
                            using.unwrap_or(DEFAULT_METHOD_NAME),
                            e
                        )
                    })?;

            // Fallback: parse the descriptor directly from method_proto_map when cache lacks it
            if args_descriptor.is_none() {
                let method_name = using.unwrap_or(DEFAULT_METHOD_NAME);
                args_descriptor = Self::parse_job_args_schema_descriptor(rdata, method_name)?;
            }

            tracing::debug!(
                "job args (using: {:?}): {:#?}",
                using,
                redact_sensitive_args(&job_args)
            );
            if let Some(desc) = args_descriptor {
                Ok(
                    ProtobufDescriptor::json_value_to_message(desc, &job_args, true, true)
                        .map_err(|e| {
                            anyhow::anyhow!("Failed to parse job_args schema: {:#?}", e)
                        })?,
                )
            } else {
                // Fallback: serialize as JSON string
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

    /// Transform raw job result output from Protobuf binary to JSON
    /// Transform job result using protobuf schema, supporting method-specific schema resolution
    #[inline]
    fn transform_raw_output(
        &self,
        rid: &RunnerId,
        rdata: &RunnerData,
        output: &[u8],       // raw job output
        using: Option<&str>, // method name for MCP/Plugin runners
    ) -> impl std::future::Future<Output = Result<serde_json::Value>> + Send {
        async move {
            // with cache
            let descriptors = self.parse_proto_with_cache(rid, rdata).await?;

            let mut result_descriptor = descriptors
                .get_job_result_message_descriptor_for_method(using)
                .map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to get result descriptor for method '{}': {:#?}",
                        using.unwrap_or(DEFAULT_METHOD_NAME),
                        e
                    )
                })?;

            // Fallback: parse the descriptor directly from method_proto_map when cache lacks it
            if result_descriptor.is_none() {
                let method_name = using.unwrap_or(DEFAULT_METHOD_NAME);
                result_descriptor = Self::parse_job_result_schema_descriptor(rdata, method_name)?;
            }

            tracing::debug!("job output length (using: {:?}): {}", using, output.len());
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
    #[inline]
    fn decode_job_result_output(
        &self,
        runner_id: Option<&RunnerId>, // runner(runner) id
        runner_name: Option<&str>,    // runner(runner) name
        output: &[u8],                // enqueue job args
        using: Option<&str>,          // method name for MCP/Plugin runners
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
                self.transform_raw_output(&rid, &rdata, output, using).await
            } else {
                Err(anyhow::anyhow!("Not found runner for job result output"))
            }
        }
    }
}

fn worker_definition_matches_expected(existing: &WorkerData, expected: &WorkerData) -> bool {
    existing.runner_id == expected.runner_id
        && existing.runner_settings == expected.runner_settings
        && existing.use_static == expected.use_static
}

fn worker_data_with_expected_definition(
    mut existing: WorkerData,
    expected: WorkerData,
) -> WorkerData {
    existing.runner_id = expected.runner_id;
    existing.runner_settings = expected.runner_settings;
    existing.use_static = expected.use_static;
    existing
}

/// Placeholder substituted for redacted sensitive values in logs.
const REDACTED: &str = "***REDACTED***";

/// Header names whose values carry credentials and must never be logged.
/// Matched case-insensitively against the `key` of `{key, value}` pairs (the
/// shape the HTTP_REQUEST runner uses for headers/queries). This is a generic
/// HTTP-credential rule, not tied to any single runner type.
const SENSITIVE_HEADER_KEYS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
];

/// Object field names whose string value carries a credential and must never be
/// logged. Matched case-insensitively against object keys (the flat shape used
/// by runner settings, e.g. the GRPC runner's `auth_token` resolved from a
/// `WORKFLOW_SECRET_*`). Only string values are masked so that structural keys
/// of the same name are left intact.
const SENSITIVE_FLAT_KEYS: &[&str] = &[
    "auth_token",
    "token",
    "access_token",
    "refresh_token",
    "password",
    "secret",
    "api_key",
    "apikey",
];

/// Return a copy of `args` with credential-bearing values masked, for safe
/// logging. Two shapes are redacted: `{key, value}` pairs whose `key` is a
/// sensitive header name (e.g. a transformed `call.http`'s `Authorization`
/// header derived from a `WORKFLOW_SECRET_*`), and flat object fields whose key
/// is a sensitive credential name with a string value (e.g. the GRPC runner's
/// `auth_token`). All other content is preserved so the log stays useful.
/// Non-sensitive job args incur a clone only when debug logging is on.
fn redact_sensitive_args(args: &serde_json::Value) -> serde_json::Value {
    match args {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(k, v)| {
                    // Mask a flat credential field (string value only, so that a
                    // structural key of the same name is not corrupted).
                    if matches!(v, serde_json::Value::String(_))
                        && SENSITIVE_FLAT_KEYS
                            .iter()
                            .any(|s| k.eq_ignore_ascii_case(s))
                    {
                        (k.clone(), serde_json::Value::String(REDACTED.to_string()))
                    } else {
                        (k.clone(), redact_sensitive_args(v))
                    }
                })
                .collect(),
        ),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(redact_key_value_or_recurse).collect())
        }
        other => other.clone(),
    }
}

/// For array elements: if the element is a `{key, value}` pair with a sensitive
/// `key`, replace its `value` with the placeholder; otherwise recurse.
fn redact_key_value_or_recurse(item: &serde_json::Value) -> serde_json::Value {
    if let serde_json::Value::Object(obj) = item
        && let Some(serde_json::Value::String(key)) = obj.get("key")
        && obj.contains_key("value")
        && SENSITIVE_HEADER_KEYS
            .iter()
            .any(|s| key.eq_ignore_ascii_case(s))
    {
        let mut redacted = obj.clone();
        redacted.insert(
            "value".to_string(),
            serde_json::Value::String(REDACTED.to_string()),
        );
        return serde_json::Value::Object(redacted);
    }
    redact_sensitive_args(item)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::test::create_rdb_chan_test_app;
    use infra_utils::infra::test::TEST_RUNTIME;
    use proto::jobworkerp::data::{JobProcessingStatus, QueueType, ResponseType};
    use std::time::Duration;

    const COMMAND_RUNNER_ID: i64 = 1;
    const HTTP_REQUEST_RUNNER_ID: i64 = 2;

    #[test]
    fn test_enqueue_with_worker_name_channel_returns_job_id_before_result() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_rdb_chan_test_app(false, false).await?);
            let worker_name = "named-worker-channel-test-command";
            app_module
                .worker_app
                .create(&WorkerData {
                    name: worker_name.to_string(),
                    description: "named worker channel enqueue test".to_string(),
                    runner_id: Some(RunnerId {
                        value: COMMAND_RUNNER_ID,
                    }),
                    runner_settings: Vec::new(),
                    periodic_interval: 0,
                    channel: None,
                    queue_type: QueueType::Normal as i32,
                    response_type: ResponseType::Direct as i32,
                    store_success: false,
                    store_failure: true,
                    use_static: false,
                    retry_policy: None,
                    broadcast_results: true,
                })
                .await?;

            let wrapper = JobExecutorWrapper::new(app_module.clone());
            let child = tokio::time::timeout(
                Duration::from_secs(1),
                wrapper.enqueue_with_worker_name_channel(
                    Arc::new(HashMap::new()),
                    worker_name,
                    &serde_json::json!({
                        "command": "sleep",
                        "args": ["30"]
                    }),
                    None,
                    30,
                    StreamingType::None,
                    None,
                    None,
                ),
            )
            .await
            .expect("channel enqueue must return before the Direct result is ready")?;

            assert!(
                child.job_id.value > 0,
                "channel enqueue should expose the child job id immediately"
            );
            assert_eq!(
                app_module.job_app.find_job_status(&child.job_id).await?,
                Some(JobProcessingStatus::Pending)
            );

            app_module.job_app.delete_job(&child.job_id).await?;
            Ok(())
        })
    }

    #[test]
    fn find_or_create_worker_updates_existing_static_worker_definition() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_rdb_chan_test_app(false, false).await?);
            let wrapper = JobExecutorWrapper::new(app_module.clone());
            let worker_name = "find-or-create-stale-static-worker";
            let original_settings = b"old-settings".to_vec();
            let expected_settings = Vec::new();

            let original_id = app_module
                .worker_app
                .create(&WorkerData {
                    name: worker_name.to_string(),
                    description: "stale static worker".to_string(),
                    runner_id: Some(RunnerId {
                        value: COMMAND_RUNNER_ID,
                    }),
                    runner_settings: original_settings,
                    use_static: true,
                    ..Default::default()
                })
                .await?;

            let worker = wrapper
                .find_or_create_worker(WorkerData {
                    name: worker_name.to_string(),
                    description: "refreshed static worker".to_string(),
                    runner_id: Some(RunnerId {
                        value: HTTP_REQUEST_RUNNER_ID,
                    }),
                    runner_settings: expected_settings.clone(),
                    use_static: true,
                    ..Default::default()
                })
                .await?;

            assert_eq!(worker.id, Some(original_id));
            let data = worker.data.expect("worker data should be returned");
            assert_eq!(
                data.runner_id,
                Some(RunnerId {
                    value: HTTP_REQUEST_RUNNER_ID
                })
            );
            assert_eq!(data.runner_settings, expected_settings);
            assert_eq!(data.description, "stale static worker");

            app_module.worker_app.delete(&original_id).await?;
            Ok(())
        })
    }

    #[test]
    fn find_or_create_worker_updates_existing_static_worker_settings() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_rdb_chan_test_app(false, false).await?);
            let wrapper = JobExecutorWrapper::new(app_module.clone());
            let worker_name = "find-or-create-stale-static-worker-settings";
            let expected_settings = b"updated-command-settings".to_vec();

            let original_id = app_module
                .worker_app
                .create(&WorkerData {
                    name: worker_name.to_string(),
                    description: "stale static worker settings".to_string(),
                    runner_id: Some(RunnerId {
                        value: COMMAND_RUNNER_ID,
                    }),
                    runner_settings: b"old-command-settings".to_vec(),
                    use_static: true,
                    ..Default::default()
                })
                .await?;

            let worker = wrapper
                .find_or_create_worker(WorkerData {
                    name: worker_name.to_string(),
                    description: "refreshed static worker settings".to_string(),
                    runner_id: Some(RunnerId {
                        value: COMMAND_RUNNER_ID,
                    }),
                    runner_settings: expected_settings.clone(),
                    use_static: true,
                    ..Default::default()
                })
                .await?;

            assert_eq!(worker.id, Some(original_id));
            let data = worker.data.expect("worker data should be returned");
            assert_eq!(data.runner_settings, expected_settings);
            assert_eq!(data.description, "stale static worker settings");

            app_module.worker_app.delete(&original_id).await?;
            Ok(())
        })
    }

    #[test]
    fn find_or_create_worker_keeps_matching_static_worker_unchanged() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_rdb_chan_test_app(false, false).await?);
            let wrapper = JobExecutorWrapper::new(app_module.clone());
            let worker_name = "find-or-create-static-worker-noop";
            let runner_settings = b"operator-owned-settings".to_vec();

            let original_id = app_module
                .worker_app
                .create(&WorkerData {
                    name: worker_name.to_string(),
                    description: "operator maintained description".to_string(),
                    runner_id: Some(RunnerId {
                        value: COMMAND_RUNNER_ID,
                    }),
                    runner_settings: runner_settings.clone(),
                    channel: Some("operator-channel".to_string()),
                    queue_type: QueueType::WithBackup as i32,
                    response_type: ResponseType::NoResult as i32,
                    store_success: true,
                    store_failure: false,
                    use_static: true,
                    broadcast_results: false,
                    ..Default::default()
                })
                .await?;

            let worker = wrapper
                .find_or_create_worker(WorkerData {
                    name: worker_name.to_string(),
                    description: String::new(),
                    runner_id: Some(RunnerId {
                        value: COMMAND_RUNNER_ID,
                    }),
                    runner_settings: runner_settings.clone(),
                    channel: None,
                    queue_type: QueueType::Normal as i32,
                    response_type: ResponseType::Direct as i32,
                    store_success: false,
                    store_failure: true,
                    use_static: true,
                    broadcast_results: true,
                    ..Default::default()
                })
                .await?;

            assert_eq!(worker.id, Some(original_id));
            let data = worker.data.expect("worker data should be returned");
            assert_eq!(data.description, "operator maintained description");
            assert_eq!(data.channel.as_deref(), Some("operator-channel"));
            assert_eq!(data.queue_type, QueueType::WithBackup as i32);
            assert_eq!(data.response_type, ResponseType::NoResult as i32);
            assert!(data.store_success);
            assert!(!data.store_failure);
            assert!(!data.broadcast_results);

            let stored = app_module
                .worker_app
                .find(&original_id)
                .await?
                .and_then(|worker| worker.data)
                .expect("stored worker should exist");
            assert_eq!(stored.description, "operator maintained description");
            assert_eq!(stored.channel.as_deref(), Some("operator-channel"));
            assert_eq!(stored.queue_type, QueueType::WithBackup as i32);
            assert_eq!(stored.response_type, ResponseType::NoResult as i32);
            assert!(stored.store_success);
            assert!(!stored.store_failure);
            assert!(!stored.broadcast_results);

            app_module.worker_app.delete(&original_id).await?;
            Ok(())
        })
    }

    #[test]
    fn find_or_create_worker_updates_runner_definition_without_overwriting_operator_fields()
    -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_rdb_chan_test_app(false, false).await?);
            let wrapper = JobExecutorWrapper::new(app_module.clone());
            let worker_name = "find-or-create-static-worker-preserve-custom";
            let expected_settings = Vec::new();

            let original_id = app_module
                .worker_app
                .create(&WorkerData {
                    name: worker_name.to_string(),
                    description: "operator maintained description".to_string(),
                    runner_id: Some(RunnerId {
                        value: COMMAND_RUNNER_ID,
                    }),
                    runner_settings: b"old-runner-settings".to_vec(),
                    channel: Some("operator-channel".to_string()),
                    queue_type: QueueType::WithBackup as i32,
                    response_type: ResponseType::NoResult as i32,
                    store_success: true,
                    store_failure: false,
                    use_static: true,
                    broadcast_results: false,
                    ..Default::default()
                })
                .await?;

            let worker = wrapper
                .find_or_create_worker(WorkerData {
                    name: worker_name.to_string(),
                    description: String::new(),
                    runner_id: Some(RunnerId {
                        value: HTTP_REQUEST_RUNNER_ID,
                    }),
                    runner_settings: expected_settings.clone(),
                    channel: None,
                    queue_type: QueueType::Normal as i32,
                    response_type: ResponseType::Direct as i32,
                    store_success: false,
                    store_failure: true,
                    use_static: true,
                    broadcast_results: true,
                    ..Default::default()
                })
                .await?;

            assert_eq!(worker.id, Some(original_id));
            let data = worker.data.expect("worker data should be returned");
            assert_eq!(
                data.runner_id,
                Some(RunnerId {
                    value: HTTP_REQUEST_RUNNER_ID
                })
            );
            assert_eq!(data.runner_settings, expected_settings);
            assert_eq!(data.description, "operator maintained description");
            assert_eq!(data.channel.as_deref(), Some("operator-channel"));
            assert_eq!(data.queue_type, QueueType::WithBackup as i32);
            assert_eq!(data.response_type, ResponseType::NoResult as i32);
            assert!(data.store_success);
            assert!(!data.store_failure);
            assert!(!data.broadcast_results);

            app_module.worker_app.delete(&original_id).await?;
            Ok(())
        })
    }

    #[test]
    fn find_or_create_worker_keeps_none_channel_as_default() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_rdb_chan_test_app(false, false).await?);
            let wrapper = JobExecutorWrapper::new(app_module.clone());
            let worker_name = "find-or-create-static-worker-default-channel";

            let worker = wrapper
                .find_or_create_worker(WorkerData {
                    name: worker_name.to_string(),
                    description: "static worker with default channel".to_string(),
                    runner_id: Some(RunnerId {
                        value: COMMAND_RUNNER_ID,
                    }),
                    use_static: true,
                    channel: None,
                    ..Default::default()
                })
                .await?;

            let worker_id = worker.id.expect("worker id should be returned");
            // The domain represents the default channel as `None`; the executor no
            // longer fills in the default name (that is the storage layer's concern).
            let data = worker.data.expect("worker data should be returned");
            assert_eq!(data.channel, None);

            // The persisted worker also reads back as `None` (default) through the
            // storage read boundary.
            let stored = app_module
                .worker_app
                .find(&worker_id)
                .await?
                .and_then(|w| w.data)
                .expect("stored worker should exist");
            assert_eq!(stored.channel, None);

            app_module.worker_app.delete(&worker_id).await?;
            Ok(())
        })
    }

    #[test]
    fn enqueue_job_with_channel_rejects_worker_without_id_or_data() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_rdb_chan_test_app(false, false).await?);
            let valid_data = WorkerData {
                name: "malformed-worker-for-channel".to_string(),
                runner_id: Some(RunnerId {
                    value: COMMAND_RUNNER_ID,
                }),
                ..Default::default()
            };

            let missing_id = app_module
                .job_app
                .enqueue_job_with_channel(
                    Arc::new(HashMap::new()),
                    Worker {
                        id: None,
                        data: Some(valid_data),
                    },
                    Vec::new(),
                    None,
                    0,
                    Priority::Medium as i32,
                    1000,
                    None,
                    StreamingType::None,
                    None,
                    None,
                )
                .await;
            assert_worker_missing_data_error(missing_id);

            let missing_data = app_module
                .job_app
                .enqueue_job_with_channel(
                    Arc::new(HashMap::new()),
                    Worker {
                        id: Some(WorkerId { value: 1 }),
                        data: None,
                    },
                    Vec::new(),
                    None,
                    0,
                    Priority::Medium as i32,
                    1000,
                    None,
                    StreamingType::None,
                    None,
                    None,
                )
                .await;
            assert_worker_missing_data_error(missing_data);

            Ok(())
        })
    }

    #[test]
    fn setup_worker_and_enqueue_with_json_uses_temp_worker_for_non_static() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_rdb_chan_test_app(false, false).await?);
            let wrapper = JobExecutorWrapper::new(app_module.clone());

            let (job_id, result, stream) = wrapper
                .setup_worker_and_enqueue_with_json_full_output(
                    Arc::new(HashMap::new()),
                    "COMMAND",
                    WorkerData {
                        name: "non-static-temp-worker-enqueue".to_string(),
                        runner_id: Some(RunnerId {
                            value: COMMAND_RUNNER_ID,
                        }),
                        queue_type: QueueType::Normal as i32,
                        response_type: ResponseType::NoResult as i32,
                        store_success: false,
                        store_failure: true,
                        use_static: false,
                        ..Default::default()
                    },
                    serde_json::json!({
                        "command": "echo",
                        "args": ["temp-worker"]
                    }),
                    None,
                    5,
                    StreamingType::None,
                    None,
                )
                .await?;

            assert!(job_id.value > 0);
            assert!(result.is_none());
            assert!(stream.is_none());

            app_module.job_app.delete_job(&job_id).await?;
            Ok(())
        })
    }

    fn assert_worker_missing_data_error(result: Result<(JobId, ChannelJobResultFuture)>) {
        let Some(err) = result.err() else {
            panic!("malformed worker must fail");
        };
        match err.downcast_ref::<JobWorkerError>() {
            Some(JobWorkerError::WorkerNotFound(message)) => {
                assert_eq!(message, "worker id or data is missing");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn redact_masks_authorization_header_value() {
        let args = serde_json::json!({
            "method": "GET",
            "headers": [
                {"key": "Authorization", "value": "Bearer s3cret-token"},
                {"key": "Content-Type", "value": "application/json"},
            ],
        });
        let redacted = redact_sensitive_args(&args);
        let headers = redacted["headers"].as_array().unwrap();
        let auth = headers
            .iter()
            .find(|h| h["key"] == "Authorization")
            .unwrap();
        assert_eq!(auth["value"], REDACTED);
        // The secret must not survive anywhere in the redacted output.
        assert!(!redacted.to_string().contains("s3cret-token"));
        // Non-sensitive headers are preserved verbatim.
        let ct = headers.iter().find(|h| h["key"] == "Content-Type").unwrap();
        assert_eq!(ct["value"], "application/json");
    }

    #[test]
    fn redact_matches_header_key_case_insensitively() {
        let args = serde_json::json!({
            "headers": [{"key": "authorization", "value": "Basic dXNlcjpwYXNz"}],
        });
        let redacted = redact_sensitive_args(&args);
        assert_eq!(redacted["headers"][0]["value"], REDACTED);
        assert!(!redacted.to_string().contains("dXNlcjpwYXNz"));
    }

    #[test]
    fn redact_preserves_non_sensitive_args() {
        // A plain value resembling a header key but not in {key,value} form must
        // be left untouched.
        let args = serde_json::json!({
            "queries": [{"key": "q", "value": "authorization"}],
            "body": "authorization details",
        });
        let redacted = redact_sensitive_args(&args);
        assert_eq!(redacted, args);
    }

    #[test]
    fn redact_handles_nested_structures() {
        let args = serde_json::json!({
            "outer": {
                "headers": [{"key": "Cookie", "value": "session=abc"}],
            },
        });
        let redacted = redact_sensitive_args(&args);
        assert_eq!(redacted["outer"]["headers"][0]["value"], REDACTED);
        assert!(!redacted.to_string().contains("session=abc"));
    }

    #[test]
    fn redact_masks_flat_auth_token_in_runner_settings() {
        // Shape of GRPC runner settings with a bearer secret resolved into a flat
        // `auth_token` field.
        let settings = serde_json::json!({
            "host": "example.com",
            "port": 50051,
            "use_reflection": true,
            "auth_token": "Bearer-or-bare-s3cret",
        });
        let redacted = redact_sensitive_args(&settings);
        assert_eq!(redacted["auth_token"], REDACTED);
        assert!(!redacted.to_string().contains("s3cret"));
        // Non-sensitive fields are preserved.
        assert_eq!(redacted["host"], "example.com");
        assert_eq!(redacted["port"], 50051);
        assert_eq!(redacted["use_reflection"], true);
    }

    #[test]
    fn redact_masks_flat_credential_keys_case_insensitively() {
        let settings = serde_json::json!({
            "Password": "p@ss",
            "API_KEY": "ak-123",
            "nested": {"access_token": "at-456"},
        });
        let redacted = redact_sensitive_args(&settings);
        assert_eq!(redacted["Password"], REDACTED);
        assert_eq!(redacted["API_KEY"], REDACTED);
        assert_eq!(redacted["nested"]["access_token"], REDACTED);
        for leaked in ["p@ss", "ak-123", "at-456"] {
            assert!(!redacted.to_string().contains(leaked));
        }
    }

    #[test]
    fn redact_does_not_corrupt_non_string_credential_keys() {
        // A key named like a credential but holding structural (non-string) data
        // must be recursed into, not blindly masked, so its contents remain
        // inspectable while any nested credential strings are still masked.
        let args = serde_json::json!({
            "token": {"id": 7, "value": "nested-secret", "auth_token": "deep-s3cret"},
        });
        let redacted = redact_sensitive_args(&args);
        // The object value is preserved as an object (not replaced by a string).
        assert!(redacted["token"].is_object());
        assert_eq!(redacted["token"]["id"], 7);
        // A nested flat credential field is still masked.
        assert_eq!(redacted["token"]["auth_token"], REDACTED);
        assert!(!redacted.to_string().contains("deep-s3cret"));
    }
}
