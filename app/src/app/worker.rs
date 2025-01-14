pub mod hybrid;
pub mod rdb;
pub mod redis;

use anyhow::Result;
use async_trait::async_trait;
use command_utils::util::option::{Exists, FlatMap};
use infra::error::JobWorkerError;
use infra::infra::job::rows::{JobqueueAndCodec, UseJobqueueAndCodec};
use infra_utils::infra::memory::{MemoryCacheImpl, UseMemoryCache};
use proto::jobworkerp::data::{Worker, WorkerData, WorkerId};
use std::fmt;
use std::sync::Arc;

#[async_trait]
pub trait WorkerAppCacheHelper: Send + Sync {
    fn memory_cache(&self) -> &MemoryCacheImpl<Arc<String>, Worker>;
    fn list_memory_cache(&self) -> &MemoryCacheImpl<Arc<String>, Vec<Worker>>;
    //cache control
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
        let kl = Arc::new(Self::find_all_list_cache_key());
        let _ = self.list_memory_cache().delete_cache(&kl).await; // ignore error
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
pub trait WorkerApp: fmt::Debug + Send + Sync + 'static {
    // async fn reflesh_redis_record_from_rdb(&self) -> Result<()> ;

    async fn create(&self, worker: &WorkerData) -> Result<WorkerId>;
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
            .map(|w| w.flat_map(|w| w.data))
    }

    async fn find_data(&self, id: &WorkerId) -> Result<Option<WorkerData>>
    where
        Self: Send + 'static,
    {
        self.find(id).await.map(|w| w.flat_map(|w| w.data))
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

    async fn find_list(&self, limit: Option<i32>, offset: Option<i64>) -> Result<Vec<Worker>>
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
                        "cannot listen job which worker is None: id={}",
                        wname
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

    // TODO use memory cache or not.
    // XXX use find_all_worker_list()
    async fn find_worker_ids_by_channel(&self, channel: &String) -> Result<Vec<WorkerId>> {
        self.find_all_worker_list().await.map(|ws| {
            ws.into_iter()
                .filter(|w| {
                    w.data.as_ref().map(|d| d.channel.as_ref()).exists(|c| {
                        c.unwrap_or(&JobqueueAndCodec::DEFAULT_CHANNEL_NAME.to_string()) == channel
                    })
                })
                .filter_map(|w| w.id)
                .collect()
        })
    }
    // for pubsub
    async fn clear_cache_by(&self, id: Option<&WorkerId>, name: Option<&String>) -> Result<()>;
}

pub trait UseWorkerApp {
    fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static>;
}
