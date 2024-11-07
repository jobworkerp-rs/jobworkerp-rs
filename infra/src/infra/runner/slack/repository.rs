use std::borrow::Cow;

use super::client::{Attachment, AttachmentField, PostMessageRequest, SlackMessageClientImpl};
use crate::jobworkerp::runner::ResultMessageData;
use anyhow::Result;
use command_utils::util::datetime;
use itertools::Itertools;

use super::SlackConfig;

#[derive(Clone, Debug)]
pub struct SlackRepository {
    pub config: SlackConfig,
    pub client: SlackMessageClientImpl,
}

impl SlackRepository {
    //const TEXT_LEN: usize = 40000;
    const TEXT_LEN: usize = 38000; // rest 2000 chars
    pub fn new(config: SlackConfig) -> Self {
        Self {
            config,
            client: SlackMessageClientImpl::new(),
        }
    }

    pub async fn send_result(&self, res: &ResultMessageData, is_error: bool) -> Result<()> {
        if is_error && !self.config.notify_failure || !is_error && !self.config.notify_success {
            // not notify by setting and status
            tracing::debug!(
                "not notify by setting and status. job_id: {:?}, worker: {}",
                &res.job_id,
                &res.worker_name
            );
            return Ok(());
        }
        let data: Vec<Cow<str>> = res
            .output
            .as_ref()
            .map(|l| {
                l.items
                    .iter()
                    .map(|t| String::from_utf8_lossy(t))
                    .collect_vec()
            })
            .unwrap_or_default();

        // TODO outout multiple results properly
        let text = data.join("\n\n");
        if text.trim().is_empty() {
            // not notify without data
            tracing::info!(
                "not notify empty data. worker: {}, job_id: {:?}",
                &res.worker_name,
                &res.job_id,
            );
            return Ok(());
        }
        let param = self.build_message(res, &text, is_error);
        let response = self
            .client
            .post_message(&self.config.bot_token, &param)
            .await;
        tracing::debug!("slack response: {:?}", response);
        Ok(())
    }

    fn build_message(
        &self,
        res: &ResultMessageData,
        text: &str,
        is_error: bool,
    ) -> PostMessageRequest {
        let title = self
            .config
            .title
            .clone()
            .unwrap_or("job result".to_string());
        // extract first line as article_title from res.data, rest of lines as text
        let article_title = text.lines().next().unwrap_or("");
        let mut text = text.lines().skip(1).collect::<Vec<&str>>().join("\n");
        text.truncate(Self::TEXT_LEN);
        let mut fields = vec![];
        fields.extend(if res.run_after_time > 0 {
            vec![AttachmentField {
                title: Some("Scheduled time".to_string()),
                value: Some(datetime::from_epoch_milli(res.run_after_time).to_string()),
                short: Some(true),
            }]
        } else {
            vec![]
        });
        fields.extend(vec![
            AttachmentField {
                title: Some("Start time".to_string()),
                value: Some(datetime::from_epoch_milli(res.start_time).to_string()),
                short: Some(true),
            },
            AttachmentField {
                title: Some("End time".to_string()),
                value: Some(datetime::from_epoch_milli(res.end_time).to_string()),
                short: Some(true),
            },
            AttachmentField {
                title: Some("Worker".to_string()),
                value: Some(res.worker_name.clone()),
                short: Some(true),
            },
        ]);

        PostMessageRequest {
            channel: self.config.channel.clone(),
            text: None,
            attachments: Some(vec![Attachment {
                color: Some(if is_error {
                    "#ff0000".to_string()
                } else {
                    "#36f64f".to_string()
                }),
                author_icon: None,
                author_link: None,
                author_name: Some(title),
                fields: Some(fields),
                // footer: Some("footer".to_string()),
                // footer_icon: Some("https://example.com/test.png".to_string(), ),
                text: Some(text),
                title: Some(article_title.to_string()),
                title_link: None, // TODO link to page
                pretext: None,
                thumb_url: None,
                ts: Some(datetime::now_seconds() as i32),
                ..Default::default()
            }]),
            ..Default::default()
        }
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
