pub mod client;
pub mod repository;

use crate::worker::runner::Runner;

use self::repository::SlackRepository;
use anyhow::{anyhow, Result};
use app::app::worker::builtin::slack::SLACK_WORKER_NAME;
use async_trait::async_trait;
use infra::{
    error::JobWorkerError,
    infra::job::rows::{JobqueueAndCodec, UseJobqueueAndCodec},
};
use proto::jobworkerp::data::{ResultStatus, SlackJobResultArg};
use serde::Deserialize;

#[derive(Clone, Deserialize, Debug, Default)] // for test only
pub struct SlackConfig {
    pub title: Option<String>,
    pub channel: String,
    pub bot_token: String,
    // pub app_token: String,
    // pub user_token: String,
    pub notify_success: bool,
    pub notify_failure: bool,
}

#[derive(Clone, Debug)]
pub struct SlackResultNotificationRunner {
    slack: SlackRepository,
}

impl SlackResultNotificationRunner {
    pub fn new() -> Self {
        Self {
            slack: SlackRepository::new_by_env(),
        }
    }
}

impl Default for SlackResultNotificationRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Runner for SlackResultNotificationRunner {
    async fn name(&self) -> String {
        String::from(SLACK_WORKER_NAME)
    }
    async fn run(&mut self, arg: &[u8]) -> Result<Vec<Vec<u8>>> {
        let res = JobqueueAndCodec::deserialize_message::<SlackJobResultArg>(arg)?;
        match &res.message {
            // XXX not use channel ()
            Some(data) => {
                let status = data.status;
                // result in success or error -> notify to slack
                if status == ResultStatus::Success as i32
                    || status != ResultStatus::ErrorAndRetry as i32
                {
                    tracing::debug!(
                        "try to send slack notification: result id={}",
                        &data.result_id
                    );
                    let r = self
                        .slack
                        .send_result(data, status != ResultStatus::Success as i32)
                        .await;
                    match r {
                        Ok(()) => {
                            tracing::debug!(
                                "slack notification was sent: result_id={}",
                                &data.result_id
                            );
                            Ok(vec!["OK".bytes().collect()])
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
            }
            jr => {
                tracing::error!("cannot get job result data: {jr:?}");
                Err(
                    JobWorkerError::InvalidParameter(format!("cannot get job result data: {jr:?}"))
                        .into(),
                )
            }
        }
    }

    async fn cancel(&mut self) {
        // do nothing
    }
    fn operation_proto(&self) -> String {
        include_str!("../../../../../protobuf/slack_operation.proto").to_string()
    }
    fn job_args_proto(&self) -> String {
        include_str!("../../../../../protobuf/slack_job_args.proto").to_string()
    }
    fn use_job_result(&self) -> bool {
        true
    }
}
