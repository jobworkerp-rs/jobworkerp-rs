//! End-to-end check of the stream error contract through a real worker.
//! Requires Redis (uses create_hybrid_test_app / Scalable mode).

use anyhow::{Context, Result, ensure};
use futures::StreamExt;
use infra_utils::infra::test::TEST_RUNTIME;
use jobworkerp_runner::jobworkerp::runner::CommandArgs;
use prost::Message;
use proto::jobworkerp::data::{
    JobId, JobResultData, ResponseType, ResultStatus, StreamErrorCode, StreamingType, Trailer,
    WorkerData, WorkerId, result_output_item,
};
use std::{collections::HashMap, sync::Arc};
use tests_with_worker::start_test_worker;
use tokio::time::{Duration, timeout};

async fn command_stream_end(
    app: &app::module::AppModule,
    worker_id: &WorkerId,
    script: &str,
) -> Result<(JobId, Trailer)> {
    let args = CommandArgs {
        command: "/bin/sh".into(),
        args: vec!["-c".into(), script.into()],
        treat_nonzero_as_error: true,
        ..Default::default()
    };
    let (job_id, result, stream) = timeout(
        Duration::from_secs(60),
        app.job_app.enqueue_job(
            Arc::new(HashMap::new()),
            Some(worker_id),
            None,
            args.encode_to_vec(),
            None,
            0,
            0,
            30000,
            None,
            StreamingType::Response,
            None,
            None,
        ),
    )
    .await
    .context("COMMAND enqueue timed out")??;
    let mut stream =
        stream.with_context(|| format!("COMMAND must return a response stream: {result:?}"))?;
    while let Some(item) = timeout(Duration::from_secs(60), stream.next())
        .await
        .context("COMMAND stream stalled")?
    {
        if let Some(result_output_item::Item::End(trailer)) = item.item {
            return Ok((job_id, trailer));
        }
    }
    anyhow::bail!("COMMAND stream ended without an End item")
}

async fn create_command_worker(
    app: &app::module::AppModule,
    store_results: bool,
) -> Result<WorkerId> {
    let catalog = app.runner_app.find_runner_list(false, None, None).await?;
    let runner_id = catalog
        .into_iter()
        .find(|runner| runner.data.as_ref().is_some_and(|d| d.name == "COMMAND"))
        .and_then(|runner| runner.id)
        .context("COMMAND runner must be registered")?;
    app.worker_app
        .create(&WorkerData {
            name: format!("stream_error_e2e_{}", app.job_app.generate_job_id()?.value),
            runner_id: Some(runner_id),
            use_static: false,
            response_type: ResponseType::Direct as i32,
            store_success: store_results,
            store_failure: store_results,
            ..Default::default()
        })
        .await
}

/// Waits for the stored result to reach `expected`: a failed stream settles
/// the stored result after the stream is drained, which can be just after the
/// consumer has read the End.
async fn stored_result(
    app: &app::module::AppModule,
    job_id: &JobId,
    expected: ResultStatus,
) -> Result<JobResultData> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let stored = app
            .job_result_app
            .find_job_result_list_by_job_id(job_id)
            .await?
            .into_iter()
            .find_map(|result| result.data);
        match stored {
            Some(data) if data.status() == expected => return Ok(data),
            stored if tokio::time::Instant::now() >= deadline => {
                anyhow::bail!("stored result never became {expected:?}: {stored:?}")
            }
            _ => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
}

#[test]
fn command_stream_end_reports_success_and_failure() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = Arc::new(app::module::test::create_hybrid_test_app().await?);
        let worker_id = create_command_worker(&app, false).await?;

        let handle = start_test_worker(app.clone()).await?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let result = async {
            let (_, success) = command_stream_end(&app, &worker_id, "echo ok").await?;
            ensure!(success.error.is_none(), "unexpected failure: {success:?}");

            let (_, failure) = command_stream_end(&app, &worker_id, "echo failing; exit 3").await?;
            let error = failure
                .error
                .as_ref()
                .with_context(|| format!("End must carry the failure: {failure:?}"))?;
            ensure!(error.code == StreamErrorCode::ExecutionFailed as i32);
            ensure!(error.origin == "COMMAND");
            ensure!(
                failure
                    .metadata
                    .contains_key(proto::stream_error::STREAM_ERROR_METADATA_KEY),
                "the v1 JSON value is dual-written: {failure:?}"
            );
            Ok::<(), anyhow::Error>(())
        }
        .await;
        handle.shutdown().await;
        result
    })
}

#[test]
fn command_stream_failure_settles_the_stored_result() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = Arc::new(app::module::test::create_hybrid_test_app().await?);
        let worker_id = create_command_worker(&app, true).await?;

        let handle = start_test_worker(app.clone()).await?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let result = async {
            let (job_id, _) = command_stream_end(&app, &worker_id, "echo failing; exit 3").await?;
            let failure = stored_result(&app, &job_id, ResultStatus::MaxRetry).await?;
            let output =
                String::from_utf8_lossy(&failure.output.unwrap_or_default().items).into_owned();
            ensure!(
                output.contains("COMMAND EXECUTION_FAILED"),
                "the stored output must describe the failure: {output}"
            );

            let (job_id, _) = command_stream_end(&app, &worker_id, "echo ok").await?;
            stored_result(&app, &job_id, ResultStatus::Success).await?;
            Ok::<(), anyhow::Error>(())
        }
        .await;
        handle.shutdown().await;
        result
    })
}
