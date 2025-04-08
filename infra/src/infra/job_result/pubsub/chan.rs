use super::{JobResultPublisher, JobResultSubscriber};
use crate::infra::{job::rows::UseJobqueueAndCodec, JobQueueConfig, UseJobQueueConfig};
use anyhow::Result;
use async_trait::async_trait;
use futures::{future, stream::BoxStream, Stream, StreamExt};
use infra_utils::infra::chan::{
    broadcast::{BroadcastChan, UseBroadcastChanBuffer},
    ChanBuffer, ChanBufferItem,
};
use jobworkerp_base::{
    codec::{ProstMessageCodec, UseProstCodec},
    error::JobWorkerError,
};
use proto::jobworkerp::data::{
    result_output_item, JobId, JobResult, JobResultData, JobResultId, ResultOutputItem, WorkerId,
};
use std::{pin::Pin, sync::Arc, time::Duration};

/// publish job result to channel
/// (note: assume single instance use)
#[async_trait]
impl JobResultPublisher for ChanJobResultPubSubRepositoryImpl {
    async fn publish_result(
        &self,
        id: &JobResultId,
        data: &JobResultData,
        to_listen: bool,
    ) -> Result<bool> {
        // TODO send to worker id channel if listening clients exist
        // chan.receiver_count()
        let jid = data
            .job_id
            .as_ref()
            .ok_or(JobWorkerError::InvalidParameter(format!(
                "job_id not found: result_id={}",
                &id.value
            )))?;
        tracing::debug!(
            "publish_result: job_id={}, result_id={}",
            &jid.value,
            &id.value
        );
        let wid = data
            .worker_id
            .as_ref()
            .ok_or(JobWorkerError::InvalidParameter(format!(
                "worker_id not found: job_id={}",
                &jid.value
            )))?;
        let result_data = Self::serialize_job_result(*id, data.clone());
        // TODO not publish if worker.broadcast_result is false
        // (Currently we're preventing subscription by closing the receiving end(listen, listen_by_worker),
        //  but it's no sense to publish when not needed)
        let res = if to_listen {
            self.broadcast_chan_buf()
                .send_to_chan(
                    Self::job_result_pubsub_channel_name(jid).as_str(),
                    result_data.clone(),
                    None,
                    Some(&Duration::from_secs(
                        self.job_queue_config().expire_job_result_seconds as u64,
                    )),
                    false,
                )
                .await
        } else {
            Ok(false)
        };
        let res2 = self
            .broadcast_chan_buf()
            .send_to_chan(
                Self::job_result_by_worker_pubsub_channel_name(wid).as_str(),
                result_data,
                None,
                None,
                false,
            )
            .await
            .inspect_err(|e| tracing::warn!("send_to_chan_err:{:?}", e))?;
        res.map(|r| r || res2)
    }

