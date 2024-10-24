use anyhow::Result;
use app::app::job::JobApp;
use app::app::job::UseJobApp;
use app::app::job_result::JobResultApp;
use app::app::job_result::UseJobResultApp;
use app::app::worker::UseWorkerApp;
use app::app::worker::WorkerApp;
use app::app::worker_schema::UseWorkerSchemaApp;
use app::app::worker_schema::WorkerSchemaApp;
use app::app::JobBuilder;
use app::app::StorageConfig;
use app::app::UseStorageConfig;
use app::app::UseWorkerConfig;
use app::app::WorkerConfig;
use app::module::AppConfigModule;
use app::module::AppModule;
use command_utils::util::option::Exists;
use debug_stub_derive::DebugStub;
use infra::error::JobWorkerError;
use infra::infra::job::rows::JobqueueAndCodec;
use infra::infra::job::rows::UseJobqueueAndCodec;
use proto::jobworkerp::data::JobResultData;
use proto::jobworkerp::data::JobResultId;
use proto::jobworkerp::data::ResultStatus;
use proto::jobworkerp::data::Worker;
use proto::jobworkerp::data::WorkerSchema;
use proto::jobworkerp::data::{JobResult, WorkerData};
use std::sync::Arc;
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
                if let Ok(Some(Worker {
                    id: Some(wid),
                    data: Some(wdat),
                })) = self.worker_app().find(wid).await
                {
                    // TODO move find-runner logic to method(worker_schema_app?)
                    let runner = if let Some(wsid) = &wdat.schema_id {
                        if let Ok(Some(WorkerSchema {
                            id: _,
                            data: Some(wsdat),
                        })) = self
                            .worker_schema_app()
                            .find_worker_schema(wsid, None)
                            .await
                        {
                            if let Some(r) = self
                                .config_module
                                .runner_factory
                                .create_by_name(&wsdat.name, false)
                                .await
                            {
                                Ok(r)
                            } else {
                                tracing::error!("runner not found: {:?}", &wsdat.name);
                                Err(JobWorkerError::NotFound(format!(
                                    "runner not found: {:?}",
                                    &wsdat.name
                                )))
                            }
                        } else {
                            tracing::error!("worker schema not found: {:?}", &wsid);
                            Err(JobWorkerError::NotFound(format!(
                                "worker schema not found: {:?}",
                                &wsid
                            )))
                        }
                    } else {
                        tracing::error!("worker schema id not found: {:?}", &wid);
                        Err(JobWorkerError::NotFound(format!(
                            "worker schema id not found: {:?}",
                            &wid
                        )))
                    }?;

                    // use job result as argument for worker
                    if runner.use_job_result() {
                        // no result data, no enqueue
                        if dat.output.is_none()
                            || dat.output.as_ref().exists(|o| o.items.is_empty())
                        {
                            tracing::warn!(
                                "(builtin) noop because output is empty: next_worker: {:?}, from worker: {}, job_result: {:?}",
                                &wdat.name,
                                &worker.name,
                                &id
                            );
                        } else {
                            let arg = JobqueueAndCodec::serialize_message(&JobResult {
                                id: Some(*id),
                                data: Some(dat.clone()),
                            });
                            self.job_app()
                                .enqueue_job(
                                    Some(&wid),
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
                                    &wdat.name,
                                    &worker.name,
                                    &id
                                );
                                continue;
                            }
                            self.job_app()
                                .enqueue_job(
                                    Some(&wid),
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
impl UseWorkerSchemaApp for ResultProcessorImpl {
    fn worker_schema_app(&self) -> Arc<dyn WorkerSchemaApp + 'static> {
        self.app_module.worker_schema_app.clone()
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
