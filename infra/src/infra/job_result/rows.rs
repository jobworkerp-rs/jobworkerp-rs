use anyhow::{Result, bail};
use jobworkerp_base::error::JobWorkerError;
use prost::Message;
use proto::jobworkerp::data::{
    JobId, JobResult, JobResultData, JobResultId, ResultOutput, SandboxExecutionObservation,
    WorkerId,
};
use proto::sandbox_observation::sha256_digest;
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
    pub sandbox_execution_observation: Option<Vec<u8>>,
}

impl JobResultRow {
    // Fields not stored in RDB are filled with defaults:
    // worker_name, max_retry, response_type, store_success, store_failure, broadcast_results
    #[allow(deprecated)]
    pub fn to_proto(&self) -> Result<JobResult> {
        let sandbox_execution_observation = self
            .sandbox_execution_observation
            .as_deref()
            .map(SandboxExecutionObservation::decode_validated)
            .transpose()?;
        if let Some(observation) = &sandbox_execution_observation
            && !self.matches_observation(observation)
        {
            bail!("sandbox observation does not match its job result row identity");
        }
        Ok(JobResult {
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
                broadcast_results: false,
                resolved_retry_policy: None,
            }),
            sandbox_execution_observation,
            ..Default::default()
        })
    }

    pub fn matches_observation(&self, observation: &SandboxExecutionObservation) -> bool {
        self.id == observation.result_id
            && self.job_id == observation.job_id
            && self.worker_id == observation.worker_id
            && sha256_digest(&self.args).as_slice() == observation.dispatch_args_sha256.as_slice()
            && self.status == observation.stored_result_status
            && u32::try_from(self.retried).ok() == Some(observation.retry_ordinal)
            && self.using.as_deref().unwrap_or_default() == observation.using
    }

    pub fn matches_persisted_data(&self, data: &JobResultData) -> Result<bool> {
        // Compare actual RDB columns, not display fields restored from worker defaults.
        // Option::None and Some(empty) intentionally compare equal when both encode to an empty blob.
        let output = data
            .output
            .as_ref()
            .map(Self::serialize_result_output)
            .transpose()?
            .unwrap_or_default();
        Ok(data
            .job_id
            .as_ref()
            .is_some_and(|id| id.value == self.job_id)
            && data
                .worker_id
                .as_ref()
                .is_some_and(|id| id.value == self.worker_id)
            && data.args == self.args
            && data.uniq_key == self.uniq_key
            && data.status == self.status
            && output == self.output
            && data.retried as i64 == self.retried
            && data.priority == self.priority
            && data.timeout as i64 == self.timeout
            && data.streaming_type == self.streaming_type
            && data.enqueue_time == self.enqueue_time
            && data.run_after_time == self.run_after_time
            && data.start_time == self.start_time
            && data.end_time == self.end_time
            && data.using == self.using)
    }

    pub fn serialize_result_output(list: &ResultOutput) -> Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(list.encoded_len());
        list.encode(&mut buf)?;
        Ok(buf)
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
            let serialized_output = JobResultRow::serialize_result_output(&output).unwrap();

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
                sandbox_execution_observation: None,
            };

            let job_result = row.to_proto().unwrap();
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

        let serialized = JobResultRow::serialize_result_output(&output).unwrap();
        let deserialized = JobResultRow::deserialize_result_output(&serialized).unwrap();

        assert_eq!(output.items, deserialized.items);
    }

    #[test]
    fn row_observation_round_trips_and_corrupt_bytes_fail_closed() {
        use proto::jobworkerp::data::{
            ResultStatus, SandboxExecutionEndState, SandboxExecutionProducerState,
        };
        use proto::sandbox_observation::{SANDBOX_OBSERVATION_SCHEMA_VERSION, sha256_digest};

        let mut observation = SandboxExecutionObservation {
            schema_version: SANDBOX_OBSERVATION_SCHEMA_VERSION,
            job_id: 2,
            worker_id: 3,
            runner_id: 4,
            result_id: 1,
            dispatch_args_sha256: sha256_digest(b"args").to_vec(),
            worker_settings_sha256: sha256_digest(b"worker").to_vec(),
            method_schema_sha256: sha256_digest(b"method").to_vec(),
            host_settings_sha256: sha256_digest(b"host").to_vec(),
            using: String::new(),
            retry_ordinal: 0,
            stored_result_status: ResultStatus::Success as i32,
            cli_exit_code: None,
            end_state: SandboxExecutionEndState::Unknown as i32,
            producer_state: SandboxExecutionProducerState::Unknown as i32,
            ..Default::default()
        };
        observation.seal().unwrap();

        let row = JobResultRow {
            id: 1,
            job_id: 2,
            worker_id: 3,
            args: b"args".to_vec(),
            uniq_key: None,
            status: ResultStatus::Success as i32,
            output: Vec::new(),
            retried: 0,
            priority: 0,
            timeout: 0,
            streaming_type: 0,
            enqueue_time: 0,
            run_after_time: 0,
            start_time: 0,
            end_time: 0,
            using: None,
            sandbox_execution_observation: Some(observation.encode_validated().unwrap()),
        };
        assert_eq!(
            row.to_proto().unwrap().sandbox_execution_observation,
            Some(observation)
        );

        let corrupt = JobResultRow {
            sandbox_execution_observation: Some(vec![0xff; 64 * 1024 + 1]),
            ..row
        };
        assert!(corrupt.to_proto().is_err());
    }
}
