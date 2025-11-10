use anyhow::Result;
use proto::jobworkerp::data::{RetryPolicy, RunnerId, Worker, WorkerData, WorkerId};

// db row definitions
#[derive(sqlx::FromRow)]
pub struct WorkerRow {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub runner_id: i64,
    pub runner_settings: Vec<u8>,
    pub retry_type: i32,
    pub interval: i64,     // u32 // cannot use u32 in sqlx any db
    pub max_interval: i64, // u32
    pub max_retry: i64,    // u32
    #[cfg(not(feature = "mysql"))]
    pub basis: f64,
    #[cfg(feature = "mysql")]
    pub basis: f32,
    pub periodic_interval: i64, // u32
    pub channel: Option<String>,
    pub queue_type: i32,
    pub response_type: i32,
    pub store_success: bool,
    pub store_failure: bool,
    pub use_static: bool,
    pub broadcast_results: bool,
    pub created_at: i64,
}

impl WorkerRow {
    pub fn to_proto(&self) -> Result<Worker> {
        Ok(Worker {
            id: Some(WorkerId { value: self.id }),
            data: Some(WorkerData {
                name: self.name.clone(),
                description: self.description.clone(),
                runner_id: Some(RunnerId {
                    value: self.runner_id,
                }),
                runner_settings: self.runner_settings.clone(),
                retry_policy: Some(RetryPolicy {
                    r#type: self.retry_type,
                    interval: self.interval as u32,
                    max_interval: self.max_interval as u32,
                    max_retry: self.max_retry as u32,
                    #[allow(clippy::unnecessary_cast)]
                    basis: self.basis as f32, // XXX downcast
                }),
                periodic_interval: self.periodic_interval as u32,
                channel: self.channel.clone(),
                queue_type: self.queue_type,
                response_type: self.response_type,
                store_success: self.store_success,
                store_failure: self.store_failure,
                use_static: self.use_static,
                broadcast_results: self.broadcast_results,
            }),
        })
    }
}
