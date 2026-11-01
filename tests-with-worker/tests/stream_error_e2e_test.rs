//! End-to-end check of the stream error contract through a real worker.
//! Requires Redis (uses create_hybrid_test_app / Scalable mode).

use anyhow::{Context, Result, ensure};
use futures::StreamExt;
use infra_utils::infra::test::TEST_RUNTIME;
use jobworkerp_runner::jobworkerp::runner::CommandArgs;
use prost::Message;
use proto::jobworkerp::data::{
    ResponseType, StreamErrorCode, StreamingType, Trailer, WorkerData, WorkerId, result_output_item,
};
use std::{collections::HashMap, sync::Arc};
use tests_with_worker::start_test_worker;
use tokio::time::{Duration, timeout};

async fn command_stream_end(
    app: &app::module::AppModule,
    worker_id: &WorkerId,
    script: &str,
) -> Result<Trailer> {
    let args = CommandArgs {
        command: "/bin/sh".into(),
        args: vec!["-c".into(), script.into()],
        treat_nonzero_as_error: true,
        ..Default::default()
    };
    let (_, result, stream) = timeout(
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
            return Ok(trailer);
        }
    }
    anyhow::bail!("COMMAND stream ended without an End item")
}

#[test]
fn command_stream_end_reports_success_and_failure() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = Arc::new(app::module::test::create_hybrid_test_app().await?);
        let catalog = app.runner_app.find_runner_list(false, None, None).await?;
        let runner_id = catalog
            .into_iter()
            .find(|runner| runner.data.as_ref().is_some_and(|d| d.name == "COMMAND"))
            .and_then(|runner| runner.id)
            .context("COMMAND runner must be registered")?;
        let worker_id = app
            .worker_app
            .create(&WorkerData {
                name: format!("stream_error_e2e_{}", app.job_app.generate_job_id()?.value),
                runner_id: Some(runner_id),
                use_static: false,
                response_type: ResponseType::Direct as i32,
                store_success: false,
                store_failure: false,
                ..Default::default()
            })
            .await?;

        let handle = start_test_worker(app.clone()).await?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let result = async {
            let success = command_stream_end(&app, &worker_id, "echo ok").await?;
            ensure!(success.error.is_none(), "unexpected failure: {success:?}");

            let failure = command_stream_end(&app, &worker_id, "echo failing; exit 3").await?;
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
