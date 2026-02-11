use crate::infra::JobQueueConfig;
use crate::infra::UseJobQueueConfig;
use crate::infra::job::queue::JobQueueCancellationRepository;
use crate::infra::job::rows::UseJobqueueAndCodec;
use anyhow::Result;
use async_trait::async_trait;
use futures::future::BoxFuture;
use jobworkerp_base::codec::UseProstCodec;
use jobworkerp_base::error::JobWorkerError;
use memory_utils::chan::broadcast::BroadcastChan;
use memory_utils::chan::mpmc::{Chan, UseChanBuffer};
use memory_utils::chan::{ChanBuffer, ChanBufferItem};
use proto::jobworkerp::data::{Job, JobId, Priority};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex;

#[async_trait]
pub trait ChanJobQueueRepository:
    UseChanBuffer<Item = Vec<u8>>
    + UseChanQueueBuffer
    + UseJobqueueAndCodec
    + UseJobQueueConfig
    + Sync
    + 'static
{
    // for front (send job to worker)
    // return: jobqueue size
    #[inline]
    async fn enqueue_job(&self, channel_name: Option<&String>, job: &Job) -> Result<i64>
    where
        Self: Send + Sync,
    {
        let cn = channel_name
            .unwrap_or(&Self::DEFAULT_CHANNEL_NAME.to_string())
            .to_owned();
        let qn = Self::queue_channel_name(cn.clone(), job.data.as_ref().map(|d| &d.priority));

        // Add to shared_buffer FIRST to prevent race condition (consumer receiving before it's added)
        {
            let mut shared_buffer = self.queue_list_buffer().lock().await;
            shared_buffer
                .entry(qn.clone())
                .or_insert_with(Vec::new)
                .push(job.clone());
        }

        match self
            .chan_buf()
            .send_to_chan(
                &qn,
                Self::serialize_message(job)?,
                job.data.as_ref().and_then(|d| d.uniq_key.clone()),
                None,
                false,
            ) // expect for multiple value
            .await
        {
            Ok(b) => {
                if !b {
                    // send_to_chan returns false when all receivers have been dropped.
                    // Roll back the shared_buffer entry to keep it consistent with
                    // the actual channel state.
                    let mut shared_buffer = self.queue_list_buffer().lock().await;
                    if let Some(v) = shared_buffer.get_mut(&qn) {
                        v.retain(|x| x.id != job.id);
                    }
                }
                Ok(self.chan_buf().count_chan_opt(qn).await.unwrap_or(0) as i64)
            }
            Err(e) => {
                // Rollback on error
                let mut shared_buffer = self.queue_list_buffer().lock().await;
                if let Some(v) = shared_buffer.get_mut(&qn) {
                    v.retain(|x| x.id != job.id);
                }
                Err(JobWorkerError::ChanError(e).into())
            }
        }
    }

    // channel names are ordered by priority (first is highest)
    #[inline]
    async fn receive_job_from_channels(&self, queue_channel_names: Vec<String>) -> Result<Job>
    where
        Self: Send + Sync,
    {
        use futures::FutureExt;
        // receive from multiple channels immediately (if queue is empty, select each channel for waiting)
        for qn in &queue_channel_names {
            tracing::debug!("receive_job_from_channels: channel: {:?}", qn);
            match self.chan_buf().try_receive_from_chan(qn, None).await {
                Ok(v) => {
                    let r = Self::deserialize_message::<Job>(&v)?;
                    if let Some(v) = self.queue_list_buffer().lock().await.get_mut(qn) {
                        // remove job from shared buffer
                        v.retain(|x| x.id != r.id)
                    }
                    return Ok(r);
                }
                Err(e) => {
                    // channel is empty (or other error)
                    tracing::trace!("try_receive_job_from_channels: {:?}", e);
                }
            }
        }
        // wait for multiple channels
        let (res, idx, _l) = futures::future::select_all(
            queue_channel_names
                .iter()
                .map(|cn| self.chan_buf().receive_from_chan(cn, None, None).boxed()),
        )
        .await;
        match res.map_err(|e| JobWorkerError::ChanError(e).into()) {
            Ok(v) => {
                let r = Self::deserialize_message::<Job>(&v)?;
                if let Some(j) = self
                    .queue_list_buffer()
                    .lock()
                    .await
                    .get_mut(&queue_channel_names[idx])
                {
                    j.retain(|x| x.id.as_ref() != r.id.as_ref())
                }
                Ok(r)
            }
            Err(e) => Err(e),
        }
    }

    // // send job result from worker to front directly
    // #[inline]
    // async fn enqueue_result_direct(
    //     &self,
    //     id: &JobResultId,
    //     res: &JobResultData,
    //     stream: Option<BoxStream<'static, Vec<u8>>>,
    // ) -> Result<bool>
    // where
    //     Self: Send + Sync,
    // {
    //     use std::time::Duration;
    //     let v = Self::serialize_job_result(*id, res.clone());
    //     if let Some(jid) = res.job_id.as_ref() {
    //         let cn = Self::result_queue_name(jid);
    //         tracing::debug!("send_result_direct: job_id: {:?}, queue: {}", jid, &cn);
    //         // job id based queue (onetime, use ttl)
    //         let _ = self
    //             .chan_buf()
    //             .send_to_chan(
    //                 &cn,
    //                 v,
    //                 None,
    //                 // set expire for not calling listen_after api
    //                 Some(&Duration::from_secs(
    //                     self.job_queue_config().expire_job_result_seconds as u64,
    //                 )),
    //                 true, // only if exists
    //             )
    //             .await
    //             .map_err(JobWorkerError::ChanError)?;
    //         if let Some(stream) = stream {
    //             // send stream to pubsub channel if exists
    //             // TODO decode item and add end item to stream
    //             tracing::debug!("==== send_result_direct: send_stream_to_chan: {:?}", jid);
    //             self.chan_buf()
    //                 .send_stream_to_chan(
    //                     &Self::job_result_stream_pubsub_channel_name(jid),
    //                     stream,
    //                     None,
    //                     Some(&Duration::from_secs(
    //                         self.job_queue_config().expire_job_result_seconds as u64,
    //                     )),
    //                     true,
    //                 )
    //                 .await?;
    //         }
    //         tracing::debug!("===== send_result_direct: : {:?}", jid);
    //         Ok(true)
    //     } else {
    //         tracing::warn!("job_id is not set in job_result: {:?}", res);
    //         Ok(false)
    //     }
    // }

    // // wait response from worker for direct response job
    // // TODO shutdown lock until receive result ? (but not recorded...)
    // #[inline]
    // async fn wait_for_result_queue_for_response(
    //     &self,
    //     job_id: &JobId,
    //     timeout: Option<u64>,
    //     request_streaming: bool,
    // ) -> Result<(JobResult, Option<BoxStream<'static, ResultOutputItem>>)>
    // where
    //     Self: Send + Sync,
    // {
    //     use std::time::Duration;
    //     use tokio_stream::StreamExt;
    //     // TODO retry control
    //     let nm = Self::result_queue_name(job_id);
    //     tracing::debug!(
    //         "wait_for_result_data_for_response: job_id: {:?} timeout:{}, mode: {}",
    //         job_id,
    //         timeout.unwrap_or(0),
    //         if request_streaming {
    //             "streaming"
    //         } else {
    //             "direct"
    //         }
    //     );
    //     let pop_fut = async {
    //         tokio::select! {
    //             _ = tokio::spawn(async {
    //                 let mut stream = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).expect("signal error");
    //                 stream.recv().await
    //             }) => {
    //                 Err(JobWorkerError::RuntimeError("interrupt direct waiting process".to_string()).into())
    //             },
    //             val = self.chan_buf().receive_from_chan(nm, timeout.and_then(|t| if t == 0 {None} else {Some(Duration::from_millis(t))}), None) => {
    //                 let r: Result<JobResult> = val.map_err(|e|JobWorkerError::ChanError(e).into())
    //                     .and_then(|v| Self::deserialize_job_result(&v));
    //                 tracing::debug!("====== wait_for_result_queue_for_response(in future): got res: {:?}", &r);
    //                 r
    //             },
    //         }
    //     };
    //     let chan_buf_clone = self.chan_buf().clone();
    //     let nm_stream = Self::job_result_stream_pubsub_channel_name(job_id);
    //     let stream_fut = async {
    //         if request_streaming {
    //             let v = chan_buf_clone
    //                 .receive_stream_from_chan(nm_stream, timeout.map(Duration::from_millis))
    //                 .await
    //                 .inspect_err(|e| {
    //                     tracing::error!("stream error: {:?}", e);
    //                 })
    //                 .ok();
    //             tracing::debug!("====== wait_for_result_queue_for_response: got stream",);
    //             v
    //         } else {
    //             None
    //         }
    //     };

    //     // First wait for job result, then handle streaming
    //     // This prevents race condition where job status gets deleted before stream is set up
    //     let pop_result = pop_fut.await;

    //     let stream_result = if request_streaming && pop_result.is_ok() {
    //         stream_fut.await
    //     } else {
    //         None
    //     };

    //     // Handle streaming request inconsistency: if streaming was requested but stream creation failed
    //     if request_streaming && stream_result.is_none() {
    //         tracing::warn!(
    //             "wait_for_result_queue_for_response: streaming requested but no stream available for job_id: {:?}",
    //             job_id
    //         );
    //     }

    //     tracing::debug!(
    //         "wait_for_result_queue_for_response: got res: {:?}",
    //         &pop_result
    //     );
    //     match pop_result {
    //         Ok(job_result) => {
    //             // Check if job result indicates error status - in that case, don't return stream
    //             // even if streaming was requested, to prevent HTTP/2 protocol errors
    //             let should_disable_stream = if let Some(ref data) = job_result.data {
    //                 use proto::jobworkerp::data::ResultStatus;
    //                 data.status != ResultStatus::Success as i32
    //             } else {
    //                 false
    //             };

    //             let final_stream = if should_disable_stream {
    //                 None
    //             } else if let Some(s) = stream_result {
    //                 let mapped_stream = s.map(|v| ResultOutputItem {
    //                     item: Some(result_output_item::Item::Data(v)),
    //                 });
    //                 Some(Box::pin(mapped_stream) as BoxStream<'static, ResultOutputItem>)
    //             } else {
    //                 None
    //             };

    //             Ok((job_result, final_stream))
    //         }
    //         Err(e) => Err(e),
    //     }
    // }
    // // from shared buffer
    // async fn find_from_queue(&self, channel: Option<&String>, id: &JobId) -> Result<Option<Job>> {
    //     let default_name = Self::DEFAULT_CHANNEL_NAME.to_string();
    //     let cnl = channel.unwrap_or(&default_name);
    //     let cl = Self::queue_channel_name(cnl, Some(Priority::Low as i32).as_ref());
    //     let cm = Self::queue_channel_name(cnl, Some(Priority::Medium as i32).as_ref());
    //     let ch = Self::queue_channel_name(cnl, Some(Priority::High as i32).as_ref());

    //     let c = vec![ch, cm, cl]; // priority
    //     for cn in c {
    //         if let Some(v) = self.queue_list_buffer().lock().await.get(&cn) {
    //             if let Some(j) = v.iter().find(|x| x.id.as_ref() == Some(id)) {
    //                 return Ok(Some(j.clone()));
    //             }
    //         }
    //     }
    //     Ok(None)
    // }
    // cannot iterate channel buffer
    async fn find_multi_from_queue(
        &self,
        channel: Option<&str>,
        ids: Option<&HashSet<i64>>,
    ) -> Result<Vec<Job>> {
        // Ok(self
        //     .shared_buffer()
        //     .lock()
        //     .await
        //     .get(channel.unwrap_or(&Self::DEFAULT_CHANNEL_NAME.to_string()))
        //     .map(|v| {
        //         v.iter()
        //             .filter(|x| ids.is_none_or(|ids| ids.contains(&x.id.unwrap().value)))
        //             .map(|j| j.clone())
        //             .collect()
        //     })
        //     .unwrap_or_default())
        // for each channel
        let default_name = Self::DEFAULT_CHANNEL_NAME.to_string();
        let cnl = channel.unwrap_or(&default_name);
        let cl = Self::queue_channel_name(cnl, Some(Priority::Low as i32).as_ref());
        let cm = Self::queue_channel_name(cnl, Some(Priority::Medium as i32).as_ref());
        let ch = Self::queue_channel_name(cnl, Some(Priority::High as i32).as_ref());

        let c = vec![ch, cm, cl]; // priority
        let mut res = vec![];
        for cn in c {
            if let Some(v) = self.queue_list_buffer().lock().await.get(&cn) {
                res.extend(
                    v.iter()
                        .filter(|x| ids.is_none() || ids.unwrap().contains(&x.id.unwrap().value))
                        .cloned(),
                );
            }
        }
        Ok(res)
    }

    // async fn delete_from_queue(
    //     &self,
    //     _channel: Option<&String>,
    //     _priority: Priority,
    //     _job: &Job,
    // ) -> Result<i32> {
    //     // TODO implement
    //     todo!()
    // }
    // for test
    async fn count_queue(&self, channel: Option<&String>, priority: Priority) -> Result<i64> {
        let c = Self::queue_channel_name(
            channel.unwrap_or(&Self::DEFAULT_CHANNEL_NAME.to_string()),
            Some(priority as i32).as_ref(),
        );
        Ok(self.chan_buf().count_chan_opt(c).await.unwrap_or(0) as i64)
    }
}

