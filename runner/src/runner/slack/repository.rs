use super::client::{
    Attachment, AttachmentField, ChatPostMessageRequest, ChatPostMessageResponse,
    SlackMessageClientImpl,
};
use crate::jobworkerp::runner::{SlackChatPostMessageArgs, SlackRunnerSettings};
use anyhow::{Context, Result};
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
        let request = Self::convert_to_request(req)?;
        tracing::debug!("slack request: {:?}", serde_json::to_string(&request));
        let response = self
            .client
            .post_message(&self.config.bot_token, &request)
            .await?;
        tracing::debug!("slack response: {:?}", &response);
        serde_json::from_str(&response)
            .map_err(|e| anyhow::anyhow!("failed to parse slack response: {:?}", e))
    }

    pub async fn send_json(&self, req: &serde_json::Value) -> Result<String> {
        let response = self.client.post_json(&self.config.bot_token, req).await;
        tracing::debug!("slack response: {:?}", &response);
        response
    }

    fn convert_to_request(args: &SlackChatPostMessageArgs) -> Result<ChatPostMessageRequest> {
        // Convert blocks: parse each JSON string to serde_json::Value
        let blocks = if args.blocks.is_empty() {
            None
        } else {
            let parsed_blocks: Result<Vec<serde_json::Value>, _> = args
                .blocks
                .iter()
                .enumerate()
                .map(|(i, block_str)| {
                    serde_json::from_str(block_str.trim())
                        .with_context(|| format!("failed to parse block[{i}] as JSON: {block_str}"))
                })
                .collect();
            Some(parsed_blocks?)
        };

        // Convert attachments: map proto Attachment to client Attachment
        let attachments = if args.attachments.is_empty() {
            None
        } else {
            let converted: Vec<Attachment> = args
                .attachments
                .iter()
                .map(|a| Attachment {
                    color: a.color.clone(),
                    author_icon: a.author_icon.clone(),
                    author_link: a.author_link.clone(),
                    author_name: a.author_name.clone(),
                    fallback: a.fallback.clone(),
                    fields: if a.fields.is_empty() {
                        None
                    } else {
                        Some(
                            a.fields
                                .iter()
                                .map(|f| AttachmentField {
                                    title: f.title.clone(),
                                    value: f.value.clone(),
                                    short: f.short,
                                })
                                .collect(),
                        )
                    },
                    footer: a.footer.clone(),
                    footer_icon: a.footer_icon.clone(),
                    image_url: a.image_url.clone(),
                    mrkdwn_in: if a.mrkdwn_in.is_empty() {
                        None
                    } else {
                        Some(a.mrkdwn_in.clone())
                    },
                    pretext: a.pretext.clone(),
                    text: a.text.clone(),
                    title: a.title.clone(),
                    title_link: a.title_link.clone(),
                    thumb_url: a.thumb_url.clone(),
                    ts: a.ts,
                })
                .collect();
            Some(converted)
        };

        Ok(ChatPostMessageRequest {
            channel: args.channel.clone(),
            attachments,
            blocks,
            text: args.text.clone(),
            icon_emoji: args.icon_emoji.clone(),
            icon_url: args.icon_url.clone(),
            link_names: args.link_names,
            mrkdwn: args.mrkdwn,
            parse: args.parse.clone(),
            reply_broadcast: args.reply_broadcast,
            thread_ts: args.thread_ts.clone(),
            unfurl_links: args.unfurl_links,
            unfurl_media: args.unfurl_media,
            username: args.username.clone(),
        })
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
