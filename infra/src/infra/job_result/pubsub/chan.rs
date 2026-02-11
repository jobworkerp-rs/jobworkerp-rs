use super::{JobResultPublisher, JobResultSubscriber};
use crate::infra::{JobQueueConfig, UseJobQueueConfig, job::rows::UseJobqueueAndCodec};
use anyhow::Result;
use async_trait::async_trait;
use futures::{Stream, StreamExt, stream::BoxStream};
use jobworkerp_base::{
    codec::{ProstMessageCodec, UseProstCodec},
    error::JobWorkerError,
};
use memory_utils::chan::{
    ChanBuffer, ChanBufferItem,
    broadcast::{BroadcastChan, UseBroadcastChanBuffer},
};
use proto::jobworkerp::data::{
    JobId, JobResult, JobResultData, JobResultId, ResultOutputItem, WorkerId, result_output_item,
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
        let job_result = JobResult {
            id: Some(*id),
            data: Some(data.clone()),
            ..Default::default()
        };
        let result_data = Self::serialize_message(&job_result)?;
        // broadcast_results flag is now propagated via JobResultData and controls to_listen parameter
        let res = if to_listen {
            let channel_name = Self::job_result_pubsub_channel_name(jid);

            // Wait for a subscriber-created channel to appear (race condition mitigation).
            // Subscribe side (DIRECT enqueue, listen_result, etc.) always creates the
            // channel via receive_from_chan → get_or_create_chan BEFORE publish runs.
            // For NO_RESULT with no external listener, the channel never appears and
            // we skip sending — preventing stretto cache accumulation (memory leak).
            let max_wait_attempts = 10;
            let wait_interval = Duration::from_millis(10);
            let mut has_channel = false;
            for attempt in 0..max_wait_attempts {
                if self
                    .broadcast_chan_buf()
                    .get_chan_if_exists(channel_name.as_str())
                    .await
                    .is_some()
                {
                    has_channel = true;
                    break;
                }
                if attempt < max_wait_attempts - 1 {
                    tokio::time::sleep(wait_interval).await;
                }
            }

            if has_channel {
                self.broadcast_chan_buf()
                    .send_to_chan(
                        channel_name.as_str(),
                        result_data.clone(),
                        None,
                        None,
                        true, // never create channel from publish side
                    )
                    .await
            } else {
                tracing::debug!(
                    "publish_result: no subscriber channel for job_id={}, skipping broadcast",
                    &jid.value
                );
                Ok(false)
            }
        } else {
            Ok(false)
        };
        // Worker ID channel: best-effort send for ListenByWorker subscribers.
        // Errors here should not fail the primary result publish (job_id channel),
        // as that would break DIRECT response delivery.
        let res2 = match self
            .broadcast_chan_buf()
            .send_to_chan(
                Self::job_result_by_worker_pubsub_channel_name(wid).as_str(),
                result_data,
                None,
                None,
                true,
            )
            .await
        {
            Ok(sent) => sent,
            Err(e) => {
                tracing::error!(
                    "failed to send result to worker_id={} channel: {:?}",
                    wid.value,
                    e
                );
                false
            }
        };
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
                None,
                true, // never create channel from publish side
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

        // Clean up the channel if no other subscribers (like Redis unsubscribe)
        if self.broadcast_chan_buf().receiver_count(cn.as_str()).await == 0 {
            let _ = self
                .broadcast_chan_buf()
                .delete_chan(cn.as_str())
                .await
                .inspect_err(|e| tracing::warn!("failed to delete channel: {:?}", e));
        }

        let res = Self::deserialize_message::<JobResult>(&message)
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
                    return Err(JobWorkerError::RuntimeError(format!("subscribe_result_stream timeout after {timeout_ms}ms")).into());
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
        // Emit End marker with Trailer, then terminate stream immediately
        // Capture chan_buf for cleanup when stream terminates
        let chan_buf = self.broadcast_chan_buf().clone();
        let cn_for_cleanup = cn.clone();
        let transformed_stream = stream
            .filter_map(|b| async move {
                match ProstMessageCodec::deserialize_message::<ResultOutputItem>(b.as_slice()) {
                    Ok(item) if item.item.is_some() => Some(item),
                    Ok(invalid) => {
                        tracing::error!("invalid message: {:?}", invalid);
                        None
                    }
                    Err(e) => {
                        tracing::error!("deserialize_message_err:{:?}", e);
                        None
                    }
                }
            })
            // For End marker: emit it, then emit None to trigger take_while termination
            .flat_map(|item| {
                if matches!(item.item, Some(result_output_item::Item::End(_))) {
                    // End marker: return End followed by None (termination signal)
                    futures::stream::iter(vec![Some(item), None])
                } else {
                    // Data item: wrap in Some
                    futures::stream::iter(vec![Some(item)])
                }
            })
            // Terminate when None is received (after End marker)
            .take_while(|opt| futures::future::ready(opt.is_some()))
            // Unwrap Option
            .filter_map(futures::future::ready)
            // Clean up channel when stream completes normally (e.g., via End marker or timeout).
            // Note: This cleanup does NOT run on early drop (e.g., client disconnect).
            // For early-drop cleanup, a Drop wrapper would be needed, but TTL-based
            // eviction serves as a fallback for those cases.
            .chain(futures::stream::once(async move {
                // This runs after the previous stream completes - clean up if no other subscribers
                if chan_buf.receiver_count(cn_for_cleanup.as_str()).await == 0 {
                    let _ = chan_buf
                        .delete_chan(cn_for_cleanup.as_str())
                        .await
                        .inspect_err(|e| {
                            tracing::warn!("failed to delete stream channel: {:?}", e)
                        });
                }
                // Return a marker that will be filtered out
                ResultOutputItem { item: None }
            }))
            // Filter out the cleanup marker
            .filter(|item| futures::future::ready(item.item.is_some()))
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
                    Self::deserialize_message::<JobResult>(&r)
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

    /// Subscribe to job result with a fallback check executed after receiver registration.
    /// Returns (JobResult, bool) where bool indicates if the result came from the fallback check.
    pub async fn subscribe_result_with_fallback<F, Fut>(
        &self,
        job_id: &JobId,
        timeout: Option<u64>,
        check: F,
    ) -> Result<(JobResult, bool)>
    where
        F: FnOnce() -> Fut + Send,
        Fut: std::future::Future<Output = Option<Vec<u8>>> + Send,
    {
        let from_fallback = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = from_fallback.clone();
        let wrapped_check = move || async move {
            let result = check().await;
            if result.is_some() {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            // Wrap in ChanBufferItem format: (None, bytes)
            result.map(|v| (None, v))
        };

        let cn = Self::job_result_pubsub_channel_name(job_id);
        tracing::debug!(
            "subscribe_result_with_fallback: job_id={}, ch={}",
            &job_id.value,
            &cn
        );

        let message = self
            .broadcast_chan_buf()
            .receive_from_chan_with_check(
                cn.as_str(),
                timeout.map(Duration::from_millis),
                Some(&Duration::from_secs(
                    self.job_queue_config().expire_job_result_seconds as u64,
                )),
                wrapped_check,
            )
            .await
            .inspect_err(|e| tracing::error!("receive_from_chan_err:{:?}", e))?;

        if self.broadcast_chan_buf().receiver_count(cn.as_str()).await == 0 {
            let _ = self
                .broadcast_chan_buf()
                .delete_chan(cn.as_str())
                .await
                .inspect_err(|e| tracing::warn!("failed to delete channel: {:?}", e));
        }

        let res = Self::deserialize_message::<JobResult>(&message)
            .inspect_err(|e| tracing::error!("deserialize_result:{:?}", e))?;
        tracing::debug!("subscribe_result_received: result={:?}", &res.id);
        let is_fallback = from_fallback.load(std::sync::atomic::Ordering::Relaxed);
        Ok((res, is_fallback))
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
    use std::collections::HashMap;

    use super::*;
    use crate::infra::JobQueueConfig;
    use anyhow::Result;
    use memory_utils::chan::ChanBuffer;
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
                channel_capacity: 10000,
                pubsub_channel_capacity: 128,
                max_channels: 10_000,
                cancel_channel_capacity: 1_000,
            }),
        };
        let job_id = JobId { value: 11 };
        let job_result_id = JobResultId { value: 1212 };
        let worker_id = WorkerId { value: 1 };
        let data = JobResultData {
            job_id: Some(job_id),
            worker_id: Some(worker_id),
            output: Some(ResultOutput {
                items: b"test".to_vec(),
            }),
            ..JobResultData::default()
        };
        let job_result = JobResult {
            id: Some(job_result_id),
            data: Some(data.clone()),
            metadata: HashMap::new(),
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
                channel_capacity: 10000,
                pubsub_channel_capacity: 128,
                max_channels: 10_000,
                cancel_channel_capacity: 1_000,
            }),
        };
        let worker_id = WorkerId { value: 1 };
        let job_result_id = JobResultId { value: 1212 };
        let data = JobResultData {
            job_id: Some(JobId { value: 11 }),
            worker_id: Some(worker_id),
            output: Some(ResultOutput {
                items: b"test".to_vec(),
            }),
            ..JobResultData::default()
        };
        let job_result = JobResult {
            id: Some(job_result_id),
            data: Some(data.clone()),
            metadata: HashMap::new(),
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

    /// Test subscribe_result_stream with End marker - verifies stream terminates correctly
    #[tokio::test]
    async fn test_subscribe_result_stream_terminates_on_end() -> Result<()> {
        let app = ChanJobResultPubSubRepositoryImpl {
            broadcast_chan_buf: ChanBuffer::new(None, 10000),
            job_queue_config: Arc::new(JobQueueConfig {
                expire_job_result_seconds: 60,
                fetch_interval: 1000,
                channel_capacity: 10000,
                pubsub_channel_capacity: 128,
                max_channels: 10_000,
                cancel_channel_capacity: 1_000,
            }),
        };
        let job_id = JobId { value: 999 };

        // Start subscriber first
        let app_clone = app.clone();
        let job_id_clone = job_id;
        let subscriber_handle = tokio::spawn(async move {
            let mut stream = app_clone
                .subscribe_result_stream(&job_id_clone, Some(10000))
                .await
                .expect("subscribe_result_stream failed");

            let mut received_items = vec![];
            let start = std::time::Instant::now();

            while let Some(item) = stream.next().await {
                let elapsed = start.elapsed();
                eprintln!(
                    "[{:>6.3}s] Received item: {:?}",
                    elapsed.as_secs_f64(),
                    item.item
                );
                received_items.push(item.clone());

                // Check if this is the End marker
                if matches!(item.item, Some(result_output_item::Item::End(_))) {
                    eprintln!(
                        "[{:>6.3}s] End marker received, stream should terminate",
                        elapsed.as_secs_f64()
                    );
                }
            }

            eprintln!("Stream terminated after {} items", received_items.len());
            received_items
        });

        // Wait for subscriber to be ready
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Publish stream data with Data items and End marker
        let data_items: Vec<ResultOutputItem> = vec![
            ResultOutputItem {
                item: Some(result_output_item::Item::Data(b"data1".to_vec())),
            },
            ResultOutputItem {
                item: Some(result_output_item::Item::Data(b"data2".to_vec())),
            },
            ResultOutputItem {
                item: Some(result_output_item::Item::End(
                    proto::jobworkerp::data::Trailer {
                        metadata: HashMap::new(),
                    },
                )),
            },
        ];

        let stream = futures::stream::iter(data_items).boxed();
        app.publish_result_stream_data(job_id, stream).await?;

        eprintln!("Published stream data, waiting for subscriber to complete...");

        // Wait for subscriber with timeout
        let result = tokio::time::timeout(Duration::from_secs(5), subscriber_handle).await;

        match result {
            Ok(Ok(items)) => {
                eprintln!(
                    "Subscriber completed successfully with {} items",
                    items.len()
                );
                assert_eq!(items.len(), 3, "Should receive 3 items (2 data + 1 end)");
                assert!(matches!(
                    items[0].item,
                    Some(result_output_item::Item::Data(_))
                ));
                assert!(matches!(
                    items[1].item,
                    Some(result_output_item::Item::Data(_))
                ));
                assert!(matches!(
                    items[2].item,
                    Some(result_output_item::Item::End(_))
                ));
            }
            Ok(Err(e)) => {
                panic!("Subscriber task failed: {:?}", e);
            }
            Err(_) => {
                panic!("TIMEOUT: Stream did not terminate after End marker within 5 seconds");
            }
        }

        Ok(())
    }

    /// Test to verify that publish_result polling prevents race condition:
    /// Even when publish_result is called slightly before subscribe_result,
    /// the polling mechanism (max_wait_attempts / wait_interval in publish_result)
    /// allows the subscriber to connect and receive the message.
    ///
    /// Background: tokio::sync::broadcast only delivers messages to subscribers
    /// who are already subscribed at the time of send. To mitigate this race condition,
    /// publish_result polls for up to ~100ms (10 attempts * 10ms interval) waiting
    /// for subscribers before sending.
    #[tokio::test]
    async fn test_publish_polling_prevents_race_condition() -> Result<()> {
        let app = ChanJobResultPubSubRepositoryImpl {
            broadcast_chan_buf: ChanBuffer::new(None, 10000),
            job_queue_config: Arc::new(JobQueueConfig {
                expire_job_result_seconds: 60,
                fetch_interval: 1000,
                channel_capacity: 10000,
                pubsub_channel_capacity: 128,
                max_channels: 10_000,
                cancel_channel_capacity: 1_000,
            }),
        };
        let job_id = JobId { value: 12345 };
        let job_result_id = JobResultId { value: 6789 };
        let worker_id = WorkerId { value: 1 };
        let data = JobResultData {
            job_id: Some(job_id),
            worker_id: Some(worker_id),
            output: Some(ResultOutput {
                items: b"test_race".to_vec(),
            }),
            ..JobResultData::default()
        };
        let expected_result = JobResult {
            id: Some(job_result_id),
            data: Some(data.clone()),
            metadata: HashMap::new(),
        };

        // Start publisher FIRST - it will poll for up to ~100ms waiting for subscribers
        let app_clone = app.clone();
        let data_clone = data.clone();
        let publish_handle = tokio::spawn(async move {
            eprintln!("[Polling Test] Publisher started, polling for subscribers...");
            let result = app_clone
                .publish_result(&job_result_id, &data_clone, true)
                .await;
            eprintln!("[Polling Test] Publisher completed");
            result
        });

        // Start subscriber shortly after (within the ~100ms polling window)
        // This simulates a realistic race condition scenario where subscriber
        // starts slightly after publisher
        tokio::time::sleep(Duration::from_millis(20)).await;
        eprintln!("[Polling Test] Starting subscriber within polling window...");

        let app_clone2 = app.clone();
        let subscribe_handle =
            tokio::spawn(async move { app_clone2.subscribe_result(&job_id, Some(3000)).await });

        // Wait for both to complete
        let (publish_result, subscribe_result) = tokio::join!(publish_handle, subscribe_handle);

        // Verify publisher succeeded
        let publish_ok = publish_result
            .expect("publish task panicked")
            .expect("publish_result failed");
        assert!(
            publish_ok,
            "publish_result should return true when message was sent"
        );

        // Verify subscriber received the message (polling prevented loss)
        let received = subscribe_result
            .expect("subscribe task panicked")
            .expect("subscribe_result failed - polling did not prevent race condition");

        eprintln!(
            "[Polling Test] SUCCESS: Subscriber received message despite starting after publisher"
        );
        assert_eq!(received.id, expected_result.id);
        assert_eq!(received.data, expected_result.data);

        Ok(())
    }

    /// Test to verify that publish_result_stream_data exhibits the race condition.
    ///
    /// Unlike publish_result which has polling to wait for subscribers,
    /// publish_result_stream_data does NOT have such polling mechanism.
    /// Therefore, if stream data is published before any subscriber exists,
    /// the messages will be lost.
    ///
    /// This test documents this known limitation of the stream publishing API.
    #[tokio::test]
    async fn test_race_condition_stream_publish_before_subscribe() -> Result<()> {
        let app = ChanJobResultPubSubRepositoryImpl {
            broadcast_chan_buf: ChanBuffer::new(None, 10000),
            job_queue_config: Arc::new(JobQueueConfig {
                expire_job_result_seconds: 60,
                fetch_interval: 1000,
                channel_capacity: 10000,
                pubsub_channel_capacity: 128,
                max_channels: 10_000,
                cancel_channel_capacity: 1_000,
            }),
        };
        let job_id = JobId { value: 99999 };

        // CRITICAL: Publish stream data BEFORE any subscriber exists
        eprintln!("[Stream Race Test] Publishing stream data BEFORE any subscriber...");
        let data_items: Vec<ResultOutputItem> = vec![
            ResultOutputItem {
                item: Some(result_output_item::Item::Data(b"stream_data".to_vec())),
            },
            ResultOutputItem {
                item: Some(result_output_item::Item::End(
                    proto::jobworkerp::data::Trailer {
                        metadata: HashMap::new(),
                    },
                )),
            },
        ];
        let stream = futures::stream::iter(data_items).boxed();
        app.publish_result_stream_data(job_id, stream).await?;
        eprintln!("[Stream Race Test] Published. Now starting subscriber...");

        // Small delay to ensure publish is complete
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Now try to subscribe to stream - should timeout because messages were already sent
        let app_clone = app.clone();
        let job_id_clone = job_id;
        let subscribe_handle = tokio::spawn(async move {
            let mut stream = app_clone
                .subscribe_result_stream(&job_id_clone, Some(2000))
                .await
                .expect("subscribe_result_stream failed");

            let mut count = 0;
            while let Some(_item) = stream.next().await {
                count += 1;
            }
            count
        });

        let result = tokio::time::timeout(Duration::from_secs(3), subscribe_handle).await;

        match result {
            Ok(Ok(count)) => {
                if count == 0 {
                    eprintln!(
                        "[Stream Race Test] Received 0 items as expected - messages were lost"
                    );
                } else {
                    panic!(
                        "UNEXPECTED: Received {} items that were published before subscription!",
                        count
                    );
                }
            }
            Ok(Err(e)) => {
                eprintln!("[Stream Race Test] Task error: {:?}", e);
            }
            Err(_) => {
                eprintln!(
                    "[Stream Race Test] TIMEOUT - stream never terminated (no End marker received)"
                );
                // This also confirms the race condition - the End marker was lost
            }
        }

        Ok(())
    }

    /// Test subscribe_result_with_fallback: check returns Some (immediate result)
    #[tokio::test]
    async fn test_subscribe_result_with_fallback_check_returns_some() -> Result<()> {
        let app = ChanJobResultPubSubRepositoryImpl {
            broadcast_chan_buf: ChanBuffer::new(None, 10000),
            job_queue_config: Arc::new(JobQueueConfig {
                expire_job_result_seconds: 60,
                fetch_interval: 1000,
                channel_capacity: 10000,
                pubsub_channel_capacity: 128,
                max_channels: 10_000,
                cancel_channel_capacity: 1_000,
            }),
        };
        let job_id = JobId { value: 20001 };
        let job_result_id = JobResultId { value: 30001 };
        let worker_id = WorkerId { value: 1 };
        let data = JobResultData {
            job_id: Some(job_id),
            worker_id: Some(worker_id),
            output: Some(ResultOutput {
                items: b"fallback_hit".to_vec(),
            }),
            ..JobResultData::default()
        };
        let expected = JobResult {
            id: Some(job_result_id),
            data: Some(data.clone()),
            metadata: HashMap::new(),
        };
        let serialized =
            <ChanJobResultPubSubRepositoryImpl as UseProstCodec>::serialize_message(&expected)?;

        let (result, from_fallback) = app
            .subscribe_result_with_fallback(&job_id, Some(3000), || {
                let s = serialized.clone();
                async move { Some(s) }
            })
            .await?;

        assert!(from_fallback);
        assert_eq!(result.id, expected.id);
        assert_eq!(result.data, expected.data);
        Ok(())
    }

    /// Test subscribe_result_with_fallback: check returns None, receives via channel
    #[tokio::test]
    async fn test_subscribe_result_with_fallback_check_returns_none() -> Result<()> {
        let app = ChanJobResultPubSubRepositoryImpl {
            broadcast_chan_buf: ChanBuffer::new(None, 10000),
            job_queue_config: Arc::new(JobQueueConfig {
                expire_job_result_seconds: 60,
                fetch_interval: 1000,
                channel_capacity: 10000,
                pubsub_channel_capacity: 128,
                max_channels: 10_000,
                cancel_channel_capacity: 1_000,
            }),
        };
        let job_id = JobId { value: 20002 };
        let job_result_id = JobResultId { value: 30002 };
        let worker_id = WorkerId { value: 1 };
        let data = JobResultData {
            job_id: Some(job_id),
            worker_id: Some(worker_id),
            output: Some(ResultOutput {
                items: b"from_channel".to_vec(),
            }),
            ..JobResultData::default()
        };
        let expected = JobResult {
            id: Some(job_result_id),
            data: Some(data.clone()),
            metadata: HashMap::new(),
        };

        let app_clone = app.clone();
        let handle = tokio::spawn(async move {
            app_clone
                .subscribe_result_with_fallback(&job_id, Some(5000), || async { None })
                .await
        });

        tokio::time::sleep(Duration::from_millis(200)).await;
        app.publish_result(&job_result_id, &data, true).await?;

        let (result, from_fallback) = handle.await.unwrap()?;
        assert!(!from_fallback);
        assert_eq!(result.id, expected.id);
        assert_eq!(result.data, expected.data);
        Ok(())
    }

    /// Test subscribe_result_with_fallback: publish during check execution (race condition reproduction)
    #[tokio::test]
    async fn test_subscribe_result_with_fallback_race_publish_during_check() -> Result<()> {
        let app = ChanJobResultPubSubRepositoryImpl {
            broadcast_chan_buf: ChanBuffer::new(None, 10000),
            job_queue_config: Arc::new(JobQueueConfig {
                expire_job_result_seconds: 60,
                fetch_interval: 1000,
                channel_capacity: 10000,
                pubsub_channel_capacity: 128,
                max_channels: 10_000,
                cancel_channel_capacity: 1_000,
            }),
        };
        let job_id = JobId { value: 20003 };
        let job_result_id = JobResultId { value: 30003 };
        let worker_id = WorkerId { value: 1 };
        let data = JobResultData {
            job_id: Some(job_id),
            worker_id: Some(worker_id),
            output: Some(ResultOutput {
                items: b"race_test".to_vec(),
            }),
            ..JobResultData::default()
        };
        let expected = JobResult {
            id: Some(job_result_id),
            data: Some(data.clone()),
            metadata: HashMap::new(),
        };

        let check_started = Arc::new(tokio::sync::Notify::new());
        let check_started_clone = check_started.clone();

        let app_clone = app.clone();
        let handle = tokio::spawn(async move {
            app_clone
                .subscribe_result_with_fallback(&job_id, Some(5000), || {
                    let notify = check_started_clone;
                    async move {
                        notify.notify_one();
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        None
                    }
                })
                .await
        });

        // Wait until check starts (receiver is already registered at this point)
        check_started.notified().await;
        // Publish while check is sleeping — the broadcast receiver should capture this
        app.publish_result(&job_result_id, &data, true).await?;

        let (result, from_fallback) = handle.await.unwrap()?;
        assert!(!from_fallback);
        assert_eq!(result.id, expected.id);
        assert_eq!(result.data, expected.data);
        Ok(())
    }

    /// Control test: Verify that subscribe BEFORE publish works correctly
    #[tokio::test]
    async fn test_no_race_condition_subscribe_before_publish() -> Result<()> {
        let app = ChanJobResultPubSubRepositoryImpl {
            broadcast_chan_buf: ChanBuffer::new(None, 10000),
            job_queue_config: Arc::new(JobQueueConfig {
                expire_job_result_seconds: 60,
                fetch_interval: 1000,
                channel_capacity: 10000,
                pubsub_channel_capacity: 128,
                max_channels: 10_000,
                cancel_channel_capacity: 1_000,
            }),
        };
        let job_id = JobId { value: 54321 };
        let job_result_id = JobResultId { value: 9876 };
        let worker_id = WorkerId { value: 1 };
        let data = JobResultData {
            job_id: Some(job_id),
            worker_id: Some(worker_id),
            output: Some(ResultOutput {
                items: b"test_no_race".to_vec(),
            }),
            ..JobResultData::default()
        };
        let expected_result = JobResult {
            id: Some(job_result_id),
            data: Some(data.clone()),
            metadata: HashMap::new(),
        };

        // Start subscriber FIRST
        let app_clone = app.clone();
        let job_id_clone = job_id;
        let expected = expected_result.clone();
        let subscriber_handle =
            tokio::spawn(
                async move { app_clone.subscribe_result(&job_id_clone, Some(5000)).await },
            );

        // Wait for subscriber to be ready
        tokio::time::sleep(Duration::from_millis(100)).await;

        // THEN publish
        eprintln!("[Control Test] Publishing AFTER subscriber is ready...");
        app.publish_result(&job_result_id, &data, true).await?;

        let result = tokio::time::timeout(Duration::from_secs(2), subscriber_handle).await;

        match result {
            Ok(Ok(Ok(received))) => {
                eprintln!("[Control Test] SUCCESS: Received message correctly");
                assert_eq!(received.id, expected.id);
                assert_eq!(received.data, expected.data);
            }
            Ok(Ok(Err(e))) => {
                panic!("[Control Test] Subscribe error: {:?}", e);
            }
            Ok(Err(e)) => {
                panic!("[Control Test] Task join error: {:?}", e);
            }
            Err(_) => {
                panic!(
                    "[Control Test] TIMEOUT - this should NOT happen when subscribing before publishing!"
                );
            }
        }

        Ok(())
    }
}
