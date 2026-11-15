//! Integration tests: a streaming run task must subscribe to the child job's
//! result stream before the job becomes visible to a worker.
//!
//! Redis pub/sub drops messages nobody is subscribed to, so a job that
//! finishes before the subscription loses its whole stream and the task waits
//! until its timeout. The Redis tests replace the worker with a fake one that
//! publishes the stream the moment it pops the job, which is the worst case
//! for a late subscription.
//!
//! Run with:
//! ```sh
//! cargo test --package tests-with-worker --test app_wrapper -- \
//!     workflow_stream_subscribe_before_enqueue_test --test-threads=1 --nocapture
//! ```
//!
//! Prerequisites: Redis must be accessible (Hybrid/Scalable mode).

use anyhow::{Context as _, Result};
use app::app::job::execute::JobExecutorWrapper;
use app::module::AppModule;
use app_wrapper::workflow::{
    definition::{WorkflowLoader, workflow},
    execute::{
        context::{TaskContext, WorkflowContext, WorkflowStatus, WorkflowStreamEvent},
        task::{NamedTimeouts, StreamTaskExecutorTrait, stream::run::RunStreamTaskExecutor},
    },
};
use futures::StreamExt;
use infra::infra::job::rows::{JobqueueAndCodec, UseJobqueueAndCodec};
use infra::infra::job_result::pubsub::JobResultPublisher;
use infra::infra::job_result::pubsub::redis::RedisJobResultPubSubRepositoryImpl;
use infra_utils::infra::redis::UseRedisClient;
use infra_utils::infra::test::TEST_RUNTIME;
use jobworkerp_runner::jobworkerp::runner::CommandResult;
use prost::Message;
use proto::jobworkerp::data::{
    Priority, QueueType, ResponseType, ResultOutputItem, RunnerId, Trailer, WorkerData,
    result_output_item,
};
use redis::AsyncCommands;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tests_with_worker::start_test_worker;
use tokio::sync::{Mutex, RwLock};

/// No real worker serves this channel, so only the fake worker pops its jobs.
const FAKE_WORKER_CHANNEL: &str = "issue400-fake-worker";
/// Short enough that a lost stream fails the test quickly instead of hanging.
const TASK_TIMEOUT: Duration = Duration::from_secs(5);
const FAKE_STDOUT: &str = "fake worker output";

fn command_worker_data(name: &str, channel: Option<&str>) -> WorkerData {
    WorkerData {
        name: name.to_string(),
        description: "Test worker for streaming subscription order".to_string(),
        runner_id: Some(RunnerId { value: 1 }), // COMMAND runner
        runner_settings: vec![],
        retry_policy: None,
        periodic_interval: 0,
        channel: channel.map(str::to_string),
        queue_type: QueueType::Normal as i32,
        response_type: ResponseType::Direct as i32,
        store_success: true,
        store_failure: true,
        use_static: false,
        broadcast_results: true,
    }
}

fn runner_task(channel: Option<&str>) -> Result<workflow::RunTask> {
    let mut runner = json!({
        "name": "COMMAND",
        "arguments": {"command": "echo", "args": [FAKE_STDOUT]},
    });
    if let Some(channel) = channel {
        runner["options"] = json!({"channel": channel});
    }
    Ok(serde_json::from_value(json!({
        "run": {"runner": runner},
        "useStreaming": true,
    }))?)
}

fn worker_task(worker_name: &str) -> Result<workflow::RunTask> {
    Ok(serde_json::from_value(json!({
        "run": {"worker": {
            "name": worker_name,
            "arguments": {"command": "echo", "args": [FAKE_STDOUT]},
        }},
        "useStreaming": true,
    }))?)
}

/// Runs one streaming run task and returns its output.
async fn run_streaming_task(
    app_module: Arc<AppModule>,
    run_task: workflow::RunTask,
) -> Result<serde_json::Value> {
    let flow = WorkflowLoader::new_local_only()
        .load_workflow(
            Some(&format!(
                "{}/../app-wrapper/test-files/ls-test.yaml",
                env!("CARGO_MANIFEST_DIR")
            )),
            None,
            false,
        )
        .await?;
    let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
        &flow,
        Arc::new(json!({})),
        Arc::new(json!({})),
        None,
    )));
    workflow_context.write().await.status = WorkflowStatus::Running;

    let executor = RunStreamTaskExecutor::new(
        workflow_context,
        Some(TASK_TIMEOUT),
        Arc::new(NamedTimeouts::new()),
        Arc::new(JobExecutorWrapper::new(app_module)),
        run_task,
        Arc::new(HashMap::new()),
    );
    let task_context = TaskContext::new(
        None,
        Arc::new(json!({})),
        Arc::new(Mutex::new(Default::default())),
    );
    let stream = executor.execute_stream(
        Arc::new(opentelemetry::Context::new()),
        Arc::new("stream_subscribe_order".to_string()),
        task_context,
    );
    futures::pin_mut!(stream);

    let collect = async {
        while let Some(event) = stream.next().await {
            match event {
                Ok(WorkflowStreamEvent::StreamingJobCompleted { context, .. }) => {
                    return Ok(context.raw_output.as_ref().clone());
                }
                Ok(_) => {}
                Err(e) => anyhow::bail!("streaming task failed: {e:?}"),
            }
        }
        anyhow::bail!("streaming task ended without completing")
    };
    // The task timeout bounds each stream item; this also bounds a stuck setup.
    tokio::time::timeout(TASK_TIMEOUT * 2, collect)
        .await
        .context("streaming task did not finish")?
}