pub trait UseChanQueueBuffer {
    fn queue_list_buffer(&self) -> &Mutex<HashMap<String, Vec<Job>>>;
}

#[derive(Clone, Debug)]
pub struct ChanJobQueueRepositoryImpl {
    pub chan_pool: ChanBuffer<Vec<u8>, Chan<ChanBufferItem<Vec<u8>>>>,
    pub shared_buffer: Arc<Mutex<HashMap<String, Vec<proto::jobworkerp::data::Job>>>>,
    pub job_queue_config: Arc<JobQueueConfig>,
    pub broadcast_cancel_chan_buf: BroadcastChan<Vec<u8>>,
}
impl UseChanBuffer for ChanJobQueueRepositoryImpl {
    type Item = Vec<u8>;
    fn chan_buf(&self) -> &ChanBuffer<Vec<u8>, Chan<ChanBufferItem<Vec<u8>>>> {
        &self.chan_pool
    }
}
impl UseChanQueueBuffer for ChanJobQueueRepositoryImpl {
    fn queue_list_buffer(&self) -> &Mutex<HashMap<String, Vec<Job>>> {
        &self.shared_buffer
    }
}
impl UseJobQueueConfig for ChanJobQueueRepositoryImpl {
    fn job_queue_config(&self) -> &JobQueueConfig {
        &self.job_queue_config
    }
}
// BroadcastChan access methods
impl ChanJobQueueRepositoryImpl {
    pub fn broadcast_cancel_chan(&self) -> &BroadcastChan<Vec<u8>> {
        &self.broadcast_cancel_chan_buf
    }
}

