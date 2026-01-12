use super::{JobResultPublisher, JobResultSubscriber};
use crate::infra::{job::rows::UseJobqueueAndCodec, JobQueueConfig, UseJobQueueConfig};
use anyhow::Result;
use async_trait::async_trait;
use futures::{stream::BoxStream, Stream, StreamExt};
use jobworkerp_base::{
    codec::{ProstMessageCodec, UseProstCodec},
    error::JobWorkerError,
};
use memory_utils::chan::{
    broadcast::{BroadcastChan, UseBroadcastChanBuffer},
    ChanBuffer, ChanBufferItem,
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
        // TODO not publish if worker.broadcast_result is false
        // (Currently we're preventing subscription by closing the receiving end(listen, listen_by_worker),
        //  but it's no sense to publish when not needed)
        let res = if to_listen {
            let channel_name = Self::job_result_pubsub_channel_name(jid);
            let ttl = Some(Duration::from_secs(
                self.job_queue_config().expire_job_result_seconds as u64,
            ));

            // Wait for subscriber if no receivers exist (race condition mitigation)
            // BroadcastChan only delivers to subscribers who subscribed BEFORE publish
            let max_wait_attempts = 10;
            let wait_interval = Duration::from_millis(10);
            for attempt in 0..max_wait_attempts {
                let receiver_count = self
                    .broadcast_chan_buf()
                    .receiver_count(channel_name.as_str())
                    .await;
                if receiver_count > 0 {
                    break;
                }
                if attempt < max_wait_attempts - 1 {
                    tokio::time::sleep(wait_interval).await;
                }
            }

            self.broadcast_chan_buf()
                .send_to_chan(
                    channel_name.as_str(),
                    result_data.clone(),
                    None,
                    ttl.as_ref(),
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

    /// Test to verify the race condition hypothesis:
    /// When publish_result is called BEFORE subscribe_result subscribes,
    /// the message is lost because tokio::sync::broadcast only delivers
    /// messages to subscribers who are already subscribed at the time of send.
    #[tokio::test]
    async fn test_race_condition_publish_before_subscribe_loses_message() -> Result<()> {
        let app = ChanJobResultPubSubRepositoryImpl {
            broadcast_chan_buf: ChanBuffer::new(None, 10000),
            job_queue_config: Arc::new(JobQueueConfig {
                expire_job_result_seconds: 60,
                fetch_interval: 1000,
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

        // CRITICAL: Publish BEFORE any subscriber exists
        eprintln!("[Race Test] Publishing result BEFORE any subscriber...");
        app.publish_result(&job_result_id, &data, true).await?;
        eprintln!("[Race Test] Published. Now starting subscriber...");

        // Small delay to ensure publish is complete
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Now try to subscribe - should timeout because message was already sent
        let app_clone = app.clone();
        let subscribe_result = tokio::time::timeout(
            Duration::from_secs(2),
            app_clone.subscribe_result(&job_id, Some(2000)),
        )
        .await;

        match subscribe_result {
            Ok(Ok(_)) => {
                panic!("UNEXPECTED: Subscriber received message that was published before subscription!");
            }
            Ok(Err(e)) => {
                eprintln!("[Race Test] Subscribe returned error (expected): {:?}", e);
                // This is expected - timeout or no message
            }
            Err(_) => {
                eprintln!("[Race Test] TIMEOUT as expected - message was lost because it was published before subscription");
                // This confirms our hypothesis!
            }
        }

        Ok(())
    }

    /// Test to verify that subscribe_result_stream also exhibits the race condition
    #[tokio::test]
    async fn test_race_condition_stream_publish_before_subscribe() -> Result<()> {
        let app = ChanJobResultPubSubRepositoryImpl {
            broadcast_chan_buf: ChanBuffer::new(None, 10000),
            job_queue_config: Arc::new(JobQueueConfig {
                expire_job_result_seconds: 60,
                fetch_interval: 1000,
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

    /// Control test: Verify that subscribe BEFORE publish works correctly
    #[tokio::test]
    async fn test_no_race_condition_subscribe_before_publish() -> Result<()> {
        let app = ChanJobResultPubSubRepositoryImpl {
            broadcast_chan_buf: ChanBuffer::new(None, 10000),
            job_queue_config: Arc::new(JobQueueConfig {
                expire_job_result_seconds: 60,
                fetch_interval: 1000,
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
                panic!("[Control Test] TIMEOUT - this should NOT happen when subscribing before publishing!");
            }
        }

        Ok(())
    }
}
