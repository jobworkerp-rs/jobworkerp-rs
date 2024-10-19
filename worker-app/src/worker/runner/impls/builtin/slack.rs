pub mod client;
pub mod repository;

use crate::worker::runner::Runner;

use self::repository::SlackRepository;
use crate::jobworkerp::runner::{SlackJobResultArg, SlackJobResultOperation};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use infra::error::JobWorkerError;
use infra::infra::job::rows::{JobqueueAndCodec, UseJobqueueAndCodec};
use once_cell::sync::Lazy;
use proto::jobworkerp::data::ResultStatus;
use proto::jobworkerp::data::{
    QueueType, ResponseType, RetryPolicy, RetryType, Worker, WorkerData, WorkerSchemaId,
};
use serde::Deserialize;
pub const SLACK_WORKER_NAME: &str = "__SLACK_NOTIFICATION_WORKER__"; //XXX
pub const SLACK_RUNNER_OPERATION: crate::jobworkerp::runner::SlackJobResultOperation =
    SlackJobResultOperation {};

/// treat arg as serialized JobResult
pub static SLACK_WORKER: Lazy<Worker> = Lazy::new(|| Worker {
    id: Some(super::BuiltinWorkerIds::SlackWorkerId.to_worker_id()),
    data: Some(WorkerData {
        name: SLACK_WORKER_NAME.to_string(),
        schema_id: Some(WorkerSchemaId { value: 0 }),
        operation: JobqueueAndCodec::serialize_message(&SLACK_RUNNER_OPERATION),
        channel: None,
        response_type: ResponseType::NoResult as i32,
        periodic_interval: 0,
        retry_policy: Some(RetryPolicy {
            r#type: RetryType::Exponential as i32,
            interval: 1000,
            max_interval: 20000,
            max_retry: 3,
            basis: 2.0,
        }),
        queue_type: QueueType::Redis as i32,
        store_failure: false,
        store_success: false,
        next_workers: vec![],
        use_static: false,
    }),
});

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
        include_str!("../../../../../protobuf/jobworkerp/runner/slack_operation.proto").to_string()
    }
    fn job_args_proto(&self) -> String {
        include_str!("../../../../../protobuf/jobworkerp/runner/slack_args.proto").to_string()
    }
    fn use_job_result(&self) -> bool {
        true
    }
}