impl UseProstCodec for ChanJobQueueRepositoryImpl {}
impl UseJobqueueAndCodec for ChanJobQueueRepositoryImpl {}
impl ChanJobQueueRepository for ChanJobQueueRepositoryImpl {}

#[async_trait]
impl JobQueueCancellationRepository for ChanJobQueueRepositoryImpl {
    /// Memory environment cancellation notification broadcast (using BroadcastChan)
    async fn broadcast_job_cancellation(&self, job_id: &JobId) -> Result<()> {
        let job_id_bytes = Self::serialize_message(job_id)?;

        match self.broadcast_cancel_chan().send(job_id_bytes) {
            Ok(sent) => {
                if sent {
                    tracing::info!(
                        "Broadcasted cancellation for job {} in memory environment",
                        job_id.value
                    );
                } else {
                    tracing::warn!(
                        "No receivers for cancellation broadcast of job {}",
                        job_id.value
                    );
                }
                Ok(())
            }
            Err(e) => {
                tracing::error!(
                    "Failed to broadcast cancellation for job {}: {:?}",
                    job_id.value,
                    e
                );
                Err(anyhow::anyhow!("Broadcast error: {:?}", e))
            }
        }
    }

    /// Subscribe to job cancellation notifications with timeout and cleanup support
    ///
    /// **BroadcastChan implementation (Memory environment)** also supports timeout functionality
    /// **timeout=0 means unlimited**: No automatic timeout, waits until cleanup signal
    async fn subscribe_job_cancellation_with_timeout(
        &self,
        callback: Box<dyn Fn(JobId) -> BoxFuture<'static, Result<()>> + Send + Sync + 'static>,
        job_timeout_ms: u64,
        mut cleanup_receiver: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<()> {
        let broadcast_chan = self.broadcast_cancel_chan().clone();

        // timeout=0 means unlimited (no timeout), otherwise job timeout + 30 seconds margin
        let timeout_duration = if job_timeout_ms == 0 {
            None
        } else {
            Some(std::time::Duration::from_millis(job_timeout_ms + 30_000))
        };

        if let Some(timeout) = timeout_duration {
            tracing::debug!(
                "Started memory cancellation subscription with {} ms timeout",
                timeout.as_millis()
            );
        } else {
            tracing::debug!("Started memory cancellation subscription with no timeout (unlimited)");
        }

        tokio::spawn(async move {
            use futures::StreamExt;
            use tokio_stream::wrappers::BroadcastStream;

            let receiver = broadcast_chan.receiver().await;
            let mut stream = BroadcastStream::new(receiver);

            loop {
                // Handle message reception with optional timeout
                let msg_opt = if let Some(timeout) = timeout_duration {
                    // With timeout
                    tokio::select! {
                        msg_result = tokio::time::timeout(timeout, stream.next()) => {
                            match msg_result {
                                Ok(msg) => Some(msg),
                                Err(_timeout) => {
                                    tracing::info!("Memory cancellation subscription timed out after {} ms", timeout.as_millis());
                                    break;
                                }
                            }
                        }
                        _ = &mut cleanup_receiver => {
                            tracing::debug!("Received cleanup signal, terminating memory subscription");
                            break;
                        }
                    }
                } else {
                    // No timeout (unlimited) - wait indefinitely
                    tokio::select! {
                        msg = stream.next() => Some(msg),
                        _ = &mut cleanup_receiver => {
                            tracing::debug!("Received cleanup signal, terminating memory subscription");
                            break;
                        }
                    }
                };

                // Process received message
                match msg_opt {
                    Some(Some(Ok(data))) => {
                        if let Ok(job_id) =
                            <ChanJobQueueRepositoryImpl as UseProstCodec>::deserialize_message::<
                                JobId,
                            >(&data)
                            && let Err(e) = callback(job_id).await
                        {
                            tracing::error!("Cancellation callback error: {:?}", e);
                        }
                    }
                    Some(Some(Err(e))) => {
                        tracing::error!("Broadcast receive error: {:?}", e);
                        break;
                    }
                    Some(None) => {
                        tracing::info!("Memory broadcast stream ended");
                        break;
                    }
                    None => {
                        // This case shouldn't happen, but handle it gracefully
                        break;
                    }
                }
            }

            tracing::debug!("Memory cancellation subscription terminated");
        });

        Ok(())
    }
}
impl ChanJobQueueRepositoryImpl {
    pub fn new(
        job_queue_config: Arc<JobQueueConfig>,
        chan_pool: ChanBuffer<Vec<u8>, Chan<ChanBufferItem<Vec<u8>>>>,
        broadcast_chan_buf: BroadcastChan<Vec<u8>>,
    ) -> Self {
        ChanJobQueueRepositoryImpl {
            chan_pool,
            job_queue_config,
            shared_buffer: Arc::new(Mutex::new(HashMap::new())),
            broadcast_cancel_chan_buf: broadcast_chan_buf,
        }
    }
}
pub trait UseChanJobQueueRepository {
    fn chan_job_queue_repository(&self) -> &ChanJobQueueRepositoryImpl;
}

#[cfg(test)]
// create test (functional test without mock)
mod test {
    use super::*;
    use crate::infra::JobQueueConfig;
    use command_utils::util::datetime;
    use jobworkerp_base::codec::ProstMessageCodec;
    use memory_utils::chan::ChanBuffer;
    use memory_utils::chan::ChanBufferItem;
    use memory_utils::chan::mpmc::Chan;
    use memory_utils::chan::mpmc::UseChanBuffer;
    use proto::jobworkerp::data::{Job, JobData, JobId, WorkerId};
    use std::sync::Arc;

