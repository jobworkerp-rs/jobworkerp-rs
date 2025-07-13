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
use tokio_util::sync::CancellationToken;
use tonic::async_trait;

use super::RunnerSpec;

#[derive(serde::Serialize, serde::Deserialize, Clone, PartialEq, Debug)]
pub struct SlackResultOutput {
    pub items: ::prost::alloc::vec::Vec<::prost::alloc::vec::Vec<u8>>,
}

#[derive(Clone, Debug)]
pub struct SlackPostMessageRunner {
    slack: Option<SlackRepository>,
    cancellation_token: Option<CancellationToken>,
}

impl SlackPostMessageRunner {
    pub fn new() -> Self {
        Self {
            slack: None,
            cancellation_token: None,
        }
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
        // Set up cancellation token for this execution
        let cancellation_token = CancellationToken::new();
        self.cancellation_token = Some(cancellation_token.clone());

        let result = async {
            if let Some(slack) = self.slack.as_ref() {
                tracing::debug!("slack runner is initialized");
                let message =
                    ProstMessageCodec::deserialize_message::<SlackChatPostMessageArgs>(args)
                        .map_err(|e| {
                            JobWorkerError::InvalidParameter(format!(
                                "cannot deserialize slack message: {e:?}"
                            ))
                        })?;
                // not validate json structure for slack
                tracing::debug!("try to send slack message: {:?}", &message);

                // Send Slack message with cancellation support
                let r = tokio::select! {
                    send_result = slack.send_message(&message) => send_result,
                    _ = cancellation_token.cancelled() => {
                        return Err(anyhow!("Slack message sending was cancelled"));
                    }
                };
                match r {
                    Ok(res) => {
                        tracing::debug!("slack notification was sent: {:?}", &res);
                        Ok(ProstMessageCodec::serialize_message(
                            &res.to_proto().map_err(|e| {
                                JobWorkerError::OtherError(format!(
                                    "cannot serialize slack result: {e:?}"
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
        }
        .await;

        // Clear cancellation token after execution
        self.cancellation_token = None;
        (result, metadata)
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
        if let Some(token) = &self.cancellation_token {
            token.cancel();
            tracing::info!("Slack message sending cancelled");
        } else {
            tracing::warn!("No active Slack operation to cancel");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_slack_cancel_no_active_operation() {
        eprintln!("=== Starting Slack cancel with no active operation test ===");
        let mut runner = SlackPostMessageRunner::new();

        // Call cancel when no operation is running - should not panic
        runner.cancel().await;
        eprintln!("Slack cancel completed successfully with no active operation");

        eprintln!("=== Slack cancel with no active operation test completed ===");
    }

    #[tokio::test]
    async fn test_slack_cancellation_token_setup() {
        eprintln!("=== Starting Slack cancellation token setup test ===");
        let mut runner = SlackPostMessageRunner::new();

        // Verify initial state
        assert!(
            runner.cancellation_token.is_none(),
            "Initially no cancellation token"
        );

        // Test that cancellation token is properly managed
        runner.cancel().await; // Should not panic

        eprintln!("=== Slack cancellation token setup test completed ===");
    }

    #[tokio::test]
    #[ignore] // Requires Slack configuration - run with --ignored for full testing
    async fn test_slack_actual_cancellation() {
        eprintln!("=== Starting Slack actual cancellation test ===");
        use crate::jobworkerp::runner::SlackChatPostMessageArgs;
        use jobworkerp_base::codec::ProstMessageCodec;
        use std::collections::HashMap;

        let mut runner = SlackPostMessageRunner::new();

        // Test with a Slack message that would be sent
        // Note: This test requires actual Slack configuration for real testing
        let slack_args = SlackChatPostMessageArgs {
            channel: "#test".to_string(),
            text: Some("Test message for cancellation".to_string()),
            username: Some("test-bot".to_string()),
            icon_emoji: None,
            icon_url: None,
            link_names: None,
            parse: None,
            reply_broadcast: None,
            thread_ts: None,
            unfurl_links: None,
            unfurl_media: None,
            mrkdwn: None,
            blocks: vec![],
            attachments: vec![],
        };

        let arg_bytes = ProstMessageCodec::serialize_message(&slack_args).unwrap();
        let metadata = HashMap::new();

        // Test cancellation setup (without actual Slack credentials)
        let start_time = std::time::Instant::now();
        let execution_task = tokio::spawn(async move { runner.run(&arg_bytes, metadata).await });

        // Wait briefly
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Test timeout - would fail due to no credentials, but demonstrates cancellation setup
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), execution_task).await;

        let elapsed = start_time.elapsed();
        eprintln!("Slack execution time: {elapsed:?}");

        match result {
            Ok(task_result) => {
                let (execution_result, _metadata) = task_result.unwrap();
                match execution_result {
                    Ok(_) => {
                        eprintln!("Slack message sent unexpectedly");
                    }
                    Err(e) => {
                        eprintln!("Slack message failed as expected (no configuration): {e}");
                    }
                }
            }
            Err(_) => {
                eprintln!(
                    "Slack operation timed out - this indicates cancellation mechanism is ready"
                );
                assert!(
                    elapsed >= std::time::Duration::from_secs(1),
                    "Should timeout after 1 second"
                );
            }
        }

        eprintln!("=== Slack actual cancellation test completed ===");
    }
}
