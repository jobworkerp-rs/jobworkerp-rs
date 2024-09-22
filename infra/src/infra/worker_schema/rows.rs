use proto::jobworkerp::data::{WorkerSchema, WorkerSchemaData, WorkerSchemaId};

// db row definitions
#[derive(sqlx::FromRow)]
pub struct WorkerSchemaRow {
    pub id: i64,
    pub name: String,
    pub operation_type: i32,
    pub job_arg_proto: String,
}

impl WorkerSchemaRow {
    pub fn to_proto(&self) -> WorkerSchema {
        WorkerSchema {
            id: Some(WorkerSchemaId { value: self.id }),
            data: Some(WorkerSchemaData {
                name: self.name.clone(),
                operation_type: self.operation_type,
                job_arg_proto: self.job_arg_proto.clone(),
            }),
        }
    }
}
