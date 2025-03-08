use anyhow::Result;
use command_utils::util::result::{TapErr, ToOption};
use jobworkerp_base::error::JobWorkerError;
use prost::Message;
use proto::jobworkerp::data::{
    JobId, JobResult, JobResultData, JobResultId, ResultOutput, WorkerId,
};
use std::io::Cursor;

// db row definitions
#[derive(sqlx::FromRow)]
pub struct JobResultRow {
    pub id: i64,
    pub job_id: i64,
    pub worker_id: i64,
    pub args: Vec<u8>,
    pub uniq_key: Option<String>,
    pub status: i32,
    pub output: Vec<u8>, // serialized
    pub retried: i64,    // u32
    pub priority: i32,
    pub timeout: i64,
    pub enqueue_time: i64,
    pub run_after_time: i64,
    pub start_time: i64,
    pub end_time: i64,
}

impl JobResultRow {
    //.XXX fill in without worker_name, max_retry, response_type, store_success, store_failure
    pub fn to_proto(&self) -> JobResult {
        JobResult {
            id: Some(JobResultId { value: self.id }),
            data: Some(JobResultData {
                job_id: Some(JobId { value: self.job_id }),
                worker_id: Some(WorkerId {
                    value: self.worker_id,
                }),
                worker_name: String::from(""),
                args: self.args.clone(),
                uniq_key: self.uniq_key.clone(),
                status: self.status,
                output: Self::deserialize_result_output(&self.output)
                    .tap_err(|e| tracing::error!("deserialize_error: {:?}", e))
                    .to_option(),
                max_retry: 0,
                retried: self.retried as u32,
                priority: self.priority,
                timeout: self.timeout as u64,
                enqueue_time: self.enqueue_time,
                run_after_time: self.run_after_time,
                start_time: self.start_time,
                end_time: self.end_time,
                response_type: 0,
                store_success: false,
                store_failure: false,
            }),
        }
    }

    pub fn serialize_result_output(list: &ResultOutput) -> Vec<u8> {
        let mut buf = Vec::with_capacity(list.encoded_len());
        list.encode(&mut buf).unwrap();
        buf
    }

    pub fn deserialize_result_output(buf: &Vec<u8>) -> Result<ResultOutput> {
        ResultOutput::decode(&mut Cursor::new(buf))
            .map_err(|e| JobWorkerError::CodecError(e).into())
    }
}