    #[derive(Clone)]
    struct ChanJobQueueRepositoryImpl {
        job_queue_config: Arc<JobQueueConfig>,
        chan_buf: ChanBuffer<Vec<u8>, Chan<ChanBufferItem<Vec<u8>>>>,
        shared_buffer: Arc<Mutex<HashMap<String, Vec<Job>>>>,
    }
    impl UseJobQueueConfig for ChanJobQueueRepositoryImpl {
        fn job_queue_config(&self) -> &JobQueueConfig {
            &self.job_queue_config
        }
    }
    impl UseChanBuffer for ChanJobQueueRepositoryImpl {
        type Item = Vec<u8>;
        fn chan_buf(&self) -> &ChanBuffer<Vec<u8>, Chan<ChanBufferItem<Vec<u8>>>> {
            &self.chan_buf
        }
    }
    impl UseChanQueueBuffer for ChanJobQueueRepositoryImpl {
        fn queue_list_buffer(&self) -> &Mutex<HashMap<String, Vec<Job>>> {
            &self.shared_buffer
        }
    }
    impl UseProstCodec for ChanJobQueueRepositoryImpl {}
    impl UseJobqueueAndCodec for ChanJobQueueRepositoryImpl {}
    impl ChanJobQueueRepository for ChanJobQueueRepositoryImpl {}

