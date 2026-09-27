//! Local end-to-end SANDBOX check. Requires Redis, Linux/KVM, msb and an OCI image.
//! Run with `UV_VENV_CLEAR=1 cargo test -p tests-with-worker --test sandbox_worker_e2e_test -- --ignored --test-threads=1 --nocapture`.

use anyhow::{Context, Result, ensure};
use futures::StreamExt;
use infra_utils::infra::test::TEST_RUNTIME;
use jobworkerp_runner::jobworkerp::runner::{
    SandboxExecArgs, SandboxExecResult, SandboxRunnerSettings, SandboxVmConfig, sandbox_exec_result,
};
use prost::Message;
use proto::jobworkerp::data::{ResponseType, StreamingType, WorkerData, result_output_item};
use std::{collections::HashMap, sync::Arc};
use tests_with_worker::start_test_worker_with_sandbox;
use tokio::time::{Duration, timeout};

#[test]
#[ignore = "requires Redis, Linux/KVM, microsandbox runtime and alpine:3.21 image"]
fn sandbox_executes_in_real_worker() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = Arc::new(app::module::test::create_hybrid_test_app().await?);
        let catalog = app.runner_app.find_runner_list(false, None, None).await?;
        let runner = catalog
            .into_iter()
            .find(|runner| {
                runner
                    .data
                    .as_ref()
                    .is_some_and(|data| data.name == "SANDBOX")
            })
            .context("SANDBOX must be visible in the RunnerService FindList catalog")?;
        let runner_id = runner.id.context("SANDBOX runner ID is missing")?;
        let settings = SandboxRunnerSettings {
            vm: Some(SandboxVmConfig {
                image: Some("alpine:3.21".into()),
                cpus: Some(1),
                memory_mib: Some(512),
                root_disk_mib: Some(1024),
                ..Default::default()
            }),
            allowed_images: vec!["alpine:3.21".into()],
            ..Default::default()
        };
        let worker = app
            .worker_app
            .create(&WorkerData {
                name: format!("sandbox_e2e_{}", app.job_app.generate_job_id()?.value),
                runner_id: Some(runner_id),
                runner_settings: settings.encode_to_vec(),
                use_static: false,
                response_type: ResponseType::Direct as i32,
                store_success: false,
                store_failure: false,
                ..Default::default()
            })
            .await?;

        let handle = start_test_worker_with_sandbox(app.clone()).await?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let result = async {
            let args = SandboxExecArgs {
                command: "sh".into(),
                args: vec!["-c".into(), "printf sandbox-e2e-ok".into()],
                ..Default::default()
            };
            let (_, result, stream) = timeout(
                Duration::from_secs(180),
                app.job_app.enqueue_job(
                    Arc::new(HashMap::new()),
                    Some(&worker),
                    None,
                    args.encode_to_vec(),
                    None,
                    0,
                    0,
                    120000,
                    None,
                    StreamingType::Response,
                    Some("run".into()),
                    None,
                ),
            )
            .await
            .context("SANDBOX enqueue timed out")??;
            let mut stream = stream
                .with_context(|| format!("SANDBOX must return a response stream: {result:?}"))?;
            let mut output = Vec::new();
            let mut exit = None;
            let mut ended = false;
            while let Some(item) = timeout(Duration::from_secs(180), stream.next())
                .await
                .context("SANDBOX stream stalled")?
            {
                match item.item {
                    Some(result_output_item::Item::Data(bytes)) => {
                        let decoded = SandboxExecResult::decode(bytes.as_slice())?;
                        match decoded.result {
                            Some(sandbox_exec_result::Result::Output(chunk)) => {
                                output.extend(chunk.data)
                            }
                            Some(sandbox_exec_result::Result::Exit(status)) => {
                                exit = Some(status.exit_code)
                            }
                            None => anyhow::bail!("SANDBOX returned empty result"),
                        }
                    }
                    Some(result_output_item::Item::End(trailer)) => {
                        ensure!(
                            !trailer.metadata.contains_key("stream_error"),
                            "SANDBOX failed: {trailer:?}"
                        );
                        ended = true;
                        break;
                    }
                    _ => {}
                }
            }
            ensure!(ended, "SANDBOX response did not end");
            ensure!(exit == Some(0), "SANDBOX exit code: {exit:?}");
            ensure!(output == b"sandbox-e2e-ok", "SANDBOX output: {output:?}");
            Ok::<(), anyhow::Error>(())
        }
        .await;
        handle.shutdown().await;
        result
    })
}
