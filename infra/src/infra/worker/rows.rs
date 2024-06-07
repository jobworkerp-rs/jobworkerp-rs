use anyhow::Result;
use itertools::Itertools;
use proto::jobworkerp::data::{RetryPolicy, Worker, WorkerData, WorkerId};

use crate::infra::job::rows::{JobqueueAndCodec, UseJobqueueAndCodec};

// db row definitions
#[derive(sqlx::FromRow)]
pub struct WorkerRow {
    pub id: i64,
    pub name: String,
    pub r#type: i32,
    pub operation: Vec<u8>,
    pub retry_type: i32,
    pub interval: i64,          // u32 // cannot use u32 in sqlx any db
    pub max_interval: i64,      // u32
    pub max_retry: i64,         // u32
    pub basis: f64,             // f32 (for sqlx sqlite3)
    pub periodic_interval: i64, // u32
    pub channel: Option<String>,
    pub queue_type: i32,
    pub response_type: i32,
    pub store_success: bool,
    pub store_failure: bool,
    pub next_workers: String,
    pub use_static: bool,
}

impl WorkerRow {
    pub fn to_proto(&self) -> Result<Worker> {
        let operation = JobqueueAndCodec::deserialize_worker_operation(&self.operation)?;
        Ok(Worker {
            id: Some(WorkerId { value: self.id }),
            data: Some(WorkerData {
                name: self.name.clone(),
                r#type: self.r#type,
                operation: Some(operation),
                retry_policy: Some(RetryPolicy {
                    r#type: self.retry_type,
                    interval: self.interval as u32,
                    max_interval: self.max_interval as u32,
                    max_retry: self.max_retry as u32,
                    basis: self.basis as f32, // XXX downcast
                }),
                periodic_interval: self.periodic_interval as u32,
                channel: self.channel.clone(),
                queue_type: self.queue_type,
                response_type: self.response_type,
                store_success: self.store_success,
                store_failure: self.store_failure,
                next_workers: Self::deserialize_worker_ids(self.next_workers.as_ref()),
                use_static: self.use_static,
            }),
        })
    }
    fn deserialize_worker_ids(s: &str) -> Vec<WorkerId> {
        s.split(',')
            .flat_map(|i| i.parse().map(|d| WorkerId { value: d }))
            .collect_vec()
    }
    pub fn serialize_worker_ids(v: &[WorkerId]) -> String {
        v.iter().map(|i| i.value.to_string()).join(",")
    }
}
