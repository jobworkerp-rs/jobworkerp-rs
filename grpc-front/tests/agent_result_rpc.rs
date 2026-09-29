use app::app::job::ChannelJobResultFuture;
use futures::StreamExt;
use grpc_front::proto::jobworkerp::data::{
    JobId, JobResult, JobResultData, JobResultId, QueueType, ResponseType, ResultOutput,
    ResultStatus, Worker as WorkerConfig, WorkerData, WorkerId,
};
use grpc_front::proto::jobworkerp::service::JobRequest;
use grpc_front::proto::jobworkerp::service::job_request::Worker;
use grpc_front::service::job::{
    enqueue_for_result_with, job_result_stream, job_result_stream_from_future, response_with_job_id,
};
use prost::Message;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);

impl Drop for DropSignal {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

fn worker(queue_type: QueueType, response_type: ResponseType) -> WorkerConfig {
    WorkerConfig {
        id: Some(WorkerId { value: 11 }),
        data: Some(WorkerData {
            queue_type: queue_type as i32,
            response_type: response_type as i32,
            ..Default::default()
        }),
    }
}

fn request() -> JobRequest {
    JobRequest {
        worker: Some(Worker::WorkerId(WorkerId { value: 11 })),
        ..Default::default()
    }
}

fn result(status: ResultStatus) -> JobResult {
    JobResult {
        id: Some(JobResultId { value: 900 }),
        data: Some(JobResultData {
            job_id: Some(JobId { value: 42 }),
            status: status as i32,
            output: Some(ResultOutput {
                items: b"job output".to_vec(),
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn result_future(value: JobResult) -> ChannelJobResultFuture {
    ChannelJobResultFuture::new(Box::pin(async move { Ok(Some(value)) }), None)
}

fn enqueue_callback(
    enqueued: Arc<AtomicBool>,
) -> impl FnOnce(
    WorkerConfig,
    JobRequest,
) -> std::future::Ready<anyhow::Result<(JobId, ChannelJobResultFuture)>> {
    move |_, _| {
        enqueued.store(true, Ordering::SeqCst);
        std::future::ready(Ok((
            JobId { value: 42 },
            result_future(result(ResultStatus::Success)),
        )))
    }
}

#[tokio::test]
async fn response_returns_job_id_before_final_result_is_polled() {
    let pending_result =
        Box::pin(async { std::future::pending::<anyhow::Result<Option<JobResult>>>().await });
    let response = response_with_job_id(JobId { value: 42 }, job_result_stream(pending_result));

    let id_bytes = response
        .metadata()
        .get_bin(grpc_front::service::JOB_ID_HEADER_NAME)
        .expect("eager job id metadata")
        .to_bytes()
        .expect("binary metadata bytes");
    assert_eq!(JobId::decode(id_bytes).unwrap().value, 42);
}

#[tokio::test]
async fn success_and_failure_are_each_returned_as_one_typed_result() {
    for expected in [
        result(ResultStatus::Success),
        result(ResultStatus::FatalError),
    ] {
        let (result_future, _) = result_future(expected.clone()).into_parts();
        let mut stream = job_result_stream(result_future);

        let actual = stream
            .next()
            .await
            .expect("one final JobResult")
            .expect("job failures are typed results, not RPC errors");
        assert_eq!(actual, expected);
        assert!(stream.next().await.is_none(), "exactly one result is sent");
    }
}

#[tokio::test]
async fn dropping_result_response_releases_spawned_subscription_without_stopping_job() {
    let repositories = infra::infra::module::rdb::test::setup_test_rdb_module(false).await;
    let pubsub = repositories.chan_job_result_pubsub_repository.clone();
    let job_id = JobId { value: 987654321 };
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (subscription_dropped_tx, subscription_dropped_rx) = tokio::sync::oneshot::channel();
    let subscription_job_id = job_id;
    let subscription = tokio::spawn(async move {
        let _drop_signal = DropSignal(Some(subscription_dropped_tx));
        pubsub
            .subscribe_result_with_ready(&subscription_job_id, None, Some(ready_tx))
            .await
            .map(Some)
    });
    let subscription_abort_handle = subscription.abort_handle();
    ready_rx
        .await
        .expect("subscriber must register before the response is dropped");

    let result_future = ChannelJobResultFuture::new_with_result_wait_abort_handle(
        Box::pin(async move { subscription.await? }),
        None,
        subscription_abort_handle,
    );
    let response = response_with_job_id(job_id, job_result_stream_from_future(result_future));

    let (job_started_tx, job_started_rx) = tokio::sync::oneshot::channel();
    let job_task = tokio::spawn(async move {
        let _ = job_started_tx.send(());
        std::future::pending::<()>().await;
    });
    job_started_rx.await.expect("job task must start");

    drop(response.into_inner());

    tokio::time::timeout(Duration::from_secs(1), subscription_dropped_rx)
        .await
        .expect("disconnect must release the subscription promptly")
        .expect("subscription task must be dropped");
    assert!(
        !job_task.is_finished(),
        "dropping the response must not stop the enqueued job"
    );
    job_task.abort();
    let _ = job_task.await;
}

#[tokio::test]
async fn direct_worker_without_result_storage_or_broadcast_is_accepted() {
    let mut worker = worker(QueueType::Normal, ResponseType::Direct);
    let data = worker.data.as_mut().unwrap();
    data.store_success = false;
    data.store_failure = false;
    data.broadcast_results = false;
    let enqueued = Arc::new(AtomicBool::new(false));

    let (job_id, _) = enqueue_for_result_with(
        request(),
        worker,
        async { Ok(()) },
        enqueue_callback(enqueued.clone()),
    )
    .await
    .expect("direct result does not depend on persistence/broadcast flags");

    assert_eq!(job_id.value, 42);
    assert!(enqueued.load(Ordering::SeqCst));
}

#[tokio::test]
async fn unsupported_requests_and_worker_configs_never_enqueue() {
    let cases = [
        (
            JobRequest {
                run_after_time: Some(10),
                ..request()
            },
            worker(QueueType::Normal, ResponseType::Direct),
        ),
        (
            request(),
            WorkerConfig {
                data: Some(WorkerData {
                    periodic_interval: 100,
                    queue_type: QueueType::Normal as i32,
                    response_type: ResponseType::Direct as i32,
                    ..Default::default()
                }),
                ..worker(QueueType::Normal, ResponseType::Direct)
            },
        ),
        (request(), worker(QueueType::DbOnly, ResponseType::Direct)),
        (request(), worker(QueueType::Normal, ResponseType::NoResult)),
    ];

    for (request, worker) in cases {
        let enqueued = Arc::new(AtomicBool::new(false));
        let checked_capability = Arc::new(AtomicBool::new(false));
        let check_marker = checked_capability.clone();
        let response = enqueue_for_result_with(
            request,
            worker,
            async move {
                check_marker.store(true, Ordering::SeqCst);
                Ok(())
            },
            enqueue_callback(enqueued.clone()),
        )
        .await;

        assert!(response.is_err());
        assert!(!checked_capability.load(Ordering::SeqCst));
        assert!(!enqueued.load(Ordering::SeqCst));
    }
}

#[tokio::test]
async fn client_stream_only_capability_failure_never_enqueues() {
    let checked_capability = Arc::new(AtomicBool::new(false));
    let check_marker = checked_capability.clone();
    let enqueued = Arc::new(AtomicBool::new(false));
    let response = enqueue_for_result_with(
        request(),
        worker(QueueType::WithBackup, ResponseType::Direct),
        async move {
            check_marker.store(true, Ordering::SeqCst);
            Err(anyhow::anyhow!("runner requires client streaming input"))
        },
        enqueue_callback(enqueued.clone()),
    )
    .await;

    assert!(response.is_err());
    assert!(checked_capability.load(Ordering::SeqCst));
    assert!(!enqueued.load(Ordering::SeqCst));
}
