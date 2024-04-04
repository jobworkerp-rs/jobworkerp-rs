use crate::error::JobWorkerError;
use anyhow::Result;
use prost::Message;
use proto::jobworkerp::data::{
    worker_operation::Operation, Job, JobData, JobId, JobResult, JobResultData, JobResultId,
    RunnerArg, RunnerType, Worker, WorkerId, WorkerOperation,
};
use std::io::Cursor;

// db row definitions
#[derive(sqlx::FromRow)]
pub struct JobRow {
    pub id: i64,
    pub worker_id: i64,
    pub arg: Vec<u8>,
    pub uniq_key: Option<String>,
    pub enqueue_time: i64,
    pub grabbed_until_time: Option<i64>,
    pub run_after_time: i64,
    pub retried: i64, // u32
    pub priority: i32,
    pub timeout: i64,
}

impl JobRow {
    pub fn to_proto(&self) -> Job {
        Job {
            id: Some(JobId { value: self.id }),
            data: Some(JobData {
                worker_id: Some(WorkerId {
                    value: self.worker_id,
                }),
                arg: Some(
                    JobqueueAndCodec::deserialize_runner_arg(&self.arg).unwrap_or_else(|_| {
                        tracing::warn!(
                            "deserialize arg failed: job_id={}, worker_id={}, arg={:?}",
                            &self.id,
                            &self.worker_id,
                            &self.arg
                        );
                        RunnerArg::default()
                    }),
                ),
                uniq_key: self.uniq_key.clone(),
                enqueue_time: self.enqueue_time,
                grabbed_until_time: self.grabbed_until_time,
                run_after_time: self.run_after_time,
                retried: self.retried as u32,
                priority: self.priority,
                timeout: self.timeout as u64,
            }),
        }
    }
}

// use job queue (store and the data)
pub trait UseJobqueueAndCodec {
    // TODO specify arbitrary job channel name
    const DEFAULT_CHANNEL_NAME: &'static str = "__default_job_channel__";

    // worker pubsub channel name (for cache clear)
    const WORKER_PUBSUB_CHANNEL_NAME: &'static str = "__worker_pubsub_channel__";

    // job queue channel name with channel name
    fn queue_channel_name(channel_name: impl Into<String>, p: Option<&i32>) -> String {
        format!("q{}:{}:", p.unwrap_or(&0), channel_name.into())
    }

    // channel name for job with run_after_time (no priority)
    fn run_after_job_zset_key() -> String {
        "rc:zset".to_string()
    }

    fn run_after_job_key(job_id: &JobId) -> String {
        format!("rj:{}", job_id.value)
    }

    // jobId based channel name for direct or listen after response
    fn result_queue_name(job_id: &JobId) -> String {
        format!("r:d:{}", job_id.value)
    }
    // pubsub channel
    fn job_result_pubsub_channel_name(job_id: &JobId) -> String {
        format!("job_result_changed:job:{}", job_id.value)
    }

    fn serialize_runner_arg(arg: &RunnerArg) -> Vec<u8> {
        let mut buf = Vec::with_capacity(arg.encoded_len());
        arg.encode(&mut buf).unwrap();
        buf
    }

    fn deserialize_runner_arg(buf: &Vec<u8>) -> Result<RunnerArg> {
        RunnerArg::decode(&mut Cursor::new(buf)).map_err(|e| JobWorkerError::CodecError(e).into())
    }

    fn serialize_job(job: &Job) -> Vec<u8> {
        let mut buf = Vec::with_capacity(job.encoded_len());
        job.encode(&mut buf).unwrap();
        buf
    }

    fn deserialize_job(buf: &Vec<u8>) -> Result<Job> {
        Job::decode(&mut Cursor::new(buf)).map_err(|e| JobWorkerError::CodecError(e).into())
    }

    fn serialize_job_result(id: JobResultId, res: JobResultData) -> Vec<u8> {
        let j = JobResult {
            id: Some(id),
            data: Some(res),
        };
        let mut buf = Vec::with_capacity(j.encoded_len());
        j.encode(&mut buf).unwrap();
        buf
    }

    fn deserialize_job_result(buf: &Vec<u8>) -> Result<JobResult> {
        JobResult::decode(&mut Cursor::new(buf)).map_err(|e| JobWorkerError::CodecError(e).into())
    }

    fn serialize_worker(worker: &Worker) -> Vec<u8> {
        let mut buf = Vec::with_capacity(worker.encoded_len());
        worker.encode(&mut buf).unwrap();
        buf
    }

    fn deserialize_worker(buf: &Vec<u8>) -> Result<Worker> {
        Worker::decode(&mut Cursor::new(buf)).map_err(|e| JobWorkerError::CodecError(e).into())
    }

    fn serialize_worker_operation(arg: &WorkerOperation) -> Vec<u8> {
        let mut buf = Vec::with_capacity(arg.encoded_len());
        arg.encode(&mut buf).unwrap();
        buf
    }

