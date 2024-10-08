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
use command_utils::util::option::Exists;
use debug_stub_derive::DebugStub;
use infra::infra::job::rows::JobqueueAndCodec;
use infra::infra::job::rows::UseJobqueueAndCodec;
use proto::jobworkerp::data::slack_job_result_arg::ResultMessageData;
use proto::jobworkerp::data::JobResultData;
use proto::jobworkerp::data::JobResultId;
use proto::jobworkerp::data::ResultStatus;
use proto::jobworkerp::data::SlackJobResultArg;
use proto::jobworkerp::data::WorkerSchemaId;
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
            // complete job (delete first for unique key)
            let res = self.job_app().complete_job(id, dat).await.map(|_| ());
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
            res
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
                    // builtin worker (only slack internal now)
                    if w.data
                        .as_ref()
                        // TODO define schema_id for builtin worker
                        .exists(|wd| wd.schema_id == Some(WorkerSchemaId { value: -1 }))
                    {
                        // no result data, no enqueue
                        if dat.output.is_none()
                            || dat.output.as_ref().exists(|o| o.items.is_empty())
                        {
                            tracing::warn!(
                                "(builtin) noop because output is empty: next_worker: {:?}, from worker: {}, job_result: {:?}",
                                &w.data.as_ref().map(|d|&d.name),
                                &worker.name,
                                &id
                            );
                        } else if !BuiltinWorkerIds::iter()
                            .any(|i| w.id.as_ref().exists(|wid| wid.value == i as i64))
                        {
                            // ERROR: builtin type worker not found in BuiltinWorkerIds (matched worker in db)
                            tracing::error!(
                                "next builtin-worker is not defined as builtin: id={:?}, worker={:?}",
                                wid,
                                worker
                            );
                        } else {
                            let arg = JobqueueAndCodec::serialize_message(&SlackJobResultArg {
                                message: Some(Self::job_result_to_message(id, dat)),
                                channel: None, // TODO
                            });
                            self.job_app()
                                .enqueue_job(
                                    w.id.as_ref(),
                                    None,
                                    // specify job result as argument for builtin worker
                                    arg,
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
                            .into_iter()
                        {
                            // no result, no enqueue
                            if arg.is_empty() {
                                tracing::warn!(
                                    "noop because output is empty: next_worker: {:?}, from worker: {}, job_result: {:?}",
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
                                    arg,
                                    dat.uniq_key.clone(),
                                    dat.run_after_time,
                                    dat.priority,
                                    dat.timeout,
                                )
                                .await?;
                        }
                    }
                } else {
                    tracing::error!("next worker not found: id={:?}, worker={:?}", wid, worker);
                }
            }
        }
        Ok(())
    }
    fn job_result_to_message(id: &JobResultId, dat: &JobResultData) -> ResultMessageData {
        ResultMessageData {
            result_id: id.value,
            job_id: dat.job_id.as_ref().map(|j| j.value).unwrap_or(0),
            worker_name: dat.worker_name.clone(),
            status: dat.status,
            output: dat.output.clone(),
            retried: dat.retried,
            enqueue_time: dat.enqueue_time,
            run_after_time: dat.run_after_time,
            start_time: dat.start_time,
            end_time: dat.end_time,
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
