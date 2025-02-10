// TODO remove (unuse)
use crate::error::JobWorkerError;
use crate::infra::job::rows::{JobqueueAndCodec, UseJobqueueAndCodec};
use crate::infra::runner::RunnerTrait;
use crate::jobworkerp::runner::SlackRunnerSettings;
use anyhow::{anyhow, Result};
use futures::stream::BoxStream;
use proto::jobworkerp::data::{JobResult, JobResultData, JobResultId, ResultStatus};
use proto::jobworkerp::data::{ResultOutputItem, RunnerType};
use serde::Deserialize;
use tonic::async_trait;

use super::slack::repository::SlackRepository;

#[derive(serde::Serialize, serde::Deserialize, Clone, PartialEq, Debug)]
pub struct SlackResultOutput {
    pub items: ::prost::alloc::vec::Vec<::prost::alloc::vec::Vec<u8>>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, PartialEq, Debug)]
pub struct ResultMessageData {
    /// job result id (jobworkerp.data.JobResultId::value)
    pub result_id: i64,
    /// job id (jobworkerp.data.JobId::value)
    pub job_id: i64,
    pub worker_name: ::prost::alloc::string::String,
    /// job result status
    pub status: i32,
    /// job response data
    pub output: ::core::option::Option<SlackResultOutput>,
    pub retried: u32,
    /// job enqueue time
    pub enqueue_time: i64,
    /// job run after this time (specified by client)
    pub run_after_time: i64,
    /// job start time
    pub start_time: i64,
    /// job end time
    pub end_time: i64,
}

#[derive(Clone, Deserialize, Debug, Default)] // for test only
pub struct SlackJobResultConfig {
    pub title: Option<String>,
    pub channel: String,
    pub bot_token: String,
    // pub app_token: String,
    // pub user_token: String,
    pub notify_success: bool,
    pub notify_failure: bool,
}
