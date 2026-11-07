//! Enqueues registered workers whose live response stream backs an AG-UI run.

use anyhow::{Context, Result, bail};
use app::app::job::ChannelJobResultFuture;
use app::app::job::execute::{UseJobExecutor, WorkerForEnqueue};
use app::module::AppModule;
use proto::jobworkerp::data::{JobId, QueueType, ResponseType, StreamingType, Worker, WorkerData};
use std::collections::HashMap;
use std::sync::Arc;

/// Whether enqueueing to this worker yields a live response stream. Mirrors the
/// job layer, which subscribes to results before enqueueing only for immediate
/// Direct-response jobs on a Redis-backed queue.
fn streams_live_response(worker: &WorkerData) -> bool {
    worker.response_type == ResponseType::Direct as i32
        && worker.periodic_interval == 0
        && (worker.queue_type == QueueType::Normal as i32
            || worker.queue_type == QueueType::WithBackup as i32)
}

/// A registered worker checked to stream its response to an AG-UI client.
///
/// Callers validate before committing state that a later enqueue failure would
/// have to undo, then enqueue the same worker without looking it up again.
pub(crate) struct LiveStreamWorker(Worker);

impl LiveStreamWorker {
    pub(crate) fn try_new(worker_name: &str, worker: Worker) -> Result<Self> {
        let data = worker
            .data
            .as_ref()
            .with_context(|| format!("worker '{worker_name}' has no data"))?;
        if !streams_live_response(data) {
            bail!(
                "worker '{worker_name}' cannot provide a live response stream: it must be a \
                 non-periodic Direct-response worker on a Normal or WithBackup queue"
            );
        }
        Ok(Self(worker))
    }

    pub(crate) fn data(&self) -> &WorkerData {
        self.0.data.as_ref().expect("validated in try_new")
    }
}

/// Find a registered worker that can stream its response to an AG-UI client.
pub(crate) async fn resolve_live_stream_worker(
    app_module: &AppModule,
    worker_name: &str,
) -> Result<LiveStreamWorker> {
    let worker = app_module
        .worker_app
        .find_by_name(worker_name)
        .await?
        .with_context(|| format!("worker '{worker_name}' is not registered"))?;
    LiveStreamWorker::try_new(worker_name, worker)
}

/// Enqueue `method` with protobuf arguments and subscribe to its response stream,
/// as the gRPC streaming enqueue does. The job ID is returned before the result so
/// the run can be cancelled while it streams.
pub(crate) async fn enqueue_live_stream(
    app_module: &AppModule,
    worker: LiveStreamWorker,
    method: &str,
    arguments: Vec<u8>,
    timeout_sec: u32,
) -> Result<(JobId, ChannelJobResultFuture)> {
    app_module
        .function_app
        .enqueue_with_worker_or_temp_channel(
            Arc::new(HashMap::new()),
            WorkerForEnqueue::Existing(worker.0),
            arguments,
            None,
            timeout_sec,
            StreamingType::Response,
            Some(method.to_string()),
            None,
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker_data(
        response_type: ResponseType,
        periodic_interval: u32,
        queue: QueueType,
    ) -> WorkerData {
        WorkerData {
            response_type: response_type as i32,
            periodic_interval,
            queue_type: queue as i32,
            ..Default::default()
        }
    }

    #[test]
    fn live_response_requires_an_immediate_direct_worker_on_a_redis_capable_queue() {
        assert!(streams_live_response(&worker_data(
            ResponseType::Direct,
            0,
            QueueType::Normal
        )));
        assert!(streams_live_response(&worker_data(
            ResponseType::Direct,
            0,
            QueueType::WithBackup
        )));
        assert!(!streams_live_response(&worker_data(
            ResponseType::NoResult,
            0,
            QueueType::Normal
        )));
        assert!(!streams_live_response(&worker_data(
            ResponseType::Direct,
            1000,
            QueueType::Normal
        )));
        assert!(!streams_live_response(&worker_data(
            ResponseType::Direct,
            0,
            QueueType::DbOnly
        )));
    }

    #[test]
    fn live_stream_worker_rejects_missing_data_and_non_streaming_workers() {
        let streaming = Worker {
            id: None,
            data: Some(worker_data(ResponseType::Direct, 0, QueueType::Normal)),
        };
        assert_eq!(
            LiveStreamWorker::try_new("live", streaming.clone())
                .unwrap()
                .data(),
            streaming.data.as_ref().unwrap()
        );
        assert!(LiveStreamWorker::try_new("empty", Worker::default()).is_err());
        let no_result = Worker {
            id: None,
            data: Some(worker_data(ResponseType::NoResult, 0, QueueType::Normal)),
        };
        let error = LiveStreamWorker::try_new("no-result", no_result)
            .err()
            .expect("NoResult workers cannot stream");
        assert!(error.to_string().contains("live response stream"));
    }
}
