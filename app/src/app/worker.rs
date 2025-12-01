pub mod hybrid;
pub mod rdb;

use anyhow::Result;
use async_trait::async_trait;
use infra::infra::job::rows::{JobqueueAndCodec, UseJobqueueAndCodec};
use infra::infra::runner::rows::RunnerWithSchema;
use jobworkerp_base::error::JobWorkerError;
use memory_utils::cache::moka::{MokaCacheImpl, UseMokaCache};
use proto::jobworkerp::data::{Worker, WorkerData, WorkerId};
use std::fmt;
use std::sync::Arc;

use super::runner::UseRunnerApp;

#[async_trait]
pub trait WorkerAppCacheHelper: Send + Sync {
    fn memory_cache(&self) -> &MokaCacheImpl<Arc<String>, Worker>;
    fn list_memory_cache(&self) -> &MokaCacheImpl<Arc<String>, Vec<Worker>>;
    //cache control
    // create cache for temporary worker
    async fn create_cache(&self, id: &WorkerId, worker: &Worker) -> Result<()> {
        let k = Arc::new(Self::find_cache_key(id));
        self.memory_cache()
            .set_cache(k.clone(), worker.clone())
            .await;
        Ok(())
    }
    async fn clear_cache(&self, id: &WorkerId) {
        let k = Arc::new(Self::find_cache_key(id));
        let _ = self.memory_cache().delete_cache(&k).await; // ignore error
        self.clear_all_list_cache().await;
    }
    async fn clear_cache_by_name(&self, name: &str) {
        let k = Arc::new(Self::find_name_cache_key(name));
        let _ = self.memory_cache().delete_cache(&k).await; // ignore error
        self.clear_all_list_cache().await;
    }
    async fn clear_all_list_cache(&self) {
        let _ = self.list_memory_cache().clear().await; // XXX clear all list cache
    }
    // all clear
    async fn clear_cache_all(&self) {
        let _ = self.memory_cache().clear().await; // ignore error
        let _ = self.list_memory_cache().clear().await; // ignore error
    }

    fn find_list_cache_key(limit: Option<i32>, offset: Option<i64>) -> String {
        if let Some(l) = limit {
            [
                "worker_list:",
                l.to_string().as_str(),
                ":",
                offset.unwrap_or(0i64).to_string().as_str(),
            ]
            .join("")
        } else {
            Self::find_all_list_cache_key()
        }
    }
    fn find_all_list_cache_key() -> String {
        "worker_list:all".to_string()
    }

    fn find_cache_key(id: &WorkerId) -> String {
        ["worker_id:", &id.value.to_string()].join("")
    }

    fn find_name_cache_key(name: &str) -> String {
        ["wn:", name].join("")
    }
}

