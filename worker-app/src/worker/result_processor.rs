use anyhow::Result;
use app::app::job::JobApp;
use app::app::job::UseJobApp;
use app::app::job_result::JobResultApp;
use app::app::job_result::UseJobResultApp;
use app::app::runner::RunnerApp;
use app::app::runner::UseRunnerApp;
use app::app::worker::UseWorkerApp;
use app::app::worker::WorkerApp;
use app::app::JobBuilder;
use app::app::StorageConfig;
use app::app::UseStorageConfig;
use app::app::UseWorkerConfig;
use app::app::WorkerConfig;
use app::module::AppConfigModule;
use app::module::AppModule;
use debug_stub_derive::DebugStub;
use futures::stream::BoxStream;
use infra::infra::job::rows::UseJobqueueAndCodec;
use infra_utils::infra::trace::Tracing;
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::JobResultData;
use proto::jobworkerp::data::JobResultId;
use proto::jobworkerp::data::ResultOutputItem;
use proto::jobworkerp::data::{JobResult, WorkerData};
use std::collections::HashMap;
use std::sync::Arc;
use tracing;

#[derive(DebugStub, Clone)]
pub struct ResultProcessorImpl {
    pub config_module: Arc<AppConfigModule>,
    #[debug_stub = "AppModule"]
    pub app_module: Arc<AppModule>,
}

impl Tracing for ResultProcessorImpl {}
impl ResultProcessorImpl {
    pub fn new(
        config_module: Arc<AppConfigModule>,
        app_module: Arc<AppModule>,
    ) -> ResultProcessorImpl {
        // for shutdown notification (spmc broadcast)::<()>
        Self {
            config_module,
            app_module,
        }
    }

    pub async fn process_result(
        &self,
        jr: JobResult,
        st_data: Option<BoxStream<'static, ResultOutputItem>>,
        w: WorkerData,
    ) -> Result<JobResult> {
        tracing::debug!("got job_result: {:?}, worker: {:?}", &jr.id, &w.name);
        if let JobResult {
            id: Some(id),
            data: Some(data),
            metadata,
        } = jr
        {
            // retry first if necessary
            let retried = self
                .process_complete_or_retry_condition(&id, &data, st_data, &w, &metadata)
                .await;
            // store result if necessary by result status and worker setting
            match self
                .job_result_app()
                .create_job_result_if_necessary(&id, &data, w.broadcast_results)
                .await
            {
                Ok(_r) => {
                    retried?; // error if retried err
                    Ok(JobResult {
                        id: Some(id),
                        data: Some(data),
                        metadata,
                    })
                }
                Err(e) => {
                    tracing::error!("job result store error: {:?}, retried: {:?}", e, retried);
                    Err(e)
                }
            }
        } else {
            tracing::warn!("job result without id or data: {:?}", jr);
            Err(JobWorkerError::NotFound("job result without id or data".to_string()).into())
        }
    }

    async fn process_complete_or_retry_condition(
        &self,
        id: &JobResultId,
        dat: &JobResultData,
        stream: Option<BoxStream<'static, ResultOutputItem>>,
        worker: &WorkerData,
        metadata: &HashMap<String, String>,
    ) -> Result<()> {
        // retry or periodic job
        let jopt = Self::build_retry_job(dat, worker, metadata);
        // need to retry
        if let Some(j) = jopt {
            // update or insert job for retry or periodic
            tracing::debug!("need to retry worker: {:?}, job: {:?}", &worker.name, &j);
            self.job_app().update_job(&j).await?;
            Ok(())
        } else {
            // complete job (delete first for unique key)
            let res = self
                .job_app()
                .complete_job(id, dat, stream)
                .await
                .map(|_| ());
            // the job finished
            // if finished periodic job, enqueue next periodic job
            if let Some(pj) = Self::build_next_periodic_job(dat, worker) {
                let pjres = self
                    .job_app()
                    .enqueue_job(
                        Arc::new(metadata.clone()),
                        pj.worker_id.as_ref(),
                        None,
                        pj.args,
                        pj.uniq_key,
                        pj.run_after_time,
                        pj.priority,
                        pj.timeout,
                        dat.job_id, // use same job id for periodic job if possible
                        pj.request_streaming,
                    )
                    .await?;
                tracing::info!(
                    "Next periodic job id: {:?}, worker id:{:?}",
                    pjres.0,
                    &pj.worker_id
                );
            };
            res
        }
    }
}

impl UseJobqueueAndCodec for ResultProcessorImpl {}
impl UseJobResultApp for ResultProcessorImpl {
    fn job_result_app(&self) -> &Arc<dyn JobResultApp + 'static> {
        &self.app_module.job_result_app
    }
}
impl UseJobApp for ResultProcessorImpl {
    fn job_app(&self) -> &Arc<dyn JobApp + 'static> {
        &self.app_module.job_app
    }
}
impl UseWorkerApp for ResultProcessorImpl {
    fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static> {
        &self.app_module.worker_app
    }
}
impl UseRunnerApp for ResultProcessorImpl {
    fn runner_app(&self) -> Arc<dyn RunnerApp + 'static> {
        self.app_module.runner_app.clone()
    }
}
impl JobBuilder for ResultProcessorImpl {}

impl UseWorkerConfig for ResultProcessorImpl {
    fn worker_config(&self) -> &WorkerConfig {
        &self.config_module.worker_config
    }
}
impl UseStorageConfig for ResultProcessorImpl {
    fn storage_config(&self) -> &StorageConfig {
        &self.config_module.storage_config
    }
}
pub trait UseResultProcessor {
    fn result_processor(&self) -> &ResultProcessorImpl;
}

//impl UseIdGenerator for ResultProcessorImpl {
//    fn id_generator(&self) -> &IdGeneratorWrapper {
//        &self.id_generator
//    }
//}
