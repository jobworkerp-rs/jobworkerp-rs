use anyhow::Result;
use itertools::Itertools;
use proto::jobworkerp::data::{RetryPolicy, Worker, WorkerData, WorkerId, WorkerSchemaId};

// db row definitions
#[derive(sqlx::FromRow)]
pub struct WorkerRow {
    pub id: i64,
    pub name: String,
    pub schema_id: i64,
    pub operation: Vec<u8>,
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
    pub next_workers: String,
    pub use_static: bool,
}

impl WorkerRow {
    pub fn to_proto(&self) -> Result<Worker> {
        Ok(Worker {
            id: Some(WorkerId { value: self.id }),
            data: Some(WorkerData {
                name: self.name.clone(),
                schema_id: Some(WorkerSchemaId {
                    value: self.schema_id,
                }),
                operation: self.operation.clone(),
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
