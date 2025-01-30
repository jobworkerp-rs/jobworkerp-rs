// TODO remove (unuse)
use crate::error::JobWorkerError;
use crate::infra::job::rows::{JobqueueAndCodec, UseJobqueueAndCodec};
use crate::infra::runner::RunnerTrait;
use crate::jobworkerp::runner::SlackNotificationRunnerSettings;
use anyhow::{anyhow, Result};
use proto::jobworkerp::data::RunnerType;
use proto::jobworkerp::data::{JobResult, JobResultData, JobResultId, ResultStatus};
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
impl From<SlackNotificationRunnerSettings> for SlackJobResultConfig {
    fn from(op: SlackNotificationRunnerSettings) -> Self {
        Self {
            title: if op.title.is_empty() {
                None
            } else {
                Some(op.title)
            },
            channel: op.channel,
            bot_token: op.bot_token,
            notify_success: op.notify_success,
            notify_failure: op.notify_failure,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SlackResultNotificationRunner {
    slack: Option<SlackRepository>,
}

impl SlackResultNotificationRunner {
    pub fn new() -> Self {
        Self { slack: None }
    }
    fn job_result_to_message(id: &JobResultId, dat: &JobResultData) -> ResultMessageData {
        ResultMessageData {
            result_id: id.value,
            job_id: dat.job_id.as_ref().map(|j| j.value).unwrap_or(0),
            worker_name: dat.worker_name.clone(),
            status: dat.status,
            output: dat.output.as_ref().map(|out| SlackResultOutput {
                items: out.items.clone(),
            }),
            retried: dat.retried,
            enqueue_time: dat.enqueue_time,
            run_after_time: dat.run_after_time,
            start_time: dat.start_time,
            end_time: dat.end_time,
        }
    }
}

impl Default for SlackResultNotificationRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RunnerTrait for SlackResultNotificationRunner {
    fn name(&self) -> String {
        RunnerType::SlackNotification.as_str_name().to_string()
    }
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        let res =
            JobqueueAndCodec::deserialize_message::<SlackNotificationRunnerSettings>(&settings)?;
        self.slack = Some(SlackRepository::new(res.into()));
        Ok(())
    }
    async fn run(&mut self, arg: &[u8]) -> Result<Vec<Vec<u8>>> {
        if let Some(slack) = self.slack.as_ref() {
            let job_res = JobqueueAndCodec::deserialize_message::<JobResult>(arg)?;
            if let JobResult {
                id: Some(jid),
                data: Some(jdata),
            } = &job_res
            {
                let data = Self::job_result_to_message(jid, jdata);
                // XXX not use channel ()
                let status = data.status;
                // result in success or error -> notify to slack
                if status == ResultStatus::Success as i32
                    || status != ResultStatus::ErrorAndRetry as i32
                {
                    tracing::debug!(
                        "try to send slack notification: result id={}",
                        &data.result_id
                    );
                    let r = slack
                        .send_result(&data, status != ResultStatus::Success as i32, true, true) // XXX
                        .await;
                    match r {
                        Ok(()) => {
                            tracing::debug!(
                                "slack notification was sent: result_id={}",
                                &data.result_id
                            );
                            Ok(vec![])
                        }
                        Err(e) => {
                            tracing::error!(
                                "slack error: result_id: {:?} {:?}",
                                &data.result_id,
                                e
                            );
                            Err(anyhow!("slack error: {:?}", e))
                        }
                    }
                } else {
                    tracing::error!("no data in job result: {:?}", &data);
                    Err(anyhow!("no data in job result: {:?}", &data))
                }
            } else {
                Err(JobWorkerError::InvalidParameter(
                    "slack repository is not initialized".to_string(),
                )
                .into())
            }
        } else {
            Err(
                JobWorkerError::InvalidParameter("slack repository is not initialized".to_string())
                    .into(),
            )
        }
    }

    async fn cancel(&mut self) {
        // do nothing
    }
    fn runner_settings_proto(&self) -> String {
        include_str!("../../../protobuf/jobworkerp/runner/slack_runner.proto").to_string()
    }
    // use JobResult as job_args
    fn job_args_proto(&self) -> String {
        "".to_string()
    }
    fn result_output_proto(&self) -> Option<String> {
        None
    }
    fn use_job_result(&self) -> bool {
        true
    }
}
