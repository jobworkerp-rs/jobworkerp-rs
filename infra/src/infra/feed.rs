pub mod chan;
pub mod redis;

use anyhow::{Result, bail};
use async_trait::async_trait;
use jobworkerp_runner::runner::FeedData;
use proto::jobworkerp::data::{FeedDataTransport, JobId};

/// Publish feed data to a running streaming job via `EnqueueWithClientStream`.
/// Implementations deliver data to the runner via in-process channels (Standalone)
/// or Redis List + bridge task (Scalable).
#[async_trait]
pub trait FeedPublisher: Send + Sync + std::fmt::Debug {
    async fn publish_feed(&self, job_id: &JobId, data: Vec<u8>, is_final: bool) -> Result<()>;

    /// Publish an opaque input frame or typed control frame. Older publisher
    /// implementations remain source-compatible but fail closed for controls.
    async fn publish_frame(&self, job_id: &JobId, frame: FeedDataTransport) -> Result<()> {
        validate_feed_data_transport(&frame)?;
        if frame.pty_resize_control.is_some() {
            bail!("feed publisher does not support typed PTY resize controls");
        }
        self.publish_feed(job_id, frame.data, frame.is_final).await
    }
}

/// Redis List key for buffered feed data delivery (Scalable mode)
pub fn job_feed_buf_key(job_id: &JobId) -> String {
    format!("job_feed_buf:{}", job_id.value)
}

pub fn feed_data_from_transport(msg: FeedDataTransport) -> Result<FeedData> {
    validate_feed_data_transport(&msg)?;
    Ok(FeedData {
        data: msg.data,
        is_final: msg.is_final,
        pty_resize_control: msg.pty_resize_control,
    })
}

/// Validate frame shape and PTY dimensions before a feed is enqueued or
/// forwarded. Opaque data frames are left byte-for-byte unchanged.
pub fn validate_feed_data_transport(msg: &FeedDataTransport) -> Result<()> {
    FeedData::validate_frame(&msg.data, msg.is_final, msg.pty_resize_control.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug, Default)]
    struct LegacyFeedPublisher {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl FeedPublisher for LegacyFeedPublisher {
        async fn publish_feed(
            &self,
            _job_id: &JobId,
            _data: Vec<u8>,
            _is_final: bool,
        ) -> Result<()> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    #[test]
    fn legacy_feed_encoding_keeps_data_and_final_tags_and_default_control_absent() {
        let frame = FeedDataTransport {
            data: vec![0, 255],
            is_final: true,
            pty_resize_control: None,
        };

        assert_eq!(frame.encode_to_vec(), vec![0x0a, 2, 0, 255, 0x10, 1]);
        let decoded = FeedDataTransport::decode(frame.encode_to_vec().as_slice()).unwrap();
        assert_eq!(decoded.data, vec![0, 255]);
        assert!(decoded.is_final);
        assert!(decoded.pty_resize_control.is_none());
    }

    #[test]
    fn resize_control_round_trips_dimensions_without_using_data_or_final() {
        let frame = FeedDataTransport {
            data: Vec::new(),
            is_final: false,
            pty_resize_control: Some(proto::jobworkerp::data::PtyResizeControl {
                rows: 24,
                cols: 80,
            }),
        };

        let decoded = FeedDataTransport::decode(frame.encode_to_vec().as_slice()).unwrap();
        validate_feed_data_transport(&decoded).unwrap();
        assert!(decoded.data.is_empty());
        assert!(!decoded.is_final);
        let control = decoded.pty_resize_control.unwrap();
        assert_eq!((control.rows, control.cols), (24, 80));
    }

    #[test]
    fn resize_control_validation_rejects_ambiguous_or_out_of_range_frames() {
        let controls = [(0, 80), (24, 0), (1001, 80), (24, 1001)];
        for (rows, cols) in controls {
            let frame = FeedDataTransport {
                pty_resize_control: Some(proto::jobworkerp::data::PtyResizeControl { rows, cols }),
                ..Default::default()
            };
            assert!(validate_feed_data_transport(&frame).is_err());
        }

        let mixed = FeedDataTransport {
            data: b"must not become stdin".to_vec(),
            pty_resize_control: Some(proto::jobworkerp::data::PtyResizeControl {
                rows: 24,
                cols: 80,
            }),
            ..Default::default()
        };
        assert!(validate_feed_data_transport(&mixed).is_err());

        let final_control = FeedDataTransport {
            is_final: true,
            pty_resize_control: Some(proto::jobworkerp::data::PtyResizeControl {
                rows: 24,
                cols: 80,
            }),
            ..Default::default()
        };
        assert!(validate_feed_data_transport(&final_control).is_err());
    }

    #[test]
    fn resize_control_accepts_dimension_boundaries() {
        for (rows, cols) in [(1, 1), (1000, 1000)] {
            let frame = FeedDataTransport {
                pty_resize_control: Some(proto::jobworkerp::data::PtyResizeControl { rows, cols }),
                ..Default::default()
            };
            validate_feed_data_transport(&frame).unwrap();
        }
    }

    #[test]
    fn opaque_feed_bytes_remain_unchanged_when_no_control_is_present() {
        let frame = FeedDataTransport {
            data: vec![0, 0xff, 0x04, 0x00],
            is_final: false,
            pty_resize_control: None,
        };

        validate_feed_data_transport(&frame).unwrap();
        let feed = feed_data_from_transport(frame).unwrap();
        assert_eq!(feed.data, vec![0, 0xff, 0x04, 0x00]);
        assert!(feed.pty_resize_control.is_none());
    }

    #[tokio::test]
    async fn legacy_publishers_reject_resize_without_publishing_an_empty_feed() {
        let publisher = LegacyFeedPublisher::default();
        let result = publisher
            .publish_frame(
                &JobId { value: 1 },
                FeedDataTransport {
                    pty_resize_control: Some(proto::jobworkerp::data::PtyResizeControl {
                        rows: 24,
                        cols: 80,
                    }),
                    ..Default::default()
                },
            )
            .await;

        assert!(result.is_err());
        assert_eq!(publisher.calls.load(Ordering::Relaxed), 0);
    }
}