#[async_trait]
pub trait WorkerApp: UseRunnerApp + fmt::Debug + Send + Sync + 'static {
    // async fn reflesh_redis_record_from_rdb(&self) -> Result<()> ;

    async fn create(&self, worker: &WorkerData) -> Result<WorkerId>;
    // create a temp worker (only in redis or memory)
    async fn create_temp(&self, worker: WorkerData, with_random_name: bool) -> Result<WorkerId>;
    // if worker is None, clear cache only
    async fn update(&self, id: &WorkerId, worker: &Option<WorkerData>) -> Result<bool>;
    async fn delete(&self, id: &WorkerId) -> Result<bool>;
    async fn delete_all(&self) -> Result<bool>;

    async fn find_data_by_name(&self, name: &str) -> Result<Option<WorkerData>>
    where
        Self: Send + 'static,
    {
        self.find_by_name(name)
            .await
            .map(|w| w.and_then(|w| w.data))
    }

    async fn find_data(&self, id: &WorkerId) -> Result<Option<WorkerData>>
    where
        Self: Send + 'static,
    {
        self.find(id).await.map(|w| w.and_then(|w| w.data))
    }

    async fn find_data_by_opt(&self, idopt: Option<&WorkerId>) -> Result<Option<WorkerData>>
    where
        Self: Send + 'static,
    {
        if let Some(id) = idopt {
            self.find_data(id).await
        } else {
            Ok(None)
        }
    }

    async fn find_by_opt(&self, idopt: Option<&WorkerId>) -> Result<Option<Worker>>
    where
        Self: Send + 'static,
    {
        if let Some(id) = idopt {
            self.find(id).await
        } else {
            Ok(None)
        }
    }

    async fn find(&self, id: &WorkerId) -> Result<Option<Worker>>
    where
        Self: Send + 'static;

    async fn find_by_name(&self, name: &str) -> Result<Option<Worker>>
    where
        Self: Send + 'static;

    #[allow(clippy::too_many_arguments)]
    async fn find_list(
        &self,
        runner_types: Vec<i32>,
        channel: Option<String>,
        limit: Option<i32>,
        offset: Option<i64>,
        name_filter: Option<String>,
        is_periodic: Option<bool>,
        runner_ids: Vec<i64>,
        sort_by: Option<proto::jobworkerp::data::WorkerSortField>,
        ascending: Option<bool>,
    ) -> Result<Vec<Worker>>
    where
        Self: Send + 'static;

    async fn find_by_id_or_name(
        &self,
        id: Option<&WorkerId>,
        name: Option<&String>,
    ) -> Result<Worker>
    where
        Self: Send + 'static,
    {
        // get worker data
        match (id, name) {
            (Some(wid), _) => {
                // found worker_id: use it
                self.find(wid).await?.ok_or(
                    JobWorkerError::InvalidParameter(format!(
                        "cannot listen job which worker is None: id={}",
                        wid.value
                    ))
                    .into(),
                )
            }
            (_, Some(wname)) => {
                // found worker_name: use it
                self.find_by_name(wname).await?.ok_or(
                    JobWorkerError::InvalidParameter(format!(
                        "cannot listen job which worker is None: id={wname}"
                    ))
                    .into(),
                )
            }
            (None, None) => Err(JobWorkerError::InvalidParameter(
                "cannot listen job which worker_id is None".to_string(),
            )
            .into()),
        }
    }

    async fn find_all_worker_list(&self) -> Result<Vec<Worker>>
    where
        Self: Send + 'static;

    async fn count(&self) -> Result<i64>
    where
        Self: Send + 'static;

    async fn count_by(
        &self,
        runner_types: Vec<i32>,
        channel: Option<String>,
        name_filter: Option<String>,
        is_periodic: Option<bool>,
        runner_ids: Vec<i64>,
    ) -> Result<i64>
    where
        Self: Send + 'static;

    async fn count_by_channel(&self) -> Result<Vec<(String, i64)>>
    where
        Self: Send + 'static;

    // TODO use memory cache or not.
    // XXX use find_all_worker_list()
    async fn find_worker_ids_by_channel(&self, channel: &String) -> Result<Vec<WorkerId>> {
        self.find_all_worker_list().await.map(|ws| {
            ws.into_iter()
                .filter(|w| {
                    w.data
                        .as_ref()
                        .map(|d| d.channel.as_ref())
                        .is_some_and(|c| {
                            c.unwrap_or(&JobqueueAndCodec::DEFAULT_CHANNEL_NAME.to_string())
                                == channel
                        })
                })
                .filter_map(|w| w.id)
                .collect()
        })
    }
    // for pubsub
    async fn clear_cache_by(&self, id: Option<&WorkerId>, name: Option<&String>) -> Result<()>;

    async fn check_worker_streaming(&self, id: &WorkerId, request_streaming: bool) -> Result<()> {
        let runner_id = if let Some(Worker {
            id: _,
            data: Some(wd),
        }) = self.find(id).await?
        {
            wd.runner_id.ok_or_else(|| {
                anyhow::Error::from(JobWorkerError::InvalidParameter(format!(
                    "worker does not have runner_id: id={}",
                    &id.value
                )))
            })?
        } else {
            return Err(
                JobWorkerError::NotFound(format!("worker not found: id={}", &id.value)).into(),
            );
        };
        if let Some(RunnerWithSchema {
            id: _,
            data: Some(runner_data),
            ..
        }) = self.runner_app().find_runner(&runner_id).await?
        {
            let output_type = if let Some(ref method_proto_map) = runner_data.method_proto_map {
                method_proto_map
                    .schemas
                    .values()
                    .next()
                    .map(|schema| schema.output_type)
                    .unwrap_or(proto::jobworkerp::data::StreamingOutputType::NonStreaming as i32)
            } else {
                proto::jobworkerp::data::StreamingOutputType::NonStreaming as i32
            };

            match proto::jobworkerp::data::StreamingOutputType::try_from(output_type).ok() {
                Some(proto::jobworkerp::data::StreamingOutputType::Streaming) => {
                    if request_streaming {
                        Ok(())
                    } else {
                        Err(JobWorkerError::InvalidParameter(
                            "runner does not support streaming".to_string(),
                        )
                        .into())
                    }
                }
                Some(proto::jobworkerp::data::StreamingOutputType::NonStreaming) => {
                    if request_streaming {
                        Err(JobWorkerError::InvalidParameter(
                            "runner does not support streaming".to_string(),
                        )
                        .into())
                    } else {
                        Ok(())
                    }
                }
                Some(_) => Ok(()), // Both
                None => Err(JobWorkerError::InvalidParameter(format!(
                    "runner does not support streaming mode: {}",
                    output_type
                ))
                .into()),
            }
        } else {
            Err(JobWorkerError::InvalidParameter("runner not found".to_string()).into())
        }
    }
}

pub trait UseWorkerApp {
    fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static>;
}
