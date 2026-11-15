//! Integration test: `run.runner.options.channel` is a runtime expression, so a
//! workflow can send its jobs to a channel given in its input.
//!
//! A channel no worker serves leaves the job queued forever, so a run that
//! finishes proves the evaluated channel was used.
//!
//! Run with:
//! ```sh
//! cargo test --package tests-with-worker --test app_wrapper -- \
//!     workflow_runner_channel_expression_test --ignored --test-threads=1 --nocapture
//! ```
//!
//! Prerequisites: Redis must be accessible (Hybrid/Scalable mode). The backend
//! worker is started automatically by `start_test_worker`.

use anyhow::Result;
use app_wrapper::modules::test::create_test_app_wrapper_module;
use app_wrapper::workflow::definition::WorkflowLoader;
use app_wrapper::workflow::execute::workflow::WorkflowExecutor;
use futures::{StreamExt, pin_mut};
use infra_utils::infra::test::TEST_RUNTIME;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tests_with_worker::start_test_worker;

fn workflow_yaml(use_streaming: bool) -> String {
    format!(
        r#"document:
  dsl: 1.0.0
  namespace: test-channel
  name: runner-channel-expression
  version: 1.0.0
do:
  - echo:
      useStreaming: {use_streaming}
      run:
        runner:
          name: COMMAND
          options:
            channel: '${{$workflow.input.channel}}'
          arguments:
            command: echo
            args: ["routed"]
"#
    )
}

async fn run_on_input_channel(use_streaming: bool) -> Result<serde_json::Value> {
    let app_module = Arc::new(app::module::test::create_hybrid_test_app().await?);
    let (channel, _) = app_module
        .config_module
        .worker_config
        .channel_concurrency_pair()
        .into_iter()
        .next()
        .expect("a dispatched channel");
    let worker_handle = start_test_worker(app_module.clone()).await?;

    let workflow = WorkflowLoader::new_local_only()
        .load_workflow(None, Some(&workflow_yaml(use_streaming)), false)
        .await?;
    let executor = WorkflowExecutor::init(
        Arc::new(create_test_app_wrapper_module(app_module.clone())),
        app_module,
        Arc::new(workflow),
        Arc::new(json!({"channel": channel})),
        None,
        Arc::new(json!({})),
        Arc::new(HashMap::new()),
        None,
    )
    .await
    .map_err(|e| anyhow::anyhow!("Failed to initialize WorkflowExecutor: {e:?}"))?;

    let run = async {
        let stream = executor.execute_workflow(Arc::new(opentelemetry::Context::current()));
        pin_mut!(stream);
        let mut output = None;
        while let Some(result) = stream.next().await {
            let wfc = result.map_err(|e| anyhow::anyhow!("Workflow execution failed: {e:?}"))?;
            output = wfc.output.clone();
        }
        output.ok_or_else(|| anyhow::anyhow!("No output produced by workflow"))
    };
    let result = tokio::time::timeout(Duration::from_secs(30), run).await;
    worker_handle.shutdown().await;
    Ok((*result.map_err(|_| anyhow::anyhow!("the job was not processed"))??).clone())
}

#[test]
#[ignore = "Requires Redis backend worker"]
fn test_runner_channel_is_taken_from_the_workflow_input() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let output = run_on_input_channel(false).await?;
        assert_eq!(output["stdout"], json!("routed"));
        Ok(())
    })
}

#[test]
#[ignore = "Requires Redis backend worker"]
fn test_streaming_runner_channel_is_taken_from_the_workflow_input() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let output = run_on_input_channel(true).await?;
        assert_eq!(output["stdout"], json!("routed"));
        Ok(())
    })
}
