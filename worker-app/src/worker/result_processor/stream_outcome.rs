//! Final outcome of a result stream, used to settle the stored result of a
//! streaming job once its stream has been drained.
//!
//! A streaming job's result is recorded when the runner returns its stream, so
//! it reports success even if the stream later ends with a failed End.

use futures::StreamExt;
use futures::stream::BoxStream;
use proto::jobworkerp::data::{
    JobResultData, ResultOutput, ResultOutputItem, ResultStatus, StreamError, result_output_item,
};
use proto::stream_error::StreamErrorOutcome;
use std::sync::{Arc, OnceLock};

/// Records the outcome of the first End that passes through an observed stream.
#[derive(Clone, Debug, Default)]
pub(super) struct StreamEndProbe(Arc<OnceLock<StreamErrorOutcome>>);

impl StreamEndProbe {
    /// Outcome of the first End, or `None` if no End has passed yet.
    pub(super) fn outcome(&self) -> Option<StreamErrorOutcome> {
        self.0.get().cloned()
    }
}

/// Whether the End of the stream can change what was recorded: only a recorded
/// success can turn out wrong (any other status was decided before the stream
/// started), and only if it is kept somewhere (RDB or the listen_after cache).
pub(super) fn can_be_settled(recorded: &JobResultData) -> bool {
    recorded.status == ResultStatus::Success as i32
        && (recorded.store_success || recorded.store_failure || recorded.broadcast_results)
}

/// Passes `stream` through unchanged while recording its first End in the probe.
pub(super) fn observe_end(
    stream: BoxStream<'static, ResultOutputItem>,
) -> (BoxStream<'static, ResultOutputItem>, StreamEndProbe) {
    let probe = StreamEndProbe::default();
    let recorder = probe.0.clone();
    let observed = stream
        .inspect(move |item| {
            if let Some(result_output_item::Item::End(trailer)) = &item.item {
                // Later Ends break the stream contract; the first one decides.
                let _ = recorder.set(proto::stream_error::inspect_end(trailer));
            }
        })
        .boxed();
    (observed, probe)
}

/// The result to store instead of the recorded success once the stream ended
/// with `error`.
pub(super) fn settled_result(recorded: &JobResultData, error: &StreamError) -> JobResultData {
    JobResultData {
        status: proto::stream_error::result_status_for(error.code) as i32,
        output: Some(ResultOutput {
            items: proto::stream_error::stream_failure_output(error).into_bytes(),
        }),
        error: Some(error.clone()),
        ..recorded.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::jobworkerp::data::{JobId, StreamErrorCode, Trailer, WorkerId};
    use std::collections::HashMap;

    fn data_item(bytes: &[u8]) -> ResultOutputItem {
        ResultOutputItem {
            item: Some(result_output_item::Item::Data(bytes.to_vec())),
        }
    }

    fn failed_end(code: StreamErrorCode) -> ResultOutputItem {
        proto::stream_error::error_end_item(HashMap::new(), code, "boom", "COMMAND")
    }

    fn success_end() -> ResultOutputItem {
        proto::stream_error::normal_end_item(HashMap::new())
    }

    async fn drain(items: Vec<ResultOutputItem>) -> (Vec<ResultOutputItem>, StreamEndProbe) {
        let (stream, probe) = observe_end(futures::stream::iter(items).boxed());
        (stream.collect().await, probe)
    }

    fn recorded_success() -> JobResultData {
        JobResultData {
            job_id: Some(JobId { value: 1 }),
            worker_id: Some(WorkerId { value: 2 }),
            status: ResultStatus::Success as i32,
            retried: 1,
            start_time: 10,
            end_time: 20,
            store_success: true,
            store_failure: true,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn failed_end_passes_through_and_is_recorded() {
        let items = vec![
            data_item(b"a"),
            failed_end(StreamErrorCode::ExecutionFailed),
        ];
        let (received, probe) = drain(items.clone()).await;
        assert_eq!(received, items);
        match probe.outcome() {
            Some(StreamErrorOutcome::Failed(error)) => {
                assert_eq!(error.code, StreamErrorCode::ExecutionFailed as i32);
                assert_eq!(error.origin, "COMMAND");
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    #[tokio::test]
    async fn successful_end_is_recorded_as_success() {
        let (_, probe) = drain(vec![data_item(b"a"), success_end()]).await;
        assert_eq!(probe.outcome(), Some(StreamErrorOutcome::Success));
    }

    #[tokio::test]
    async fn stream_without_end_records_nothing() {
        let (_, probe) = drain(vec![data_item(b"a")]).await;
        assert_eq!(probe.outcome(), None);
    }

    #[tokio::test]
    async fn stream_dropped_before_its_end_records_nothing() {
        let (mut stream, probe) =
            observe_end(futures::stream::iter(vec![data_item(b"a"), success_end()]).boxed());
        assert!(stream.next().await.is_some());
        drop(stream);
        assert_eq!(probe.outcome(), None);
    }

    #[tokio::test]
    async fn only_the_first_end_counts() {
        let (_, probe) = drain(vec![
            success_end(),
            failed_end(StreamErrorCode::ExecutionFailed),
        ])
        .await;
        assert_eq!(probe.outcome(), Some(StreamErrorOutcome::Success));
    }

    #[tokio::test]
    async fn end_with_only_the_legacy_plugin_key_is_a_failure() {
        let legacy = ResultOutputItem {
            item: Some(result_output_item::Item::End(Trailer {
                metadata: HashMap::from([(
                    proto::stream_error::LEGACY_PLUGIN_STREAM_ERROR_KEY.to_string(),
                    "plugin failed".to_string(),
                )]),
                error: None,
            })),
        };
        let (_, probe) = drain(vec![legacy]).await;
        assert!(matches!(
            probe.outcome(),
            Some(StreamErrorOutcome::Failed(_))
        ));
    }

    #[test]
    fn only_a_kept_success_can_be_settled() {
        assert!(can_be_settled(&recorded_success()));

        let mut failed = recorded_success();
        failed.status = ResultStatus::Cancelled as i32;
        assert!(!can_be_settled(&failed), "status decided before the stream");

        let kept_by = |store_success, store_failure, broadcast_results| {
            can_be_settled(&JobResultData {
                store_success,
                store_failure,
                broadcast_results,
                ..recorded_success()
            })
        };
        assert!(kept_by(true, false, false));
        assert!(kept_by(false, true, false));
        assert!(kept_by(false, false, true));
        assert!(!kept_by(false, false, false), "kept nowhere");
    }

    #[test]
    fn failure_settles_status_error_and_output() {
        let error = proto::stream_error::classified_error(
            StreamErrorCode::ExecutionFailed,
            "exit 3",
            "COMMAND",
        );
        let recorded = recorded_success();
        let settled = settled_result(&recorded, &error);

        assert_eq!(settled.status, ResultStatus::MaxRetry as i32);
        assert_eq!(settled.error, Some(error));
        assert_eq!(
            settled.output,
            Some(ResultOutput {
                items: b"stream error: COMMAND EXECUTION_FAILED: exit 3".to_vec(),
            })
        );
        // Everything else is the recorded result as it was.
        let mut unchanged = settled.clone();
        unchanged.status = recorded.status;
        unchanged.error = recorded.error.clone();
        unchanged.output = recorded.output.clone();
        assert_eq!(unchanged, recorded);
    }
}
