use proto::jobworkerp::data::{WorkerSchema, WorkerSchemaData, WorkerSchemaId};

use crate::infra::runner::Runner;

// db row definitions
#[derive(sqlx::FromRow, Debug, Clone)]
pub struct WorkerSchemaRow {
    pub id: i64,
    pub name: String,
    pub file_name: String,
    pub r#type: i32,
}

impl WorkerSchemaRow {
    pub fn to_proto(&self, runner: Box<dyn Runner + Send + Sync>) -> WorkerSchema {
        WorkerSchema {
            id: Some(WorkerSchemaId { value: self.id }),
            data: Some(WorkerSchemaData {
                name: self.name.clone(),
                runner_type: self.r#type,
                operation_proto: runner.operation_proto(),
                job_arg_proto: runner.job_args_proto(),
            }),
        }
    }
}
