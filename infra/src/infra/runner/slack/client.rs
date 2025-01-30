use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

/// slach post chat message api
/// https://api.slack.com/methods/chat.postMessage
#[derive(Clone, Debug)]
pub struct SlackMessageClientImpl {
    client: reqwest::Client,
}
impl SlackMessageClientImpl {
    const SLACK_API_BASE_URL: &'static str = "https://slack.com/api/";

    fn get_slack_url(method: &str) -> String {
        format!("{}{}", Self::SLACK_API_BASE_URL, method)
    }
    fn get_post_message_url() -> String {
        Self::get_slack_url("chat.postMessage")
    }
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
    pub async fn post_message(&self, token: &str, param: &PostMessageRequest) -> Result<String> {
        let json = serde_json::json!(param);
        self.post_json(token, &json).await
    }
    pub async fn post_json(&self, token: &str, json: &serde_json::Value) -> Result<String> {
        let parsed_url = url::Url::parse(&Self::get_post_message_url())?;

        match self
            .client
            .post(parsed_url)
            .header("Authorization", format!("Bearer {}", token))
            .header("Content-type", "application/json; charset=utf-8")
            .body(json.to_string())
            .send()
            .await
        {
            Ok(res) => {
                if res.status() != reqwest::StatusCode::OK {
                    return Err(anyhow::anyhow!("slack error: {:?}", res));
                }
                let res = res
                    .error_for_status()
                    .map_err(|e| anyhow::anyhow!("slack error: {:?}", e))?
                    .bytes()
                    .await?;
                Ok(String::from_utf8_lossy(res.as_ref()).to_string())
            }
            Err(e) => Err(anyhow::anyhow!("slack error: {:?}", e)),
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Default, PartialEq)]
pub struct PostMessageRequest {
    pub channel: String,
    pub attachments: Option<Vec<Attachment>>,
    pub text: Option<String>,
    pub icon_emoji: Option<String>,
    pub icon_url: Option<String>,
    pub link_names: Option<bool>,
    pub mrkdwn: Option<bool>,
    pub parse: Option<String>,
    pub reply_broadcast: Option<bool>,
    pub thread_ts: Option<String>,
    pub unfurl_links: Option<bool>,
    pub unfurl_media: Option<bool>,
    pub username: Option<String>,
}

/// See: <https://api.slack.com/reference/messaging/attachments#legacy_fields>
#[skip_serializing_none]
#[derive(Deserialize, Serialize, Debug, Default, PartialEq)]
pub struct Attachment {
    pub color: Option<String>,
    pub author_icon: Option<String>,
    pub author_link: Option<String>,
    pub author_name: Option<String>,
    pub fallback: Option<String>,
    pub fields: Option<Vec<AttachmentField>>,
    pub footer: Option<String>,
    pub footer_icon: Option<String>,
    pub image_url: Option<String>,
    pub mrkdwn_in: Option<Vec<String>>,
    pub pretext: Option<String>,
    pub text: Option<String>,
    pub title: Option<String>,
    pub title_link: Option<String>,
    pub thumb_url: Option<String>,
    pub ts: Option<i32>,
}

/// See: <https://api.slack.com/reference/messaging/attachments#field_objects>
#[skip_serializing_none]
#[derive(Deserialize, Serialize, Debug, Default, PartialEq)]
pub struct AttachmentField {
    pub title: Option<String>,
    pub value: Option<String>,
    pub short: Option<bool>,
}
