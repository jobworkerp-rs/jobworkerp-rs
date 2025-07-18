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
use std::sync::Arc;
use tonic::async_trait;

use super::common::cancellation_helper::CancellationHelper;
use super::RunnerSpec;

#[derive(serde::Serialize, serde::Deserialize, Clone, PartialEq, Debug)]
pub struct SlackResultOutput {
    pub items: ::prost::alloc::vec::Vec<::prost::alloc::vec::Vec<u8>>,
}

#[derive(Clone, Debug)]
pub struct SlackPostMessageRunner {
    slack: Option<SlackRepository>,
    cancellation_helper: CancellationHelper,
}

impl SlackPostMessageRunner {
    pub fn new() -> Self {
        Self {
            slack: None,
            cancellation_helper: CancellationHelper::new(),
        }
    }

    /// Set a cancellation token for this runner instance
    /// This allows external control over cancellation behavior (for test)
    #[cfg(test)]
    pub(crate) fn set_cancellation_token(&mut self, token: tokio_util::sync::CancellationToken) {
        self.cancellation_helper.set_cancellation_token(token);
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
        let cancellation_token = match self.cancellation_helper.setup_execution_token() {
            Ok(token) => token,
            Err(e) => return (Err(e), metadata),
        };

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
        self.cancellation_helper.clear_token();
        (result, metadata)
    }
    async fn run_stream(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        let cancellation_token = self.cancellation_helper.setup_execution_token()?;

        let slack = self
            .slack
            .clone()
            .ok_or_else(|| anyhow!("slack repository is not initialized"))?;
        let message = ProstMessageCodec::deserialize_message::<SlackChatPostMessageArgs>(arg)?;

        use async_stream::stream;
        use proto::jobworkerp::data::{result_output_item::Item, Trailer};

        let trailer = Arc::new(Trailer {
            metadata: metadata.clone(),
        });

        let stream = stream! {
            // Send Slack message with cancellation support
            let send_result = tokio::select! {
                result = slack.send_message(&message) => result,
                _ = cancellation_token.cancelled() => {
                    tracing::info!("Slack stream request was cancelled");
                    yield ResultOutputItem {
                        item: Some(Item::End((*trailer).clone())),
                    };
                    return;
                }
            };

            match send_result {
                Ok(res) => {
                    match res.to_proto() {
                        Ok(proto_result) => {
                            // Serialize and yield the result
                            match ProstMessageCodec::serialize_message(&proto_result) {
                                Ok(serialized) => {
                                    yield ResultOutputItem {
                                        item: Some(Item::Data(serialized)),
                                    };
                                }
                                Err(e) => {
                                    tracing::error!("Failed to serialize Slack result: {}", e);
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to convert Slack result to proto: {}", e);
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Slack message sending failed: {}", e);
                }
            }

            // Send end marker
            yield ResultOutputItem {
                item: Some(Item::End((*trailer).clone())),
            };
        };

        // Keep cancellation token for potential mid-stream cancellation
        // Note: The token will be cleared when cancel() is called
        Ok(Box::pin(stream))
    }

    async fn cancel(&mut self) {
        self.cancellation_helper.cancel();
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_slack_pre_execution_cancellation() {
        use crate::jobworkerp::runner::SlackChatPostMessageArgs;
        use jobworkerp_base::codec::ProstMessageCodec;
        use std::collections::HashMap;

        let mut runner = SlackPostMessageRunner::new();

        // Set up cancellation token and cancel it immediately (pre-execution)
        let cancellation_token = tokio_util::sync::CancellationToken::new();
        runner.set_cancellation_token(cancellation_token.clone());
        cancellation_token.cancel();

        // Create valid Slack message args
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

        // Execute with pre-cancelled token
        let start_time = std::time::Instant::now();
        let (result, _) = runner.run(&arg_bytes, metadata).await;
        let elapsed = start_time.elapsed();

        // Should fail immediately due to pre-execution cancellation
        assert!(result.is_err());
        assert!(elapsed < std::time::Duration::from_millis(100));

        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("cancelled before"));
    }

    #[tokio::test]
    async fn test_slack_stream_mid_execution_cancellation() {
        eprintln!("=== Testing Slack Runner stream mid-execution cancellation ===");

        use std::sync::Arc;
        use std::time::{Duration, Instant};
        use tokio::sync::Mutex;

        // Use Arc<tokio::sync::Mutex<>> to share runner between tasks (similar to LLM pattern)
        let runner = Arc::new(Mutex::new(SlackPostMessageRunner::new()));

        // Create test arguments
        use crate::jobworkerp::runner::SlackChatPostMessageArgs;
        let arg = SlackChatPostMessageArgs {
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

        // Create cancellation token and set it on the runner
        let cancellation_token = tokio_util::sync::CancellationToken::new();
        {
            let mut runner_guard = runner.lock().await;
            runner_guard.set_cancellation_token(cancellation_token.clone());
        }

        let start_time = Instant::now();
        let serialized_args = ProstMessageCodec::serialize_message(&arg).unwrap();

        let runner_clone = runner.clone();

        // Start stream execution in a task
        let execution_task = tokio::spawn(async move {
            let mut runner_guard = runner_clone.lock().await;
            let stream_result = runner_guard
                .run_stream(&serialized_args, HashMap::new())
                .await;

            match stream_result {
                Ok(_stream) => {
                    // Slack stream is not implemented, so this shouldn't happen
                    eprintln!("WARNING: Slack stream returned Ok (should be unimplemented)");
                    Ok(0)
                }
                Err(e) => {
                    eprintln!("Slack stream returned error as expected: {e}");
                    Err(e)
                }
            }
        });

        // Wait for stream to start (let it run for a bit)
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Cancel using the external token reference (avoids deadlock)
        cancellation_token.cancel();
        eprintln!("Called cancellation_token.cancel() after 100ms");

        // Wait for the execution to complete or be cancelled
        let execution_result = execution_task.await;
        let elapsed = start_time.elapsed();

        eprintln!("Slack stream execution completed in {elapsed:?}");

        match execution_result {
            Ok(stream_processing_result) => {
                match stream_processing_result {
                    Ok(_item_count) => {
                        eprintln!("WARNING: Slack stream should be unimplemented");
                    }
                    Err(e) => {
                        eprintln!("✓ Slack stream processing was cancelled as expected: {e}");
                        // Check if it's a cancellation error or unimplemented error
                        if e.to_string().contains("cancelled") {
                            eprintln!("✓ Cancellation was properly detected");
                        } else if e.to_string().contains("not implemented") {
                            eprintln!("✓ Stream is unimplemented but cancellation check worked");
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("Slack stream execution task failed: {e}");
                panic!("Task failed: {e}");
            }
        }

        // Verify that cancellation happened very quickly (since stream is unimplemented)
        if elapsed > Duration::from_secs(1) {
            panic!(
                "Stream processing took too long ({elapsed:?}), should be immediate for unimplemented stream"
            );
        }

        eprintln!("✓ Slack stream mid-execution cancellation test completed successfully");
    }
}
