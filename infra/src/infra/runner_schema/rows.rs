use proto::jobworkerp::data::{RunnerSchema, RunnerSchemaData, RunnerSchemaId};

// db row definitions
#[derive(sqlx::FromRow)]
pub struct RunnerSchemaRow {
    pub id: i64,
    pub name: String,
    pub operation_type: i32,
    pub operation_proto: String,
    pub job_arg_proto: String,
}

impl RunnerSchemaRow {
    pub fn to_proto(&self) -> RunnerSchema {
        RunnerSchema {
            id: Some(RunnerSchemaId { value: self.id }),
            data: Some(RunnerSchemaData {
                name: self.name.clone(),
                operation_type: self.operation_type,
                operation_proto: self.operation_proto.clone(),
                job_arg_proto: self.job_arg_proto.clone(),
            }),
        }
    }
}