    async fn publish_result_stream_data(
        &self,
        job_id: JobId,
        stream: BoxStream<'static, ResultOutputItem>,
    ) -> Result<bool> {
        let cn = Self::job_result_stream_pubsub_channel_name(&job_id);
        tracing::debug!(
            "publish_result_stream_data: job_id={}, ch={}",
            &job_id.value,
            &cn
        );
        let res_stream = stream
            .filter_map(|item| async move { ProstMessageCodec::serialize_message(&item).ok() });

        let res = self
            .broadcast_chan_buf()
            .send_stream_to_chan(
                cn.as_str(),
                res_stream,
                None,
                Some(&Duration::from_secs(
                    self.job_queue_config().expire_job_result_seconds as u64,
                )),
                false,
            )
            .await
            .inspect_err(|e| tracing::error!("send_stream_to_chan_err:{:?}", e))?;
        Ok(res)
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
            .inspect_err(|e| tracing::error!("receive_from_chan_err:{:?}", e))?;
        let res = Self::deserialize_job_result(&message)
            .inspect_err(|e| tracing::error!("deserialize_result:{:?}", e))?;
        tracing::debug!("subscribe_result_received: result={:?}", &res.id);
        Ok(res)
    }
    // subscribe job result of listen after by all listening client using broadcast_chan and return got result immediately
    async fn subscribe_result_stream(
        &self,
        job_id: &JobId,
        timeout: Option<u64>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        let cn = Arc::new(Self::job_result_stream_pubsub_channel_name(job_id));
        tracing::debug!(
            "subscribe_result_stream: job_id={}, ch={}",
            &job_id.value,
            &cn
        );

        let stream_future = self.broadcast_chan_buf().receive_stream_from_chan(
            cn.clone().to_string(),
            Some(Duration::from_secs(
                self.job_queue_config().expire_job_result_seconds as u64,
            )),
        );

        let stream = if let Some(timeout_ms) = timeout {
            tokio::select! {
                result = stream_future =>
                    result.inspect_err(|e| tracing::error!("receive_from_chan_err:{:?}", e))?,
                _ = tokio::time::sleep(Duration::from_millis(timeout_ms)) => {
                    return Err(JobWorkerError::RuntimeError(format!("subscribe_result_stream timeout after {}ms", timeout_ms)).into());
                }
            }
        } else {
            stream_future
                .await
                .inspect_err(|e| tracing::error!("receive_from_chan_err:{:?}", e))?
        };
        tracing::debug!(
            "subscribe_result_stream_receiving: job_id={}",
            &job_id.value
        );
        let transformed_stream = stream
            .filter_map(|b| async move {
                let out = ProstMessageCodec::deserialize_message::<ResultOutputItem>(b.as_slice());
                match out {
                    Ok(ResultOutputItem {
                        item: Some(result_output_item::Item::Data(data)),
                    }) => Some(ResultOutputItem {
                        item: Some(result_output_item::Item::Data(data)),
                    }),
                    Ok(ResultOutputItem {
                        item: Some(result_output_item::Item::End(_)),
                    }) => Some(ResultOutputItem { item: None }),
                    Ok(_) => {
                        tracing::error!("invalid message: {:?}", out);
                        None
                    }
                    Err(e) => {
                        tracing::error!("deserialize_message_err:{:?}", e);
                        None
                    }
                }
            })
            .take_while(|r| future::ready(r.item.is_some()))
            .boxed();
        Ok(transformed_stream)
    }
    async fn subscribe_result_stream_by_worker(
        &self,
        worker_id: WorkerId,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<JobResult>> + Send>>> {
        let cn = Self::job_result_by_worker_pubsub_channel_name(&worker_id);
        tracing::debug!(
            "subscribe_result_by_worker: worker_id={}, ch={}",
            &worker_id.value,
            &cn
        );
        let res: Pin<Box<dyn Stream<Item = Result<JobResult>> + Send>> = Box::pin(
            self.broadcast_chan_buf()
                .receive_stream_from_chan(
                    cn,
                    Some(Duration::from_secs(
                        self.job_queue_config().expire_job_result_seconds as u64,
                    )),
                )
                .await
                .inspect_err(|e| tracing::error!("receive_from_chan_err:{:?}", e))?
                .then(|r| async move {
                    Self::deserialize_job_result(&r)
                        .inspect_err(|e| tracing::error!("deserialize_result:{:?}", e))
                }),
        );
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
impl UseProstCodec for ChanJobResultPubSubRepositoryImpl {}
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
            broadcast_chan_buf: ChanBuffer::new(None, 10000),
            job_queue_config: Arc::new(JobQueueConfig {
                expire_job_result_seconds: 60,
                fetch_interval: 1000,
            }),
        };
        let job_id = JobId { value: 11 };
        let job_result_id = JobResultId { value: 1212 };
        let worker_id = WorkerId { value: 1 };
        let data = JobResultData {
            job_id: Some(job_id),
            worker_id: Some(worker_id),
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
        app.publish_result(&job_result_id, &data, true).await?;
        let res = futures::future::join_all(jhv.into_iter()).await;
        assert_eq!(res.len(), 10);
        assert!(res.iter().all(|r| r.is_ok()));

        Ok(())
    }
    #[tokio::test]
    async fn test_subscribe_result_stream_by_worker() -> Result<()> {
        let app = ChanJobResultPubSubRepositoryImpl {
            broadcast_chan_buf: ChanBuffer::new(None, 10000),
            job_queue_config: Arc::new(JobQueueConfig {
                expire_job_result_seconds: 60,
                fetch_interval: 1000,
            }),
        };
        let worker_id = WorkerId { value: 1 };
        let job_result_id = JobResultId { value: 1212 };
        let data = JobResultData {
            job_id: Some(JobId { value: 11 }),
            worker_id: Some(worker_id),
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
            let worker_id1 = worker_id;
            let jr1 = job_result.clone();
            let jh = tokio::spawn(async move {
                let mut stream = app1
                    .subscribe_result_stream_by_worker(worker_id1)
                    .await
                    .unwrap();
                // listen stream 10 times
                let res = stream.next().await.unwrap().unwrap();
                assert_eq!(res.id, jr1.id);
                assert_eq!(res.data, jr1.data);
            });
            jhv.push(jh);
        }
        tokio::time::sleep(Duration::from_millis(800)).await;
        // publish 10 times
        for _i in 0..10 {
            app.publish_result(&job_result_id, &data, true).await?;
        }
        let res = futures::future::join_all(jhv.into_iter()).await;
        assert_eq!(res.len(), 10);
        assert!(res.iter().all(|r| r.is_ok()));

        Ok(())
    }
}
