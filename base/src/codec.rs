use crate::error::JobWorkerError;
use anyhow::Result;
use prost::Message;
use std::io::Cursor;

// use job queue (store and the data)
pub trait UseProstCodec {
    // // TODO specify arbitrary job channel name
    // const DEFAULT_CHANNEL_NAME: &'static str = "__default_job_channel__";

    // // worker pubsub channel name (for cache clear)
    // const WORKER_PUBSUB_CHANNEL_NAME: &'static str = "__worker_pubsub_channel__";
    // // // runner_settings pubsub channel name (for cache clear)
    // // const RUNNER_SETTINGS_PUBSUB_CHANNEL_NAME: &'static str = "__runner_settings_pubsub_channel__";

    // // job queue channel name with channel name
    // fn queue_channel_name(channel_name: impl Into<String>, p: Option<&i32>) -> String {
    //     format!("q{}:{}:", p.unwrap_or(&0), channel_name.into())
    // }

    // // channel name for job with run_after_time (no priority)
    // fn run_after_job_zset_key() -> String {
    //     "rc:zset".to_string()
    // }

    fn serialize_message<T: Message>(args: &T) -> Vec<u8> {
        let mut buf = Vec::with_capacity(args.encoded_len());
        args.encode(&mut buf).unwrap();
        buf
    }

    fn deserialize_message<T: Message + Default>(buf: &[u8]) -> Result<T> {
        T::decode(&mut Cursor::new(buf)).map_err(|e| JobWorkerError::CodecError(e).into())
    }
}

pub struct ProstMessageCodec {}
impl UseProstCodec for ProstMessageCodec {}