    // #[tokio::test]
    // async fn test_wait_for_result_queue_for_response_timeout() -> Result<()> {
    //     let chan_buf = ChanBuffer::new(None, 10000);
    //     let job_queue_config = Arc::new(JobQueueConfig {
    //         expire_job_result_seconds: 10,
    //         fetch_interval: 1000,
    //     });
    //     let repo = ChanJobQueueRepositoryImpl {
    //         job_queue_config,
    //         chan_buf: chan_buf.clone(),
    //         shared_buffer: Arc::new(Mutex::new(HashMap::new())),
    //     };

    //     let job_id = JobId { value: 12345 };

    //     // Test timeout behavior - should timeout after 100ms
    //     let start = std::time::Instant::now();
    //     let result = repo
    //         .wait_for_result_queue_for_response(&job_id, Some(100), false)
    //         .await;
    //     let elapsed = start.elapsed();

    //     // Should have timed out and returned an error
    //     assert!(result.is_err());
    //     // Should have taken approximately 100ms (allowing for some variance)
    //     assert!(elapsed >= std::time::Duration::from_millis(90));
    //     assert!(elapsed <= std::time::Duration::from_millis(500));

    //     println!("Timeout test completed in {elapsed:?}");
    //     Ok(())
    // }

    // #[tokio::test]
    // async fn send_job_test() -> Result<()> {
    //     let chan_buf = ChanBuffer::new(None, 10000);
    //     let job_queue_config = Arc::new(JobQueueConfig {
    //         expire_job_result_seconds: 10,
    //         fetch_interval: 1000,
    //     });
    //     let repo = ChanJobQueueRepositoryImpl {
    //         job_queue_config,
    //         chan_buf: chan_buf.clone(),
    //         shared_buffer: Arc::new(Mutex::new(HashMap::new())),
    //     };
    //     let args = ProstMessageCodec::serialize_message(&proto::TestArgs {
    //         args: vec!["test".to_string()],
    //     })?;
    //     let job_id = JobId { value: 123 };
    //     let job_id2 = JobId { value: 321 };
    //     let job = Job {
    //         id: job_id.into(),
    //         data: Some(JobData {
    //             worker_id: Some(WorkerId { value: 1 }),
    //             args,
    //             uniq_key: Some("test".to_string()),
    //             enqueue_time: datetime::now_millis(),
    //             grabbed_until_time: None,
    //             run_after_time: 0i64,
    //             retried: 0,
    //             priority: Priority::High as i32,
    //             timeout: 1000,
    //             request_streaming: false,
    //             streaming_type: 0,
    //             using: None,
    //         }),
    //         metadata: HashMap::new(),
    //     };
    //     let r = repo.enqueue_job(None, &job).await?;
    //     assert_eq!(r, 1);
    //     assert_eq!(repo.queue_list_buffer().lock().await.len(), 1);
    //     assert_eq!(
    //         repo.find_from_queue(None, &job_id).await?,
    //         Some(job.clone())
    //     );
    //     assert_eq!(repo.find_from_queue(None, &job_id2).await?, None);
    //     assert_eq!(
    //         repo.find_multi_from_queue(None, None).await?,
    //         vec![job.clone()]
    //     );

