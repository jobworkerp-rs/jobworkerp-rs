use jobworkerp_runner::runner::RunnerSpec;
use proto::jobworkerp::data::{Runner, RunnerData, RunnerId};

// db row definitions
#[derive(sqlx::FromRow, Debug, Clone)]
pub struct RunnerRow {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub file_name: String,
    pub r#type: i32,
}

impl RunnerRow {
    pub fn to_runner_with_schema(
        &self,
        runner: Box<dyn RunnerSpec + Send + Sync>,
    ) -> RunnerWithSchema {
        RunnerWithSchema {
            id: Some(RunnerId { value: self.id }),
            data: Some(RunnerData {
                name: self.name.clone(),
                description: self.description.clone(),
                runner_type: self.r#type,
                runner_settings_proto: runner.runner_settings_proto(),
                job_args_proto: runner.job_args_proto(),
                result_output_proto: runner.result_output_proto(),
                output_type: runner.output_type() as i32,
            }),
            settings_schema: runner.settings_schema(),
            arguments_schema: runner.arguments_schema(),
            output_schema: runner.output_schema(),
        }
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize, PartialEq, ::prost::Message)]
pub struct RunnerWithSchema {
    #[prost(message, tag = "1")]
    pub id: Option<RunnerId>,
    #[prost(message, tag = "2")]
    pub data: Option<RunnerData>,
    #[prost(string, tag = "3")]
    pub settings_schema: String,
    #[prost(string, tag = "4")]
    pub arguments_schema: String,
    #[prost(string, optional, tag = "5")]
    pub output_schema: Option<String>,
}

impl RunnerWithSchema {
    pub fn to_proto(&self) -> Runner {
        Runner {
            id: self.id,
            data: self.data.clone(),
        }
    }
    pub fn into_proto(self) -> Runner {
        Runner {
            id: self.id,
            data: self.data,
        }
    }
}
