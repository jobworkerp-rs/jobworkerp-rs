use jobworkerp_runner::runner::RunnerSpec;
use proto::jobworkerp::data::{Runner, RunnerData, RunnerId};

// db row definitions
#[derive(sqlx::FromRow, Debug, Clone)]
pub struct RunnerRow {
    pub id: i64,
    pub name: String,
    pub file_name: String,
    pub r#type: i32,
}

impl RunnerRow {
    pub fn to_proto(&self, runner: Box<dyn RunnerSpec + Send + Sync>) -> Runner {
        Runner {
            id: Some(RunnerId { value: self.id }),
            data: Some(RunnerData {
                name: self.name.clone(),
                runner_type: self.r#type,
                runner_settings_proto: runner.runner_settings_proto(),
                job_args_proto: runner.job_args_proto(),
                result_output_proto: runner.result_output_proto(),
                output_as_stream: runner.output_as_stream(),
            }),
        }
    }
}
