use super::client::{ChatPostMessageResponse, SlackMessageClientImpl};
use crate::jobworkerp::runner::{SlackChatPostMessageArgs, SlackRunnerSettings};
use anyhow::Result;
use serde::Deserialize;

#[derive(Clone, Deserialize, Debug, Default)] // for test only
pub struct SlackConfig {
    pub bot_name: Option<String>, //
    pub bot_token: String,        // required
}
impl From<SlackRunnerSettings> for SlackConfig {
    fn from(op: SlackRunnerSettings) -> Self {
        Self {
            bot_name: op.bot_name,
            bot_token: op.bot_token,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SlackRepository {
    pub config: SlackConfig,
    pub client: SlackMessageClientImpl,
}

impl SlackRepository {
    //const TEXT_LEN: usize = 40000;
    pub fn new(config: SlackConfig) -> Self {
        Self {
            config,
            client: SlackMessageClientImpl::new(),
        }
    }
    pub async fn send_message(
        &self,
        req: &SlackChatPostMessageArgs,
    ) -> Result<ChatPostMessageResponse> {
        let response = self.send_json(&serde_json::to_value(req)?).await;
        serde_json::from_str(&response?)
            .map_err(|e| anyhow::anyhow!("failed to parse slack response: {:?}", e))
    }
    pub async fn send_json(&self, req: &serde_json::Value) -> Result<String> {
        let response = self.client.post_json(&self.config.bot_token, req).await;
        tracing::debug!("slack response: {:?}", &response);
        response
    }
}

// // temporary send test
// #[tokio::test]
// async fn send_test() {
//     let slack = SlackRepository::new(__TODO__);
//     let result = JobResultData {
//         job_id: None,
//         worker_id: Some(WorkerId { value: 1 }),
//         worker_name: "testWorker".to_string(),
//         arg: vec![1, 2, 3],
//         uniq_key: None,
//         status: 0,
//         data: "response test".as_bytes().to_vec(),
//         retried: 0,
//         max_retry: 0,
//         priority: 0,
//         timeout: 0,
//         enqueue_time: 0,
//         run_after_time: 0,
//         start_time: datetime::now_millis() - 3600,
//         end_time: datetime::now_millis(),
//         response_type: 0,
//         store_success: false,
//         store_failure: false,
//     };
//     let res = slack.send_result(&result, true).await;
//     println!("{:?}", res);
//     assert!(res.is_ok());
// }