    //     assert_eq!(repo.find_from_queue(None, &job_id2).await?, None);
    //     let mut hash_set: HashSet<i64> = [job_id2.value].iter().cloned().collect();
    //     assert_eq!(
    //         repo.find_multi_from_queue(None, Some(&hash_set)).await?,
    //         vec![]
    //     );
    //     hash_set.insert(job_id.value);
    //     assert_eq!(
    //         repo.find_multi_from_queue(None, Some(&hash_set)).await?,
    //         vec![job.clone()]
    //     );

    //     assert_eq!(
    //         chan_buf
    //             .get_chan_if_exists(JobqueueAndCodec::queue_channel_name(
    //                 ChanJobQueueRepositoryImpl::DEFAULT_CHANNEL_NAME,
    //                 Some(&1),
    //             ))
    //             .await
    //             .unwrap()
    //             .count(),
    //         1
    //     );
    //     Ok(())
    // }
    // // test of 'send_result()': store job result with send_result() to chan and get job result value from wait_for_result_data_directly()
    // #[tokio::test]
    // async fn send_result_test() -> Result<()> {
    //     let chan_buf = ChanBuffer::new(None, 10000);
    //     let job_queue_config = Arc::new(JobQueueConfig {
    //         expire_job_result_seconds: 10,
    //         fetch_interval: 1000,
    //     });
    //     let repo = Arc::new(ChanJobQueueRepositoryImpl {
    //         job_queue_config,
    //         chan_buf,
    //         shared_buffer: Arc::new(Mutex::new(HashMap::new())),
    //     });
    //     let job_result_id = JobResultId { value: 111 };
    //     let job_id = JobId { value: 1 };
    //     let job_result_data = JobResultData {
    //         job_id: Some(job_id),
    //         status: ResultStatus::Success as i32,
    //         output: Some(ResultOutput {
    //             items: vec!["test".as_bytes().to_owned()],
    //         }),
    //         timeout: 2000,
    //         enqueue_time: datetime::now_millis() - 10000,
    //         run_after_time: datetime::now_millis() - 10000,
    //         start_time: datetime::now_millis() - 1000,
    //         end_time: datetime::now_millis(),
    //         ..Default::default()
    //     };
    //     // let r = repo.send_result_direct(job_result_data.clone()).await?;
    //     // assert!(r);
    //     // let res = repo.wait_for_result_data_for_response(&job_id).await?;
    //     let repo2 = repo.clone();
    //     let jr2 = job_result_data.clone();
    //     let jh = tokio::task::spawn(async move {
    //         let res = repo2
    //             .wait_for_result_queue_for_response(&job_id, None, false)
    //             .await
    //             .unwrap();
    //         assert_eq!(res.data.unwrap(), jr2);
    //     });
    //     tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    //     let r = repo
    //         .enqueue_result_direct(&job_result_id, &job_result_data, None)
    //         .await?;
    //     assert!(r);
    //     jh.await?;

