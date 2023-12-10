// TODO remove
use anyhow::Result;
use app::app::job::JobApp;
use app::app::job::UseJobApp;
use app::app::job_result::JobResultApp;
use app::app::job_result::UseJobResultApp;
use app::app::worker::builtin::BuiltinWorkerIds;
use app::app::worker::UseWorkerApp;
use app::app::worker::WorkerApp;
use app::app::JobBuilder;
use app::app::StorageConfig;
use app::app::UseStorageConfig;
use app::app::UseWorkerConfig;
use app::app::WorkerConfig;
use app::module::AppConfigModule;
use app::module::AppModule;
use common::util::option::Exists;
use debug_stub_derive::DebugStub;
use infra::infra::job::rows::UseJobqueueAndCodec;
use prost::Message;
use proto::jobworkerp::data::JobResultData;
use proto::jobworkerp::data::JobResultId;
use proto::jobworkerp::data::ResultStatus;
use proto::jobworkerp::data::RunnerType;
use proto::jobworkerp::data::{JobResult, WorkerData};
use std::sync::Arc;
use strum::IntoEnumIterator;
use tracing;

#[derive(DebugStub, Clone)]
pub struct ResultProcessorImpl {
    pub config_module: Arc<AppConfigModule>,
    #[debug_stub = "AppModule"]
    pub app_module: Arc<AppModule>,
}

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
        id: JobResultId,
        data: JobResultData,
        w: WorkerData,
    ) -> Result<JobResult> {
        tracing::debug!("got job_result: {:?}, worker: {:?}", &id, &w.name);
        // retry first if necessary
        let retried = self
            .process_complete_or_retry_condition(&id, &data, &w)
            .await;
        // store result if necessary by result status and worker setting
        match self
            .job_result_app()
            .create_job_result_if_necessary(&id, &data)
            .await
        {
            Ok(_r) => {
                retried?; // error if retried err
                Ok(JobResult {
                    id: Some(id),
                    data: Some(data),
                })
            }
            Err(e) => {
                tracing::error!("job result store error: {:?}, retried: {:?}", e, retried);
                Err(e)
            }
        }
    }

    async fn process_complete_or_retry_condition(
        &self,
        id: &JobResultId,
        dat: &JobResultData,
        worker: &WorkerData,
    ) -> Result<()> {
        // retry or periodic job
        let jopt = Self::build_retry_job(dat, worker);
        // need to retry
        if let Some(j) = jopt {
            // update or insert job for retry or periodic
            tracing::debug!("need to retry worker: {:?}, job: {:?}", &worker.name, &j);
            self.job_app().update_job(&j).await?;
            Ok(())
        } else {
            // the job finished
            // enqueue next worker jobs
            self.enqueue_next_worker_jobs(id, dat, worker).await?;
            // if finished periodic job, enqueue next periodic job
            if let Some(pj) = Self::build_next_periodic_job(dat, worker) {
                let pjres = self
                    .job_app()
                    .enqueue_job(
                        pj.worker_id.as_ref(),
                        None,
                        pj.arg,
                        pj.uniq_key,
                        pj.run_after_time,
                        pj.priority,
                        pj.timeout,
                    )
                    .await?;
                tracing::info!(
                    "Next periodic job id: {:?}, worker id:{:?}",
                    pjres.0,
                    &pj.worker_id
                );
            };
            self.job_app().complete_job(id, dat).await.map(|_| ())
        }
    }
    //TODO test
    async fn enqueue_next_worker_jobs(
        &self,
        id: &JobResultId,
        dat: &JobResultData,
        worker: &WorkerData,
    ) -> Result<()> {
        if dat.status == ResultStatus::Success as i32 && !worker.next_workers.is_empty() {
            tracing::debug!(
                "enqueue next worker: {:?}, from worker: {}, job_result: {:?}",
                &worker.next_workers,
                &worker.name,
                &id
            );
            for wid in worker.next_workers.iter() {
                if let Ok(Some(w)) = self.worker_app().find(wid).await {
                    // builtin worker
                    if w.data
                        .as_ref()
                        .exists(|wd| wd.r#type == RunnerType::Builtin as i32)
                        && BuiltinWorkerIds::iter()
                            .any(|i| w.id.as_ref().exists(|wid| wid.value == i as i64))
                    {
                        // no result data, no enqueue
                        if dat.output.is_none()
                            || dat.output.as_ref().exists(|o| o.items.is_empty())
                        {
                            tracing::debug!(
                                "(builtin) noop because output is empty: next_worker: {:?}, from worker: {}, job_result: {:?}",
                                &w.data.as_ref().map(|d|&d.name),
                                &worker.name,
                                &id
                            );
                            continue;
                        } else {
                            self.job_app()
                                .enqueue_job(
                                    w.id.as_ref(),
                                    None,
                                    // specify job result as argument for builtin worker
                                    JobResult {
                                        id: Some(id.clone()),
                                        data: Some(dat.clone()),
                                    }
                                    .encode_to_vec(),
                                    dat.uniq_key.clone(),
                                    dat.run_after_time,
                                    dat.priority,
                                    dat.timeout,
                                )
                                .await?;
                        }
                    } else {
                        // normal worker
                        for arg in dat
                            .output
                            .as_ref()
                            .map(|d| d.items.clone())
                            .unwrap_or(vec![vec![]])
                            .iter()
                        {
                            // no result, no enqueue
                            if arg.is_empty() {
                                tracing::debug!(
                                    "(builtin) noop because output is empty: next_worker: {:?}, from worker: {}, job_result: {:?}",
                                    &w.data.as_ref().map(|d|&d.name),
                                    &worker.name,
                                    &id
                                );
                                continue;
                            }
                            self.job_app()
                                .enqueue_job(
                                    w.id.as_ref(),
                                    None,
                                    arg.clone(),
                                    dat.uniq_key.clone(),
                                    dat.run_after_time,
                                    dat.priority,
                                    dat.timeout,
                                )
                                .await?;
                        }
                    }
                }
            }
        }
        Ok(())
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
