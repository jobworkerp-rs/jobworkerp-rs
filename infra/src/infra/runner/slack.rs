pub mod client;
pub mod repository;

use self::repository::SlackRepository;
use crate::error::JobWorkerError;
use crate::infra::job::rows::{JobqueueAndCodec, UseJobqueueAndCodec};
use crate::infra::runner::RunnerTrait;
use crate::jobworkerp::runner::SlackNotificationRunnerSettings;
use anyhow::{anyhow, Result};
use proto::jobworkerp::data::RunnerType;
use tonic::async_trait;

#[derive(serde::Serialize, serde::Deserialize, Clone, PartialEq, Debug)]
pub struct SlackResultOutput {
    pub items: ::prost::alloc::vec::Vec<::prost::alloc::vec::Vec<u8>>,
}

#[derive(Clone, Debug)]
pub struct SlackNotificationRunner {
    slack: Option<SlackRepository>,
}

impl SlackNotificationRunner {
    pub fn new() -> Self {
        Self { slack: None }
    }
}

impl Default for SlackNotificationRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RunnerTrait for SlackNotificationRunner {
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
            let message = String::from_utf8_lossy(arg);
            // not validate json structure for slack
            let req = serde_json::from_str(&message)?;
            tracing::debug!("try to send slack message: {:?}", &req);
            let r = slack.send_json(&req).await;
            match r {
                Ok(res) => {
                    tracing::debug!("slack notification was sent: {}", &res);
                    Ok(vec![])
                }
                Err(e) => {
                    tracing::error!("slack error: {:?}", &e);
                    Err(anyhow!("slack error: {:?}", e))
                }
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
    // use json string
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