    //     Ok(())
    // }

    // test of 'receive_job_from_channels()' : get job from chan and check the value (priority)
    #[tokio::test]
    async fn send_jobs_and_receive_job_test() -> Result<()> {
        let chan_buf = ChanBuffer::new(None, 10000);
        let job_queue_config = Arc::new(JobQueueConfig {
            expire_job_result_seconds: 10,
            fetch_interval: 1000,
            channel_capacity: 10000,
            pubsub_channel_capacity: 128,
            max_channels: 10_000,
        });
        let repo = ChanJobQueueRepositoryImpl {
            job_queue_config,
            chan_buf,
            shared_buffer: Arc::new(Mutex::new(HashMap::new())),
        };
        let args = ProstMessageCodec::serialize_message(&proto::TestArgs {
            args: vec!["test".to_string()],
        })?;
        let job1 = Job {
            id: Some(JobId { value: 1 }),
            data: Some(JobData {
                worker_id: Some(WorkerId { value: 1 }),
                args: args.clone(),
                uniq_key: None,
                enqueue_time: datetime::now_millis(),
                grabbed_until_time: None,
                run_after_time: 0i64,
                retried: 0,
                priority: Priority::Low as i32,
                timeout: 1000,
                streaming_type: 0,
                using: None,
            }),
            metadata: HashMap::new(),
        };
        let job2 = Job {
            id: Some(JobId { value: 2 }),
            data: Some(JobData {
                worker_id: Some(WorkerId { value: 1 }),
                args: args.clone(),
                uniq_key: None,
                enqueue_time: datetime::now_millis(),
                grabbed_until_time: None,
                run_after_time: 0i64,
                retried: 0,
                priority: Priority::High as i32,
                timeout: 1000,
                streaming_type: 0,
                using: None,
            }),
            metadata: HashMap::new(),
        };
        let r = repo.enqueue_job(None, &job1).await?;
        assert_eq!(r, 1);
        let r = repo.enqueue_job(None, &job2).await?;
        assert_eq!(r, 1);
        assert_eq!(
            repo.find_multi_from_queue(None, None).await?,
            vec![job2.clone(), job1.clone(),]
        );

        let qn1 = ChanJobQueueRepositoryImpl::queue_channel_name(
            ChanJobQueueRepositoryImpl::DEFAULT_CHANNEL_NAME,
            Some(Priority::Low as i32).as_ref(),
        );
        let qn2 = ChanJobQueueRepositoryImpl::queue_channel_name(
            ChanJobQueueRepositoryImpl::DEFAULT_CHANNEL_NAME,
            Some(Priority::High as i32).as_ref(),
        );
        let qcn = vec![qn2, qn1];
        // receive job from multiple channels
        let res = repo.receive_job_from_channels(qcn.clone()).await?;
        assert_eq!(res.id.unwrap(), job2.id.unwrap());
        assert_eq!(
            repo.find_multi_from_queue(None, None).await?,
            vec![job1.clone()]
        );
        let res = repo.receive_job_from_channels(qcn).await?;
        assert_eq!(res.id.unwrap(), job1.id.unwrap());
        assert_eq!(repo.find_multi_from_queue(None, None).await?, vec![]);
        Ok(())
    }

