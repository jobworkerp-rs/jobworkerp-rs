use anyhow::Result;
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
    // DB column name remains "request_streaming" for backward compatibility
    // but stores StreamingType enum value (0=None, 1=Response, 2=Internal)
    #[sqlx(rename = "request_streaming")]
    pub streaming_type: i32,
    pub enqueue_time: i64,
    pub run_after_time: i64,
    pub start_time: i64,
    pub end_time: i64,
    pub using: Option<String>, // sub-method name for MCP/Plugin runners
}

impl JobResultRow {
    //.XXX fill in without worker_name, max_retry, response_type, store_success, store_failure
    #[allow(deprecated)]
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
                    .inspect_err(|e| tracing::error!("deserialize_error: {:?}", e))
                    .ok(),
                max_retry: 0,
                retried: self.retried as u32,
                priority: self.priority,
                timeout: self.timeout as u64,
                streaming_type: self.streaming_type,
                enqueue_time: self.enqueue_time,
                run_after_time: self.run_after_time,
                start_time: self.start_time,
                end_time: self.end_time,
                response_type: 0,
                store_success: false,
                store_failure: false,
                using: self.using.clone(),
            }),
            ..Default::default()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_job_result_row_to_proto_streaming_type() {
        // Test JobResultRow.to_proto() correctly converts streaming_type for all values
        let streaming_type_values = [0i32, 1i32, 2i32];

        for streaming_type_value in streaming_type_values {
            let output = ResultOutput {
                items: b"test output".to_vec(),
            };
            let serialized_output = JobResultRow::serialize_result_output(&output);

            let row = JobResultRow {
                id: 1,
                job_id: 2,
                worker_id: 3,
                args: vec![1, 2, 3],
                uniq_key: Some("test".to_string()),
                status: 1,
                output: serialized_output,
                retried: 0,
                priority: 0,
                timeout: 1000,
                streaming_type: streaming_type_value,
                enqueue_time: 100,
                run_after_time: 0,
                start_time: 100,
                end_time: 200,
                using: None,
            };

            let job_result = row.to_proto();
            assert_eq!(
                job_result.data.as_ref().unwrap().streaming_type,
                streaming_type_value,
                "JobResultRow.to_proto() should preserve streaming_type value {}",
                streaming_type_value
            );
        }
    }

    #[test]
    fn test_serialize_deserialize_result_output() {
        let output = ResultOutput {
            items: b"test output data".to_vec(),
        };

        let serialized = JobResultRow::serialize_result_output(&output);
        let deserialized = JobResultRow::deserialize_result_output(&serialized).unwrap();

        assert_eq!(output.items, deserialized.items);
    }
}
