pub mod client;
pub mod repository;

use crate::worker::runner::Runner;

use self::repository::SlackRepository;
use anyhow::{anyhow, Context, Result};
use app::app::worker::builtin::slack::SLACK_RUNNER_NAME;
use async_trait::async_trait;
use infra::error::JobWorkerError;
use prost::Message;
use proto::jobworkerp::data::{JobResult, ResultStatus};
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
        String::from(SLACK_RUNNER_NAME)
    }
    async fn run(&mut self, arg: Vec<u8>) -> Result<Vec<Vec<u8>>> {
        let res =
            JobResult::decode(arg.as_slice()).context("decode JobResult in slack notification")?;
        match res {
            JobResult {
                id: Some(id),
                data: Some(data),
            } => {
                let status = data.status;
                // result in success or error -> notify to slack
                if status == ResultStatus::Success as i32
                    || status != ResultStatus::ErrorAndRetry as i32
                {
                    tracing::debug!("try to send slack notification: result id={}", &id.value);
                    let r = self
                        .slack
                        .send_result(&data, status != ResultStatus::Success as i32)
                        .await;
                    match r {
                        Ok(()) => {
                            tracing::debug!("slack notification was sent: result_id={}", &id.value);
                            Ok(vec!["OK".bytes().collect()])
                        }
                        Err(e) => {
                            tracing::error!("slack error: result_id: {:?} {:?}", &id, e);
                            Err(anyhow!("slack error: {:?}", e))
                        }
                    }
                } else {
                    tracing::error!("no data in job result: {:?}", &data);
                    Err(anyhow!("no data in job result: {:?}", &data))
                }
            }
            jr => {
                tracing::error!("cannot decode all job result data: {jr:?}");
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
}