/// Pops one job from the fake worker's channel, records how many clients
/// listen on its result stream at that moment, then publishes a successful
/// COMMAND stream for it right away.
async fn serve_one_job_as_fake_worker(
    publisher: RedisJobResultPubSubRepositoryImpl,
) -> Result<i64> {
    let mut connection = publisher
        .redis_client()
        .get_multiplexed_async_connection_with_config(
            &redis::AsyncConnectionConfig::new().set_response_timeout(None),
        )
        .await?;
    let queue =
        JobqueueAndCodec::queue_channel_name(FAKE_WORKER_CHANNEL, Some(&(Priority::Medium as i32)));
    let (_, payload): (String, Vec<u8>) = connection
        .blpop::<_, Option<(String, Vec<u8>)>>(&queue, TASK_TIMEOUT.as_secs_f64())
        .await?
        .context("no job was enqueued to the fake worker channel")?;
    let (job, _) = JobqueueAndCodec::deserialize_job_internal(&payload)?;
    let job_id = job.id.context("dequeued job has no id")?;

    let (_, subscribers) = publisher
        .numsub(
            &mut connection,
            &JobqueueAndCodec::job_result_stream_pubsub_channel_name(&job_id),
        )
        .await?;

    let collected = CommandResult {
        exit_code: Some(0),
        stdout: Some(FAKE_STDOUT.to_string()),
        ..Default::default()
    }
    .encode_to_vec();
    let items = vec![
        ResultOutputItem {
            item: Some(result_output_item::Item::FinalCollected(collected)),
        },
        ResultOutputItem {
            item: Some(result_output_item::Item::End(Trailer::default())),
        },
    ];
    publisher
        .publish_result_stream_data(job_id, futures::stream::iter(items).boxed())
        .await?;
    Ok(subscribers)
}

async fn assert_subscribed_before_enqueue(
    app_module: Arc<AppModule>,
    run_task: workflow::RunTask,
) -> Result<()> {
    let publisher = app_module
        .repositories
        .redis_module
        .as_ref()
        .context("hybrid app has no redis module")?
        .redis_job_result_pubsub_repository
        .clone();
    let fake_worker = tokio::spawn(serve_one_job_as_fake_worker(publisher));

    let output = run_streaming_task(app_module, run_task).await;
    let subscribers = match (output.as_ref(), fake_worker.await?) {
        // A task failing before the enqueue leaves the fake worker without a
        // job; report the task's error instead of only the missing job.
        (Err(task_error), Err(worker_error)) => {
            anyhow::bail!("streaming task failed: {task_error:#}; fake worker: {worker_error:#}")
        }
        (_, worker) => worker?,
    };

    assert!(
        subscribers >= 1,
        "the result stream had no subscriber when the job was dequeued"
    );
    assert_eq!(output?["stdout"], json!(FAKE_STDOUT));
    Ok(())
}

#[test]
fn test_streaming_runner_subscribes_before_enqueue() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app_module = Arc::new(app::module::test::create_hybrid_test_app().await?);
        assert_subscribed_before_enqueue(app_module, runner_task(Some(FAKE_WORKER_CHANNEL))?).await
    })
}

#[test]
fn test_streaming_worker_subscribes_before_enqueue() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app_module = Arc::new(app::module::test::create_hybrid_test_app().await?);
        let worker_name = "issue400-fake-channel-worker";
        let worker_id = app_module
            .worker_app
            .create(&command_worker_data(worker_name, Some(FAKE_WORKER_CHANNEL)))
            .await?;
        let result =
            assert_subscribed_before_enqueue(app_module.clone(), worker_task(worker_name)?).await;
        let _ = app_module.worker_app.delete(&worker_id).await;
        result
    })
}

/// Standalone delivers the stream through in-process broadcast channels, and
/// the app layer also subscribes for Direct workers; both subscriptions must
/// coexist without losing items. `worker_name` selects `run.worker` (with a
/// Direct worker) instead of `run.runner`.
async fn run_standalone_streaming_task(worker_name: Option<&str>) -> Result<serde_json::Value> {
    let app_module = Arc::new(app::module::test::create_rdb_chan_test_app(false, false).await?);
    let (task, worker_id) = match worker_name {
        Some(name) => {
            let worker_id = app_module
                .worker_app
                .create(&command_worker_data(name, None))
                .await?;
            (worker_task(name)?, Some(worker_id))
        }
        None => (runner_task(None)?, None),
    };
    let worker_handle = start_test_worker(app_module.clone()).await?;
    let output = run_streaming_task(app_module.clone(), task).await;
    worker_handle.shutdown().await;
    if let Some(worker_id) = worker_id {
        let _ = app_module.worker_app.delete(&worker_id).await;
    }
    output
}

#[test]
fn test_standalone_streaming_runner_completes() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let output = run_standalone_streaming_task(None).await?;
        assert_eq!(output["stdout"], json!(FAKE_STDOUT));
        Ok(())
    })
}

#[test]
fn test_standalone_streaming_direct_worker_completes() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let output =
            run_standalone_streaming_task(Some("issue400-standalone-direct-worker")).await?;
        assert_eq!(output["stdout"], json!(FAKE_STDOUT));
        Ok(())
    })
}
