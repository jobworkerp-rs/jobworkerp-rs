pub mod client;
pub mod repository;

use self::repository::SlackRepository;
use crate::jobworkerp::runner::{
    SlackChatPostMessageArgs, SlackChatPostMessageResult, SlackRunnerSettings,
};
use crate::runner::RunnerTrait;
use crate::{schema_to_json_string, schema_to_json_string_option};
use anyhow::{anyhow, Result};
use futures::stream::BoxStream;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{ResultOutputItem, RunnerType, StreamingOutputType};
use std::collections::HashMap;
use tonic::async_trait;

use super::RunnerSpec;

#[derive(serde::Serialize, serde::Deserialize, Clone, PartialEq, Debug)]
pub struct SlackResultOutput {
    pub items: ::prost::alloc::vec::Vec<::prost::alloc::vec::Vec<u8>>,
}

#[derive(Clone, Debug)]
pub struct SlackPostMessageRunner {
    slack: Option<SlackRepository>,
}

impl SlackPostMessageRunner {
    pub fn new() -> Self {
        Self { slack: None }
    }
}

impl Default for SlackPostMessageRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl RunnerSpec for SlackPostMessageRunner {
    fn name(&self) -> String {
        RunnerType::SlackPostMessage.as_str_name().to_string()
    }
    fn runner_settings_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/slack_runner.proto").to_string()
    }
    // use JobResult as job_args
    fn job_args_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/slack_args.proto").to_string()
    }
    fn result_output_proto(&self) -> Option<String> {
        Some(include_str!("../../protobuf/jobworkerp/runner/slack_result.proto").to_string())
    }
    fn output_type(&self) -> StreamingOutputType {
        StreamingOutputType::NonStreaming
    }
    fn settings_schema(&self) -> String {
        schema_to_json_string!(SlackRunnerSettings, "settings_schema")
    }
    fn arguments_schema(&self) -> String {
        schema_to_json_string!(SlackChatPostMessageArgs, "arguments_schema")
    }
    fn output_schema(&self) -> Option<String> {
        schema_to_json_string_option!(SlackChatPostMessageResult, "output_schema")
    }
}
#[async_trait]
impl RunnerTrait for SlackPostMessageRunner {
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        let res = ProstMessageCodec::deserialize_message::<SlackRunnerSettings>(&settings)?;
        self.slack = Some(SlackRepository::new(res.into()));
        Ok(())
    }
    async fn run(
        &mut self,
        args: &[u8],
        metadata: HashMap<String, String>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        let result = async {
            if let Some(slack) = self.slack.as_ref() {
                tracing::debug!("slack runner is initialized");
                let message =
                    ProstMessageCodec::deserialize_message::<SlackChatPostMessageArgs>(args)
                        .map_err(|e| {
                            JobWorkerError::InvalidParameter(format!(
                                "cannot deserialize slack message: {:?}",
                                e
                            ))
                        })?;
                // not validate json structure for slack
                tracing::debug!("try to send slack message: {:?}", &message);
                let r = slack.send_message(&message).await;
                match r {
                    Ok(res) => {
                        tracing::debug!("slack notification was sent: {:?}", &res);
                        Ok(ProstMessageCodec::serialize_message(
                            &res.to_proto().map_err(|e| {
                                JobWorkerError::OtherError(format!(
                                    "cannot serialize slack result: {:?}",
                                    e
                                ))
                            })?,
                        )?)
                    }
                    Err(e) => {
                        tracing::error!("slack error: {:?}", &e);
                        Err(anyhow!("slack error: {:?}", e))
                    }
                }
            } else {
                Err(JobWorkerError::InvalidParameter(
                    "slack repository is not initialized".to_string(),
                )
                .into())
            }
        };
        (result.await, metadata)
    }
    async fn run_stream(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        // default implementation (return empty)
        let _ = (arg, metadata);
        Err(anyhow::anyhow!("not implemented"))
    }

    async fn cancel(&mut self) {
        // do nothing
    }
}
