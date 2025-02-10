use crate::error::JobWorkerError;
use anyhow::Result;
use prost::Message;
use proto::jobworkerp::data::{
    Job, JobData, JobId, JobResult, JobResultData, JobResultId, Worker, WorkerId,
};
use std::io::Cursor;

// db row definitions
#[derive(sqlx::FromRow)]
pub struct JobRow {
    pub id: i64,
    pub worker_id: i64,
    pub args: Vec<u8>,
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
                args: self.args.clone(),
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
    // // runner_settings pubsub channel name (for cache clear)
    // const RUNNER_SETTINGS_PUBSUB_CHANNEL_NAME: &'static str = "__runner_settings_pubsub_channel__";

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
    // pubsub channel(streaming result)
    fn job_result_stream_pubsub_channel_name(job_id: &JobId) -> String {
        format!("job_result_streaming:job:{}", job_id.value)
    }
    // pubsub channel
    fn job_result_by_worker_pubsub_channel_name(worker_id: &WorkerId) -> String {
        format!("job_result_changed:worker:{}", worker_id.value)
    }

    // fn serialize_runner_args(args: &RunnerArg) -> Vec<u8> {
    //     let mut buf = Vec::with_capacity(args.encoded_len());
    //     args.encode(&mut buf).unwrap();
    //     buf
    // }

    // fn deserialize_runner_args(buf: &Vec<u8>) -> Result<RunnerArg> {
    //     RunnerArg::decode(&mut Cursor::new(buf)).map_err(|e| JobWorkerError::CodecError(e).into())
    // }

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

    fn serialize_message<T: Message>(args: &T) -> Vec<u8> {
        let mut buf = Vec::with_capacity(args.encoded_len());
        args.encode(&mut buf).unwrap();
        buf
    }

    fn deserialize_message<T: Message + Default>(buf: &[u8]) -> Result<T> {
        T::decode(&mut Cursor::new(buf)).map_err(|e| JobWorkerError::CodecError(e).into())
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
    use proto::jobworkerp::data::{ResponseType, ResultOutput};
    use proto::TestArgs;

    #[test]
    fn test_serialize_and_deserialize_job() {
        let args = JobqueueAndCodec::serialize_message(&TestArgs {
            args: ["test".to_string()].to_vec(),
        });
        let job = Job {
            id: Some(JobId { value: 1 }),
            data: Some(JobData {
                worker_id: Some(WorkerId { value: 1 }),
                args,
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
        let args = JobqueueAndCodec::serialize_message(&TestArgs {
            args: ["test2".to_string()].to_vec(),
        });
        let job_result_data = JobResultData {
            worker_id: Some(WorkerId { value: 2 }),
            worker_name: "hoge2".to_string(),
            job_id: Some(JobId { value: 3 }),
            args,
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
        let serialized = JobQueueImpl::serialize_job_result(id, job_result_data.clone());
        let deserialized = JobQueueImpl::deserialize_job_result(&serialized).unwrap();
        assert_eq!(id, deserialized.id.unwrap());
        assert_eq!(job_result_data, deserialized.data.unwrap());
    }
}
