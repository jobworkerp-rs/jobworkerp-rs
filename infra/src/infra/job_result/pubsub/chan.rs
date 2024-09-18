use std::{sync::Arc, time::Duration};

use anyhow::Result;
use async_trait::async_trait;
use command_utils::util::result::TapErr;
use infra_utils::infra::chan::{
    broadcast::{BroadcastChan, UseBroadcastChanBuffer},
    ChanBuffer, ChanBufferItem,
};
use proto::jobworkerp::data::{JobId, JobResult, JobResultData, JobResultId};

use crate::infra::{job::rows::UseJobqueueAndCodec, JobQueueConfig, UseJobQueueConfig};

use super::{JobResultPublisher, JobResultSubscriber};

/// publish job result to channel
/// (note: not broadcast to all receiver: assume single instance use)
#[async_trait]
impl JobResultPublisher for ChanJobResultPubSubRepositoryImpl {
    async fn publish_result(&self, id: &JobResultId, data: &JobResultData) -> Result<bool> {
        let jid = data.job_id.as_ref().unwrap();
        tracing::debug!(
            "publish_result: job_id={}, result_id={}",
            &jid.value,
            &id.value
        );
        let result_data = Self::serialize_job_result(*id, data.clone());
        self.broadcast_chan_buf()
            .send_to_chan(
                Self::job_result_pubsub_channel_name(jid).as_str(),
                result_data,
                None,
                Some(&Duration::from_secs(
                    self.job_queue_config().expire_job_result_seconds as u64,
                )),
            )
            .await
            .inspect(|&r| {
                if !r {
                    tracing::warn!("publish_result: not sent: job_id={}", &jid.value);
                }
            })
    }
}

#[async_trait]
impl JobResultSubscriber for ChanJobResultPubSubRepositoryImpl {
    // subscribe job result of listen after by all listening client using broadcast_chan and return got result immediately
    async fn subscribe_result(&self, job_id: &JobId, timeout: Option<u64>) -> Result<JobResult> {
        let cn = Self::job_result_pubsub_channel_name(job_id);
        tracing::debug!("subscribe_result: job_id={}, ch={}", &job_id.value, &cn);

        let message = self
            .broadcast_chan_buf()
            .receive_from_chan(
                cn.as_str(),
                timeout.map(Duration::from_millis),
                Some(&Duration::from_secs(
                    self.job_queue_config().expire_job_result_seconds as u64,
                )),
            )
            .await
            .tap_err(|e| tracing::error!("receive_from_chan_err:{:?}", e))?;
        let res = Self::deserialize_job_result(&message)
            .tap_err(|e| tracing::error!("deserialize_result:{:?}", e))?;
        tracing::debug!("subscribe_result_received: result={:?}", &res);
        Ok(res)
    }
}

#[derive(Clone, Debug)]
pub struct ChanJobResultPubSubRepositoryImpl {
    broadcast_chan_buf: ChanBuffer<Vec<u8>, BroadcastChan<ChanBufferItem<Vec<u8>>>>,
    job_queue_config: Arc<JobQueueConfig>,
}
impl ChanJobResultPubSubRepositoryImpl {
    pub fn new(
        broadcast_chan_buf: ChanBuffer<Vec<u8>, BroadcastChan<ChanBufferItem<Vec<u8>>>>,
        job_queue_config: Arc<JobQueueConfig>,
    ) -> Self {
        ChanJobResultPubSubRepositoryImpl {
            broadcast_chan_buf,
            job_queue_config,
        }
    }
}
impl UseBroadcastChanBuffer for ChanJobResultPubSubRepositoryImpl {
    type Item = Vec<u8>;
    fn broadcast_chan_buf(
        &self,
    ) -> &ChanBuffer<Self::Item, BroadcastChan<ChanBufferItem<Self::Item>>> {
        &self.broadcast_chan_buf
    }
}

impl UseJobqueueAndCodec for ChanJobResultPubSubRepositoryImpl {}
impl UseJobQueueConfig for ChanJobResultPubSubRepositoryImpl {
    fn job_queue_config(&self) -> &JobQueueConfig {
        &self.job_queue_config
    }
}
pub trait UseChanJobResultPubSubRepository {
    fn job_result_pubsub_repository(&self) -> &ChanJobResultPubSubRepositoryImpl;
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::infra::JobQueueConfig;
    use anyhow::Result;
    use infra_utils::infra::chan::ChanBuffer;
    use proto::jobworkerp::data::{JobResult, JobResultData, JobResultId, ResultOutput};
    use tokio::time::Duration;

    // create test of subscribe_result() and publish_result()
    #[tokio::test]
    async fn test_subscribe_result() -> Result<()> {
        let app = ChanJobResultPubSubRepositoryImpl {
            broadcast_chan_buf: ChanBuffer::new(None, 1000),
            job_queue_config: Arc::new(JobQueueConfig {
                expire_job_result_seconds: 60,
                fetch_interval: 1000,
            }),
        };
        let job_id = JobId { value: 11 };
        let job_result_id = JobResultId { value: 1212 };
        let data = JobResultData {
            job_id: Some(job_id),
            output: Some(ResultOutput {
                items: vec![b"test".to_vec()],
            }),
            ..JobResultData::default()
        };
        let job_result = JobResult {
            id: Some(job_result_id),
            data: Some(data.clone()),
        };

        let mut jhv = Vec::with_capacity(10);
        // 10 subscribers
        for _i in 0..10 {
            let app1 = app.clone();
            let job_id1 = job_id;
            let jr1 = job_result.clone();
            let jh = tokio::spawn(async move {
                let res = app1.subscribe_result(&job_id1, Some(3000)).await.unwrap();
                assert_eq!(res.id, jr1.id);
                assert_eq!(res.data, jr1.data);
            });
            jhv.push(jh);
        }
        tokio::time::sleep(Duration::from_millis(800)).await;
        app.publish_result(&job_result_id, &data).await?;
        let res = futures::future::join_all(jhv.into_iter()).await;
        assert_eq!(res.len(), 10);
        assert!(res.iter().all(|r| r.is_ok()));

        Ok(())
    }
}