    fn deserialize_worker_operation(buf: &Vec<u8>) -> Result<WorkerOperation> {
        WorkerOperation::decode(&mut Cursor::new(buf))
            .map_err(|e| JobWorkerError::CodecError(e).into())
    }
    fn deserialize_worker_operation_by_type(
        rtype: RunnerType,
        buf: &Vec<u8>,
    ) -> Result<WorkerOperation> {
        match rtype {
            RunnerType::Command => {
                let operation =
                    proto::jobworkerp::data::CommandOperation::decode(&mut Cursor::new(buf))
                        .map_err(|e| JobWorkerError::CodecError(e))?;
                Ok(WorkerOperation {
                    operation: Some(Operation::Command(operation)),
                })
            }
            RunnerType::Plugin => {
                let operation =
                    proto::jobworkerp::data::PluginOperation::decode(&mut Cursor::new(buf))
                        .map_err(|e| JobWorkerError::CodecError(e))?;
                Ok(WorkerOperation {
                    operation: Some(Operation::Plugin(operation)),
                })
            }
            RunnerType::GrpcUnary => {
                let operation =
                    proto::jobworkerp::data::GrpcUnaryOperation::decode(&mut Cursor::new(buf))
                        .map_err(|e| JobWorkerError::CodecError(e))?;
                Ok(WorkerOperation {
                    operation: Some(Operation::GrpcUnary(operation)),
                })
            }
            RunnerType::HttpRequest => {
                let operation =
                    proto::jobworkerp::data::HttpRequestOperation::decode(&mut Cursor::new(buf))
                        .map_err(|e| JobWorkerError::CodecError(e))?;
                Ok(WorkerOperation {
                    operation: Some(Operation::HttpRequest(operation)),
                })
            }
            RunnerType::Docker => {
                let operation =
                    proto::jobworkerp::data::DockerOperation::decode(&mut Cursor::new(buf))
                        .map_err(|e| JobWorkerError::CodecError(e))?;
                Ok(WorkerOperation {
                    operation: Some(Operation::Docker(operation)),
                })
            }
            RunnerType::SlackInternal => {
                let operation =
                    proto::jobworkerp::data::SlackJobResultOperation::decode(&mut Cursor::new(buf))
                        .map_err(|e| JobWorkerError::CodecError(e))?;
                Ok(WorkerOperation {
                    operation: Some(Operation::SlackInternal(operation)),
                })
            }
        }
    }
}

// for reference
pub struct JobqueueAndCodec {}
impl UseJobqueueAndCodec for JobqueueAndCodec {}

// test for serialize and deserialize equality for job, job_result_data
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use proto::jobworkerp::data::{runner_arg::Data, PluginArg, ResponseType, ResultOutput};

    #[test]
    fn test_serialize_and_deserialize_job() {
        let arg = RunnerArg {
            data: Some(Data::Plugin(PluginArg {
                arg: b"test".to_vec(),
            })),
        };
        let job = Job {
            id: Some(JobId { value: 1 }),
            data: Some(JobData {
                worker_id: Some(WorkerId { value: 1 }),
                arg: Some(arg),
                uniq_key: Some("test".to_string()),
                enqueue_time: Utc::now().timestamp_millis(),
                grabbed_until_time: None,
                run_after_time: 0,
                retried: 0,
                priority: 0,
                timeout: 1000,
            }),
        };
        struct JobQueueImpl {}
        impl UseJobqueueAndCodec for JobQueueImpl {}

        let serialized = JobQueueImpl::serialize_job(&job);
        let deserialized = JobQueueImpl::deserialize_job(&serialized).unwrap();
        assert_eq!(job, deserialized);
    }

    #[test]
    fn test_serialize_and_deserialize_job_result_data() {
        let arg = RunnerArg {
            data: Some(Data::Plugin(PluginArg {
                arg: b"test2".to_vec(),
            })),
        };
        let job_result_data = JobResultData {
            worker_id: Some(WorkerId { value: 2 }),
            worker_name: "hoge2".to_string(),
            job_id: Some(JobId { value: 3 }),
            arg: Some(arg),
            uniq_key: Some("hoge4".to_string()),
            status: 6,
            output: Some(ResultOutput {
                items: vec!["hoge7".as_bytes().to_vec()],
            }),
            max_retry: 7,
            retried: 8,
            priority: -1,
            timeout: 1000,
            enqueue_time: 9,
            run_after_time: 10,
            start_time: 11,
            end_time: 12,
            response_type: ResponseType::Direct as i32,
            store_success: true,
            store_failure: true,
        };
        struct JobQueueImpl {}
        impl UseJobqueueAndCodec for JobQueueImpl {}

        let id = JobResultId { value: 1234 };
        let serialized = JobQueueImpl::serialize_job_result(id.clone(), job_result_data.clone());
        let deserialized = JobQueueImpl::deserialize_job_result(&serialized).unwrap();
        assert_eq!(id, deserialized.id.unwrap());
        assert_eq!(job_result_data, deserialized.data.unwrap());
    }
}