    #[tokio::test]
    async fn test_job_cancellation_protobuf_broadcast() -> Result<()> {
        use super::super::JobQueueCancellationRepository;
        use futures::future::BoxFuture;
        use memory_utils::chan::broadcast::BroadcastChan;
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let chan_buf = ChanBuffer::new(None, 10000);
        let job_queue_config = Arc::new(JobQueueConfig {
            expire_job_result_seconds: 10,
            fetch_interval: 1000,
            channel_capacity: 10000,
            pubsub_channel_capacity: 128,
            max_channels: 10_000,
        });
        let broadcast_chan_buf = BroadcastChan::new(100);
        let repo =
            super::ChanJobQueueRepositoryImpl::new(job_queue_config, chan_buf, broadcast_chan_buf);

        // Test job ID
        let test_job_id = JobId { value: 12345 };

        // Set up receiver to capture the cancellation message
        let received_job_ids = Arc::new(Mutex::new(Vec::new()));
        let received_job_ids_clone = received_job_ids.clone();

        let callback: Box<dyn Fn(JobId) -> BoxFuture<'static, Result<()>> + Send + Sync + 'static> =
            Box::new(move |job_id: JobId| {
                let received_job_ids = received_job_ids_clone.clone();
                Box::pin(async move {
                    received_job_ids.lock().await.push(job_id);
                    Ok(())
                }) as BoxFuture<'static, Result<()>>
            });

        let (tx, rx) = tokio::sync::oneshot::channel();
        // Start subscription
        repo.subscribe_job_cancellation_with_timeout(callback, 1000, rx)
            .await?;

        // Small delay to ensure subscription is active
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Broadcast cancellation
        repo.broadcast_job_cancellation(&test_job_id).await?;

        // Wait for message to be processed
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let received = received_job_ids.lock().await;
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].value, test_job_id.value);

        // cleanup
        let _ = tx.send(());

        tracing::info!("Successfully sent and received JobId via protobuf in memory cancellation");
        Ok(())
    }
}
